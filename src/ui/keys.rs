use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

/// Bottom row: the keys that work right now, and the latest status message.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let keys: &[(&str, &str)] = if app.overlay.is_some() {
        &[("q", "close")]
    } else if app.queue.current().is_some() {
        &[
            ("?", "ask"),
            ("s", "skip"),
            ("M", "mode"),
            ("T", "theme"),
            ("Q", "quit"),
        ]
    } else {
        &[("T", "theme"), ("Q", "quit")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(Span::styled(
            *k,
            Style::new().fg(t.key).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}   "), Style::new().fg(t.dim)));
    }
    let base = Style::new().bg(t.panel).fg(t.fg);
    let keys = Line::from(spans);
    let keys_w = (keys.width() as u16).min(area.width);
    frame.render_widget(Paragraph::new(keys).style(base), area);
    if let Some(msg) = app.status_text() {
        // Only the space right of the keys; a message too long for it is cut at the end.
        let [_, rest] =
            Layout::horizontal([Constraint::Length(keys_w), Constraint::Fill(1)]).areas(area);
        let text = format!("{msg} ");
        let align = if text.chars().count() > rest.width as usize {
            Alignment::Left
        } else {
            Alignment::Right
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::new().fg(t.hi))
                .alignment(align),
            rest,
        );
    }
}
