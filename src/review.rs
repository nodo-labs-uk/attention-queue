//! Review board card state and its keys. Pure: no I/O.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use serde::{Deserialize, Serialize};

use crate::git::{DiffLine, FileDiff};

/// Sent by `a` when the agent is waiting.
pub const APPROVE_MESSAGE: &str = "Approved, carry on.";
const QUOTE_MAX: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    #[default]
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub path: PathBuf,
    pub line: u32,
    pub end_line: u32,
    pub side: Side,
    pub body: String,
    pub round: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffView {
    #[default]
    Split,
    Unified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub file: usize,
    /// Index into the current file's lines, all hunks in order (see `lines`).
    pub line: usize,
    /// Where a `V` selection started.
    pub anchor: Option<usize>,
    /// First key of a two-key command (`]c`, `[c`).
    pub pending: Option<char>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewCard {
    pub round: u32,
    pub from_tree: String,
    pub to_tree: String,
    /// Where the round starts: the last round's tree, or the branch base in
    /// round 1. The full-branch view changes `from_tree`, never this.
    pub round_base: String,
    pub files: Vec<FileDiff>,
    /// This round's comments and, marked by their round, earlier rounds' ones.
    pub comments: Vec<Comment>,
    pub view: DiffView,
    /// Markdown files shown as written, marks and all, instead of read.
    pub markdown_source: bool,
    pub show_full_branch: bool,
    pub cursor: Cursor,
    /// Files marked done with `d`: left out of moving around and shown
    /// ticked in the file list.
    pub done: BTreeSet<PathBuf>,
}

/// What the last sent round of a checkout left behind, saved across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRound {
    /// Tree the agent was reviewed at; the next round diffs from here.
    pub tree: String,
    pub round: u32,
    pub comments: Vec<Comment>,
}

/// Where a new comment goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: u32,
    pub end_line: u32,
    pub side: Side,
}

/// What a review key asks the app to do beyond changing the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Comment(Target),
    Send,
    Approve,
    ToggleFullBranch,
    Status(&'static str),
}

/// One row of the split view: line indices on the left (before) and right (after).
pub type Pair = (Option<usize>, Option<usize>);

impl ReviewCard {
    pub fn new(
        round: u32,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        comments: Vec<Comment>,
        show_full_branch: bool,
    ) -> Self {
        let mut card = ReviewCard {
            round,
            round_base: from_tree.clone(),
            from_tree,
            to_tree,
            files,
            comments,
            show_full_branch,
            ..Default::default()
        };
        card.set_file(0);
        card
    }

    pub fn file(&self) -> Option<&FileDiff> {
        self.files.get(self.cursor.file)
    }

    /// Replaces the diff (a new snapshot or the `r` toggle), keeping comments.
    pub fn replace_diff(
        &mut self,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        show_full_branch: bool,
    ) {
        let path = self.file().map(|f| file_path(f).to_path_buf());
        // A done file stays done only while its diff is the one looked at.
        let old = std::mem::take(&mut self.files);
        self.done.retain(|p| {
            let diff = |fs: &[FileDiff]| fs.iter().find(|f| file_path(f) == p).cloned();
            diff(&old).is_some() && diff(&old) == diff(&files)
        });
        self.from_tree = from_tree;
        self.to_tree = to_tree;
        self.files = files;
        self.show_full_branch = show_full_branch;
        let same = path.and_then(|p| self.files.iter().position(|f| file_path(f) == p));
        self.set_file(same.unwrap_or(0));
        if self.is_done(self.cursor.file) {
            self.next_file(true);
        }
    }

    /// File `i` is marked done.
    pub fn is_done(&self, i: usize) -> bool {
        self.files
            .get(i)
            .is_some_and(|f| self.done.contains(file_path(f)))
    }

    /// Every file is marked done.
    pub fn all_done(&self) -> bool {
        !self.files.is_empty() && (0..self.files.len()).all(|i| self.is_done(i))
    }

    /// `d`: marks the file done and moves to the next one not done; on a
    /// done file (once all are), unmarks it.
    fn toggle_done(&mut self) {
        let Some(path) = self.file().map(|f| file_path(f).to_path_buf()) else {
            return;
        };
        if !self.done.remove(&path) {
            self.done.insert(path);
            self.next_file(true);
        }
    }

    /// Comments of this round, the ones that get sent.
    pub fn current_comments(&self) -> impl Iterator<Item = &Comment> {
        self.comments.iter().filter(|c| c.round == self.round)
    }

    fn set_file(&mut self, file: usize) {
        self.cursor.file = file;
        self.cursor.anchor = None;
        self.cursor.line = self
            .file()
            .and_then(|f| change_starts(f).first().copied())
            .unwrap_or(0);
    }

    /// Line indices of the `V` selection, or of the cursor without one. In
    /// split view that is every line on the selected rows, both sides.
    pub fn selected_lines(&self) -> Vec<usize> {
        let Some(f) = self.file() else {
            return Vec::new();
        };
        let line = self.cursor.line;
        let anchor = self.cursor.anchor.unwrap_or(line);
        let (a, b) = (anchor.min(line), anchor.max(line));
        match self.view {
            DiffView::Unified => (a..=b).collect(),
            DiffView::Split => {
                let rows: Vec<Pair> = split_rows(f).into_iter().flatten().collect();
                let (Some(p), Some(q)) = (row_of(&rows, anchor), row_of(&rows, line)) else {
                    return vec![line];
                };
                let mut out: Vec<usize> = rows[p.min(q)..=p.max(q)]
                    .iter()
                    .flat_map(|&(l, r)| l.into_iter().chain(r))
                    .collect();
                out.sort_unstable();
                out.dedup();
                out
            }
        }
    }

    /// The selected lines as a comment target. A selection with any line of
    /// the after side comments on the after side.
    pub fn target(&self) -> Option<Target> {
        let f = self.file()?;
        let all: Vec<(Side, u32)> = lines(f).map(side_no).collect();
        let sel: Vec<(Side, u32)> = self
            .selected_lines()
            .into_iter()
            .filter_map(|i| all.get(i).copied())
            .collect();
        let side = if sel.iter().any(|(s, _)| *s == Side::New) {
            Side::New
        } else {
            Side::Old
        };
        let nos = sel.iter().filter(|(s, _)| *s == side).map(|(_, n)| *n);
        Some(Target {
            path: file_path(f).to_path_buf(),
            line: nos.clone().min()?,
            end_line: nos.max()?,
            side,
        })
    }

    fn move_line(&mut self, down: bool) {
        let Some(f) = self.file() else { return };
        let line = self.cursor.line;
        self.cursor.line = match self.view {
            DiffView::Unified => {
                let n = lines(f).count();
                if down {
                    (line + 1).min(n.saturating_sub(1))
                } else {
                    line.saturating_sub(1)
                }
            }
            DiffView::Split => {
                let rows: Vec<Pair> = split_rows(f).into_iter().flatten().collect();
                let Some(p) = row_of(&rows, line) else {
                    return;
                };
                let p = if down {
                    (p + 1).min(rows.len() - 1)
                } else {
                    p.saturating_sub(1)
                };
                primary(rows[p])
            }
        };
    }

    fn next_change(&mut self, forward: bool) {
        let Some(f) = self.file() else { return };
        let line = self.cursor.line;
        let starts = change_starts(f);
        let here = if forward {
            starts.iter().find(|&&s| s > line)
        } else {
            starts.iter().rev().find(|&&s| s < line)
        };
        if let Some(&s) = here {
            self.cursor.line = s;
            return;
        }
        let file = self.cursor.file;
        let others: Vec<usize> = if forward {
            (file + 1..self.files.len()).collect()
        } else {
            (0..file).rev().collect()
        };
        for i in others.into_iter().filter(|&i| !self.is_done(i)) {
            let starts = change_starts(&self.files[i]);
            let s = if forward {
                starts.first()
            } else {
                starts.last()
            };
            if let Some(&s) = s {
                self.cursor.file = i;
                self.cursor.line = s;
                self.cursor.anchor = None;
                return;
            }
        }
    }

    /// gg and G: the first or last line of the file.
    fn go_to_end(&mut self, end: bool) {
        let Some(f) = self.file() else { return };
        self.cursor.line = match self.view {
            DiffView::Unified if end => lines(f).count().saturating_sub(1),
            DiffView::Unified => 0,
            DiffView::Split => {
                let rows: Vec<Pair> = split_rows(f).into_iter().flatten().collect();
                let Some(&p) = (if end { rows.last() } else { rows.first() }) else {
                    return;
                };
                primary(p)
            }
        };
    }

    fn next_file(&mut self, forward: bool) {
        let n = self.files.len();
        if n == 0 {
            return;
        }
        let file = self.cursor.file;
        // The nearest file not done, wrapping; none left keeps the cursor.
        let next = (1..=n)
            .map(|k| {
                if forward {
                    (file + k) % n
                } else {
                    (file + n * k - k) % n
                }
            })
            .find(|&i| !self.is_done(i));
        if let Some(i) = next
            && i != file
        {
            self.set_file(i);
        }
    }

    /// Removes a comment of this round on the selected lines. Returns false if there is none.
    fn delete_comment(&mut self) -> bool {
        let Some(f) = self.file() else { return false };
        let path = file_path(f).to_path_buf();
        let under: Vec<(Side, u32)> = self
            .selected_lines()
            .into_iter()
            .filter_map(|i| lines(f).nth(i).map(side_no))
            .collect();
        let found = self.comments.iter().position(|c| {
            c.round == self.round
                && c.path == path
                && under
                    .iter()
                    .any(|&(s, n)| s == c.side && (c.line..=c.end_line).contains(&n))
        });
        found.map(|i| self.comments.remove(i)).is_some()
    }
}

/// Handles a review key. Keys outside the review board's set do nothing.
pub fn on_key(card: &mut ReviewCard, key: KeyEvent) -> Action {
    // ]c [c change, ]] [[ file, gg top; anything else is read as a key of its own.
    if let Some(first) = card.cursor.pending.take() {
        match (first, key.code) {
            (']' | '[', KeyCode::Char('c')) => {
                card.next_change(first == ']');
                return Action::None;
            }
            (']', KeyCode::Char(']')) | ('[', KeyCode::Char('[')) => {
                card.next_file(first == ']');
                return Action::None;
            }
            ('g', KeyCode::Char('g')) => {
                card.go_to_end(false);
                return Action::None;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char(c @ (']' | '[' | 'g')) => card.cursor.pending = Some(c),
        KeyCode::Char('G') => card.go_to_end(true),
        KeyCode::Char('}') => card.next_change(true),
        KeyCode::Char('{') => card.next_change(false),
        KeyCode::Char('v') => {
            card.view = match card.view {
                DiffView::Split => DiffView::Unified,
                DiffView::Unified => DiffView::Split,
            };
        }
        KeyCode::Char('m') => card.markdown_source = !card.markdown_source,
        KeyCode::Char('j') | KeyCode::Down => card.move_line(true),
        KeyCode::Char('k') | KeyCode::Up => card.move_line(false),
        KeyCode::Tab | KeyCode::Char('L') => card.next_file(true),
        KeyCode::BackTab | KeyCode::Char('H') => card.next_file(false),
        KeyCode::Char('V') => {
            card.cursor.anchor = match card.cursor.anchor {
                Some(_) => None,
                None => Some(card.cursor.line),
            };
        }
        KeyCode::Esc => card.cursor.anchor = None,
        KeyCode::Char('c') => {
            return match card.target() {
                Some(t) => {
                    card.cursor.anchor = None;
                    Action::Comment(t)
                }
                None => Action::Status("Nothing to comment on here"),
            };
        }
        KeyCode::Char('x') if !card.delete_comment() => {
            return Action::Status("No comment of this round on this line");
        }
        KeyCode::Char('s') => return Action::Send,
        KeyCode::Char('a') => return Action::Approve,
        KeyCode::Char('d') => card.toggle_done(),
        KeyCode::Char('u') => card.done.clear(),
        KeyCode::Char('r') => return Action::ToggleFullBranch,
        _ => {}
    }
    Action::None
}

/// The message `s` sends: this round's comments, ordered by path then line.
pub fn compose_message(card: &ReviewCard) -> String {
    let mut comments: Vec<&Comment> = card.current_comments().collect();
    comments.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    let mut out = format!("Review comments (round {}):", card.round);
    for c in comments {
        out.push_str(&format!("\n\n{}:{}", c.path.display(), c.line));
        if c.end_line != c.line {
            out.push_str(&format!("-{}", c.end_line));
        }
        if let Some(q) = quote(card, c) {
            out.push_str(&format!("\n> {q}"));
        }
        out.push('\n');
        out.push_str(&c.body);
    }
    out
}

/// The first changed line in the comment's range (or its first line if none
/// changed), trimmed to `QUOTE_MAX` characters.
fn quote(card: &ReviewCard, c: &Comment) -> Option<String> {
    let f = card.files.iter().find(|f| file_path(f) == c.path)?;
    let in_range: Vec<&DiffLine> = lines(f)
        .filter(|l| {
            let (s, n) = side_no(l);
            s == c.side && (c.line..=c.end_line).contains(&n)
        })
        .collect();
    let l = in_range
        .iter()
        .find(|l| is_change(l))
        .or(in_range.first())?;
    Some(line_text(l).trim().chars().take(QUOTE_MAX).collect())
}

pub fn file_path(f: &FileDiff) -> &Path {
    f.new_path
        .as_deref()
        .or(f.old_path.as_deref())
        .unwrap_or(Path::new(""))
}

/// All lines of a file's hunks, in order.
pub fn lines(f: &FileDiff) -> impl Iterator<Item = &DiffLine> {
    f.hunks.iter().flat_map(|h| &h.lines)
}

pub fn line_text(l: &DiffLine) -> &str {
    match l {
        DiffLine::Context { text, .. }
        | DiffLine::Add { text, .. }
        | DiffLine::Del { text, .. } => text,
    }
}

/// The side a line belongs to for comments, and its number there. Context counts as new.
pub fn side_no(l: &DiffLine) -> (Side, u32) {
    match l {
        DiffLine::Del { old, .. } => (Side::Old, *old),
        DiffLine::Add { new, .. } | DiffLine::Context { new, .. } => (Side::New, *new),
    }
}

pub fn is_change(l: &DiffLine) -> bool {
    !matches!(l, DiffLine::Context { .. })
}

/// Line index where each change (a run of added or deleted lines) starts.
pub fn change_starts(f: &FileDiff) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    for h in &f.hunks {
        let mut prev_change = false;
        for l in &h.lines {
            let change = is_change(l);
            if change && !prev_change {
                out.push(i);
            }
            prev_change = change;
            i += 1;
        }
    }
    out
}

/// Split view rows, per hunk. Each run of deletions sits beside the additions
/// that follow it; the shorter side is padded with blank rows.
pub fn split_rows(f: &FileDiff) -> Vec<Vec<Pair>> {
    let mut i = 0;
    f.hunks
        .iter()
        .map(|h| {
            let mut rows = Vec::with_capacity(h.lines.len());
            let (mut dels, mut adds) = (Vec::new(), Vec::new());
            for l in &h.lines {
                match l {
                    DiffLine::Del { .. } => {
                        if !adds.is_empty() {
                            flush(&mut rows, &mut dels, &mut adds);
                        }
                        dels.push(i);
                    }
                    DiffLine::Add { .. } => adds.push(i),
                    DiffLine::Context { .. } => {
                        flush(&mut rows, &mut dels, &mut adds);
                        rows.push((Some(i), Some(i)));
                    }
                }
                i += 1;
            }
            flush(&mut rows, &mut dels, &mut adds);
            rows
        })
        .collect()
}

fn flush(rows: &mut Vec<Pair>, dels: &mut Vec<usize>, adds: &mut Vec<usize>) {
    for k in 0..dels.len().max(adds.len()) {
        rows.push((dels.get(k).copied(), adds.get(k).copied()));
    }
    dels.clear();
    adds.clear();
}

/// Position of the split row showing line `i`.
pub fn row_of(rows: &[Pair], i: usize) -> Option<usize> {
    rows.iter().position(|&(l, r)| l == Some(i) || r == Some(i))
}

/// The line a split row stands for when the cursor is on it: the after side if any.
fn primary((l, r): Pair) -> usize {
    r.or(l).unwrap_or(0)
}

/// Line index each comment of file `file` is shown under, as (line, comment)
/// pairs. A comment goes under its last line, or the nearest line if that is
/// not in this diff (earlier rounds' comments, or a refreshed diff).
pub fn comment_anchors(card: &ReviewCard, file: usize) -> Vec<(usize, usize)> {
    let Some(f) = card.files.get(file) else {
        return Vec::new();
    };
    let path = file_path(f);
    let nos: Vec<(Side, u32)> = lines(f).map(side_no).collect();
    if nos.is_empty() {
        return Vec::new();
    }
    card.comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.path == path)
        .map(|(ci, c)| {
            let exact = nos.iter().position(|&sn| sn == (c.side, c.end_line));
            let line = exact.unwrap_or_else(|| {
                (0..nos.len())
                    .min_by_key(|&i| nos[i].1.abs_diff(c.end_line))
                    .unwrap_or(0)
            });
            (line, ci)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::git::parse_unified;

    /// Two files: `src/limit.rs` (one deletion run beside three additions) and
    /// `src/http.rs` (two separate changes).
    pub(crate) const FIXTURE: &str = "\
diff --git a/src/limit.rs b/src/limit.rs
--- a/src/limit.rs
+++ b/src/limit.rs
@@ -38,5 +38,6 @@
 pub fn check(store: &Store, key: &str)
     -> Result<(), LimitError> {
-    // TODO: real limits
-    Ok(())
+    let b = store.get(key).unwrap_or_default();
+    if b.tokens == 0 { return Err(TooMany) }
+    store.take(key)
 }
diff --git a/src/http.rs b/src/http.rs
--- a/src/http.rs
+++ b/src/http.rs
@@ -84,5 +84,6 @@
             Err(NotFound)  => StatusCode::NOT_FOUND,
             Err(Auth)      => StatusCode::UNAUTHORIZED,
+            Err(TooMany)   => StatusCode::TOO_MANY_REQUESTS,
             _              => StatusCode::INTERNAL_SERVER_ERROR,
         };
-        (status, self.to_string()).into_response()
+        (status, self.to_string()).into_response().with_retry()
";

    pub(crate) fn fixture_card(round: u32) -> ReviewCard {
        ReviewCard::new(
            round,
            "t0".into(),
            "t1".into(),
            parse_unified(FIXTURE.as_bytes()),
            Vec::new(),
            false,
        )
    }

    pub(crate) fn comment(path: &str, line: u32, end_line: u32, body: &str, round: u32) -> Comment {
        Comment {
            path: path.into(),
            line,
            end_line,
            side: Side::New,
            body: body.into(),
            round,
        }
    }

    fn press(card: &mut ReviewCard, keys: &str) -> Action {
        let mut last = Action::None;
        for c in keys.chars() {
            let code = match c {
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            last = on_key(card, KeyEvent::from(code));
        }
        last
    }

    #[test]
    fn compose_orders_by_path_then_line() {
        let mut card = fixture_card(2);
        card.comments = vec![
            comment("src/limit.rs", 41, 41, "zero tokens?", 2),
            comment("src/http.rs", 86, 86, "429 needs Retry-After", 2),
            comment("src/limit.rs", 40, 40, "what if the store is down?", 2),
            comment("src/limit.rs", 38, 38, "already sent", 1),
        ];
        assert_eq!(
            compose_message(&card),
            "Review comments (round 2):\n\
             \n\
             src/http.rs:86\n\
             > Err(TooMany)   => StatusCode::TOO_MANY_REQUESTS,\n\
             429 needs Retry-After\n\
             \n\
             src/limit.rs:40\n\
             > let b = store.get(key).unwrap_or_default();\n\
             what if the store is down?\n\
             \n\
             src/limit.rs:41\n\
             > if b.tokens == 0 { return Err(TooMany) }\n\
             zero tokens?"
        );
    }

    #[test]
    fn range_comment_formats_as_range() {
        let mut card = fixture_card(1);
        // Cursor starts on the first change of src/limit.rs: the deletion row,
        // whose after side is new line 40. Select two rows down, to new line 42.
        press(&mut card, "Vjj");
        let Action::Comment(t) = press(&mut card, "c") else {
            panic!("expected a comment target");
        };
        assert_eq!((t.line, t.end_line, t.side), (40, 42, Side::New));
        card.comments.push(Comment {
            path: t.path,
            line: t.line,
            end_line: t.end_line,
            side: t.side,
            body: "handle the store being down".into(),
            round: 1,
        });
        assert_eq!(
            compose_message(&card),
            "Review comments (round 1):\n\
             \n\
             src/limit.rs:40-42\n\
             > let b = store.get(key).unwrap_or_default();\n\
             handle the store being down"
        );
    }

    #[test]
    fn quote_is_trimmed_to_120_chars() {
        let long = format!("+    {}", "x".repeat(200));
        let diff = format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,1 @@\n{long}\n"
        );
        let mut card = ReviewCard::new(
            1,
            "a".into(),
            "b".into(),
            parse_unified(diff.as_bytes()),
            vec![],
            false,
        );
        card.comments.push(comment("a.txt", 1, 1, "long", 1));
        let msg = compose_message(&card);
        let quoted = msg.lines().find(|l| l.starts_with("> ")).unwrap();
        assert_eq!(quoted, format!("> {}", "x".repeat(120)));
    }

    #[test]
    fn deleted_lines_comment_on_old_side() {
        let mut card = fixture_card(1);
        card.view = DiffView::Unified;
        // Unified: cursor on the first deletion (old line 40); select both deletions.
        let Action::Comment(t) = press(&mut card, "Vjc") else {
            panic!("expected a comment target");
        };
        assert_eq!((t.line, t.end_line, t.side), (40, 41, Side::Old));
    }

    #[test]
    fn split_pads_shorter_side() {
        let card = fixture_card(1);
        let rows = split_rows(&card.files[0]);
        assert_eq!(
            rows[0],
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(4)),
                (Some(3), Some(5)),
                (None, Some(6)),
                (Some(7), Some(7)),
            ]
        );
    }

    #[test]
    fn change_keys_cross_files_and_tab_switches() {
        let mut card = fixture_card(1);
        assert_eq!((card.cursor.file, card.cursor.line), (0, 2));
        press(&mut card, "]c");
        assert_eq!((card.cursor.file, card.cursor.line), (1, 2));
        press(&mut card, "]c");
        assert_eq!((card.cursor.file, card.cursor.line), (1, 5));
        press(&mut card, "[c[c");
        assert_eq!((card.cursor.file, card.cursor.line), (0, 2));
        press(&mut card, "\t");
        assert_eq!((card.cursor.file, card.cursor.line), (1, 2));
        press(&mut card, "\t");
        assert_eq!(card.cursor.file, 0, "tab wraps");
    }

    #[test]
    fn d_marks_a_file_done_and_moving_skips_it() {
        let mut card = fixture_card(1);
        assert_eq!(card.cursor.file, 0);
        press(&mut card, "d");
        assert!(card.is_done(0));
        assert_eq!(card.cursor.file, 1, "on to the next file");
        for keys in ["L", "H", "]]", "[[", "}", "{"] {
            press(&mut card, keys);
            assert_eq!(card.cursor.file, 1, "{keys} skips the done file");
        }
        assert!(!card.all_done());
        press(&mut card, "d");
        assert!(card.all_done());
        press(&mut card, "d");
        assert!(!card.all_done(), "d on a done file unmarks it");
        press(&mut card, "u");
        assert!(card.done.is_empty());
        press(&mut card, "L");
        assert_eq!(card.cursor.file, 0);
    }

    #[test]
    fn a_done_file_comes_back_when_its_diff_changes() {
        let mut card = fixture_card(1);
        press(&mut card, "dd");
        assert!(card.all_done());
        let (from, to) = (card.from_tree.clone(), card.to_tree.clone());
        card.replace_diff(from.clone(), to.clone(), card.files.clone(), false);
        assert!(card.all_done(), "the same diff stays done");
        let mut files = card.files.clone();
        files[1].hunks[0].lines.push(DiffLine::Add {
            new: 99,
            text: "more".into(),
        });
        card.replace_diff(from, to, files, false);
        assert!(card.is_done(0) && !card.is_done(1));
        assert_eq!(card.cursor.file, 1, "the changed file is shown");
    }

    #[test]
    fn vim_keys_jump_through_the_diff() {
        let mut card = fixture_card(1);
        press(&mut card, "}");
        assert_eq!((card.cursor.file, card.cursor.line), (1, 2), "}} is ]c");
        press(&mut card, "{");
        assert_eq!((card.cursor.file, card.cursor.line), (0, 2), "{{ is [c");
        press(&mut card, "]]");
        assert_eq!(card.cursor.file, 1);
        press(&mut card, "[[");
        assert_eq!(card.cursor.file, 0);
        press(&mut card, "L");
        assert_eq!(card.cursor.file, 1);
        press(&mut card, "H");
        assert_eq!(card.cursor.file, 0);
        press(&mut card, "G");
        let last = card.cursor.line;
        press(&mut card, "j");
        assert_eq!(card.cursor.line, last, "G is the last line");
        press(&mut card, "gg");
        let first = card.cursor.line;
        press(&mut card, "k");
        assert_eq!(card.cursor.line, first, "gg is the first line");
        assert!(first < last);
    }

    #[test]
    fn view_toggle_keeps_cursor_line() {
        let mut card = fixture_card(1);
        press(&mut card, "j");
        // Split: from the row (del 2, add 4) down to (del 3, add 5); the cursor takes the after side.
        assert_eq!(card.cursor.line, 5);
        press(&mut card, "v");
        assert_eq!(card.view, DiffView::Unified);
        press(&mut card, "k");
        assert_eq!(card.cursor.line, 4);
    }

    #[test]
    fn x_deletes_only_this_rounds_comment_under_cursor() {
        let mut card = fixture_card(2);
        card.comments = vec![
            comment("src/limit.rs", 40, 40, "old", 1),
            comment("src/limit.rs", 40, 40, "new", 2),
        ];
        assert_eq!(press(&mut card, "x"), Action::None);
        assert_eq!(card.comments.len(), 1);
        assert_eq!(card.comments[0].body, "old");
        assert!(matches!(press(&mut card, "x"), Action::Status(_)));
        assert_eq!(card.comments.len(), 1);
    }

    #[test]
    fn old_comment_anchors_to_nearest_line() {
        let mut card = fixture_card(2);
        card.comments = vec![comment("src/limit.rs", 99, 99, "far below", 1)];
        assert_eq!(comment_anchors(&card, 0), [(7, 0)]);
    }
}
