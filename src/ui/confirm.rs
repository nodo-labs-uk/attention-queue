use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::theme::Theme;

/// A yes/no question in the same rounded box as the text inputs.
pub fn render(frame: &mut Frame, area: Rect, title: &str, lines: &[String], t: &Theme) {
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
    let hints = Line::from(vec![
        Span::styled(" y", key),
        Span::styled(" yes  ", dim),
        Span::styled("n", key),
        Span::styled(" no ", dim),
    ]);
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
