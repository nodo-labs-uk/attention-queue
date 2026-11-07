//! Folding a step card's file around its change: each change keeps its
//! enclosing block (found by indentation) and three lines either side.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::step::{Row, clusters};

/// Lines shown either side of a change's block.
pub const CONTEXT: usize = 3;
const TAB_WIDTH: usize = 4;

/// One line of the file view: a row, or a closed fold of rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Row(usize),
    Fold(Range<usize>),
}

impl Seg {
    pub fn contains(&self, row: usize) -> bool {
        match self {
            Seg::Row(i) => *i == row,
            Seg::Fold(r) => r.contains(&row),
        }
    }

    pub fn first(&self) -> usize {
        match self {
            Seg::Row(i) => *i,
            Seg::Fold(r) => r.start,
        }
    }
}

/// Leading whitespace width, or `None` for a blank line.
fn indent(text: &str) -> Option<usize> {
    if text.trim().is_empty() {
        return None;
    }
    Some(
        text.chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
            .sum(),
    )
}

/// Row ranges folded by default: everything outside each change's block
/// and its context. A gap of a single row is shown rather than folded.
pub fn folds(rows: &[Row]) -> Vec<Range<usize>> {
    let mut keep = vec![false; rows.len()];
    for c in clusters(rows) {
        let (from, to) = block(rows, &c);
        let from = from.saturating_sub(CONTEXT);
        let to = (to + CONTEXT).min(rows.len() - 1);
        keep[from..=to].fill(true);
    }
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, _) in keep.iter().enumerate().filter(|(_, k)| !**k) {
        match out.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out.retain(|r| r.len() > 1);
    out
}

/// First and last row of the block enclosing `change`: from the nearest
/// line above that is indented less than the change, to the first line
/// after it indented no deeper than that line. A change at the top level
/// is its own block.
fn block(rows: &[Row], change: &Range<usize>) -> (usize, usize) {
    let own = (change.start, change.end - 1);
    let Some(depth) = rows[change.clone()]
        .iter()
        .filter_map(|r| indent(&r.text))
        .min()
    else {
        return own;
    };
    let head = rows[..change.start]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, r)| indent(&r.text).filter(|&d| d < depth).map(|d| (i, d)));
    let Some((head, head_depth)) = head else {
        return own;
    };
    let tail = rows[change.end..]
        .iter()
        .position(|r| indent(&r.text).is_some_and(|d| d <= head_depth))
        .map_or(rows.len() - 1, |i| change.end + i);
    (head, tail)
}

/// The view's lines: open folds show their rows, closed ones one line.
pub fn segments(n_rows: usize, folds: &[Range<usize>], open: &BTreeSet<usize>) -> Vec<Seg> {
    let mut out = Vec::with_capacity(n_rows);
    let mut closed = folds.iter().filter(|f| !open.contains(&f.start)).peekable();
    let mut i = 0;
    while i < n_rows {
        match closed.peek() {
            Some(f) if f.start == i => {
                out.push(Seg::Fold((*f).clone()));
                i = f.end;
                closed.next();
            }
            _ => {
                out.push(Seg::Row(i));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step::RowKind;

    fn rows(lines: &[(&str, RowKind)]) -> Vec<Row> {
        lines
            .iter()
            .enumerate()
            .map(|(i, (text, kind))| Row {
                kind: *kind,
                text: text.to_string(),
                no: i as u32 + 1,
                at: i as u32,
            })
            .collect()
    }

    #[test]
    fn keeps_the_enclosing_block_and_context() {
        use RowKind::{Add, Same};
        let mut lines: Vec<(&str, RowKind)> = vec![("top", Same); 10];
        lines.extend([
            ("fn f() {", Same),
            ("    a();", Same),
            ("    if x {", Same),
            ("        b();", Same),
            ("    }", Same),
            ("    c();", Add),
            ("    d();", Same),
            ("    e();", Same),
            ("}", Same),
        ]);
        lines.extend(vec![("bottom", Same); 10]);
        // Block rows 10..=18, kept with 3 either side: 7..=21.
        assert_eq!(folds(&rows(&lines)), [0..7, 22..29]);
    }

    #[test]
    fn top_level_change_keeps_just_context_and_tiny_gaps_stay() {
        use RowKind::{Add, Same};
        let lines = [
            ("a", Same),
            ("b", Same),
            ("c", Same),
            ("d", Same),
            ("e", Add),
            ("f", Same),
            ("g", Same),
            ("h", Same),
            ("i", Same),
        ];
        // Row 0 alone would fold to one line, so it shows.
        assert_eq!(folds(&rows(&lines)), Vec::<Range<usize>>::new());
        let mut more = lines.to_vec();
        more.extend([("j", Same), ("k", Same)]);
        assert_eq!(folds(&rows(&more)), vec![8..11]);
    }

    #[test]
    fn segments_open_and_close() {
        let folds = [0..3, 5..8];
        let closed = segments(9, &folds, &BTreeSet::new());
        assert_eq!(
            closed,
            [
                Seg::Fold(0..3),
                Seg::Row(3),
                Seg::Row(4),
                Seg::Fold(5..8),
                Seg::Row(8)
            ]
        );
        let open = segments(9, &folds, &BTreeSet::from([5]));
        assert_eq!(open.len(), 7);
        assert_eq!(open[3], Seg::Row(5));
    }
}
