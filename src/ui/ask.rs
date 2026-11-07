use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::input;
use crate::ask::{AskState, LineKind, panel};
use crate::theme::Theme;

/// The questions and answers in a panel over the card, with the question
/// being typed in a box along its bottom edge.
pub fn render(frame: &mut Frame, area: Rect, ask: &AskState, t: &Theme) {
    let panel = panel(area);
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.line))
        .title(" Ask about this card ")
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (rows, _) = ask.lines(usize::from(inner.width));
    let h = usize::from(inner.height);
    let scroll = ask.scroll.min(rows.len().saturating_sub(h));
    let lines: Vec<Line> = rows
        .into_iter()
        .skip(scroll)
        .take(h)
        .map(|(kind, text)| {
            let style = match kind {
                LineKind::Question => Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
                LineKind::Answer => Style::new().fg(t.fg),
                LineKind::Pending => Style::new().fg(t.dim),
                LineKind::Error => Style::new().fg(t.hi),
            };
            Line::styled(text, style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some(i) = &ask.input {
        let h = panel.height.min(3);
        let bottom = Rect::new(panel.x, panel.bottom() - h, panel.width, h);
        input::render(frame, bottom, i, "Ask about this card", t);
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{Msg, Overlay};
    use crate::ask::{Answer, Entry};
    use crate::model::{CardKind, ReviewCard};
    use crate::queue::tests::card;
    use crate::ui::tests::{app, render};

    #[test]
    fn overlay_shows_pending_answer_and_error_with_retry() {
        let mut a = app();
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 0));
        a.update(key('?'));
        let Some(Overlay::Ask(ask)) = &mut a.overlay else {
            panic!()
        };
        ask.input = None;
        ask.entries = vec![
            Entry {
                question: "why?".into(),
                answer: Answer::Pending,
            },
            Entry {
                question: "how?".into(),
                answer: Answer::Failed("took too long".into()),
            },
            Entry {
                question: "who?".into(),
                answer: Answer::Done("Kiwi did.".into()),
            },
        ];
        let screen = render(&a, 80, 24);
        for want in [
            "? why?",
            "Asking…",
            "took too long",
            "r to retry",
            "Kiwi did.",
        ] {
            assert!(screen.contains(want), "{want} in\n{screen}");
        }
    }

    fn key(c: char) -> Msg {
        Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c)),
            std::time::Instant::now(),
        )
    }
}
