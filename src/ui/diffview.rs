//! The diff of the review board's current file, split or unified. Only the
//! rows on screen are highlighted and drawn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};

use crate::git::{DiffLine, FileDiff, FileStatus};
use crate::highlight::Highlighter;
use crate::review::{
    DiffView, Pair, ReviewCard, Side, comment_anchors, file_path, line_text, lines, split_rows,
};
use crate::theme::Theme;
use crate::visible::visible_code;

enum Row {
    Hunk(usize),
    Unified(usize),
    Split(Pair),
    /// One line of comment `ci`'s body.
    Comment {
        ci: usize,
        part: usize,
    },
}

impl Row {
    fn has(&self, i: usize) -> bool {
        match *self {
            Row::Unified(j) => j == i,
            Row::Split((l, r)) => l == Some(i) || r == Some(i),
            _ => false,
        }
    }
}

/// First row to show so the cursor sits a third of the way down, without
/// scrolling past the end.
pub fn scroll_top(cursor_row: usize, total: usize, height: usize) -> usize {
    cursor_row
        .saturating_sub(height / 3)
        .min(total.saturating_sub(height))
}

pub fn render(frame: &mut Frame, area: Rect, rc: &ReviewCard, t: &Theme, hl: &mut Highlighter) {
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().bg(t.bg).fg(t.fg));
    let dim = Style::new().fg(t.dim);
    let Some(f) = rc.file() else {
        buf.set_stringn(area.x + 1, area.y, "No changes", area.width as usize, dim);
        return;
    };
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let new_path = file_path(f);
    let old_path = f.old_path.as_deref().unwrap_or(new_path);
    match rc.view {
        DiffView::Split => {
            let [l, _, r] = columns(head);
            header(buf, l, old_path, "before", t);
            header(buf, r, new_path, "after", t);
        }
        DiffView::Unified if f.status == FileStatus::Renamed => {
            let from = visible_code(&format!("from {}", old_path.display()));
            header(buf, head, new_path, &from, t);
        }
        DiffView::Unified => header(buf, head, new_path, status_label(f.status), t),
    }
    let note = if f.binary {
        "Binary file, not shown"
    } else if f.hunks.is_empty() {
        "No line changes"
    } else {
        ""
    };
    if !note.is_empty() {
        buf.set_stringn(body.x + 1, body.y, note, body.width as usize, dim);
        return;
    }

    let all: Vec<&DiffLine> = lines(f).collect();
    let rows = build_rows(rc, f);
    let find = |i: usize| rows.iter().position(|r| r.has(i));
    let cursor_row = find(rc.cursor.line).unwrap_or(0);
    let anchor_row = rc.cursor.anchor.and_then(find).unwrap_or(cursor_row);
    let selected = anchor_row.min(cursor_row)..=anchor_row.max(cursor_row);
    let h = body.height as usize;
    let top = scroll_top(cursor_row, rows.len(), h);
    let ctx = Ctx {
        t,
        gutter: gutter_width(f),
        new_key: new_path.to_path_buf(),
        // The two sides keep separate highlighter state.
        old_key: Path::new("\0old").join(old_path),
    };

    for (k, row) in rows.iter().enumerate().skip(top).take(h) {
        let y = body.y + (k - top) as u16;
        let line = Rect::new(body.x, y, body.width, 1);
        let mark = Mark {
            selected: selected.contains(&k),
            cursor: k == cursor_row,
        };
        match *row {
            Row::Hunk(i) => {
                let hk = &f.hunks[i];
                let text = format!(
                    "⋯ @@ -{},{} +{},{} @@",
                    hk.old_start, hk.old_len, hk.new_start, hk.new_len
                );
                buf.set_stringn(line.x + 1, y, text, line.width as usize, dim);
            }
            Row::Unified(i) => {
                let l = all[i];
                let (key, no) = match l {
                    DiffLine::Del { old, .. } => (&ctx.old_key, *old),
                    DiffLine::Add { new, .. } | DiffLine::Context { new, .. } => {
                        (&ctx.new_key, *new)
                    }
                };
                ctx.line(buf, hl, line, Some((l, key, no)), true, mark);
            }
            Row::Split((li, ri)) => {
                let [lr, div, rr] = columns(line);
                let left = li.map(|i| {
                    let l = all[i];
                    let no = match l {
                        DiffLine::Del { old, .. } | DiffLine::Context { old, .. } => *old,
                        DiffLine::Add { new, .. } => *new,
                    };
                    (l, &ctx.old_key, no)
                });
                let right = ri.map(|i| {
                    let l = all[i];
                    let no = match l {
                        DiffLine::Add { new, .. } | DiffLine::Context { new, .. } => *new,
                        DiffLine::Del { old, .. } => *old,
                    };
                    (l, &ctx.new_key, no)
                });
                ctx.line(buf, hl, lr, left, false, mark);
                buf.set_string(div.x, y, "│", Style::new().fg(t.line));
                ctx.line(buf, hl, rr, right, false, mark);
            }
            Row::Comment { ci, part } => {
                let area = match rc.view {
                    DiffView::Unified => line,
                    DiffView::Split => {
                        let [lr, div, rr] = columns(line);
                        buf.set_string(div.x, y, "│", Style::new().fg(t.line));
                        if rc.comments[ci].side == Side::Old {
                            lr
                        } else {
                            rr
                        }
                    }
                };
                ctx.comment(buf, area, rc, ci, part);
            }
        }
    }
}

fn columns(area: Rect) -> [Rect; 3] {
    Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area)
}

fn status_label(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Added => "new file",
        FileStatus::Deleted => "deleted",
        FileStatus::Untracked => "untracked",
        FileStatus::Modified | FileStatus::Renamed => "",
    }
}

fn header(buf: &mut Buffer, area: Rect, path: &Path, label: &str, t: &Theme) {
    buf.set_style(area, Style::new().bg(t.panel));
    let (x, _) = buf.set_stringn(
        area.x + 1,
        area.y,
        visible_code(&path.display().to_string()),
        area.width.saturating_sub(1) as usize,
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
    );
    if !label.is_empty() {
        let room = area.right().saturating_sub(x) as usize;
        buf.set_stringn(
            x,
            area.y,
            format!(" · {label}"),
            room,
            Style::new().fg(t.dim),
        );
    }
}

/// Rows of the current file: a header row per hunk, the lines, and comment
/// boxes under the line each comment is anchored to.
fn build_rows(rc: &ReviewCard, f: &FileDiff) -> Vec<Row> {
    let mut under: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (line, ci) in comment_anchors(rc, rc.cursor.file) {
        under.entry(line).or_default().push(ci);
    }
    let mut rows = Vec::new();
    let push_comments = |rows: &mut Vec<Row>, line: usize| {
        for &ci in under.get(&line).into_iter().flatten() {
            let parts = rc.comments[ci].body.lines().count().max(1);
            rows.extend((0..parts).map(|part| Row::Comment { ci, part }));
        }
    };
    match rc.view {
        DiffView::Unified => {
            let mut i = 0;
            for (h, hunk) in f.hunks.iter().enumerate() {
                rows.push(Row::Hunk(h));
                for _ in &hunk.lines {
                    rows.push(Row::Unified(i));
                    push_comments(&mut rows, i);
                    i += 1;
                }
            }
        }
        DiffView::Split => {
            for (h, pairs) in split_rows(f).into_iter().enumerate() {
                rows.push(Row::Hunk(h));
                for (l, r) in pairs {
                    rows.push(Row::Split((l, r)));
                    if let Some(l) = l {
                        push_comments(&mut rows, l);
                    }
                    if let Some(r) = r.filter(|&r| Some(r) != l) {
                        push_comments(&mut rows, r);
                    }
                }
            }
        }
    }
    rows
}

/// Digits of the largest line number, plus a space.
fn gutter_width(f: &FileDiff) -> u16 {
    let max = f
        .hunks
        .iter()
        .map(|h| (h.old_start + h.old_len).max(h.new_start + h.new_len))
        .max()
        .unwrap_or(0);
    max.to_string().len().max(3) as u16 + 1
}

#[derive(Clone, Copy)]
struct Mark {
    selected: bool,
    cursor: bool,
}

struct Ctx<'a> {
    t: &'a Theme,
    gutter: u16,
    new_key: PathBuf,
    old_key: PathBuf,
}

impl Ctx<'_> {
    /// One diff line (or a blank padding row when `line` is `None`), with its
    /// number, a `+`/`-` sign in unified view, and highlighted text.
    fn line(
        &self,
        buf: &mut Buffer,
        hl: &mut Highlighter,
        area: Rect,
        line: Option<(&DiffLine, &PathBuf, u32)>,
        sign: bool,
        mark: Mark,
    ) {
        let t = self.t;
        let (bg, fg, sign_char) = match line.map(|(l, ..)| l) {
            Some(DiffLine::Add { .. }) => (t.add_bg, t.add, '+'),
            Some(DiffLine::Del { .. }) => (t.del_bg, t.del, '-'),
            Some(DiffLine::Context { .. }) | None => (t.bg, t.fg, ' '),
        };
        let bg = if mark.selected { t.sel } else { bg };
        buf.set_style(area, Style::new().bg(bg));
        let Some((l, key, no)) = line else { return };
        let width = area.width as usize;
        let num = if mark.cursor {
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.dim)
        };
        let g = self.gutter as usize;
        let (mut x, _) =
            buf.set_stringn(area.x, area.y, format!("{no:>w$} ", w = g - 1), width, num);
        if sign {
            (x, _) = buf.set_stringn(x, area.y, sign_char.to_string(), 1, Style::new().fg(fg));
        }
        for (style, text) in hl.highlight_line(key, no, line_text(l)) {
            let room = area.right().saturating_sub(x) as usize;
            if room == 0 {
                break;
            }
            // Plain text takes the colour of the change; syntax colours stay.
            let style = if style.fg == Some(t.fg) {
                style.fg(fg)
            } else {
                style
            };
            (x, _) = buf.set_stringn(x, area.y, visible_code(&text), room, style);
        }
    }

    fn comment(&self, buf: &mut Buffer, area: Rect, rc: &ReviewCard, ci: usize, part: usize) {
        let t = self.t;
        let c = &rc.comments[ci];
        let old = c.round < rc.round;
        let x = area.x + self.gutter;
        let box_area = Rect::new(x, area.y, area.right().saturating_sub(x), 1);
        buf.set_style(box_area, Style::new().bg(t.panel));
        let label = match (old, c.line == c.end_line) {
            (false, true) => "you ✎ ".to_string(),
            (false, false) => format!("you ✎ {}-{} ", c.line, c.end_line),
            (true, true) => format!("old ✎ {} ", c.line),
            (true, false) => format!("old ✎ {}-{} ", c.line, c.end_line),
        };
        let width = label.chars().count();
        let room = box_area.width as usize;
        let mut x = x + 1;
        if part == 0 {
            let style = if old {
                Style::new().fg(t.dim)
            } else {
                Style::new().fg(t.hi).add_modifier(Modifier::BOLD)
            };
            (x, _) = buf.set_stringn(x, area.y, &label, room.saturating_sub(1), style);
        } else {
            x += width as u16;
        }
        let text = c.body.lines().nth(part).unwrap_or("");
        let fg = if old { t.dim } else { t.fg };
        let rest = area.right().saturating_sub(x) as usize;
        buf.set_stringn(x, area.y, visible_code(text), rest, Style::new().fg(fg));
    }
}
