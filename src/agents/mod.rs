pub mod claude;
pub mod procs;
pub mod tmux;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::model::{AgentId, Checkout};

pub use claude::{ClaudeProc, parse_agents_json};
pub use procs::is_ancestor;
pub use tmux::{Pane, TmuxConn, parse_list_panes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Busy,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: AgentId,
    pub session: String,
    pub window_index: u32,
    pub window_name: String,
    pub cwd: PathBuf,
    pub checkout: Option<Checkout>,
    pub session_id: String,
    pub status: AgentStatus,
    pub waiting: bool,
}

pub trait AgentConn: Send + Sync {
    fn list(&self) -> Result<Vec<Agent>>;
    fn send_text(&self, agent: &AgentId, text: &str) -> Result<()>;
    fn is_alive(&self, agent: &AgentId) -> bool;
}

/// Worktree root containing `dir`, if it is inside a git checkout.
pub fn checkout_of(dir: &Path) -> Option<Checkout> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8(out.stdout).ok()?;
    Some(Checkout::new(top.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_identity_includes_claude_pid() {
        let a = AgentId {
            pane: "%1".into(),
            claude_pid: 10,
        };
        let b = AgentId {
            pane: "%1".into(),
            claude_pid: 11,
        };
        assert_ne!(a, b);
        assert_eq!(
            a,
            AgentId {
                pane: "%1".into(),
                claude_pid: 10
            }
        );
    }

    #[test]
    fn checkout_of_repo_and_non_repo() {
        let here = checkout_of(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(here.0.is_absolute());
        let tmp = tempfile::tempdir().unwrap();
        assert!(checkout_of(tmp.path()).is_none());
    }
}
