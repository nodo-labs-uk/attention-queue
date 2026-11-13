use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::claude::{ClaudeProc, list_claude_procs};
use super::procs::{is_ancestor, pid_alive};
use super::{Agent, AgentConn, checkout_of};
use crate::model::AgentId;

pub const LIST_PANES_FORMAT: &str = "#{session_name}|#{window_index}|#{window_name}|#{pane_id}|#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{@claude_waiting}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub session: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub command: String,
    pub cwd: PathBuf,
    pub waiting: bool,
}

/// Parses `list-panes -F LIST_PANES_FORMAT` output. The first six fields are split
/// from the left and the waiting flag from the right, so a `|` in the path survives.
/// A `|` in the session or window name cannot be told apart and drops the line.
pub fn parse_list_panes(s: &str) -> Vec<Pane> {
    s.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Pane> {
    let (rest, waiting) = line.rsplit_once('|')?;
    let mut it = rest.splitn(7, '|');
    let session = it.next()?;
    let window_index = it.next()?.parse().ok()?;
    let window_name = it.next()?;
    let pane_id = it.next()?;
    let pane_pid = it.next()?.parse().ok()?;
    let command = it.next()?;
    let cwd = it.next()?;
    Some(Pane {
        session: session.into(),
        window_index,
        window_name: window_name.into(),
        pane_id: pane_id.into(),
        pane_pid,
        command: command.into(),
        cwd: cwd.into(),
        waiting: waiting == "1",
    })
}

/// The most a tmux call may take.
const TMUX_TIMEOUT: Duration = Duration::from_secs(5);
/// `claude agents --json` is costly (a node process, about 150 MB), so it
/// runs again only when the panes change, or after this long.
const PROCS_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct TmuxConn {
    /// `Some(name)` adds `-L name` (private server, used by tests).
    pub socket: Option<String>,
    /// The last `claude agents --json` list and what it was taken for.
    pub procs: Arc<Mutex<Option<ProcsSeen>>>,
}

/// A `claude agents --json` list, the panes it was taken with and when.
#[derive(Debug, Clone)]
pub struct ProcsSeen {
    pub panes: Vec<PaneKey>,
    pub at: Instant,
    pub procs: Vec<ClaudeProc>,
}

/// What of a pane says the agents in it may have changed: its id, pid,
/// command and waiting flag (a turn ending changes `claude`'s status).
pub type PaneKey = (String, u32, String, bool);

pub fn pane_keys(panes: &[Pane]) -> Vec<PaneKey> {
    panes
        .iter()
        .map(|p| (p.pane_id.clone(), p.pane_pid, p.command.clone(), p.waiting))
        .collect()
}

/// Whether `seen` can stand for the agents in `panes` at `now`.
pub fn procs_fresh(seen: Option<&ProcsSeen>, panes: &[PaneKey], now: Instant) -> bool {
    seen.is_some_and(|s| s.panes == panes && now.saturating_duration_since(s.at) < PROCS_EVERY)
}

impl TmuxConn {
    fn cmd(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(s) = &self.socket {
            c.args(["-L", s]);
        }
        c
    }

    fn list_panes(&self) -> Result<Vec<Pane>> {
        let mut cmd = self.cmd();
        cmd.args(["list-panes", "-a", "-F", LIST_PANES_FORMAT]);
        let (status, out, _) =
            super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux list-panes")?;
        anyhow::ensure!(status.success(), "tmux list-panes failed");
        Ok(parse_list_panes(&out))
    }

    /// Runs tmux with `args` and `input` on stdin, for at most `TMUX_TIMEOUT`.
    fn run(&self, args: &[&str], input: &str) -> Result<()> {
        let mut cmd = self.cmd();
        cmd.args(args);
        let (status, _, err) = super::run_for(cmd, input, TMUX_TIMEOUT)
            .with_context(|| format!("running tmux {}", args[0]))?;
        anyhow::ensure!(status.success(), "tmux {} failed: {}", args[0], err.trim());
        Ok(())
    }

    /// `pane`'s screen with its colours, on this connection's server.
    fn capture_styled(&self, pane: &str) -> Result<String> {
        let mut cmd = self.cmd();
        cmd.args(["capture-pane", "-p", "-e", "-t", pane]);
        let (status, out, err) =
            super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux capture-pane")?;
        anyhow::ensure!(status.success(), "{}", err.trim());
        Ok(out)
    }

    /// The claude processes, from the last list while the panes look the same.
    fn procs(&self, panes: &[Pane]) -> Result<Vec<ClaudeProc>> {
        let keys = pane_keys(panes);
        let mut seen = self.procs.lock().unwrap_or_else(|e| e.into_inner());
        if procs_fresh(seen.as_ref(), &keys, Instant::now())
            && let Some(s) = seen.as_ref()
        {
            return Ok(s.procs.clone());
        }
        let procs = list_claude_procs()?;
        *seen = Some(ProcsSeen {
            panes: keys,
            at: Instant::now(),
            procs: procs.clone(),
        });
        Ok(procs)
    }
}

/// Joins each claude process to the pane whose `pane_pid` is itself or an
/// ancestor of it. A claude started under another (by a hook or a tool,
/// as `claude -p`) is part of that session, not a session of its own.
fn join(
    panes: &[Pane],
    procs: &[ClaudeProc],
    is_ancestor: impl Fn(u32, u32) -> bool,
) -> Vec<Agent> {
    let mut agents = Vec::new();
    for p in procs {
        if procs.iter().any(|q| is_ancestor(q.pid, p.pid)) {
            continue;
        }
        let Some(pane) = panes
            .iter()
            .find(|pane| pane.pane_pid == p.pid || is_ancestor(pane.pane_pid, p.pid))
        else {
            continue;
        };
        agents.push(Agent {
            id: AgentId {
                pane: pane.pane_id.clone(),
                claude_pid: p.pid,
            },
            session: pane.session.clone(),
            window_index: pane.window_index,
            window_name: pane.window_name.clone(),
            cwd: pane.cwd.clone(),
            checkout: checkout_of(&pane.cwd),
            session_id: p.session_id.clone(),
            status: p.status,
            waiting: pane.waiting,
            input: super::InputBox::Unseen,
            permission_mode: None,
            suggestion: None,
            last_prompt: None,
        });
    }
    agents
}

/// How often, and (times `SENT_CHECKS`) how long, the input box is read
/// after a send.
const SENT_POLL: Duration = Duration::from_millis(250);
const SENT_CHECKS: u32 = 6;

/// Whether the input box on `screen` holds `sent`: as typed, or as the
/// placeholder Claude Code shows for a long paste.
fn holds(screen: &str, sent: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    match super::read_input_box(screen) {
        super::InputBox::Holding(t) => {
            let (t, sent) = (squash(&t), squash(sent));
            t.starts_with("[Pasted text")
                || (!t.is_empty() && sent.starts_with(&t))
                || t.starts_with(&sent)
        }
        _ => false,
    }
}

/// After a message was pasted and Enter pressed: if it still sits in the
/// input box, Enter did not take (it can arrive while Claude Code is
/// starting a turn of its own), so Enter is pressed once more. A message
/// still there after that is an error, so you hear of it.
fn confirm_sent(
    read: impl Fn() -> Result<String>,
    enter: impl Fn() -> Result<()>,
    sent: &str,
    poll: Duration,
) -> Result<()> {
    let still_there = || -> Result<bool> {
        for _ in 0..SENT_CHECKS {
            std::thread::sleep(poll);
            if !holds(&read()?, sent) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if !still_there()? {
        return Ok(());
    }
    enter()?;
    anyhow::ensure!(
        !still_there()?,
        "the message is still in its input box: press Enter in its window"
    );
    Ok(())
}

/// `text` without control characters other than newline and tab, C1 controls
/// (U+0080 to U+009F) included. Text pasted into a pane may come from diffs;
/// an escape sequence such as `ESC [201~` (end of bracketed paste) would
/// otherwise be read by the terminal as keystrokes.
pub fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

impl AgentConn for TmuxConn {
    fn list(&self) -> Result<Vec<Agent>> {
        let panes = self.list_panes()?;
        let procs = self.procs(&panes)?;
        let mut agents = join(&panes, &procs, is_ancestor);
        let server: Vec<&str> = self
            .socket
            .as_deref()
            .map(|s| vec!["-L", s])
            .unwrap_or_default();
        // Every session's footer, for the permission mode on its tab.
        for a in &mut agents {
            if let Ok(screen) = capture_pane(&a.id.pane, &server) {
                a.permission_mode = permission_mode_on(&screen);
            }
        }
        // Only an agent at its prompt can take a message, so only its screen
        // is read: one whose turn ended, or an idle one, which is how a
        // session that has not had a turn yet shows.
        for a in agents
            .iter_mut()
            .filter(|a| a.waiting || a.status == super::AgentStatus::Idle)
        {
            if let Ok(screen) = self.capture_styled(&a.id.pane) {
                a.input = super::read_input_box(&screen);
                a.suggestion = super::read_suggestion(&screen);
            }
        }
        for a in &mut agents {
            a.last_prompt = crate::transcript::last_prompt_at(&a.session_id);
        }
        Ok(agents)
    }

    fn send_text(&self, agent: &AgentId, text: &str) -> Result<()> {
        let pane = agent.pane.as_str();
        let text = strip_controls(text);
        self.run(&["load-buffer", "-b", "aq", "-"], &text)?;
        self.run(&["paste-buffer", "-p", "-d", "-b", "aq", "-t", pane], "")?;
        self.run(&["send-keys", "-t", pane, "Enter"], "")?;
        confirm_sent(
            || self.capture_styled(pane),
            || self.run(&["send-keys", "-t", pane, "Enter"], ""),
            &text,
            SENT_POLL,
        )
    }

    fn is_alive(&self, agent: &AgentId) -> bool {
        pid_alive(agent.claude_pid)
            && self
                .list_panes()
                .is_ok_and(|ps| ps.iter().any(|p| p.pane_id == agent.pane))
    }
}

/// The tmux option aq sets while it runs, so the status line can stop
/// marking waiting windows that aq already lists.
pub const OPEN_FLAG: &str = "@aq_open";

/// Sets or clears `OPEN_FLAG` on the default tmux server, and hides tmux's
/// status line while aq's window is the one showing. Without a server
/// there is nothing to mark, so failures are ignored.
pub fn mark_open(open: bool) {
    let mut cmd = Command::new("tmux");
    if open {
        cmd.args(["set-option", "-g", OPEN_FLAG, "1"]);
    } else {
        cmd.args(["set-option", "-gu", OPEN_FLAG]);
    }
    let _ = super::run_for(cmd, "", TMUX_TIMEOUT);
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        hide_status_line(&pane, open);
    }
}

/// The slot in the `session-window-changed` hook array that aq uses, out of
/// the way of hooks you set yourself.
const STATUS_HOOK: &str = "session-window-changed[4171]";

/// While `open`, aq's own session hides its status line whenever aq's
/// window (the one holding `pane`) is the current one, and shows it for
/// every other window: aq lists the sessions itself. When aq closes, the
/// hook goes and the session's status line follows the global setting again.
fn hide_status_line(pane: &str, open: bool) {
    let run = |args: &[&str]| {
        let mut cmd = Command::new("tmux");
        cmd.args(args);
        let _ = super::run_for(cmd, "", TMUX_TIMEOUT);
    };
    let Some(ids) = display(pane, "#{session_id} #{window_id}") else {
        return;
    };
    let Some((session, window)) = ids.split_once(' ') else {
        return;
    };
    if !open {
        run(&["set-hook", "-u", "-t", session, STATUS_HOOK]);
        run(&["set-option", "-u", "-t", session, "status"]);
        return;
    }
    // The flag test makes the hook do nothing once aq has closed, even if
    // it closed before it could remove the hook.
    let toggle = format!(
        "if-shell -F '#{{&&:#{{==:#{{window_id}},{window}}},#{{{OPEN_FLAG}}}}}' 'set-option status off' 'set-option -u status'"
    );
    run(&["set-hook", "-t", session, STATUS_HOOK, &toggle]);
    // Now as well: aq usually starts in the window that is showing.
    run(&[
        "if-shell",
        "-t",
        session,
        "-F",
        &format!("#{{==:#{{window_id}},{window}}}"),
        &format!("set-option -t {session} status off"),
    ]);
}

/// `format` expanded for `target`, or `None` if tmux cannot.
fn display(target: &str, format: &str) -> Option<String> {
    let mut cmd = Command::new("tmux");
    cmd.args(["display-message", "-p", "-t", target, format]);
    let (status, out, _) = super::run_for(cmd, "", TMUX_TIMEOUT).ok()?;
    status.success().then(|| out.trim().to_string())
}

/// Closes `pane`, ending what runs in it. `server` as for `capture_pane`.
pub fn kill_pane(pane: &str, server: &[&str]) -> Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.args(server).args(["kill-pane", "-t", pane]);
    let (status, _, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux kill-pane")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
}

/// A shell in `dir`, in a popup over aq's own pane; returns when it closes.
/// `typed` is copied (to the tmux buffer, and so the clipboard), and with
/// zsh it is also typed in at the prompt, not run. `zsh_dir` holds the
/// startup files that do the typing.
pub fn popup_shell(dir: &Path, title: &str, typed: Option<&str>, zsh_dir: &Path) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let mut title = title.to_string();
    let mut command = None;
    if let Some(typed) = typed {
        let _ = Command::new("tmux")
            .args(["set-buffer", "-w", "--", typed])
            .output();
        if shell.ends_with("/zsh") && write_zsh_files(zsh_dir, typed).is_ok() {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            let real = std::env::var("ZDOTDIR").unwrap_or(home);
            command = Some(format!(
                "env ZDOTDIR={} AQ_ZDOTDIR={} {}",
                shell_quote(&zsh_dir.to_string_lossy()),
                shell_quote(&real),
                shell_quote(&shell)
            ));
            title.push_str("· Claude's last ! command is typed in, Enter runs it ");
        } else {
            title.push_str("· Claude's last ! command is copied ");
        }
    }
    popup(dir, &title, command.as_deref())
}

/// `path` (under `dir`) opened at `line` in $VISUAL or $EDITOR, else vi,
/// in a popup over aq that closes with the editor.
pub fn popup_editor(dir: &Path, path: &Path, line: u32) -> Result<()> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let shown = path.display().to_string();
    // The editor setting may carry its own flags ("code --wait"), so it is
    // left for the shell to split; the path is quoted. A relative path
    // starts with ./ so a file named "+:!cmd" or "-c ..." is never read as
    // an option or an editor command.
    let arg = if path.is_absolute() {
        shown.clone()
    } else {
        format!("./{shown}")
    };
    let command = format!("{editor} +{line} {}", shell_quote(&arg));
    let title = format!(
        " {} · quit the editor to go back ",
        crate::visible::visible_code(&shown)
    );
    popup(dir, &title, Some(&command))
}

/// The key table the `/btw` popup's session uses: Ctrl-Q in it closes the
/// popup, and every other key falls through to the root table as usual.
const BTW_KEYS: &str = "aq-btw";

/// A session's own window in a popup across the top of aq, with `/btw `
/// typed in its box if `type_btw`. The popup attaches to a session of its
/// own, grouped with the agent's, so the agent's tmux session stays on the
/// window it shows. Ctrl-Q closes the popup and that session goes.
pub fn popup_btw(
    session: &str,
    window: u32,
    pane: &str,
    title: &str,
    type_btw: bool,
) -> Result<()> {
    let tmux = |args: &[&str]| -> Result<()> {
        let mut cmd = Command::new("tmux");
        cmd.args(args);
        let (status, _, err) = super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux")?;
        anyhow::ensure!(status.success(), "{}", err.trim());
        Ok(())
    };
    let view = format!("aq-btw-{}", std::process::id());
    let exact = format!("={view}");
    let _ = tmux(&["kill-session", "-t", &exact]);
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &view,
        "-t",
        &format!("={session}"),
    ])?;
    let shown = (|| {
        tmux(&["set-option", "-t", &view, "status", "off"])?;
        tmux(&["set-option", "-t", &view, "key-table", BTW_KEYS])?;
        tmux(&["bind-key", "-T", BTW_KEYS, "C-q", "detach-client"])?;
        tmux(&["select-window", "-t", &format!("{view}:{window}")])?;
        if type_btw {
            tmux(&["send-keys", "-t", pane, "-l", "/btw "])?;
        }
        // The popup runs a tmux client of its own on this server.
        let socket = std::env::var("TMUX").unwrap_or_default();
        let socket = socket.split(',').next().unwrap_or_default();
        anyhow::ensure!(!socket.is_empty(), "aq is not running inside tmux");
        let attach = format!(
            "TMUX= tmux -S {} attach -t {}",
            shell_quote(socket),
            shell_quote(&exact)
        );
        let mut cmd = Command::new("tmux");
        cmd.args([
            "display-popup",
            "-E",
            "-w",
            "100%",
            "-h",
            "60%",
            "-x",
            "0",
            "-y",
            "0",
            "-T",
            &format_literal(title),
        ]);
        if let Ok(pane) = std::env::var("TMUX_PANE") {
            cmd.args(["-t", &pane]);
        }
        cmd.arg(format_literal(&attach));
        let out = cmd.output().context("running tmux display-popup")?;
        anyhow::ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(())
    })();
    let _ = tmux(&["kill-session", "-t", &exact]);
    shown
}

/// A tmux popup over aq in `dir`, running `command` or else the shell.
fn popup(dir: &Path, title: &str, command: Option<&str>) -> Result<()> {
    let title = format_literal(title);
    let mut cmd = Command::new("tmux");
    cmd.args([
        "display-popup",
        "-E",
        "-w",
        "90%",
        "-h",
        "85%",
        "-T",
        &title,
        "-d",
    ])
    // tmux expands formats in the start directory too, and a folder name
    // is not ours to trust.
    .arg(format_literal(&dir.to_string_lossy()));
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        cmd.args(["-t", &pane]);
    }
    if let Some(c) = command {
        cmd.arg(format_literal(c));
    }
    let out = cmd.output().context("running tmux display-popup")?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Startup files for the popup's zsh: they load the user's own, then put
/// `typed` on the command line with `print -z`, once.
fn write_zsh_files(dir: &Path, typed: &str) -> Result<()> {
    crate::ipc::ensure_private_dir(dir)?;
    let q = shell_quote(&dir.to_string_lossy());
    std::fs::write(
        dir.join(".zshenv"),
        format!(
            "# Written by aq for its ! shell: your own startup files, then the\n\
             # command Claude suggested, typed in.\n\
             ZDOTDIR=$AQ_ZDOTDIR\n\
             [[ -r $ZDOTDIR/.zshenv ]] && source $ZDOTDIR/.zshenv\n\
             AQ_ZDOTDIR=$ZDOTDIR\n\
             ZDOTDIR={q}\n"
        ),
    )?;
    std::fs::write(
        dir.join(".zshrc"),
        format!(
            "ZDOTDIR=$AQ_ZDOTDIR\n\
             unset AQ_ZDOTDIR\n\
             [[ -r $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc\n\
             if [[ -r {q}/typed ]]; then\n  \
               print -z -- \"$(<{q}/typed)\"\n  \
               rm -f {q}/typed\n\
             fi\n"
        ),
    )?;
    std::fs::write(dir.join("typed"), typed)?;
    Ok(())
}

/// `s` in single quotes for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `claude` in a new tmux window in `dir`, left in the background, with
/// `prompt` as its first message. With `worktree`, Claude makes a git
/// worktree of `dir`'s repository and works there. The command goes through
/// the shell, so `claude` is found as in your own windows; the prompt is
/// quoted for it.
/// Returns the new window's pane.
pub fn new_claude(dir: &Path, prompt: Option<&str>, worktree: bool) -> Result<String> {
    let command = claude_command(prompt, worktree);
    let mut cmd = Command::new("tmux");
    cmd.args(["new-window", "-d", "-P", "-F", "#{pane_id}", "-c"])
        // The start directory is expanded as a format; the command is not.
        .arg(format_literal(&dir.to_string_lossy()))
        .arg(command);
    let (status, out, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux new-window")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(out.trim().to_string())
}

/// Starts `claude` in `dir` in tmux session `session`, as a new window
/// named after `dir`, or as the session's first if there is no such
/// session yet; `system` is added to its system prompt. Returns the pane.
/// `-d` leaves you where you are.
pub fn new_claude_in(session: &str, dir: &Path, system: &str) -> Result<String> {
    let exact = format!("={session}");
    let mut has = Command::new("tmux");
    has.args(["has-session", "-t", &exact]);
    let (status, _, _) =
        super::run_for(has, "", TMUX_TIMEOUT).context("running tmux has-session")?;
    let mut cmd = Command::new("tmux");
    if status.success() {
        cmd.args(["new-window", "-d", "-t", &format!("{exact}:")]);
    } else {
        cmd.args(["new-session", "-d", "-s", session]);
    }
    // The name is expanded as a format, like the start directory.
    if let Some(name) = dir.file_name() {
        cmd.arg("-n").arg(format_literal(&name.to_string_lossy()));
    }
    cmd.args(["-P", "-F", "#{pane_id}", "-c"])
        .arg(format_literal(&dir.to_string_lossy()))
        .arg(format!(
            "{} --append-system-prompt {}",
            claude_command(None, false),
            shell_quote(system)
        ));
    let (status, out, err) = super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(out.trim().to_string())
}

/// The second option of the menu Claude Code shows on its first start in a
/// folder nobody has trusted. Its hooks do not run until it is answered.
const TRUST_PROMPT: &str = "Yes, I trust this folder";

/// The banner Claude Code shows once it is past the trust menu, or never
/// showed it.
const STARTED: &str = "Claude Code v";

/// How long a new session has to show its banner or the trust menu.
const START_WAIT: Duration = Duration::from_secs(10);

/// What a new session's screen says about the trust menu: `Some(true)`
/// while it asks, `Some(false)` once Claude Code is past it, `None` while
/// it is still starting.
fn asks_for_trust(screen: &str) -> Option<bool> {
    if screen.contains(TRUST_PROMPT) {
        Some(true)
    } else if screen.contains(STARTED) {
        Some(false)
    } else {
        None
    }
}

/// Watches a new session's `pane` until Claude Code starts; true if it
/// stops to ask whether to trust the folder.
pub fn waits_for_trust(pane: &str) -> Result<bool> {
    let until = Instant::now() + START_WAIT;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(asks) = asks_for_trust(&capture_pane(pane, &[])?) {
            return Ok(asks);
        }
    }
    Ok(false)
}

/// Answers the trust menu in `pane` with yes. Number keys do not pick in
/// that menu; the cursor starts on "No, exit".
pub fn trust_folder(pane: &str) -> Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.args(["send-keys", "-t", pane, "Down", "Enter"]);
    let (status, _, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux send-keys")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
}

/// The flag comes last: it takes an optional name, and would take a prompt
/// after it as one.
fn claude_command(prompt: Option<&str>, worktree: bool) -> String {
    let mut command = "claude".to_string();
    if let Some(p) = prompt {
        command.push(' ');
        command.push_str(&shell_quote(p));
    }
    if worktree {
        command.push_str(" --worktree");
    }
    command
}

/// Puts `text` in the tmux buffer and, through `-w`, the clipboard.
pub fn copy(text: &str) -> Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.args(["load-buffer", "-w", "-"]);
    let (status, _, err) =
        super::run_for(cmd, text, TMUX_TIMEOUT).context("running tmux load-buffer")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
}

/// `text` for a tmux format, taken as written: a window name can be set by
/// the program in it, a folder can be named anything, and `#(...)` in a
/// format runs a command.
fn format_literal(text: &str) -> String {
    text.replace('#', "##")
}

/// Presses Escape in `pane`, which stops a Claude Code turn.
pub fn press_escape(pane: &str, server: &[&str]) -> Result<()> {
    press(pane, "Escape", server)
}

/// Presses Ctrl-L in `pane`, which makes Claude Code draw its screen again.
pub fn press_redraw(pane: &str, server: &[&str]) -> Result<()> {
    press(pane, "C-l", server)
}

fn press(pane: &str, key: &str, server: &[&str]) -> Result<()> {
    let mut cmd = Command::new("tmux");
    cmd.args(server).args(["send-keys", "-t", pane, key]);
    let (status, _, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux send-keys")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
}

/// The heading of the menu Claude Code shows when `/model` is sent to a
/// session with history: the new model has to re-read it.
const SWITCH_MODEL_MENU: &str = "Switch model?";

/// What Claude Code prints once the model is switched.
const MODEL_IS_SET: &str = "Set model to";

/// How long to watch for the menu after `/model` is sent.
const SWITCH_MODEL_WAIT: Duration = Duration::from_secs(3);

/// After `/model` was sent to `pane`: says yes to Claude Code's "Switch
/// model?" menu if it comes up, since the model was already picked in aq.
/// Its first option, yes, is the one Enter takes. `before` is the pane's
/// screen from before the send: a new "Set model to" line means no menu.
pub fn confirm_model_switch(pane: &str, before: &str, server: &[&str]) -> Result<()> {
    let set = |screen: &str| screen.matches(MODEL_IS_SET).count();
    let until = Instant::now() + SWITCH_MODEL_WAIT;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(150));
        let screen = capture_pane(pane, server)?;
        if screen.contains(SWITCH_MODEL_MENU) {
            return press(pane, "Enter", server);
        }
        if set(&screen) != set(before) {
            break;
        }
    }
    Ok(())
}

/// Claude Code's permission modes in the order Shift+Tab cycles them, each
/// with what its footer shows while it is on, mark included. Bypass permissions is
/// left out: it is only in the cycle when Claude was started with it.
pub const PERMISSION_MODES: [(&str, &str); 4] = [
    ("auto", "⏵⏵ auto mode on"),
    ("manual", "⏸ manual mode on"),
    ("accept edits", "⏵⏵ accept edits on"),
    ("plan", "⏸ plan mode on"),
];

/// How many of a screen's last non-blank rows are its footer.
const FOOTER_ROWS: usize = 3;

/// The permission mode `screen`'s footer shows, if it shows one. Only the
/// last rows are read, lowest first, so the same words higher up in the
/// conversation do not count.
pub fn permission_mode_on(screen: &str) -> Option<&'static str> {
    screen
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(FOOTER_ROWS)
        .find_map(|l| {
            PERMISSION_MODES
                .iter()
                .find(|(_, shown)| l.contains(shown))
                .map(|(mode, _)| *mode)
        })
}

/// How long to wait for the footer to change after Shift+Tab.
const MODE_CHANGE_WAIT: Duration = Duration::from_millis(1500);

/// Presses Shift+Tab in `pane` until its footer shows `mode`: at most once
/// round the cycle, each press waiting for the footer to change.
pub fn set_permission_mode(pane: &str, mode: &str, server: &[&str]) -> Result<()> {
    cycle_to(
        mode,
        || capture_pane(pane, server),
        || press(pane, "BTab", server),
        MODE_CHANGE_WAIT,
    )
}

fn cycle_to(
    mode: &str,
    mut read: impl FnMut() -> Result<String>,
    mut shift_tab: impl FnMut() -> Result<()>,
    wait: Duration,
) -> Result<()> {
    let mut now = permission_mode_on(&read()?).context(
        "its footer shows no permission mode; a menu may be open, or this Claude Code is too old",
    )?;
    for _ in 0..=PERMISSION_MODES.len() {
        if now == mode {
            return Ok(());
        }
        shift_tab()?;
        let until = Instant::now() + wait;
        let before = now;
        while now == before && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
            if let Some(m) = permission_mode_on(&read()?) {
                now = m;
            }
        }
        anyhow::ensure!(now != before, "Shift+Tab did not change its mode");
    }
    anyhow::bail!("{mode} mode is not in its Shift+Tab cycle")
}

/// The text on `pane`'s screen, one line per screen row. `server` are the
/// tmux arguments that pick the server; empty means the default one.
pub fn capture_pane(pane: &str, server: &[&str]) -> Result<String> {
    let mut cmd = Command::new("tmux");
    cmd.args(server).args(["capture-pane", "-p", "-t", pane]);
    let (status, out, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux capture-pane")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{AgentStatus, parse_agents_json};
    use super::*;

    #[test]
    fn a_new_session_is_seen_asking_for_trust_or_past_it() {
        let asking = " Accessing workspace:\n /w/new\n ❯ No, exit\n   Yes, I trust this folder\n";
        assert_eq!(asks_for_trust(asking), Some(true));
        assert_eq!(
            asks_for_trust(" ▐▛███▛█   Claude Code v2.1.322\n❯ \n"),
            Some(false)
        );
        assert_eq!(asks_for_trust(""), None, "still starting");
    }

    #[test]
    fn a_new_session_command_ends_with_the_worktree_flag() {
        assert_eq!(claude_command(None, false), "claude");
        assert_eq!(
            claude_command(Some("it's on"), false),
            r"claude 'it'\''s on'"
        );
        // After the prompt, or the prompt would be taken as the worktree's name.
        assert_eq!(
            claude_command(Some("fix it"), true),
            "claude 'fix it' --worktree"
        );
        assert_eq!(claude_command(None, true), "claude --worktree");
    }

    #[test]
    fn the_permission_mode_is_read_from_the_footer_only() {
        let footer = |f: &str| {
            format!(
                "● talk about ⏵⏵ auto mode on\n\n────\n> \n────\n  repo Opus\n  -- INSERT -- {f}\n\n"
            )
        };
        assert_eq!(
            permission_mode_on(&footer("⏵⏵ accept edits on (shift+tab to cycle)")),
            Some("accept edits")
        );
        assert_eq!(
            permission_mode_on(&footer("⏸ manual mode on")),
            Some("manual")
        );
        assert_eq!(permission_mode_on(&footer("? for shortcuts")), None);
    }

    #[test]
    fn shift_tab_is_pressed_until_the_mode_shows() {
        use std::cell::Cell;
        let at = Cell::new(0);
        let screen = || {
            Ok(format!(
                "  {} (shift+tab to cycle)",
                PERMISSION_MODES[at.get() % 4].1
            ))
        };
        let press = || {
            at.set(at.get() + 1);
            Ok(())
        };
        cycle_to("plan", screen, press, Duration::from_millis(50)).unwrap();
        assert_eq!(at.get(), 3, "auto, manual, accept edits, plan");
        // Already there: no press.
        cycle_to("plan", screen, press, Duration::from_millis(50)).unwrap();
        assert_eq!(at.get(), 3);
        // A footer that never changes gives up after one press.
        let stuck = || Ok("  ⏵⏵ auto mode on".to_string());
        let err = cycle_to("plan", stuck, || Ok(()), Duration::from_millis(50)).unwrap_err();
        assert!(err.to_string().contains("did not change"));
    }

    #[test]
    fn shell_quote_survives_quotes() {
        assert_eq!(super::shell_quote("it's"), r"'it'\''s'");
    }

    /// zsh in a private tmux server, with a bare home so your own startup
    /// files stay out of it: the command is typed at the prompt, not run.
    #[test]
    fn zsh_comes_up_with_the_command_typed_in() {
        if Command::new("zsh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let (dir, home) = (root.path().join("aq-shell"), root.path().join("home"));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join(".zshrc"), "").unwrap();
        // The system's zshrc (Ubuntu's runs compinit, which can stop to ask
        // about folder permissions) stays out too.
        std::fs::write(home.join(".zshenv"), "unsetopt GLOBAL_RCS\n").unwrap();
        super::write_zsh_files(&dir, "touch ran && echo 'it''s'").unwrap();
        let socket = format!("aq-zsh-{}", std::process::id());
        let tmux = |args: &[&str]| {
            Command::new("tmux")
                .env_remove("TMUX")
                .args(["-L", &socket, "-f", "/dev/null"])
                .args(args)
                .output()
                .unwrap()
        };
        let start = format!(
            "env ZDOTDIR={} AQ_ZDOTDIR={} zsh",
            super::shell_quote(&dir.to_string_lossy()),
            super::shell_quote(&home.to_string_lossy())
        );
        tmux(&[
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "5",
            "-c",
            &root.path().to_string_lossy(),
            &start,
        ]);
        let mut screen = String::new();
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            screen = String::from_utf8_lossy(&tmux(&["capture-pane", "-p"]).stdout).into_owned();
            if screen.contains("touch ran") {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        tmux(&["kill-server"]);
        assert!(screen.contains("touch ran && echo 'it''s'"), "{screen}");
        assert!(!root.path().join("ran").exists(), "it ran");
        assert!(!dir.join("typed").exists(), "typed once only");
    }

    #[test]
    fn popup_title_cannot_run_a_command() {
        assert_eq!(
            super::format_literal(" shell in #(touch x) "),
            " shell in ##(touch x) "
        );
    }

    #[test]
    fn strip_controls_keeps_only_newline_and_tab() {
        assert_eq!(
            strip_controls("a\x1b[201~b\x1bc\r\0d\u{9b}31m\u{80}\u{9f}e\x7f"),
            "a[201~bcd31me"
        );
        assert_eq!(
            strip_controls("line one\n\tindented, ünïcode\nend"),
            "line one\n\tindented, ünïcode\nend"
        );
    }

    #[test]
    fn parses_pane_lines() {
        let panes = parse_list_panes(include_str!("../../tests/fixtures/list-panes.txt"));
        assert_eq!(panes.len(), 8);
        let p54 = panes.iter().find(|p| p.pane_id == "%54").unwrap();
        assert_eq!(p54.cwd, PathBuf::from("/home/saman/.local/share/chezmoi"));
        assert!(!p54.waiting);
        assert_eq!(p54.window_index, 6);
        assert!(panes.iter().find(|p| p.pane_id == "%12").unwrap().waiting);
        assert_eq!(panes[7].command, "bash");
    }

    #[test]
    fn pipe_in_path_survives() {
        let panes = parse_list_panes("main|1|w|%1|42|claude|/home/a|b/c|1\n");
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].cwd, PathBuf::from("/home/a|b/c"));
        assert!(panes[0].waiting);
        let panes = parse_list_panes("main|1|w|%1|42|claude|/home/a|b/c|\n");
        assert_eq!(panes[0].cwd, PathBuf::from("/home/a|b/c"));
        assert!(!panes[0].waiting);
    }

    fn fixture_procs() -> Vec<ClaudeProc> {
        parse_agents_json(include_str!("../../tests/fixtures/agents.json")).unwrap()
    }

    fn fixture_panes() -> Vec<Pane> {
        parse_list_panes(include_str!("../../tests/fixtures/list-panes.txt"))
    }

    #[test]
    fn join_matches_pane_whose_pid_is_ancestor() {
        // chezmoi claude 279801 sits under pane %54 (pid 2466920); locus 280112 under %48 (3935219).
        let anc = |a: u32, p: u32| (a, p) == (2466920, 279801) || (a, p) == (3935219, 280112);
        let agents = join(&fixture_panes(), &fixture_procs(), anc);
        assert_eq!(agents.len(), 2);
        assert_eq!(
            agents[0].id,
            AgentId {
                pane: "%54".into(),
                claude_pid: 279801
            }
        );
        assert_eq!(agents[0].window_index, 6);
        assert_eq!(agents[0].status, AgentStatus::Busy);
        assert!(!agents[0].waiting);
        assert_eq!(
            agents[1].id,
            AgentId {
                pane: "%48".into(),
                claude_pid: 280112
            }
        );
        assert!(agents[1].waiting);
    }

    #[test]
    fn join_matches_when_pane_pid_equals_claude_pid() {
        let mut procs = fixture_procs();
        procs[0].pid = 847984; // pane %56
        let agents = join(&fixture_panes(), &procs[..1], |_, _| false);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id.pane, "%56");
    }

    fn box_holding(text: &str) -> String {
        format!("● Done.\n─────\n\x1b[39m❯\u{a0}{text}\n─────\n  ctx:5%\n")
    }

    /// Screens to read in turn (the last repeats), and how many Enters were
    /// pressed.
    fn confirm_with(screens: &[String]) -> (Result<()>, usize) {
        let read = std::cell::Cell::new(0);
        let pressed = std::cell::Cell::new(0);
        let result = confirm_sent(
            || {
                let i = read.get().min(screens.len() - 1);
                read.set(read.get() + 1);
                Ok(screens[i].clone())
            },
            || {
                pressed.set(pressed.get() + 1);
                Ok(())
            },
            "merge them in order",
            Duration::from_millis(1),
        );
        (result, pressed.get())
    }

    #[test]
    fn a_message_left_in_the_input_box_gets_enter_again() {
        // Stuck through every check, then gone after the second Enter.
        let stuck_then_sent = |text: &str| {
            let mut screens = vec![box_holding(text); SENT_CHECKS as usize];
            screens.push(box_holding(""));
            screens
        };
        let (result, pressed) = confirm_with(&stuck_then_sent("merge them in order"));
        assert!(result.is_ok());
        assert_eq!(pressed, 1);
        // Sent at once: nothing more is pressed.
        let (result, pressed) = confirm_with(&[box_holding("")]);
        assert!(result.is_ok() && pressed == 0);
        // A long message shows as a placeholder.
        let pasted = stuck_then_sent("[Pasted text #1 +12 lines]");
        assert_eq!(confirm_with(&pasted).1, 1);
        // A box that clears a moment late needs nothing more.
        let late = [box_holding("merge them in order"), box_holding("")];
        assert_eq!(confirm_with(&late).1, 0);
        // Your own words in the box are not taken for the message.
        assert_eq!(confirm_with(&[box_holding("something else")]).1, 0);
    }

    #[test]
    fn a_message_that_stays_in_the_box_is_reported() {
        let (result, pressed) = confirm_with(&[box_holding("merge them in order")]);
        assert_eq!(pressed, 1, "Enter is pressed again once only");
        let e = result.unwrap_err().to_string();
        assert!(e.contains("still in its input box"), "{e}");
    }

    #[test]
    fn a_claude_started_by_another_claude_is_not_a_session_of_its_own() {
        // A hook of the session at 279801 runs claude -p as 990001.
        let mut procs = fixture_procs();
        let mut child = procs[0].clone();
        child.pid = 990001;
        child.session_id = "headless".into();
        procs.push(child);
        let anc = |a: u32, p: u32| {
            matches!(
                (a, p),
                (2466920 | 279801, 990001) | (2466920, 279801) | (3935219, 280112)
            )
        };
        let agents = join(&fixture_panes(), &procs, anc);
        let pids: Vec<u32> = agents.iter().map(|a| a.id.claude_pid).collect();
        assert_eq!(pids, [279801, 280112]);
    }

    #[test]
    fn join_drops_process_without_pane() {
        assert!(join(&fixture_panes(), &fixture_procs(), |_, _| false).is_empty());
    }

    #[test]
    fn claude_agents_runs_again_only_when_panes_change_or_after_ten_seconds() {
        let panes = fixture_panes();
        let keys = pane_keys(&panes);
        let t = Instant::now();
        let seen = ProcsSeen {
            panes: keys.clone(),
            at: t,
            procs: Vec::new(),
        };
        assert!(!procs_fresh(None, &keys, t));
        assert!(procs_fresh(Some(&seen), &keys, t + Duration::from_secs(9)));
        assert!(!procs_fresh(Some(&seen), &keys, t + PROCS_EVERY));
        let mut waiting = panes.clone();
        waiting[0].waiting = !waiting[0].waiting;
        assert!(!procs_fresh(Some(&seen), &pane_keys(&waiting), t));
        let mut fewer = panes.clone();
        fewer.pop();
        assert!(!procs_fresh(Some(&seen), &pane_keys(&fewer), t));
        let mut started = panes;
        started[7].command = "claude".into();
        assert!(!procs_fresh(Some(&seen), &pane_keys(&started), t));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        assert!(parse_list_panes("garbage\n\nmain|x|w|%1|1|c|/|\n").is_empty());
    }
}
