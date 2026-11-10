use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::theme::Theme;

/// A question in the same rounded box as the text inputs, with the keys
/// that answer it (`hints`: key, then what it does) along the bottom.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[String],
    hints: &[(&str, &str)],
    t: &Theme,
) {
    let [h] = Layout::horizontal([Constraint::Length(64.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let height = (lines.len() as u16 + 2).min(area.height);
    let [panel] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, what)) in hints.iter().enumerate() {
        let gap = if i + 1 == hints.len() { " " } else { "  " };
        spans.push(Span::styled(*k, key));
        spans.push(Span::styled(format!(" {what}{gap}"), dim));
    }
    let hints = Line::from(spans);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" {title} "))
        .title_bottom(hints.right_aligned())
        .style(Style::new().bg(t.panel).fg(t.fg));
    let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.clone())).collect();
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        panel,
    );
}
