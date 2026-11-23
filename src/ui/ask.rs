use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::input;
use super::window::{self, Height};
use crate::ask::{AskState, LABEL_WIDTH, LineKind, SUMMARISE};
use crate::theme::Theme;

/// The questions and answers in a panel over the card, with the question
/// being typed in a box along its bottom edge.
pub fn render(frame: &mut Frame, area: Rect, ask: &AskState, t: &Theme) {
    let tldr = !ask.entries.is_empty() && ask.entries.iter().all(|e| e.question == SUMMARISE);
    let title = if tldr {
        "TL;DR"
    } else if ask.card == crate::ask::NO_CARD {
        "Ask this session"
    } else {
        "Ask about this card"
    };
    let keys: &[(&str, &str)] = if ask.input.is_some() {
        &[("Enter", "ask")]
    } else {
        &[("?", "ask more")]
    };
    // A short window while the answers fit in one, as `T`'s do.
    let short = window::place(area, Height::Short);
    let text_rows = usize::from(short.height.saturating_sub(window::INSET.1));
    let fits = ask.input.is_none()
        && ask
            .lines(usize::from(short.width.saturating_sub(window::INSET.0)))
            .0
            .len()
            <= text_rows;
    let panel = if fits {
        short
    } else {
        window::place(area, Height::Tall)
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
        .map(|(kind, text)| match kind {
            LineKind::Question => {
                Line::styled(text, Style::new().fg(t.acc).add_modifier(Modifier::BOLD))
            }
            LineKind::Answer => with_code(&text, Style::new().fg(t.fg), t),
            LineKind::Pending => Line::styled(text, Style::new().fg(t.dim)),
            LineKind::Error => Line::styled(text, Style::new().fg(t.hi)),
            LineKind::Labelled => {
                let (label, rest) = text.split_at(
                    text.char_indices()
                        .nth(LABEL_WIDTH)
                        .map_or(text.len(), |(i, _)| i),
                );
                let mut line = with_code(rest, Style::new().fg(t.fg), t);
                line.spans.insert(
                    0,
                    Span::styled(
                        label.to_string(),
                        Style::new().fg(t.dim).add_modifier(Modifier::BOLD),
                    ),
                );
                line
            }
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

/// `text` in `style`, with each `code` span between backticks in the key
/// colour and its backticks dropped. A row with an odd backtick is shown as is.
fn with_code(text: &str, style: Style, t: &Theme) -> Line<'static> {
    let parts: Vec<&str> = text.split('`').collect();
    if parts.len().is_multiple_of(2) {
        return Line::styled(text.to_string(), style);
    }
    Line::from(
        parts
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_empty())
            .map(|(i, p)| {
                let s = if i % 2 == 1 {
                    Style::new().fg(t.key)
                } else {
                    style
                };
                Span::styled(p.to_string(), s)
            })
            .collect::<Vec<_>>(),
    )
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

    #[test]
    fn a_tldr_shows_its_labelled_lines_in_a_short_window() {
        let mut a = app();
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 0));
        a.update(key('T'));
        let Some(Overlay::Ask(ask)) = &mut a.overlay else {
            panic!()
        };
        ask.entries[0].answer = Answer::Done(
            "Asked: tidy the T window.\nDid: changed `src/ui/ask.rs`.\nSaid: it is done.".into(),
        );
        let screen = render(&a, 120, 30);
        for want in [
            "TL;DR",
            "Asked  tidy the T window.",
            "Did    changed src/ui/ask.rs.",
            "Said   it is done.",
        ] {
            assert!(screen.contains(want), "{want} in\n{screen}");
        }
        let rows = screen.lines().filter(|l| l.contains('│')).count();
        assert!(rows <= 12, "a short window, not a tall one:\n{screen}");
    }

    #[test]
    fn a_tldr_in_another_shape_is_shown_as_it_came() {
        let mut a = app();
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 0));
        a.update(key('T'));
        let Some(Overlay::Ask(ask)) = &mut a.overlay else {
            panic!()
        };
        ask.entries[0].answer = Answer::Done("You asked for a fix and I made it.".into());
        let screen = render(&a, 120, 30);
        assert!(
            screen.contains("You asked for a fix and I made it."),
            "{screen}"
        );
    }

    fn key(c: char) -> Msg {
        Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c)),
            std::time::Instant::now(),
        )
    }
}
