use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

use super::AgentStatus;

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

/// Parses `claude agents --json`, keeping interactive entries only.
pub fn parse_agents_json(s: &str) -> Result<Vec<ClaudeProc>> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(s).context("parsing claude agents --json")?;
    // Filter before strict parsing so one odd non-interactive entry cannot empty the list.
    let interactive = entries
        .into_iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("interactive"));
    let mut procs = Vec::new();
    for e in interactive {
        let r: Raw = serde_json::from_value(e).context("parsing interactive agent entry")?;
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

pub fn list_claude_procs() -> Result<Vec<ClaudeProc>> {
    let out = Command::new("claude")
        .args(["agents", "--json"])
        .output()
        .context("running claude agents --json")?;
    anyhow::ensure!(out.status.success(), "claude agents --json failed");
    parse_agents_json(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agents_json_skips_non_interactive() {
        let s = include_str!("../../tests/fixtures/agents.json");
        let procs = parse_agents_json(s).unwrap();
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 279801);
        assert_eq!(procs[0].status, AgentStatus::Busy);
        assert_eq!(procs[0].name, "chezmoi-86");
        assert_eq!(procs[1].status, AgentStatus::Idle);
        assert!(procs.iter().all(|p| p.pid != 281000));
    }

    #[test]
    fn odd_non_interactive_entry_does_not_empty_list() {
        let s = r#"[{"pid":9,"kind":"background"},
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
