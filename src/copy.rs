//! What `c` copies from a card: its web links, code blocks and backticked
//! text, and the command of a permission card.

use serde_json::Value;

use crate::model::{Card, CardKind};

/// The texts on `card` worth copying, in the order they end on it, so the
/// last is the likeliest. None is empty and none repeats.
pub fn candidates(card: &Card) -> Vec<String> {
    let mut found = Vec::new();
    match &card.kind {
        CardKind::Question(qc) => in_text(&qc.text, &mut found),
        CardKind::Tell(tc) => in_text(&tc.message.text, &mut found),
        CardKind::Menu(mc) => {
            for line in mc.menu.question.iter().chain(&mc.menu.options) {
                in_text(line, &mut found);
            }
        }
        CardKind::Choice(cc) => in_json(&cc.input, &mut |s| in_text(s, &mut found)),
        // A tool's input is not prose: a backtick in it is the shell's or
        // a file's, so only its links count, and then its command.
        CardKind::Permission(pc) => {
            in_json(&pc.tool_input, &mut |s| {
                for line in s.lines() {
                    found.extend(links(line).into_iter().map(|(_, link)| link));
                }
            });
            if let Some(command) = pc.tool_input.get("command").and_then(Value::as_str) {
                found.push(command.trim().to_string());
            }
        }
        _ => {}
    }
    without_repeats(found)
}

/// The texts in the markdown `text` worth copying, as `candidates` gives
/// a card's.
pub fn in_reply(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    in_text(text, &mut found);
    without_repeats(found)
}

/// `found` without empty texts, and with only the last of each text.
fn without_repeats(found: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for text in found.into_iter().rev() {
        if !text.is_empty() && !kept.contains(&text) {
            kept.push(text);
        }
    }
    kept.reverse();
    kept
}

/// Calls `each` with every string in `value`.
fn in_json(value: &Value, each: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => each(s),
        Value::Array(items) => items.iter().for_each(|v| in_json(v, each)),
        Value::Object(map) => map.values().for_each(|v| in_json(v, each)),
        _ => {}
    }
}

/// The links, fenced code blocks and backticked text of markdown `text`.
fn in_text(text: &str, found: &mut Vec<String>) {
    let mut block: Option<String> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match block.take() {
                Some(code) => found.push(code.trim_end().to_string()),
                None => block = Some(String::new()),
            }
            continue;
        }
        let mut here = links(line);
        match &mut block {
            Some(code) => {
                code.push_str(line);
                code.push('\n');
            }
            None => here.extend(spans(line)),
        }
        here.sort_by_key(|(end, _)| *end);
        found.extend(here.into_iter().map(|(_, text)| text));
    }
}

/// Each `http://` or `https://` link in `line` with where it ends, without
/// the brackets, quotes or full stop around it.
fn links(line: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = line[from..].find("http").map(|i| from + i) {
        let rest = &line[at..];
        from = at + "http".len();
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            continue;
        }
        let len = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']')
            })
            .unwrap_or(rest.len());
        from = at + len;
        let link = rest[..len].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if link
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.is_empty())
        {
            found.push((at + link.len(), link.to_string()));
        }
    }
    found
}

/// Each text between two backticks in `line`, with where it ends.
fn spans(line: &str) -> Vec<(usize, String)> {
    let parts: Vec<&str> = line.split('`').collect();
    let mut found = Vec::new();
    let mut end = 0;
    for (i, part) in parts.iter().enumerate() {
        end += part.len() + 1;
        if i % 2 == 1 && i + 1 < parts.len() && !part.trim().is_empty() {
            found.push((end, part.trim().to_string()));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Vec<String> {
        let mut found = Vec::new();
        in_text(s, &mut found);
        found
    }

    #[test]
    fn links_are_found_without_what_surrounds_them() {
        assert_eq!(
            text("see http://a.io/x, then (https://b.io/y?z=1)."),
            ["http://a.io/x", "https://b.io/y?z=1"]
        );
        assert_eq!(text("no link, only https:// and plain text"), [""; 0]);
    }

    #[test]
    fn backticked_text_and_code_blocks_are_found_in_the_order_they_end() {
        assert_eq!(
            text(
                "Open https://github.com/login/device and enter `AB12-CD34`.\n\
                 Then run `curl https://c.io/p` or:\n\
                 ```sh\ncargo build\n  cargo test `x`\n```\n\
                 An open ` is no code, nor is an open block:\n```\nlost"
            ),
            [
                "https://github.com/login/device",
                "AB12-CD34",
                "https://c.io/p",
                "curl https://c.io/p",
                "cargo build\n  cargo test `x`",
            ]
        );
    }
}
