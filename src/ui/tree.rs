//! The review board's file list: files grouped by folder, with this round's
//! comment count per file. Files marked done are dim and ticked. Hidden,
//! it is a strip of one mark per file.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};

use super::diffview::scroll_top;
use crate::git::FileStatus;
use crate::review::{ReviewCard, file_path};
use crate::theme::Theme;

enum Row {
    Dir(String),
    File(usize),
}

/// Columns of the strip that stands in for the hidden list.
const STRIP: u16 = 3;

/// `body` cut into the file list, narrow at the left, and the diff. With
/// the list hidden (`f`), it is a strip of marks.
pub fn split(body: Rect, hidden: bool) -> (Rect, Rect) {
    let width = if hidden {
        STRIP
    } else {
        (body.width / 6).clamp(14, 22)
    };
    let [tree, diff] =
        Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(body);
    (tree, diff)
}

/// The hidden list: a mark per file, in the list's order. `✓` is a file
/// marked done, `●` the file on screen and `○` a file still to review.
fn render_strip(frame: &mut Frame, area: Rect, rc: &ReviewCard, t: &Theme) {
    let buf = frame.buffer_mut();
    let h = area.height as usize;
    let top = scroll_top(rc.cursor.file, rc.files.len(), h);
    for (i, f) in rc.files.iter().enumerate().skip(top).take(h) {
        let (mark, colour) = match f.status {
            _ if rc.is_done(i) => ("✓", t.dim),
            _ if i == rc.cursor.file => ("●", t.hi),
            FileStatus::Added => ("○", t.add),
            FileStatus::Deleted => ("○", t.del),
            FileStatus::Modified | FileStatus::Renamed => ("○", t.fg),
        };
        let y = area.y + (i - top) as u16;
        if i == rc.cursor.file {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(t.sel));
        }
        buf.set_stringn(area.x + 1, y, mark, 1, Style::new().fg(colour));
    }
}

pub fn render(frame: &mut Frame, area: Rect, rc: &ReviewCard, t: &Theme, hidden: bool) {
    frame
        .buffer_mut()
        .set_style(area, Style::new().bg(t.panel).fg(t.fg));
    if hidden {
        return render_strip(frame, area, rc, t);
    }
    let buf = frame.buffer_mut();
    let mut rows = Vec::new();
    let mut last_dir = None;
    for (i, f) in rc.files.iter().enumerate() {
        let dir = file_path(f)
            .parent()
            .map(|d| d.display().to_string())
            .filter(|d| !d.is_empty());
        if dir != last_dir {
            if let Some(d) = &dir {
                rows.push(Row::Dir(format!("{d}/")));
            }
            last_dir = dir;
        }
        rows.push(Row::File(i));
    }
    let cur = rows
        .iter()
        .position(|r| matches!(r, Row::File(i) if *i == rc.cursor.file))
        .unwrap_or(0);
    let h = area.height as usize;
    let top = scroll_top(cur, rows.len(), h);
    let width = area.width.saturating_sub(1) as usize;
    for (k, row) in rows.iter().enumerate().skip(top).take(h) {
        let y = area.y + (k - top) as u16;
        match row {
            Row::Dir(d) => {
                let d = crate::visible::visible_code(d);
                buf.set_stringn(area.x + 1, y, d, width, Style::new().fg(t.dim));
            }
            Row::File(i) => {
                let f = &rc.files[*i];
                let path = file_path(f);
                let indent = if path.parent().is_some_and(|d| !d.as_os_str().is_empty()) {
                    "  "
                } else {
                    ""
                };
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let n = rc.current_comments().filter(|c| c.path == path).count();
                let count = if n > 0 {
                    format!(" ✎{n}")
                } else {
                    String::new()
                };
                let count_w = count.chars().count();
                if k == cur {
                    buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(t.sel));
                }
                let colour = match f.status {
                    _ if rc.is_done(*i) => t.dim,
                    FileStatus::Added => t.add,
                    FileStatus::Deleted => t.del,
                    FileStatus::Modified | FileStatus::Renamed => t.fg,
                };
                let name = if rc.is_done(*i) {
                    format!("✓ {name}")
                } else {
                    name
                };
                let style = if k == cur {
                    Style::new().fg(colour).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(colour)
                };
                let room = width.saturating_sub(count_w);
                let (x, _) = buf.set_stringn(
                    area.x + 1,
                    y,
                    format!("{indent}{}", crate::visible::visible_code(&name)),
                    room,
                    style,
                );
                buf.set_stringn(x, y, count, width, Style::new().fg(t.hi));
            }
        }
    }
}
