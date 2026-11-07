//! Reading Claude Code session transcripts (one JSON object per line; each
//! assistant content block on a line of its own).

use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

/// The longest description kept, in characters.
const DESCRIPTION_MAX: usize = 300;

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
