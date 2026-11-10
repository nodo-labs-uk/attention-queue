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

/// Joins each claude process to the pane whose `pane_pid` is itself or an ancestor of it.
fn join(
    panes: &[Pane],
    procs: &[ClaudeProc],
    is_ancestor: impl Fn(u32, u32) -> bool,
) -> Vec<Agent> {
    let mut agents = Vec::new();
    for p in procs {
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
        });
    }
    agents
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
        // Only a waiting agent can take a message, so only its screen is read.
        for a in agents.iter_mut().filter(|a| a.waiting) {
            if let Ok(screen) = self.capture_styled(&a.id.pane) {
                a.input = super::read_input_box(&screen);
            }
        }
        Ok(agents)
    }

    fn send_text(&self, agent: &AgentId, text: &str) -> Result<()> {
        let pane = agent.pane.as_str();
        self.run(&["load-buffer", "-b", "aq", "-"], &strip_controls(text))?;
        self.run(&["paste-buffer", "-p", "-d", "-b", "aq", "-t", pane], "")?;
        self.run(&["send-keys", "-t", pane, "Enter"], "")
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

/// Sets or clears `OPEN_FLAG` on the default tmux server. Without a server
/// there is nothing to mark, so failures are ignored.
pub fn mark_open(open: bool) {
    let mut cmd = Command::new("tmux");
    if open {
        cmd.args(["set-option", "-g", OPEN_FLAG, "1"]);
    } else {
        cmd.args(["set-option", "-gu", OPEN_FLAG]);
    }
    let _ = super::run_for(cmd, "", TMUX_TIMEOUT);
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
    let title = format_literal(&title);
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
        cmd.arg(format_literal(&c));
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
/// `prompt` as its first message. The command goes through the shell, so
/// `claude` is found as in your own windows; the prompt is quoted for it.
pub fn new_claude(dir: &Path, prompt: Option<&str>) -> Result<()> {
    let command = match prompt {
        Some(p) => format!("claude {}", shell_quote(p)),
        None => "claude".to_string(),
    };
    let mut cmd = Command::new("tmux");
    cmd.args(["new-window", "-d", "-c"])
        // The start directory is expanded as a format; the command is not.
        .arg(format_literal(&dir.to_string_lossy()))
        .arg(command);
    let (status, _, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux new-window")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
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
    let mut cmd = Command::new("tmux");
    cmd.args(server).args(["send-keys", "-t", pane, "Escape"]);
    let (status, _, err) =
        super::run_for(cmd, "", TMUX_TIMEOUT).context("running tmux send-keys")?;
    anyhow::ensure!(status.success(), "{}", err.trim());
    Ok(())
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
