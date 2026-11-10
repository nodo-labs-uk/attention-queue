//! The question card: the session's last message, and the keys that
//! answer it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::window_of;
use crate::app::{App, project_name};
use crate::model::Card;
use crate::question::QuestionCard;

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, qc: &QuestionCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let head = vec![
        Line::styled(
            "Claude is asking",
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(format!("  window {}", window_of(app, card)), dim),
        ]),
    ];
    // The same layout as a permission card: both are a thing to read and
    // a yes or no.
    let [head_area, body, more, keys] = crate::permission::areas(area);
    frame.render_widget(Paragraph::new(head), head_area);
    let rows = qc.rows(body.width as usize);
    let top = qc
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let shown: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(body.height as usize)
        .map(|r| Line::styled(r.clone(), Style::new().fg(t.fg)))
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
    let mut spans = Vec::new();
    for (k, label) in [
        ("a", "approve"),
        ("Enter", "reply"),
        ("x", "put away"),
        ("g", "go to window"),
        ("?", "ask"),
    ] {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}  "), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
}
