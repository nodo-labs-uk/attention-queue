//! Reading Claude Code session transcripts (one JSON object per line; each
//! assistant content block on a line of its own).

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The longest description kept, in characters.
const DESCRIPTION_MAX: usize = 300;
/// How much of the end of a transcript the conversation view reads.
const CONVERSATION_TAIL: u64 = 2 * 1024 * 1024;
/// A tool call's summary is cut to this many characters.
const TOOL_SUMMARY_MAX: usize = 120;

/// One step of a conversation, as the live view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    User(String),
    Claude(String),
    /// A tool call: its name and what it was called on.
    Tool(String),
}

/// The transcript of session `session_id`, in whichever project folder
/// Claude Code keeps it under `~/.claude/projects`.
pub fn find(session_id: &str) -> Option<PathBuf> {
    let projects = PathBuf::from(std::env::var_os("HOME")?).join(".claude/projects");
    let name = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

/// The prompts, replies and tool calls in the last `CONVERSATION_TAIL`
/// bytes of `transcript`, oldest first. Thinking, tool results and
/// Claude Code's own notes are left out.
pub fn conversation(transcript: &Path) -> std::io::Result<Vec<Said>> {
    let mut file = std::fs::File::open(transcript)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(CONVERSATION_TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut text = String::new();
    BufReader::new(file).read_to_string(&mut text)?;
    // Cut mid-line: the first line is only a part.
    let lines = text.lines().skip(usize::from(start > 0));
    let mut out = Vec::new();
    for line in lines {
        if !line.starts_with(r#"{"#)
            || !(line.contains(r#""type":"user""#) || line.contains(r#""type":"assistant""#))
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if hidden(&v) {
            continue;
        }
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") => {
                if let Some(t) = prompt(&v) {
                    out.push(Said::User(t.to_string()));
                }
            }
            Some("assistant") => {
                for b in content.as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => {
                            if let Some(t) =
                                b["text"].as_str().map(str::trim).filter(|t| !t.is_empty())
                            {
                                out.push(Said::Claude(t.to_string()));
                            }
                        }
                        Some("tool_use") => out.push(Said::Tool(tool_summary(b))),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A line Claude Code writes for itself: notes, a subagent's turns, and a
/// compaction's summary, which is written as a prompt but nobody typed it.
fn hidden(v: &Value) -> bool {
    v["isMeta"] == true || v["isSidechain"] == true || v["isCompactSummary"] == true
}

/// The text of a `user` line that is a prompt: not a tool result, and not
/// a command or its output, which come wrapped in tags.
fn prompt(v: &Value) -> Option<&str> {
    let text = match &v["message"]["content"] {
        Value::String(s) => Some(s.as_str()),
        Value::Array(blocks) => blocks
            .iter()
            .find(|b| b["type"] == "text")
            .and_then(|b| b["text"].as_str()),
        _ => None,
    };
    text.map(str::trim)
        .filter(|t| !t.is_empty() && !t.starts_with('<'))
}

/// When session `session_id` was last sent a prompt, typed in its window
/// or sent by aq, as the transcript's timestamp (UTC, so later sorts
/// after). The first time, the end of the transcript is read; after that
/// only what was added since.
pub fn last_prompt_at(session_id: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Seen = (PathBuf, u64, Option<String>);
    static SEEN: Mutex<Option<HashMap<String, Seen>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen.get_or_insert_with(HashMap::new);
    let path = match seen.get(session_id) {
        Some((p, ..)) => p.clone(),
        None => find(session_id)?,
    };
    let len = std::fs::metadata(&path).ok()?.len();
    if let Some((_, l, at)) = seen.get(session_id)
        && *l == len
    {
        return at.clone();
    }
    let (from, known) = match seen.get(session_id) {
        Some((_, l, at)) if *l < len => (*l, at.clone()),
        _ => (len.saturating_sub(CONVERSATION_TAIL), None),
    };
    let (found, upto) = last_prompt_in(&path, from, len)?;
    let at = found.or(known);
    seen.insert(session_id.to_string(), (path, upto, at.clone()));
    at
}

/// The timestamp of the last prompt in the whole lines between bytes
/// `start` and `len`, and where those lines end: a line still being
/// written is read next time. A line cut at `start` does not parse, so it
/// is passed over.
fn last_prompt_in(path: &Path, start: u64, len: u64) -> Option<(Option<String>, u64)> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes).ok()?;
    let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    bytes.truncate(whole);
    // The cut at `start` can fall inside a character.
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .into_iter()
        .rev()
        .filter(|l| l.starts_with('{') && l.contains(r#""type":"user""#))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "user" && !hidden(v) && prompt(v).is_some())
        .and_then(|v| v["timestamp"].as_str().map(str::to_string));
    Some((at, start + whole as u64))
}

/// `Name(what)`: the command's first line, or the file or pattern.
fn tool_summary(block: &Value) -> String {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let what = [
        "command",
        "file_path",
        "pattern",
        "url",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .and_then(|s| s.lines().next())
    .unwrap_or("");
    let what: String = what.chars().take(TOOL_SUMMARY_MAX).collect();
    if what.is_empty() {
        name.to_string()
    } else {
        format!("{name}({what})")
    }
}

/// The last assistant text block before the `tool_use` with id
/// `tool_use_id`, cut to 300 characters. If that `tool_use` is not in the
/// file yet, the latest text block is the one before it.
pub fn description_before(transcript: &Path, tool_use_id: &str) -> Option<String> {
    let file = std::fs::File::open(transcript).ok()?;
    let mut last: Option<String> = None;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        // Transcripts are large; parse only lines that can matter.
        let has_id = !tool_use_id.is_empty() && line.contains(tool_use_id);
        if !has_id && !line.contains(r#""type":"text""#) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"] != "assistant" {
            continue;
        }
        for block in v["message"]["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str().filter(|t| !t.trim().is_empty()) {
                        last = Some(text.trim().to_string());
                    }
                }
                Some("tool_use") if block["id"] == tool_use_id => return last.map(cut),
                _ => {}
            }
        }
    }
    last.map(cut)
}

/// What Claude wrote since the last prompt, its text blocks joined.
pub fn last_reply(said: &[Said]) -> Option<String> {
    let start = said
        .iter()
        .rposition(|s| matches!(s, Said::User(_)))
        .map_or(0, |i| i + 1);
    let text: Vec<&str> = said[start..]
        .iter()
        .filter_map(|s| match s {
            Said::Claude(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| text.join("\n\n"))
}

/// The last command Claude suggested you run yourself with Claude Code's
/// `!` prefix: inline as `` `! cmd` ``, or a line `! cmd` (a line ending in
/// `\` takes the next one with it). Without the `! `. Placeholders such as
/// `! <command>` are skipped.
pub fn last_bang_command(said: &[Said]) -> Option<String> {
    said.iter().rev().find_map(|s| match s {
        Said::Claude(text) => last_bang_in(text),
        _ => None,
    })
}

fn last_bang_in(text: &str) -> Option<String> {
    let mut found: Vec<(usize, String)> = Vec::new();
    // Inline code: the odd pieces between backticks.
    let mut at = 0;
    for (i, piece) in text.split('`').enumerate() {
        if i % 2 == 1
            && let Some(cmd) = piece.strip_prefix("! ")
        {
            found.push((at, cmd.trim().to_string()));
        }
        at += piece.len() + 1;
    }
    // Whole lines, joining continuations.
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0;
    let mut i = 0;
    while i < lines.len() {
        let start = at;
        at += lines[i].len() + 1;
        if let Some(first) = lines[i].trim_start().strip_prefix("! ") {
            let mut cmd = first.trim_end().to_string();
            while cmd.ends_with('\\') && i + 1 < lines.len() {
                i += 1;
                at += lines[i].len() + 1;
                cmd.push('\n');
                cmd.push_str(lines[i].trim_end());
            }
            found.push((start, cmd));
        }
        i += 1;
    }
    found
        .into_iter()
        .filter(|(_, c)| !c.is_empty() && !c.starts_with('<'))
        .max_by_key(|(at, _)| *at)
        .map(|(_, c)| c)
}

fn cut(text: String) -> String {
    text.chars().take(DESCRIPTION_MAX).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript.jsonl")
    }

    #[test]
    fn the_last_bang_command_is_found_inline_or_on_a_line() {
        use super::{Said, last_bang_command};
        let said = vec![
            Said::Claude("Run `! aws sso login --profile nodo` first.".into()),
            Said::User("done".into()),
            Said::Claude(
                "Now this, then `! <command>` for anything else:\n\n```\n! sudo apt install \\\n    jq\n```\nand `ls` too."
                    .into(),
            ),
            Said::Tool("Bash(! not this)".into()),
        ];
        assert_eq!(
            last_bang_command(&said).as_deref(),
            Some("sudo apt install \\\n    jq")
        );
        assert_eq!(
            last_bang_command(&said[..2]).as_deref(),
            Some("aws sso login --profile nodo")
        );
        assert_eq!(
            last_bang_command(&[Said::Claude("no `!` here".into())]),
            None
        );
    }

    #[test]
    fn the_last_prompt_is_the_last_one_typed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines = [
            r#"{"type":"user","timestamp":"2026-11-12T10:00:00Z","message":{"content":"first"}}"#,
            r#"{"type":"user","timestamp":"2026-11-12T10:05:00Z","message":{"content":[{"type":"text","text":"second"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-12T10:06:00Z","message":{"content":[{"type":"text","text":"ok"}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-12T10:07:00Z","message":{"content":[{"type":"tool_result","content":"x"}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-12T10:08:00Z","message":{"content":"<task-notification>done</task-notification>"}}"#,
            r#"{"type":"user","timestamp":"2026-11-12T10:09:00Z","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#,
        ];
        let text = lines.join("\n") + "\n";
        let len = text.len() as u64;
        std::fs::write(&path, &text).unwrap();
        assert_eq!(
            last_prompt_in(&path, 0, len),
            Some((Some("2026-11-12T10:05:00Z".into()), len))
        );
        // Only what was added since: no prompt in it.
        let tail = text.find(r#"{"type":"assistant""#).unwrap() as u64;
        assert_eq!(last_prompt_in(&path, tail, len), Some((None, len)));
        // A line still being written is left for next time.
        std::fs::write(&path, format!("{text}{{\"type\":\"user\"")).unwrap();
        assert_eq!(last_prompt_in(&path, tail, len + 14), Some((None, len)));
    }

    #[test]
    fn conversation_keeps_prompts_replies_and_tool_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"add a rate limit"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: meta"}}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I will read it."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test\nmore"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/x.rs"}}]}}"#,
            r#"{"type":"system","content":"x"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(
            conversation(&path).unwrap(),
            [
                Said::User("add a rate limit".into()),
                Said::Claude("I will read it.".into()),
                Said::Tool("Bash(cargo test)".into()),
                Said::Tool("Edit(/a/x.rs)".into()),
            ]
        );
    }

    #[test]
    fn description_is_text_before_tool_use() {
        assert_eq!(
            description_before(&fixture(), "toolu_edit").as_deref(),
            Some(
                "Return 429 when a client runs out of tokens.\n\
                 The plan says limits must be visible to clients."
            )
        );
        assert_eq!(
            description_before(&fixture(), "toolu_read").as_deref(),
            Some("I will read the handler first.")
        );
    }

    #[test]
    fn unwritten_tool_use_takes_the_latest_text() {
        assert_eq!(
            description_before(&fixture(), "toolu_later").as_deref(),
            Some("Now the tests.")
        );
    }

    #[test]
    fn description_is_cut_and_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.jsonl");
        let long = "x".repeat(400);
        let line = serde_json::json!({"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "text", "text": long}]}});
        std::fs::write(&file, format!("{line}\nnot json\n")).unwrap();
        assert_eq!(
            description_before(&file, "toolu_x").map(|d| d.chars().count()),
            Some(300)
        );
        assert_eq!(description_before(&dir.path().join("none"), "t"), None);
    }
}
