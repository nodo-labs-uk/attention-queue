//! The step card: what Claude says the edit is for, the edit inside its
//! whole file (folded around the change), and this session's earlier steps.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::card_header;
use super::diffview::scroll_top;
use super::fold::Seg;
use crate::app::App;
use crate::edit::EditPreview;
use crate::highlight::Highlighter;
use crate::model::Card;
use crate::step::{RowKind, SearchList, StepCard};
use crate::theme::Theme;
use crate::visible::visible_code;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: &Card,
    sc: &StepCard,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let mut head = card_header(app, card);
    head[0].spans.push(Span::styled(
        format!("  step {}", sc.step_no),
        Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
    ));
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    match sc.description.as_deref().map(split_description) {
        Some((title, why)) => {
            head.push(Line::styled(visible_code(&title), bold));
            if let Some(why) = why {
                head.push(Line::from(vec![
                    Span::styled("Why: ", dim),
                    Span::styled(visible_code(&why), Style::new().fg(t.fg)),
                ]));
            }
        }
        None => {
            head.push(Line::styled(
                visible_code(&format!("{} {}", sc.tool_name, file_name(&sc.path))),
                bold,
            ));
            head.push(Line::styled(
                "Claude gave no description. m shows the raw input.",
                dim,
            ));
        }
    }
    let [head_area, body] = Layout::vertical([
        Constraint::Length(head.len() as u16 + 1),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(head), head_area.inner(Margin::new(1, 0)));
    let side_w = (body.width / 4).clamp(14, 26);
    let [side, file] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Fill(1)]).areas(body);
    let buf = frame.buffer_mut();
    render_side(buf, side, app, sc, t);
    let rel = sc
        .path
        .strip_prefix(&card.checkout.0)
        .map_or_else(|_| sc.path.clone(), Path::to_path_buf);
    render_file(buf, file, sc, &rel, t, hl);
}

/// The first line (or sentence) is the title; the rest is the reason.
fn split_description(d: &str) -> (String, Option<String>) {
    let d = d.trim();
    let (title, rest) = match d.split_once('\n') {
        Some(split) => split,
        None => match d.find(". ") {
            Some(i) => (&d[..=i], &d[i + 2..]),
            None => (d, ""),
        },
    };
    let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    (title.trim().to_string(), (!rest.is_empty()).then_some(rest))
}

/// The last part of `path`, safe to show.
fn file_name(path: &Path) -> String {
    visible_code(&path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    ))
}

/// "this change": where the edit lands. "earlier": this session's decided steps.
fn render_side(buf: &mut Buffer, area: Rect, app: &App, sc: &StepCard, t: &Theme) {
    buf.set_style(area, Style::new().bg(t.panel).fg(t.fg));
    let width = area.width.saturating_sub(1) as usize;
    let heading = Style::new().fg(t.dim).add_modifier(Modifier::BOLD);
    let name = file_name(&sc.path);
    let mut lines: Vec<(String, Style)> = vec![("this change".into(), heading)];
    let starts = sc.change_rows();
    let current = starts.iter().rposition(|&s| s <= sc.view.cursor);
    if sc.changed().is_empty() {
        lines.push((format!("  {name}"), Style::new().fg(t.fg)));
    }
    for (k, r) in sc.changed().iter().enumerate() {
        let style = if Some(k) == current {
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.fg)
        };
        lines.push((format!("  {name} :{}", r.start + 1), style));
    }
    let earlier: Vec<_> = app
        .steps
        .history
        .iter()
        .filter(|d| d.session_id == sc.session_id)
        .collect();
    if !earlier.is_empty() {
        lines.push((String::new(), Style::new()));
        lines.push(("earlier".into(), heading));
        // The latest steps, when they do not all fit.
        let room = (area.height as usize).saturating_sub(lines.len());
        for d in &earlier[earlier.len().saturating_sub(room)..] {
            let (mark, colour) = if d.allowed {
                ("✓", t.ok)
            } else {
                ("✗", t.del)
            };
            lines.push((
                format!("  {mark} {} {}", d.step_no, file_name(&d.path)),
                Style::new().fg(colour),
            ));
        }
    }
    for (i, (text, style)) in lines.iter().take(area.height as usize).enumerate() {
        buf.set_stringn(area.x + 1, area.y + i as u16, text, width, *style);
    }
}

fn render_file(
    buf: &mut Buffer,
    area: Rect,
    sc: &StepCard,
    rel: &Path,
    t: &Theme,
    hl: &mut Highlighter,
) {
    buf.set_style(area, Style::new().bg(t.bg).fg(t.fg));
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    buf.set_style(head, Style::new().bg(t.panel));
    let (x, _) = buf.set_stringn(
        head.x + 1,
        head.y,
        visible_code(&rel.display().to_string()),
        head.width.saturating_sub(1) as usize,
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
    );
    let (label, colour) = match &sc.preview {
        _ if sc.view.raw => ("raw tool input".to_string(), t.dim),
        EditPreview::Placed { before, .. } if before.is_empty() => ("new file".into(), t.dim),
        EditPreview::Placed { .. } => ("full file, change highlighted".into(), t.dim),
        EditPreview::Unplaced { why, .. } => (format!("not placed: {why}"), t.hi),
    };
    let room = head.right().saturating_sub(x) as usize;
    buf.set_stringn(
        x,
        head.y,
        format!(" · {label}"),
        room,
        Style::new().fg(colour),
    );

    let h = body.height as usize;
    let w = body.width as usize;
    if sc.view.raw {
        for (i, l) in sc.raw.lines().take(h).enumerate() {
            let text = visible_code(l);
            buf.set_stringn(
                body.x + 1,
                body.y + i as u16,
                text,
                w.saturating_sub(1),
                Style::new(),
            );
        }
        return;
    }
    let segs = sc.segments();
    let cursor = segs
        .iter()
        .position(|s| s.contains(sc.view.cursor))
        .unwrap_or(0);
    let top = scroll_top(cursor, segs.len(), h);
    let max_no = sc.rows.iter().map(|r| r.no).max().unwrap_or(0);
    let gutter = max_no.to_string().len().max(3);
    let new_key = sc.path.clone();
    let old_key = PathBuf::from("\0old").join(&sc.path);
    for (k, seg) in segs.iter().enumerate().skip(top).take(h) {
        let y = body.y + (k - top) as u16;
        let line = Rect::new(body.x, y, body.width, 1);
        let at_cursor = k == cursor;
        let num = if at_cursor {
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.dim)
        };
        let i = match seg {
            Seg::Fold(r) => {
                if at_cursor {
                    buf.set_style(line, Style::new().bg(t.sel));
                }
                let n = r.len();
                let text = format!("{:>gutter$}  ▸ {n} lines", "");
                buf.set_stringn(line.x, y, text, w, Style::new().fg(t.dim));
                continue;
            }
            Seg::Row(i) => *i,
        };
        let row = &sc.rows[i];
        let (bg, fg, sign, key) = match row.kind {
            RowKind::Same => (t.bg, t.fg, ' ', &new_key),
            RowKind::Add => (t.add_bg, t.add, '+', &new_key),
            RowKind::Del => (t.del_bg, t.del, '-', &old_key),
        };
        buf.set_style(line, Style::new().bg(bg));
        let (x, _) = buf.set_stringn(line.x, y, format!("{:>gutter$} ", row.no), w, num);
        let (mut x, _) = buf.set_stringn(x, y, sign.to_string(), 1, Style::new().fg(fg));
        for (style, text) in hl.highlight_line(key, row.no, &row.text) {
            let room = line.right().saturating_sub(x) as usize;
            if room == 0 {
                break;
            }
            // Plain text takes the colour of the change; syntax colours stay.
            let style = if style.fg == Some(t.fg) {
                style.fg(fg)
            } else {
                style
            };
            (x, _) = buf.set_stringn(x, y, visible_code(&text), room, style);
        }
    }
}

/// `gd` results, in a box over the card.
pub fn render_search(frame: &mut Frame, area: Rect, list: &SearchList, t: &Theme) {
    let [h] = Layout::horizontal([Constraint::Length(90.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Length(14.min(area.height))])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" Where {} appears ", list.ident))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    let buf = frame.buffer_mut();
    let dim = Style::new().fg(t.dim);
    let w = inner.width.saturating_sub(1) as usize;
    let hits = match &list.hits {
        None => {
            buf.set_stringn(inner.x + 1, inner.y, "Searching…", w, dim);
            return;
        }
        Some(h) if h.is_empty() => {
            buf.set_stringn(inner.x + 1, inner.y, "No matches in tracked files", w, dim);
            return;
        }
        Some(h) => h,
    };
    let h = inner.height as usize;
    let top = scroll_top(list.cursor, hits.len(), h);
    for (k, hit) in hits.iter().enumerate().skip(top).take(h) {
        let y = inner.y + (k - top) as u16;
        if k == list.cursor {
            buf.set_style(
                Rect::new(inner.x, y, inner.width, 1),
                Style::new().bg(t.sel),
            );
        }
        let place = visible_code(&format!("{}:{}", hit.path.display(), hit.line));
        let (x, _) = buf.set_stringn(inner.x + 1, y, &place, w, Style::new().fg(t.key));
        let room = inner.right().saturating_sub(x + 2) as usize;
        let text = visible_code(hit.text.trim());
        buf.set_stringn(x + 2, y, text, room, Style::new().fg(t.fg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::agent;
    use crate::model::{CardKind, Checkout};
    use crate::step::Done;
    use crate::step::tests::{input, pending, step_app};
    use crate::ui::tests::render;

    #[test]
    fn description_splits_into_title_and_why() {
        assert_eq!(
            split_description("Return 429.\nThe plan says so."),
            ("Return 429.".into(), Some("The plan says so.".into()))
        );
        assert_eq!(
            split_description("Return 429. The plan says so."),
            ("Return 429.".into(), Some("The plan says so.".into()))
        );
        assert_eq!(split_description("Just this"), ("Just this".into(), None));
    }

    #[test]
    fn hidden_characters_in_a_step_card_are_spelled_out() {
        let mut a = step_app();
        let mut i = input();
        i["new_string"] = "        2 => 404,\n        3 => 4\u{202e}29,\n".into();
        let mut e = pending("s1", "/a", i);
        e.description = Some("Return\u{2066} 429.\nWhy\u{200b} not.".into());
        a.update(Msg::PreTool {
            req_id: 1,
            edit: e,
            at: crate::step::tests::tick(),
        });
        let screen = render(&a, 100, 30);
        assert!(screen.contains("3 => 4\\u{202e}29,"), "{screen}");
        assert!(screen.contains("Return\\u{2066} 429."), "{screen}");
        assert!(screen.contains("Why\\u{200b} not."), "{screen}");
        assert!(!screen.contains('\u{202e}'));
        // The raw input too.
        a.update(Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('m')),
            crate::step::tests::tick(),
        ));
        let screen = render(&a, 100, 30);
        assert!(!screen.contains('\u{202e}'), "{screen}");
    }

    #[test]
    fn step_card_folded() {
        let mut a = step_app();
        a.agents = vec![agent("%1", 1, "api", "/a", false)];
        a.agents[0].session_id = "s1".into();
        a.steps.history = vec![
            Done {
                session_id: "s1".into(),
                step_no: 1,
                path: "/a/src/store.rs".into(),
                allowed: true,
                key: Default::default(),
                after: None,
            },
            Done {
                session_id: "s1".into(),
                step_no: 2,
                path: "/a/src/config.rs".into(),
                allowed: false,
                key: Default::default(),
                after: None,
            },
            Done {
                session_id: "other".into(),
                step_no: 1,
                path: "/a/src/hidden.rs".into(),
                allowed: true,
                key: Default::default(),
                after: None,
            },
        ];
        let mut e = pending("s1", "/a", input());
        e.description = Some(
            "Return 429 when a client runs out of tokens.\n\
             The plan says limits must be visible to clients."
                .into(),
        );
        a.update(Msg::PreTool {
            req_id: 1,
            edit: e,
            at: crate::step::tests::tick(),
        });
        assert!(matches!(a.queue.current().unwrap().kind, CardKind::Step(_)));
        assert_eq!(a.queue.current().unwrap().checkout, Checkout("/a".into()));
        insta::assert_snapshot!(render(&a, 90, 24));
    }
}
