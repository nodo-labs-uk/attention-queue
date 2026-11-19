//! The flag log: one JSON line per decision on a candidate, beside aq's other
//! state, so the user can check that flags stay rare and right.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::rules::Decision;

/// The log file in `state_dir`.
pub fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("flags.jsonl")
}

/// Appends `entry` as one line. A new file is made readable by the user only.
pub fn append(path: &Path, entry: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // One write per line, so lines from two threads never mix.
    file.write_all(format!("{entry}\n").as_bytes())
}

/// The log entry for a decision on one tool call. `at` is in Unix seconds;
/// `command` is the Bash command, or an MCP tool's input as compact JSON.
pub fn entry(tool: &str, input: &Value, decision: &Decision) -> Value {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(c) if tool == "Bash" => c.to_string(),
        _ => input.to_string(),
    };
    let (decision, label) = match decision {
        Decision::Cleared => ("cleared", None),
        Decision::Flagged { label, .. } => ("flagged", Some(*label)),
        Decision::Grey => ("grey", None),
    };
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    json!({
        "at": at,
        "tool": tool,
        "command": command,
        "decision": decision,
        "label": label,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::flag::FlagKind;

    #[test]
    fn flag_log_appends_one_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = log_path(&dir.path().join("state"));
        let push = json!({"command": "git push --force"});
        let force = Decision::Flagged {
            kind: FlagKind::HighStakes,
            label: "FORCE PUSH",
        };
        append(&path, &entry("Bash", &push, &force)).unwrap();
        let mcp = json!({"owner": "o", "repo": "r", "body": "hi"});
        append(
            &path,
            &entry("mcp__github__add_comment", &mcp, &Decision::Grey),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["tool"], "Bash");
        assert_eq!(lines[0]["command"], "git push --force");
        assert_eq!(lines[0]["decision"], "flagged");
        assert_eq!(lines[0]["label"], "FORCE PUSH");
        assert!(lines[0]["at"].as_u64().unwrap() > 0);
        assert_eq!(lines[1]["command"], mcp.to_string());
        assert_eq!(lines[1]["decision"], "grey");
        assert_eq!(lines[1]["label"], Value::Null);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
