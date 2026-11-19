//! Deciding which comments GitHub can take as line comments, and building the review body.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::git::diff::{DiffLine, parse_unified};
use crate::review::{Comment, Side};

/// The lines on each side of one file that sit inside GitHub's hunks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileLines {
    new: BTreeSet<u32>,
    old: BTreeSet<u32>,
}

/// The lines GitHub's diff of a PR shows, per path (the new path for a renamed file).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhDiff {
    files: HashMap<PathBuf, FileLines>,
}

/// Reads the diff GitHub serves for a PR (`gh pr diff`) into the lines it shows per file.
pub fn parse_gh_diff(text: &str) -> GhDiff {
    let mut files: HashMap<PathBuf, FileLines> = HashMap::new();
    for f in parse_unified(text.as_bytes()) {
        let Some(path) = f.new_path.or(f.old_path) else {
            continue;
        };
        let entry = files.entry(path).or_default();
        for line in f.hunks.iter().flat_map(|h| &h.lines) {
            match line {
                DiffLine::Context { old, new, .. } => {
                    entry.old.insert(*old);
                    entry.new.insert(*new);
                }
                DiffLine::Add { new, .. } => {
                    entry.new.insert(*new);
                }
                DiffLine::Del { old, .. } => {
                    entry.old.insert(*old);
                }
            }
        }
    }
    GhDiff { files }
}

/// How a comment can reach GitHub. Only a line comment goes on GitHub's
/// diff; the others go in the review's main text, with their path and lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A line comment: every line of its range is in one of GitHub's hunks.
    Line,
    /// The file is in GitHub's diff but the lines are not.
    File,
    /// The file is not in GitHub's diff, or GitHub's diff could not be read.
    Body,
}

/// Where comment `c` goes, given GitHub's diff if it could be read.
pub fn placement(diff: Option<&GhDiff>, c: &Comment) -> Placement {
    let Some(f) = diff.and_then(|d| d.files.get(&c.path)) else {
        return Placement::Body;
    };
    let set = match c.side {
        Side::New => &f.new,
        Side::Old => &f.old,
    };
    if c.line <= c.end_line && (c.line..=c.end_line).all(|n| set.contains(&n)) {
        Placement::Line
    } else {
        Placement::File
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Comment,
    Approve,
    RequestChanges,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Comment => "COMMENT",
            Event::Approve => "APPROVE",
            Event::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::New => "RIGHT",
        Side::Old => "LEFT",
    }
}

fn lines_text(c: &Comment) -> String {
    if c.line == c.end_line {
        c.line.to_string()
    } else {
        format!("{}-{}", c.line, c.end_line)
    }
}

fn comment_json(c: &Comment) -> Value {
    let path = c.path.to_string_lossy();
    let side = side_name(c.side);
    let mut v = json!({"path": path, "line": c.end_line, "side": side, "body": c.body});
    if c.line != c.end_line {
        v["start_line"] = json!(c.line);
        v["start_side"] = json!(side);
    }
    v
}

/// The body for GitHub's create-review call, with the comments of `round`
/// only. Line comments go in `comments`; every other comment goes in the
/// main text after the summary, as `` `path:line`: text ``. Without
/// GitHub's diff, every comment goes in the main text.
pub fn review_body(
    diff: Option<&GhDiff>,
    comments: &[Comment],
    round: u32,
    event: Event,
    summary: &str,
    commit: &str,
) -> Value {
    let round_comments = || comments.iter().filter(|c| c.round == round);
    let list: Vec<Value> = round_comments()
        .filter(|c| placement(diff, c) == Placement::Line)
        .map(comment_json)
        .collect();
    let mut paragraphs: Vec<String> = Vec::new();
    if !summary.is_empty() {
        paragraphs.push(summary.to_string());
    }
    for c in round_comments().filter(|c| placement(diff, c) != Placement::Line) {
        paragraphs.push(format!(
            "`{}:{}`: {}",
            c.path.to_string_lossy(),
            lines_text(c),
            c.body
        ));
    }
    json!({
        "commit_id": commit,
        "event": event.name(),
        "body": paragraphs.join("\n\n"),
        "comments": list,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,4 +10,4 @@ fn f() {
 ten
-eleven
+eleven new
 twelve
 thirteen
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
index 555..000
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
index 333..444 100644
--- a/old.rs
+++ b/new.rs
@@ -1,2 +1,3 @@
 one
+two
 three
";

    fn comment(path: &str, line: u32, end: u32, side: Side, round: u32) -> Comment {
        Comment {
            path: path.into(),
            line,
            end_line: end,
            side,
            body: "b".into(),
            round,
        }
    }

    /// True when every line of the comment's range is in one of GitHub's hunks.
    fn fits(d: &GhDiff, c: &Comment) -> bool {
        placement(Some(d), c) == Placement::Line
    }

    #[test]
    fn parses_hunks_per_file_and_side() {
        let d = parse_gh_diff(DIFF);
        let a = &d.files[&PathBuf::from("src/a.rs")];
        assert_eq!(a.new, BTreeSet::from([10, 11, 12, 13]));
        assert_eq!(a.old, BTreeSet::from([10, 11, 12, 13]));
        let n = &d.files[&PathBuf::from("new.rs")];
        assert_eq!(n.new, BTreeSet::from([1, 2, 3]));
        assert_eq!(n.old, BTreeSet::from([1, 2]));
        assert!(!d.files.contains_key(&PathBuf::from("old.rs")));
        let g = &d.files[&PathBuf::from("gone.rs")];
        assert!(g.new.is_empty());
        assert_eq!(g.old, BTreeSet::from([1, 2]));
        assert!(fits(&d, &comment("new.rs", 2, 2, Side::Old, 1)));
        let v = review_body(
            Some(&d),
            &[comment("new.rs", 2, 2, Side::Old, 1)],
            1,
            Event::Comment,
            "",
            "c",
        );
        assert_eq!(v["comments"][0]["path"], "new.rs");
        assert_eq!(v["comments"][0]["side"], "LEFT");
    }

    #[test]
    fn a_line_in_a_hunk_fits_and_one_outside_does_not() {
        let d = parse_gh_diff(DIFF);
        assert!(fits(&d, &comment("src/a.rs", 11, 11, Side::New, 1)));
        assert!(fits(&d, &comment("src/a.rs", 11, 11, Side::Old, 1)));
        assert!(!fits(&d, &comment("src/a.rs", 40, 40, Side::New, 1)));
        assert!(!fits(&d, &comment("other.rs", 11, 11, Side::New, 1)));
        assert!(!fits(&d, &comment("new.rs", 3, 3, Side::Old, 1)));
    }

    #[test]
    fn a_range_partly_outside_does_not_fit() {
        let d = parse_gh_diff(DIFF);
        assert!(fits(&d, &comment("src/a.rs", 10, 13, Side::New, 1)));
        assert!(!fits(&d, &comment("src/a.rs", 12, 14, Side::New, 1)));
        assert!(!fits(&d, &comment("src/a.rs", 9, 10, Side::New, 1)));
    }

    #[test]
    fn builds_the_review_body() {
        let d = parse_gh_diff(DIFF);
        let comments = [
            comment("src/a.rs", 11, 11, Side::New, 2),
            comment("src/a.rs", 10, 12, Side::New, 2),
            comment("src/a.rs", 11, 11, Side::Old, 2),
            comment("src/a.rs", 42, 42, Side::New, 2),
            comment("src/a.rs", 50, 52, Side::New, 2),
            comment("src/a.rs", 11, 11, Side::New, 1),
        ];
        let v = review_body(Some(&d), &comments, 2, Event::RequestChanges, "sum", "abc");
        // Comments off GitHub's lines go in the main text, never as file
        // comments: `subject_type` is not a field of the create-review call.
        assert_eq!(
            v,
            json!({
                "commit_id": "abc",
                "event": "REQUEST_CHANGES",
                "body": "sum\n\n`src/a.rs:42`: b\n\n`src/a.rs:50-52`: b",
                "comments": [
                    {"path": "src/a.rs", "line": 11, "side": "RIGHT", "body": "b"},
                    {"path": "src/a.rs", "line": 12, "side": "RIGHT", "body": "b",
                     "start_line": 10, "start_side": "RIGHT"},
                    {"path": "src/a.rs", "line": 11, "side": "LEFT", "body": "b"},
                ],
            })
        );
        assert_eq!(
            review_body(Some(&d), &[], 1, Event::Approve, "", "abc")["event"],
            "APPROVE"
        );
    }

    #[test]
    fn placement_tells_line_file_and_body_apart() {
        let d = parse_gh_diff(DIFF);
        assert_eq!(
            placement(Some(&d), &comment("src/a.rs", 11, 11, Side::New, 1)),
            Placement::Line
        );
        assert_eq!(
            placement(Some(&d), &comment("src/a.rs", 40, 40, Side::New, 1)),
            Placement::File
        );
        assert_eq!(
            placement(Some(&d), &comment("old.rs", 1, 1, Side::New, 1)),
            Placement::Body
        );
        // Without GitHub's diff nothing goes on a line.
        assert_eq!(
            placement(None, &comment("src/a.rs", 11, 11, Side::New, 1)),
            Placement::Body
        );
    }

    #[test]
    fn a_path_missing_from_the_diff_goes_in_the_body() {
        let d = parse_gh_diff(DIFF);
        let cs = [
            comment("old.rs", 4, 4, Side::New, 1),
            comment("old.rs", 5, 7, Side::New, 1),
        ];
        let v = review_body(Some(&d), &cs, 1, Event::Comment, "sum", "c");
        assert_eq!(v["comments"], json!([]));
        assert_eq!(v["body"], "sum\n\n`old.rs:4`: b\n\n`old.rs:5-7`: b");
        let v = review_body(Some(&d), &cs[..1], 1, Event::Comment, "", "c");
        assert_eq!(v["body"], "`old.rs:4`: b");
    }

    #[test]
    fn without_githubs_diff_every_comment_goes_in_the_main_text() {
        let cs = [
            comment("src/a.rs", 11, 11, Side::New, 1),
            comment("src/a.rs", 40, 41, Side::New, 1),
        ];
        let v = review_body(None, &cs, 1, Event::Comment, "sum", "c");
        assert_eq!(v["comments"], json!([]));
        assert_eq!(v["body"], "sum\n\n`src/a.rs:11`: b\n\n`src/a.rs:40-41`: b");
    }
}
