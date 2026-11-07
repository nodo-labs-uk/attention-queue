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
fn join(panes: &[Pane], procs: &[ClaudeProc]) -> Vec<Agent> {
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

impl AgentConn for TmuxConn {
    fn list(&self) -> Result<Vec<Agent>> {
        let panes = self.list_panes()?;
        let procs = list_claude_procs()?;
        Ok(join(&panes, &procs))
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
            .write_all(text.as_bytes())?;
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
    use super::*;

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

    #[test]
    fn malformed_lines_are_skipped() {
        assert!(parse_list_panes("garbage\n\nmain|x|w|%1|1|c|/|\n").is_empty());
    }
}
