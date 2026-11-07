//! The permission card: which tool wants what, and the keys that answer it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::window_of;
use crate::app::{App, project_name};
use crate::model::Card;
use crate::permission::PermissionCard;

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
    let [head_area, body, keys] = Layout::vertical([
        Constraint::Length(head.len() as u16 + 1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area.inner(Margin::new(1, 1)));
    frame.render_widget(Paragraph::new(head), head_area);
    frame.render_widget(
        Paragraph::new(pc.body())
            .style(Style::new().fg(t.fg))
            .wrap(Wrap { trim: false }),
        body,
    );
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
