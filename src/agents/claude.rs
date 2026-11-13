use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

use super::AgentStatus;

/// The most `claude agents --json` may take.
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeProc {
    pub pid: u32,
    pub cwd: PathBuf,
    pub session_id: String,
    pub name: String,
    pub status: AgentStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    pid: u32,
    cwd: PathBuf,
    session_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
}

/// Parses `claude agents --json`, keeping interactive and background
/// entries. A session can move into a background process that the window
/// then shows, with the window's own `claude` no longer listed; one under
/// no tmux pane, or started by a listed session, is dropped when joined.
pub fn parse_agents_json(s: &str) -> Result<Vec<ClaudeProc>> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(s).context("parsing claude agents --json")?;
    let mut procs = Vec::new();
    for e in entries {
        let r = match e.get("kind").and_then(|k| k.as_str()) {
            Some("interactive") => {
                serde_json::from_value::<Raw>(e).context("parsing interactive agent entry")?
            }
            // An odd background entry is skipped, so it cannot empty the list.
            Some("background") => match serde_json::from_value::<Raw>(e) {
                Ok(r) => r,
                Err(_) => continue,
            },
            _ => continue,
        };
        procs.push(ClaudeProc {
            pid: r.pid,
            cwd: r.cwd,
            session_id: r.session_id,
            name: r.name,
            status: match r.status.as_str() {
                "busy" => AgentStatus::Busy,
                "idle" => AgentStatus::Idle,
                _ => AgentStatus::Unknown,
            },
        });
    }
    Ok(procs)
}

/// Lists the running Claude Code sessions with `claude agents --json`.
///
/// Test hook, in debug builds only: when `AQ_AGENTS_JSON` names a file,
/// that file is read instead, so the end-to-end tests can make a fake agent
/// look like Claude. A release build ignores it, so an inherited value can
/// never feed aq someone else's agent list.
pub fn list_claude_procs() -> Result<Vec<ClaudeProc>> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("AQ_AGENTS_JSON").filter(|p| !p.is_empty()) {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.to_string_lossy()))?;
        return parse_agents_json(&text);
    }
    let mut cmd = Command::new("claude");
    cmd.args(["agents", "--json"]);
    let (status, out, _) =
        super::run_for(cmd, "", LIST_TIMEOUT).context("running claude agents --json")?;
    anyhow::ensure!(status.success(), "claude agents --json failed");
    parse_agents_json(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agents_json_keeps_interactive_and_background() {
        let s = include_str!("../../tests/fixtures/agents.json");
        let procs = parse_agents_json(s).unwrap();
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[2].pid, 281000);
        assert_eq!(procs[0].pid, 279801);
        assert_eq!(procs[0].status, AgentStatus::Busy);
        assert_eq!(procs[0].name, "chezmoi-86");
        assert_eq!(procs[1].status, AgentStatus::Idle);
    }

    #[test]
    fn odd_background_entry_does_not_empty_list() {
        let s = r#"[{"pid":9,"kind":"background"},{"pid":8,"kind":"other"},
            {"pid":1,"cwd":"/","kind":"interactive","sessionId":"x","status":"idle"}]"#;
        let procs = parse_agents_json(s).unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, 1);
    }

    #[test]
    fn unknown_status_maps_to_unknown() {
        let s = r#"[{"pid":1,"cwd":"/","kind":"interactive","sessionId":"x","status":"weird"}]"#;
        assert_eq!(
            parse_agents_json(s).unwrap()[0].status,
            AgentStatus::Unknown
        );
    }
}
