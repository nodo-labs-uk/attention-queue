//! A small text box: single or multi-line. Enter adds a new line (or submits a
//! single-line box), Ctrl-S submits, Esc cancels. Long lines wrap. The
//! shell's editing keys work on the cursor's line: Ctrl-U, Ctrl-K, Ctrl-W,
//! Ctrl-A, Ctrl-E, Alt-B, Alt-F and Alt-Backspace.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

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

    /// A single-line box holding `text`, the cursor at its end.
    pub fn single_line_with(text: &str) -> Self {
        TextInput {
            lines: vec![text.to_string()],
            row: 0,
            col: text.chars().count(),
            multi_line: false,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputEvent {
        if key.kind == KeyEventKind::Release {
            return InputEvent::Editing;
        }
        let col = self.col;
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => return InputEvent::Submit(self.text()),
                KeyCode::Char('a') => self.col = 0,
                KeyCode::Char('e') => self.col = self.len(self.row),
                KeyCode::Char('u') => self.cut(0, col),
                KeyCode::Char('k') => self.cut(col, self.len(self.row)),
                KeyCode::Char('w') => self.cut(self.word_start(char::is_whitespace), col),
                KeyCode::Left => self.col = self.word_start(not_word),
                KeyCode::Right => self.col = self.word_end(),
                _ => {}
            }
            return InputEvent::Editing;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Char('b') | KeyCode::Left => self.col = self.word_start(not_word),
                KeyCode::Char('f') | KeyCode::Right => self.col = self.word_end(),
                KeyCode::Backspace => self.cut(self.word_start(not_word), col),
                _ => {}
            }
            return InputEvent::Editing;
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
            KeyCode::Char(c) => {
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

    /// The cursor is on the last line, and that line is empty.
    pub fn on_empty_last_line(&self) -> bool {
        self.row + 1 == self.lines.len() && self.lines[self.row].is_empty()
    }

    /// Removes characters `from..to` of the cursor's line; the cursor goes
    /// to `from`.
    fn cut(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte_at(from), self.byte_at(to));
        self.lines[self.row].replace_range(a..b, "");
        self.col = from;
    }

    /// Where the word before the cursor starts: back over `gap`
    /// characters, then over the rest.
    fn word_start(&self, gap: fn(char) -> bool) -> usize {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col;
        while i > 0 && gap(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && !gap(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    /// Where the word at or after the cursor ends.
    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut i = self.col;
        while i < chars.len() && not_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && !not_word(chars[i]) {
            i += 1;
        }
        i
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn byte_at(&self, col: usize) -> usize {
        let line = &self.lines[self.row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }
}

/// Not part of a word for Alt-B, Alt-F and Alt-Backspace.
fn not_word(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_')
}

/// Draws the box centred in `area`, at a fixed size, with the cursor placed.
pub fn render(frame: &mut Frame, area: Rect, input: &TextInput, title: &str, t: &Theme) {
    let height = if input.multi_line { 10 } else { 5 };
    let submit = if input.multi_line { "save" } else { "send" };
    render_sized(frame, area, input, title, submit, t, (72, height));
}

/// `render` in a box of `size` (columns, rows), shrunk to fit `area`;
/// `submit` names what the submitting key does.
/// Where a box of `size` sits in `area`: centred.
fn panel(area: Rect, size: (u16, u16)) -> Rect {
    let [h] = Layout::horizontal([Constraint::Length(size.0.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Length(size.1.min(area.height))])
        .flex(Flex::Center)
        .areas(h);
    panel
}

/// The rows under the first inside a box of `size`, for what goes with a
/// one-line input.
pub fn below_first_row(area: Rect, size: (u16, u16)) -> Rect {
    let p = panel(area, size);
    Rect::new(
        p.x + 2,
        p.y + 2,
        p.width.saturating_sub(4),
        p.height.saturating_sub(3),
    )
}

pub fn render_sized(
    frame: &mut Frame,
    area: Rect,
    input: &TextInput,
    title: &str,
    submit: &str,
    t: &Theme,
    size: (u16, u16),
) {
    render_with_keys(frame, area, input, title, submit, &[], t, size);
}

/// As `render_sized`, with `more` keys (key, what it does) named along the
/// bottom of the box between the submit key and Esc.
#[allow(clippy::too_many_arguments)]
pub fn render_with_keys(
    frame: &mut Frame,
    area: Rect,
    input: &TextInput,
    title: &str,
    submit: &str,
    more: &[(&str, &str)],
    t: &Theme,
    size: (u16, u16),
) {
    let panel = panel(area, size);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut hints = if input.multi_line {
        vec![
            Span::styled(" Ctrl-S", key),
            Span::styled(format!(" {submit}  "), dim),
            Span::styled("Enter", key),
            Span::styled(" new line  ", dim),
        ]
    } else {
        vec![
            Span::styled(" Enter", key),
            Span::styled(format!(" {submit}  "), dim),
        ]
    };
    for (k, what) in more {
        hints.push(Span::styled(k.to_string(), key));
        hints.push(Span::styled(format!(" {what}  "), dim));
    }
    hints.extend([Span::styled("Esc", key), Span::styled(" cancel ", dim)]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" {title} "))
        .title_bottom(Line::from(hints).right_aligned())
        // A space each side keeps the text off the border.
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (w, h) = (inner.width as usize, inner.height as usize);
    let (rows, cursor) = input.wrapped(w);
    let top = cursor.1.saturating_sub(h - 1);
    let lines: Vec<Line> = rows.into_iter().skip(top).take(h).map(Line::raw).collect();
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position(Position::new(
        inner.x + cursor.0 as u16,
        inner.y + (cursor.1 - top) as u16,
    ));
}

impl TextInput {
    /// The text cut into rows of `width` characters, and the cursor as
    /// (column, row) among them. Each line takes one row more than it
    /// fills when full, so the cursor at its end has a place.
    fn wrapped(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut cursor = (0, 0);
        for (i, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if i == self.row {
                cursor = (self.col % width, rows.len() + self.col / width);
            }
            for n in 0..=chars.len() / width {
                let end = ((n + 1) * width).min(chars.len());
                rows.push(chars[n * width..end].iter().collect());
            }
        }
        (rows, cursor)
    }
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
    fn long_lines_wrap_and_the_cursor_follows() {
        let mut i = TextInput::single_line();
        typed(&mut i, "abcdefg");
        let (rows, cursor) = i.wrapped(3);
        assert_eq!(rows, ["abc", "def", "g"]);
        assert_eq!(cursor, (1, 2));
        typed(&mut i, "hi");
        let (rows, cursor) = i.wrapped(3);
        assert_eq!(
            rows,
            ["abc", "def", "ghi", ""],
            "a full row leaves room for the cursor"
        );
        assert_eq!(cursor, (0, 3));
        let mut m = TextInput::multi_line();
        typed(&mut m, "abcd\nx");
        assert_eq!(
            m.wrapped(3),
            (vec!["abc".into(), "d".into(), "x".into()], (1, 2))
        );
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

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    #[test]
    fn shell_editing_keys() {
        let mut i = TextInput::single_line();
        typed(&mut i, "git push origin-main now");
        i.handle_key(ctrl('w'));
        assert_eq!(i.text(), "git push origin-main ");
        i.handle_key(alt(KeyCode::Backspace));
        assert_eq!(i.text(), "git push origin-", "Alt-Backspace stops at -");
        i.handle_key(alt(KeyCode::Char('b')));
        i.handle_key(alt(KeyCode::Char('b')));
        assert_eq!(i.col, 4, "on push");
        i.handle_key(ctrl('k'));
        assert_eq!(i.text(), "git ");
        typed(&mut i, "pull");
        i.handle_key(ctrl('a'));
        i.handle_key(alt(KeyCode::Char('f')));
        assert_eq!(i.col, 3);
        i.handle_key(ctrl('e'));
        i.handle_key(ctrl('u'));
        assert_eq!(i.text(), "");
        let mut m = TextInput::multi_line();
        typed(&mut m, "keep\ncut this");
        m.handle_key(ctrl('u'));
        assert_eq!(m.text(), "keep\n", "only the cursor's line");
        m.handle_key(alt(KeyCode::Char('x')));
        assert_eq!(m.text(), "keep\n", "other Alt keys type nothing");
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
