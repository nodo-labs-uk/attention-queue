use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::input;
use super::window::{self, Height};
use crate::ask::{AskState, LineKind};
use crate::theme::Theme;

/// The questions and answers in a panel over the card, with the question
/// being typed in a box along its bottom edge.
pub fn render(frame: &mut Frame, area: Rect, ask: &AskState, t: &Theme) {
    let panel = window::place(area, Height::Tall);
    let title = if ask.card == crate::ask::NO_CARD {
        "Ask this session"
    } else {
        "Ask about this card"
    };
    let keys: &[(&str, &str)] = if ask.input.is_some() {
        &[("Enter", "ask")]
    } else {
        &[]
    };
    let inner = window::draw(frame, panel, title, keys, t);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // The question being typed takes the bottom rows, under a rule.
    let input_rows = if ask.input.is_some() {
        2.min(inner.height)
    } else {
        0
    };
    let text = Rect {
        height: inner.height - input_rows,
        ..inner
    };
    let (rows, _) = ask.lines(usize::from(text.width));
    let h = usize::from(text.height);
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
    frame.render_widget(Paragraph::new(lines), text);
    if let Some(i) = &ask.input {
        let rule = Rect::new(inner.x, text.bottom(), inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::styled(
                "─".repeat(usize::from(inner.width)),
                Style::new().fg(t.dim),
            )),
            rule,
        );
        let typed = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        input::draw_text(frame, typed, i, None, t);
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
                fallback: false,
            },
            Entry {
                question: "how?".into(),
                answer: Answer::Failed("took too long".into()),
                fallback: false,
            },
            Entry {
                question: "who?".into(),
                answer: Answer::Done("Kiwi did.".into()),
                fallback: false,
            },
        ];
        let screen = render(&a, 80, 24);
        for want in [
            "why?",
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
