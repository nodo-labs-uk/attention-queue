//! What an edit tool call would do to its file, worked out before it runs.
//! Pure: the caller reads the file.

// Line ranges are data here, often just one.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPreview {
    /// The edit lands in the file. `changed` holds 0-based, half-open line
    /// ranges of `after`; an empty range marks lines that were only removed.
    Placed {
        before: String,
        after: String,
        changed: Vec<Range<u32>>,
    },
    /// The edit could not be placed in the file, so it is shown as raw text.
    Unplaced {
        old: String,
        new: String,
        why: String,
    },
}

/// An empty unplaced edit, for a blank step card.
impl Default for EditPreview {
    fn default() -> Self {
        EditPreview::Unplaced {
            old: String::new(),
            new: String::new(),
            why: String::new(),
        }
    }
}

/// The file after `tool_name` with `input` runs on `current` (`None`: the
/// file does not exist). Claude's own rules apply: an `Edit` whose text is
/// missing, or found more than once without `replace_all`, would fail.
pub fn apply_edit(tool_name: &str, input: &Value, current: Option<&str>) -> EditPreview {
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let replace = |v: &Value| Replace {
        old: field(v, "old_string"),
        new: field(v, "new_string"),
        all: v
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    match tool_name {
        "Write" => {
            let before = current.unwrap_or("").to_string();
            let after = field(input, "content");
            let changed = changed_lines(&before, &after);
            EditPreview::Placed {
                before,
                after,
                changed,
            }
        }
        "Edit" => apply_replaces(current, &[replace(input)]),
        "MultiEdit" => {
            let edits: Vec<Replace> = input
                .get("edits")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(replace).collect())
                .unwrap_or_default();
            apply_replaces(current, &edits)
        }
        "NotebookEdit" => {
            let cell = input
                .get("cell_id")
                .and_then(Value::as_str)
                .unwrap_or("new");
            let mode = input
                .get("edit_mode")
                .and_then(Value::as_str)
                .unwrap_or("replace");
            EditPreview::Unplaced {
                old: String::new(),
                new: field(input, "new_source"),
                why: format!("notebook cell {cell} ({mode})"),
            }
        }
        _ => EditPreview::Unplaced {
            old: String::new(),
            new: serde_json::to_string_pretty(input).unwrap_or_default(),
            why: format!("{tool_name} is not a file edit"),
        },
    }
}

struct Replace {
    old: String,
    new: String,
    all: bool,
}

/// `Edit` and `MultiEdit`: each replacement in order, on the text so far.
fn apply_replaces(current: Option<&str>, edits: &[Replace]) -> EditPreview {
    let unplaced = |why: &str| {
        let join = |f: fn(&Replace) -> &str| edits.iter().map(f).collect::<Vec<_>>().join("\n⋯\n");
        EditPreview::Unplaced {
            old: join(|e| &e.old),
            new: join(|e| &e.new),
            why: why.to_string(),
        }
    };
    let mut exists = current.is_some();
    let mut text = current.unwrap_or("").to_string();
    // Changed byte ranges of `text`.
    let mut spans: Vec<Range<usize>> = Vec::new();
    for e in edits {
        if e.old.is_empty() {
            // An empty old_string creates the file, and only a new one.
            if exists {
                return unplaced("the text to replace is empty but the file exists");
            }
            text = e.new.clone();
            spans = vec![0..text.len()];
            exists = true;
            continue;
        }
        if !exists {
            return unplaced("the file does not exist yet");
        }
        let hits: Vec<usize> = text.match_indices(&e.old).map(|(i, _)| i).collect();
        match hits.len() {
            0 => return unplaced("the text to replace is not in the file"),
            n if n > 1 && !e.all => {
                return unplaced(&format!("the text to replace is in the file {n} times"));
            }
            _ => {}
        }
        // From the end, so earlier offsets stay valid.
        for &at in hits.iter().rev() {
            replace_at(&mut text, &mut spans, at, &e.old, &e.new);
        }
    }
    let before = current.unwrap_or("").to_string();
    let changed = line_ranges(&text, &spans);
    EditPreview::Placed {
        before,
        after: text,
        changed,
    }
}

/// Replaces `old` at byte `at` with `new`, marking only the part that differs
/// and moving the marks already made.
fn replace_at(text: &mut String, spans: &mut Vec<Range<usize>>, at: usize, old: &str, new: &str) {
    let old_end = at + old.len();
    let new_end = at + new.len();
    let p = common_prefix(old, new);
    let s = common_suffix(&old[p..], &new[p..]);
    let line_start = at + p == 0 || text.as_bytes()[at + p - 1] == b'\n';
    let removed_lines = old[p..old.len() - s].ends_with('\n');
    text.replace_range(at..old_end, new);
    let mut mark = at + p..new_end - s;
    if mark.is_empty() && !(line_start && removed_lines) {
        // Text removed from within a line: mark that line.
        mark = match text[mark.start..].chars().next() {
            Some(c) => mark.start..mark.start + c.len_utf8(),
            None => {
                let prev = text[..mark.start]
                    .chars()
                    .next_back()
                    .map_or(0, char::len_utf8);
                mark.start - prev..mark.start
            }
        };
    }
    let delta = new.len() as isize - old.len() as isize;
    spans.retain_mut(|r| {
        if r.end < at {
            return true;
        }
        if r.start > old_end {
            *r = r.start.saturating_add_signed(delta)..r.end.saturating_add_signed(delta);
            return true;
        }
        // Overlaps the replaced text: fold it into this mark.
        let end = if r.end > old_end {
            r.end.saturating_add_signed(delta)
        } else {
            new_end
        };
        mark = mark.start.min(r.start.min(at))..mark.end.max(end);
        false
    });
    spans.push(mark);
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
}

fn common_suffix(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(c, _)| c.len_utf8())
        .sum()
}

/// Byte ranges of `text` as merged line ranges. An empty byte range at the
/// start of a line becomes an empty line range there.
fn line_ranges(text: &str, spans: &[Range<usize>]) -> Vec<Range<u32>> {
    let newlines: Vec<usize> = text.match_indices('\n').map(|(i, _)| i).collect();
    let line_of = |b: usize| newlines.partition_point(|&n| n < b) as u32;
    let mut lines: Vec<Range<u32>> = spans
        .iter()
        .map(|r| {
            if r.is_empty() {
                let l = line_of(r.start);
                l..l
            } else {
                line_of(r.start)..line_of(r.end - 1) + 1
            }
        })
        .collect();
    lines.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<u32>> = Vec::new();
    for r in lines {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// `Write`: the lines between the common head and tail of the two texts.
fn changed_lines(before: &str, after: &str) -> Vec<Range<u32>> {
    let b: Vec<&str> = before.split_inclusive('\n').collect();
    let a: Vec<&str> = after.split_inclusive('\n').collect();
    if a == b {
        return Vec::new();
    }
    let p = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let s = b[p..]
        .iter()
        .rev()
        .zip(a[p..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    vec![p as u32..(a.len() - s) as u32]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn placed(p: EditPreview) -> (String, String, Vec<Range<u32>>) {
        match p {
            EditPreview::Placed {
                before,
                after,
                changed,
            } => (before, after, changed),
            p => panic!("not placed: {p:?}"),
        }
    }

    const FILE: &str = "fn a() {\n    one();\n}\n\nfn b() {\n    one();\n}\n";

    #[test]
    fn edit_places_change() {
        let input = json!({"file_path": "/x.rs", "old_string": "    one();\n}\n\nfn b", "new_string": "    two();\n    three();\n}\n\nfn b"});
        let (before, after, changed) = placed(apply_edit("Edit", &input, Some(FILE)));
        assert_eq!(before, FILE);
        assert_eq!(
            after,
            "fn a() {\n    two();\n    three();\n}\n\nfn b() {\n    one();\n}\n"
        );
        assert_eq!(changed, [1..3], "only the lines that differ");

        let input = json!({"old_string": "fn b()", "new_string": "fn bee()"});
        let (_, _, changed) = placed(apply_edit("Edit", &input, Some(FILE)));
        assert_eq!(changed, [4..5]);
    }

    #[test]
    fn replace_all_marks_every_range() {
        let input = json!({"old_string": "one", "new_string": "uno", "replace_all": true});
        let (_, after, changed) = placed(apply_edit("Edit", &input, Some(FILE)));
        assert_eq!(after, FILE.replace("one", "uno"));
        assert_eq!(changed, [1..2, 5..6]);
    }

    #[test]
    fn edit_matching_twice_without_replace_all_is_unplaced() {
        let input = json!({"old_string": "one", "new_string": "uno"});
        let EditPreview::Unplaced { why, .. } = apply_edit("Edit", &input, Some(FILE)) else {
            panic!("expected unplaced");
        };
        assert_eq!(why, "the text to replace is in the file 2 times");
    }

    #[test]
    fn multiedit_applies_in_order() {
        // The second edit only matches text the first one wrote.
        let input = json!({"file_path": "/x.rs", "edits": [
            {"old_string": "fn a() {\n    one();", "new_string": "fn a() {\n    first();"},
            {"old_string": "first();", "new_string": "first();\n    second();"},
            {"old_string": "fn b()", "new_string": "fn bee()"},
        ]});
        let (_, after, changed) = placed(apply_edit("MultiEdit", &input, Some(FILE)));
        assert_eq!(
            after,
            "fn a() {\n    first();\n    second();\n}\n\nfn bee() {\n    one();\n}\n"
        );
        assert_eq!(changed, [1..3, 5..6]);
    }

    #[test]
    fn write_new_file_has_empty_before() {
        let input = json!({"file_path": "/new.rs", "content": "a\nb\n"});
        let (before, after, changed) = placed(apply_edit("Write", &input, None));
        assert_eq!((before.as_str(), after.as_str()), ("", "a\nb\n"));
        assert_eq!(changed, [0..2]);

        // Over an existing file, only the lines that differ are marked.
        let input = json!({"content": "fn a() {\n    two();\n}\n\nfn b() {\n    one();\n}\n"});
        let (before, _, changed) = placed(apply_edit("Write", &input, Some(FILE)));
        assert_eq!(before, FILE);
        assert_eq!(changed, [1..2]);
    }

    #[test]
    fn deletion_marks_an_empty_range() {
        let input = json!({"old_string": "\nfn b() {\n    one();\n}\n", "new_string": ""});
        let (_, after, changed) = placed(apply_edit("Edit", &input, Some(FILE)));
        assert_eq!(after, "fn a() {\n    one();\n}\n");
        assert_eq!(changed, [3..3]);
    }

    #[test]
    fn removing_part_of_a_line_marks_that_line() {
        let input =
            json!({"old_string": "fn a() {\n    one();", "new_string": "fn a() {\n    on();"});
        let (_, after, changed) = placed(apply_edit("Edit", &input, Some(FILE)));
        assert!(after.starts_with("fn a() {\n    on();\n"));
        assert_eq!(changed, [1..2]);
    }

    #[test]
    fn edit_not_found_falls_back_to_raw() {
        let input = json!({"old_string": "nope", "new_string": "yes"});
        assert_eq!(
            apply_edit("Edit", &input, Some(FILE)),
            EditPreview::Unplaced {
                old: "nope".into(),
                new: "yes".into(),
                why: "the text to replace is not in the file".into(),
            }
        );
        let EditPreview::Unplaced { why, .. } = apply_edit("Edit", &input, None) else {
            panic!("expected unplaced");
        };
        assert_eq!(why, "the file does not exist yet");
    }

    #[test]
    fn edit_with_empty_old_string_creates_a_file() {
        let input = json!({"old_string": "", "new_string": "hi\n"});
        let (before, after, changed) = placed(apply_edit("Edit", &input, None));
        assert_eq!((before.as_str(), after.as_str()), ("", "hi\n"));
        assert_eq!(changed, [0..1]);
    }

    #[test]
    fn notebook_edit_is_shown_raw() {
        let input = json!({"notebook_path": "/n.ipynb", "cell_id": "c1", "new_source": "x = 1", "edit_mode": "replace"});
        assert_eq!(
            apply_edit("NotebookEdit", &input, Some("{}")),
            EditPreview::Unplaced {
                old: String::new(),
                new: "x = 1".into(),
                why: "notebook cell c1 (replace)".into(),
            }
        );
    }
}
