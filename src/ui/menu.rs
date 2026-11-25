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

/// The widest the card's text runs, and the narrowest the card is drawn.
const MEASURE: usize = 76;
const NARROWEST: usize = 44;

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, mc: &MenuCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut lines = vec![
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
        let mark = if i == mc.sel { "❯" } else { " " };
        // The option as drawn starts with the same number as its key.
        let text = option
            .strip_prefix(&format!("{}.", i + 1))
            .map_or(option.as_str(), str::trim_start);
        lines.push(Line::from(vec![
            Span::styled(format!(" {n} "), key),
            Span::styled(format!("{mark} "), dim),
            Span::styled(visible_code(text), Style::new().fg(t.fg)),
        ]));
    }
    if mc.picked {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Picked: waiting for the menu to close", dim));
    }
    // The box fits its widest line; a longer one wraps inside it.
    let width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .clamp(NARROWEST, MEASURE)
        .min(usize::from(area.width.saturating_sub(4)))
        .max(1);
    let height: usize = lines.iter().map(|l| l.width().div_ceil(width).max(1)).sum();
    let inner = super::confirm::card_box(
        frame,
        area,
        "Claude Code asks",
        (width as u16, height as u16),
        t,
    );
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
