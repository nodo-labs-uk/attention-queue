use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::theme::Theme;

/// A question in a short window, with the keys that answer it (`hints`:
/// key, then what it does) along the bottom.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[String],
    hints: &[(&str, &str)],
    t: &Theme,
) {
    let panel = super::window::place(area, super::window::Height::Short);
    let inner = super::window::draw(frame, panel, title, hints, t);
    let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.clone())).collect();
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

/// A card drawn as a box in the middle of `area`, in the same style as the
/// boxes above. `size` is the text inside it, in columns and rows; the
/// box keeps a column of space on each side. Returns the area for the text.
pub fn card_box(frame: &mut Frame, area: Rect, title: &str, size: (u16, u16), t: &Theme) -> Rect {
    let panel = super::input::centred(area, (size.0 + 4, size.1 + 2));
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    inner
}
