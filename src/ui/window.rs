//! The one box every window over aq is drawn in, tmux popups included:
//! the same width, border, colours and title, `Ctrl-Q close` at the top
//! right, and the window's own keys along the bottom.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding};

use crate::theme::Theme;

/// The widest a window gets: text this wide reads in one sweep.
const WIDTH: u16 = 100;

/// Rows of a short window, border included.
const SHORT_ROWS: u16 = 12;

/// The key that closes every window, and what the top border says of it.
pub const CLOSE: (&str, &str) = ("Ctrl-Q", "close");

/// How tall a window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Height {
    /// Nearly the whole body: for windows that show text, lists or a session.
    Tall,
    /// A fixed few rows: for prompts and questions.
    Short,
}

/// Where a window of `height` goes in `area`: centred, at the same width
/// whatever its height.
pub fn place(area: Rect, height: Height) -> Rect {
    let w = (area.width * 4 / 5).min(WIDTH).max(area.width.min(20));
    let h = match height {
        Height::Tall => area.height.saturating_sub(2).max(area.height.min(6)),
        Height::Short => SHORT_ROWS.min(area.height),
    };
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Columns and rows the border and padding take from a window.
pub const INSET: (u16, u16) = (4, 2);

/// `keys` (key, what it does) as a line of hints.
pub fn hints(keys: &[(&str, &str)], t: &Theme) -> Line<'static> {
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, what)) in keys.iter().enumerate() {
        let gap = if i + 1 == keys.len() { " " } else { "  " };
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(format!(" {what}{gap}"), dim));
    }
    Line::from(spans)
}

/// Clears `panel` and draws the window's box in it, titled `title`, with
/// `keys` along the bottom. Returns the area inside, for its text.
pub fn draw(frame: &mut Frame, panel: Rect, title: &str, keys: &[(&str, &str)], t: &Theme) -> Rect {
    frame.render_widget(Clear, panel);
    // A dash of border before the title and after the keys, as tmux
    // draws a popup's title, so aq's windows and its popups match.
    let edge = || Span::styled("─", Style::new().fg(t.acc));
    let at_right = |keys: &[(&str, &str)]| {
        let mut line = hints(keys, t);
        line.spans.push(edge());
        line.right_aligned()
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(Line::from(vec![
            edge(),
            Span::styled(
                format!(" {title} "),
                Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
            ),
        ]))
        .title(at_right(&[CLOSE]))
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    if !keys.is_empty() {
        block = block.title_bottom(at_right(keys));
    }
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_tall_windows_share_a_width_and_a_centre() {
        let area = Rect::new(0, 1, 160, 40);
        let tall = place(area, Height::Tall);
        let short = place(area, Height::Short);
        assert_eq!((tall.width, short.width), (100, 100));
        assert_eq!(tall.x, short.x);
        assert_eq!((tall.height, short.height), (38, 12));
        assert!(short.y > tall.y && short.bottom() < tall.bottom());
    }

    #[test]
    fn a_small_screen_shrinks_them_to_fit() {
        let area = Rect::new(0, 0, 30, 8);
        for h in [Height::Tall, Height::Short] {
            let p = place(area, h);
            assert!(p.width <= 30 && p.height <= 8, "{h:?} {p:?}");
        }
    }
}
