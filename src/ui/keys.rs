use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
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
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base), area);
    if let Some(msg) = &app.status_msg {
        frame.render_widget(
            Paragraph::new(format!("{msg} "))
                .style(Style::new().fg(t.hi))
                .alignment(Alignment::Right),
            area,
        );
    }
}
