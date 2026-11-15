//! The menu card: what Claude Code's menu asks, its options numbered as
//! the keys that pick them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::window_of;
use crate::app::{App, project_name};
use crate::menu::MenuCard;
use crate::model::Card;
use crate::visible::visible_code;

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, mc: &MenuCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut lines = vec![
        Line::styled(
            "Claude Code asks",
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(format!("  window {}", window_of(app, card)), dim),
        ]),
        Line::raw(""),
    ];
    lines.extend(
        mc.menu
            .question
            .iter()
            .map(|q| Line::styled(visible_code(q), Style::new().fg(t.fg))),
    );
    lines.push(Line::raw(""));
    for (i, option) in mc.menu.options.iter().enumerate() {
        let n = if i < 9 {
            format!("{}", i + 1)
        } else {
            " ".into()
        };
        let mark = if i == mc.menu.cursor { "❯" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(format!(" {n} "), key),
            Span::styled(format!("{mark} "), dim),
            Span::styled(visible_code(option), Style::new().fg(t.fg)),
        ]));
    }
    if mc.picked {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Picked: waiting for the menu to close", dim));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}
