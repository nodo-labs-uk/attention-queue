//! A small text box: single or multi-line. Enter adds a new line (or submits a
//! single-line box), Ctrl-S submits, Esc cancels.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    lines: Vec<String>,
    row: usize,
    /// Cursor column in characters.
    col: usize,
    multi_line: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Editing,
    Submit(String),
    Cancel,
}

impl TextInput {
    pub fn single_line() -> Self {
        Self::new(false)
    }

    pub fn multi_line() -> Self {
        Self::new(true)
    }

    fn new(multi_line: bool) -> Self {
        TextInput {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            multi_line,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputEvent {
        if key.kind == KeyEventKind::Release {
            return InputEvent::Editing;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('s') => InputEvent::Submit(self.text()),
                _ => InputEvent::Editing,
            };
        }
        match key.code {
            KeyCode::Esc => return InputEvent::Cancel,
            KeyCode::Enter if !self.multi_line => return InputEvent::Submit(self.text()),
            KeyCode::Enter => {
                let at = self.byte_at(self.col);
                let rest = self.lines[self.row].split_off(at);
                self.row += 1;
                self.lines.insert(self.row, rest);
                self.col = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                let at = self.byte_at(self.col);
                self.lines[self.row].insert(at, c);
                self.col += 1;
            }
            KeyCode::Backspace if self.col > 0 => {
                self.col -= 1;
                let at = self.byte_at(self.col);
                self.lines[self.row].remove(at);
            }
            KeyCode::Backspace if self.row > 0 => {
                let line = self.lines.remove(self.row);
                self.row -= 1;
                self.col = self.len(self.row);
                self.lines[self.row].push_str(&line);
            }
            KeyCode::Delete if self.col < self.len(self.row) => {
                let at = self.byte_at(self.col);
                self.lines[self.row].remove(at);
            }
            KeyCode::Delete if self.row + 1 < self.lines.len() => {
                let next = self.lines.remove(self.row + 1);
                self.lines[self.row].push_str(&next);
            }
            KeyCode::Left if self.col > 0 => self.col -= 1,
            KeyCode::Left if self.row > 0 => {
                self.row -= 1;
                self.col = self.len(self.row);
            }
            KeyCode::Right if self.col < self.len(self.row) => self.col += 1,
            KeyCode::Right if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Up if self.row > 0 => {
                self.row -= 1;
                self.col = self.col.min(self.len(self.row));
            }
            KeyCode::Down if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = self.col.min(self.len(self.row));
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.len(self.row),
            _ => {}
        }
        InputEvent::Editing
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte_at(&self, col: usize) -> usize {
        let line = &self.lines[self.row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }
}

/// Draws the box centred in `area`, at a fixed size, with the cursor placed.
pub fn render(frame: &mut Frame, area: Rect, input: &TextInput, title: &str, t: &Theme) {
    let height = if input.multi_line { 10 } else { 3 };
    let [h] = Layout::horizontal([Constraint::Length(72.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut hints = vec![Span::styled(" Ctrl-S", key), Span::styled(" save  ", dim)];
    if input.multi_line {
        hints.extend([Span::styled("Enter", key), Span::styled(" new line  ", dim)]);
    }
    hints.extend([Span::styled("Esc", key), Span::styled(" cancel ", dim)]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" {title} "))
        .title_bottom(Line::from(hints).right_aligned())
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (w, h) = (inner.width as usize, inner.height as usize);
    let top = input.row.saturating_sub(h - 1);
    // Long lines scroll sideways on the cursor's row only.
    let left = input.col.saturating_sub(w - 1);
    let lines: Vec<Line> = input
        .lines
        .iter()
        .enumerate()
        .skip(top)
        .take(h)
        .map(|(i, l)| {
            let skip = if i == input.row { left } else { 0 };
            Line::raw(l.chars().skip(skip).collect::<String>())
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position(Position::new(
        inner.x + (input.col - left) as u16,
        inner.y + (input.row - top) as u16,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(input: &mut TextInput, s: &str) -> InputEvent {
        let mut last = InputEvent::Editing;
        for c in s.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            last = input.handle_key(KeyEvent::from(code));
        }
        last
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn multi_line_enter_adds_line_and_ctrl_s_submits() {
        let mut i = TextInput::multi_line();
        assert_eq!(typed(&mut i, "héllo\nworld"), InputEvent::Editing);
        assert_eq!(
            i.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            InputEvent::Submit("héllo\nworld".into())
        );
    }

    #[test]
    fn single_line_enter_submits() {
        let mut i = TextInput::single_line();
        assert_eq!(typed(&mut i, "ok\n"), InputEvent::Submit("ok".into()));
    }

    #[test]
    fn esc_cancels() {
        let mut i = TextInput::multi_line();
        typed(&mut i, "x");
        assert_eq!(i.handle_key(key(KeyCode::Esc)), InputEvent::Cancel);
    }

    #[test]
    fn editing_keys() {
        let mut i = TextInput::multi_line();
        typed(&mut i, "ab\ncd");
        i.handle_key(key(KeyCode::Home));
        i.handle_key(key(KeyCode::Backspace)); // joins the lines
        assert_eq!(i.text(), "abcd");
        typed(&mut i, "é");
        assert_eq!(i.text(), "abécd");
        i.handle_key(key(KeyCode::Left));
        i.handle_key(key(KeyCode::Delete));
        assert_eq!(i.text(), "abcd");
        i.handle_key(key(KeyCode::End));
        typed(&mut i, "\nz");
        i.handle_key(key(KeyCode::Up));
        i.handle_key(key(KeyCode::End));
        i.handle_key(key(KeyCode::Delete)); // joins the next line
        assert_eq!(i.text(), "abcdz");
    }
}
