use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::window::{self, Height};
use super::workspace::shown;
use crate::reader::{Reader, focus};
use crate::theme::Theme;

/// The word on screen, its focus letter on the column the guide marks,
/// with where you are and the speed under it.
pub fn render(frame: &mut Frame, area: Rect, r: &Reader, t: &Theme) {
    let panel = window::place(area, Height::Short);
    let dim = Style::new().fg(t.dim);
    let keys = [
        ("Space", if r.playing() { "pause" } else { "play" }),
        ("↑/↓", "speed"),
        ("←/→", "back/on"),
    ];
    let inner = window::draw(frame, panel, "Reading fast", &keys, t);

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
