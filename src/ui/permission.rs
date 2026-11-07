//! The permission card: which tool wants what, and the keys that answer it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::window_of;
use crate::app::{App, project_name};
use crate::model::Card;
use crate::permission::{LineKind, PermissionCard};

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, pc: &PermissionCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let head = vec![
        Line::styled(
            format!("Permission: {}", pc.tool_name),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(
                format!("  window {}", window_of(app, card)),
                Style::new().fg(t.dim),
            ),
        ]),
    ];
    let [head_area, body, more, keys] = crate::permission::areas(area);
    frame.render_widget(Paragraph::new(head), head_area);
    let rows = pc.body_rows(body.width as usize);
    let top = pc
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let shown: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(body.height as usize)
        .map(|(kind, text)| {
            let colour = match kind {
                LineKind::Plain => t.fg,
                LineKind::Del => t.del,
                LineKind::Add => t.add,
            };
            Line::styled(text.clone(), Style::new().fg(colour))
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    let hidden = rows.len().saturating_sub(top + body.height as usize);
    if hidden > 0 {
        let s = if hidden == 1 { "" } else { "s" };
        frame.render_widget(
            Paragraph::new(format!("▾ {hidden} more line{s} (j to scroll)"))
                .style(Style::new().fg(t.hi).add_modifier(Modifier::BOLD)),
            more,
        );
    }
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut spans = Vec::new();
    for (k, label) in [
        ("y", "allow"),
        ("n", "deny"),
        ("g", "go to window"),
        ("?", "ask"),
    ] {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}  "), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
}
