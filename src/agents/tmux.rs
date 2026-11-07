use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

#[derive(Debug, Clone, Default)]
pub struct TmuxConn {
    /// `Some(name)` adds `-L name` (private server, used by tests).
    pub socket: Option<String>,
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
        let out = self
            .cmd()
            .args(["list-panes", "-a", "-F", LIST_PANES_FORMAT])
            .output()
            .context("running tmux list-panes")?;
        anyhow::ensure!(out.status.success(), "tmux list-panes failed");
        Ok(parse_list_panes(&String::from_utf8_lossy(&out.stdout)))
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        let out = self.cmd().args(args).output().context("running tmux")?;
        anyhow::ensure!(
            out.status.success(),
            "tmux {} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(())
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
        let procs = list_claude_procs()?;
        Ok(join(&panes, &procs, is_ancestor))
    }

    fn send_text(&self, agent: &AgentId, text: &str) -> Result<()> {
        let pane = agent.pane.as_str();
        let mut load = self
            .cmd()
            .args(["load-buffer", "-b", "aq", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running tmux load-buffer")?;
        load.stdin
            .take()
            .context("no stdin")?
            .write_all(strip_controls(text).as_bytes())?;
        let out = load.wait_with_output()?;
        anyhow::ensure!(out.status.success(), "tmux load-buffer failed");
        self.run(&["paste-buffer", "-p", "-d", "-b", "aq", "-t", pane])?;
        self.run(&["send-keys", "-t", pane, "Enter"])
    }

    fn is_alive(&self, agent: &AgentId) -> bool {
        pid_alive(agent.claude_pid)
            && self
                .list_panes()
                .is_ok_and(|ps| ps.iter().any(|p| p.pane_id == agent.pane))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AgentStatus, parse_agents_json};
    use super::*;

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
    fn malformed_lines_are_skipped() {
        assert!(parse_list_panes("garbage\n\nmain|x|w|%1|1|c|/|\n").is_empty());
    }
}
