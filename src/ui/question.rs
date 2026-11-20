//! The question card: the session's last message, and the keys that
//! answer it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::window_of;
use crate::app::{App, project_name};
use crate::model::Card;
use crate::question::{Kind, QuestionCard};
use crate::tell::{TellCard, tell_name};

/// The keys of a tell card, on the card and along the bottom.
pub const TELL_KEYS: [(&str, &str); 7] = [
    ("y", "send"),
    ("e", "edit first"),
    ("n", "drop"),
    ("r", "read fast"),
    ("B", "move to back"),
    ("?", "ask"),
    ("j/k", "scroll"),
];

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, qc: &QuestionCard) {
    let approve = qc.approvable().then_some(("a", "approve"));
    let keys: Vec<(&str, &str)> = approve
        .into_iter()
        .chain([
            ("Enter", "reply"),
            ("x", "stash"),
            ("r", "read fast"),
            ("g", "go to window"),
            ("B", "move to back"),
            ("?", "ask"),
        ])
        .collect();
    render_text(frame, area, app, card, qc, "Claude is asking", &keys);
}

/// `S`: the stashed questions, one row each: where it came from and its
/// first line.
pub fn render_stash(frame: &mut Frame, area: Rect, app: &App, cursor: usize) {
    use super::workspace::{list_box, row, shown};
    let t = app.theme();
    let picked = Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD);
    let rows = app
        .stashed
        .iter()
        .enumerate()
        .map(|(i, card)| {
            let first = match &card.kind {
                crate::model::CardKind::Question(q) => {
                    q.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
                }
                _ => "",
            };
            let text = format!(
                " {} · {}  {} ",
                shown(&project_name(&card.checkout)),
                shown(&window_of(app, card)),
                shown(first.trim())
            );
            row(text, i == cursor, picked, t)
        })
        .collect();
    list_box(
        frame,
        area,
        "Stashed questions",
        rows,
        &[("Enter", "bring back"), ("x", "drop")],
        t,
    );
}

/// `aq tell`: one session's message to another, drawn as a question is.
pub fn render_tell(frame: &mut Frame, area: Rect, app: &App, card: &Card, tc: &TellCard) {
    let to = app
        .agent(&card.agent)
        .map_or_else(|| project_name(&card.checkout), tell_name);
    let title = format!("{} asks {to} to do this", tc.from_name);
    render_text(frame, area, app, card, &tc.message, &title, &TELL_KEYS);
}

fn render_text(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: &Card,
    qc: &QuestionCard,
    title: &str,
    keys_shown: &[(&str, &str)],
) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let head = vec![
        Line::styled(
            title.to_string(),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(format!("  window {}", window_of(app, card)), dim),
        ]),
    ];
    // The same layout as a permission card: both are a thing to read and
    // a yes or no.
    let [head_area, body, more, keys] = crate::permission::areas(area);
    frame.render_widget(Paragraph::new(head), head_area);
    let body = super::permission::flag_band(frame, body, app, &qc.flag);
    let rows = qc.rows(body.width as usize);
    let top = qc
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let shown: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(body.height as usize)
        .map(|row| {
            Line::from(
                row.iter()
                    .map(|(kind, text)| {
                        let style = match kind {
                            Kind::Plain => Style::new().fg(t.fg),
                            Kind::Bold => bold,
                            Kind::Code => Style::new().fg(t.key),
                            Kind::Heading => Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
                            Kind::Mark => dim,
                        };
                        Span::styled(text.clone(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    let hidden = rows.len().saturating_sub(top + body.height as usize);
    if hidden > 0 {
        let s = if hidden == 1 { "" } else { "s" };
        frame.render_widget(
            Paragraph::new(format!("▾ {hidden} more line{s} (j to scroll)"))
                .style(Style::new().fg(t.hi).add_modifier(Modifier::BOLD)),
            more,
        );
    }
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for &(k, label) in keys_shown {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}  "), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::choice::{Choice, ChoiceCard, ChoiceQuestion};
    use crate::flag::{Flag, FlagKind, FlagState};
    use crate::model::{CardKind, QuestionCard};
    use crate::queue::tests::card;
    use crate::ui::tests::{app, render};

    fn question(flag: FlagState) -> String {
        let mut a = app();
        a.queue.push(card(
            1,
            "/w/socratui",
            CardKind::Question(QuestionCard {
                text: "PR #5 has changes requested.\n\nShall I fix it, then merge?".into(),
                scroll: 0,
                flag,
            }),
            10,
        ));
        render(&a, 100, 24)
    }

    #[test]
    fn a_flagged_question_shows_the_band_above_its_text() {
        let flag = Flag {
            kind: FlagKind::SomeoneElse,
            label: "SOMEONE ELSE".into(),
            target: "nodo-labs-uk/socratui PR #5 by Timonsc (outside your org)".into(),
            who: "Timonsc opened PR #5 from their fork".into(),
            happens: "fixes and merges PR #5 into main".into(),
            as_you: String::new(),
            conflict: "You requested changes on 2026-11-17; no commits since".into(),
            pending: false,
        };
        let text = question(FlagState::Flagged(flag));
        assert!(text.contains("▌▌ QUESTION"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn a_question_being_checked_keeps_one_row_for_it() {
        let text = question(FlagState::Checking);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn a_menu_being_checked_keeps_one_row_for_it() {
        let mut a = app();
        let options = ["Merge as is", "Fix, then merge", "Wait"]
            .map(|l| Choice {
                label: l.into(),
                description: String::new(),
            })
            .to_vec();
        a.queue.push(card(
            1,
            "/w/socratui",
            CardKind::Choice(ChoiceCard {
                req_id: 7,
                input: serde_json::Value::Null,
                questions: vec![ChoiceQuestion {
                    question: "How should I merge PR #5?".into(),
                    header: "Merge".into(),
                    options,
                    multi: false,
                }],
                answers: Vec::new(),
                cursor: 0,
                ticked: BTreeSet::new(),
                flag: FlagState::Checking,
            }),
            10,
        ));
        insta::assert_snapshot!(render(&a, 100, 24));
        // On a phone-width screen the box fills the width and every key
        // still shows, wrapped onto a second row.
        let narrow = render(&a, 40, 24);
        assert!(narrow.contains("╭ Claude is asking"), "{narrow}");
        assert!(narrow.contains("Fix, then merge"), "{narrow}");
        assert!(narrow.contains("answer in the"), "{narrow}");
    }

    #[test]
    fn claude_codes_own_menu_is_a_box_too() {
        let mut a = app();
        a.queue.push(card(
            1,
            "/w/socratui",
            CardKind::Menu(crate::menu::MenuCard {
                menu: crate::agents::ScreenMenu {
                    question: vec!["Do you want to proceed?".into()],
                    options: vec!["1. Yes".into(), "2. No, keep planning".into()],
                    cursor: 0,
                },
                picked: false,
            }),
            10,
        ));
        insta::assert_snapshot!(render(&a, 100, 16));
    }
}
