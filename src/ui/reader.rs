use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use super::input;
use super::workspace::shown;
use crate::reader::{Reader, focus};
use crate::theme::Theme;

/// A fixed size, so nothing moves from word to word.
const SIZE: (u16, u16) = (64, 9);

/// The word on screen, its focus letter on the column the guide marks,
/// with where you are and the speed under it.
pub fn render(frame: &mut Frame, area: Rect, r: &Reader, t: &Theme) {
    let panel = input::panel(area, SIZE);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut hints = vec![Span::raw(" ")];
    for (k, what) in [
        ("Space", if r.playing() { "pause" } else { "play" }),
        ("↑/↓", "speed"),
        ("←/→", "back/on"),
        ("Esc", "close"),
    ] {
        hints.push(Span::styled(k, key));
        hints.push(Span::styled(format!(" {what}  "), dim));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(" Reading fast ")
        .title_bottom(Line::from(hints).right_aligned())
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    // The focus letter sits a little left of the middle, as it does in the word.
    let width = inner.width as usize;
    let col = width * 2 / 5;
    let guide = |mark: &str| {
        Line::styled(
            format!(
                "{}{mark}{}",
                "─".repeat(col),
                "─".repeat(width.saturating_sub(col + 1))
            ),
            dim,
        )
    };
    let word: Vec<char> = shown(r.word()).chars().collect();
    let f = focus(r.word()).min(word.len().saturating_sub(1));
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let before: String = word[..f].iter().collect();
    let after: String = word
        .get(f + 1..)
        .map_or_else(String::new, |w| w.iter().collect());
    let word_line = Line::from(vec![
        Span::raw(" ".repeat(col.saturating_sub(f))),
        Span::styled(before, bold),
        Span::styled(
            word.get(f).map_or_else(String::new, char::to_string),
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ),
        Span::styled(after, bold),
    ]);
    let state = if !r.typed.is_empty() {
        Line::from(vec![
            Span::styled("New speed: ", dim),
            Span::styled(format!("{}▏", r.typed), bold),
            Span::styled(" wpm, Enter sets it", dim),
        ])
    } else {
        Line::styled(
            format!(
                "word {} of {} · {} wpm{} · type a number to set the speed",
                r.at + 1,
                r.words.len(),
                r.wpm,
                if r.playing() { "" } else { " · paused" }
            ),
            dim,
        )
    };
    let lines = vec![
        Line::raw(""),
        guide("┬"),
        word_line,
        guide("┴"),
        Line::raw(""),
        state,
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
