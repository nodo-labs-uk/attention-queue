use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Overlay};
use crate::model::{Card, CardKind};
use crate::quiz::{QuestionKind, QuizCard, QuizState, Verdict};

/// Bottom row: the keys that work right now, and the latest status message.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let keys: Vec<(&str, String)> = match (&app.overlay, app.queue.current()) {
        (Some(Overlay::Comment(_)), _) => vec![
            ("Ctrl-S", "save".into()),
            ("Enter", "new line".into()),
            ("Esc", "cancel".into()),
        ],
        (Some(Overlay::StepReason(_) | Overlay::Deny(_)), _) => {
            vec![("Enter", "send".into()), ("Esc", "cancel".into())]
        }
        (Some(Overlay::QuizAnswer(_)), _) => vec![
            ("Ctrl-S", "submit".into()),
            ("Enter Enter", "submit".into()),
            ("Esc", "cancel".into()),
        ],
        (Some(Overlay::QuizCode { .. }), _) => vec![("Esc", "close".into())],
        (Some(Overlay::Search(_)), _) => vec![
            ("j/k", "move".into()),
            ("Enter", "show".into()),
            ("Esc", "close".into()),
        ],
        (Some(_), _) => vec![("q", "close".into())],
        (
            None,
            Some(Card {
                kind: CardKind::Step(sc),
                ..
            }),
        ) => vec![
            ("y", "allow".into()),
            ("n", "reject".into()),
            ("e", "change".into()),
            ("tab", "next".into()),
            ("za", "fold".into()),
            ("zR", "unfold all".into()),
            (
                "gd",
                sc.word()
                    .map_or_else(|| "find".into(), |w| format!("find {w}")),
            ),
            ("w/b", "word".into()),
            ("m", if sc.view.raw { "file" } else { "raw" }.into()),
            ("s", "skip".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Review(rc),
                ..
            }),
        ) => vec![
            ("c", "comment".into()),
            ("S", format!("send {}", rc.current_comments().count())),
            ("A", "approve".into()),
            ("v", "view".into()),
            ("]c", "change".into()),
            ("tab", "file".into()),
            ("V", "range".into()),
            ("x", "delete".into()),
            (
                "R",
                if rc.show_full_branch {
                    "this round"
                } else {
                    "whole branch"
                }
                .into(),
            ),
            ("j/k", "line".into()),
            ("s", "skip".into()),
            ("?", "ask".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Permission(_),
                ..
            }),
        ) => vec![
            ("y", "allow".into()),
            ("n", "deny".into()),
            ("g", "go to window".into()),
            ("s", "skip".into()),
            ("?", "ask".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::ShellChange(_),
                ..
            }),
        ) => vec![
            ("y", "approve".into()),
            ("u", "undo".into()),
            ("v", "view".into()),
            ("]c", "change".into()),
            ("tab", "file".into()),
            ("j/k", "line".into()),
            ("s", "skip".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Quiz(qc),
                ..
            }),
        ) => quiz_keys(qc),
        (None, None) => vec![("T", "theme".into()), ("Q", "quit".into())],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(Span::styled(
            k,
            Style::new().fg(t.key).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}   "), Style::new().fg(t.dim)));
    }
    let base = Style::new().bg(t.panel).fg(t.fg);
    let keys = Line::from(spans);
    let keys_w = (keys.width() as u16).min(area.width);
    frame.render_widget(Paragraph::new(keys).style(base), area);
    if let Some(msg) = app.status_text() {
        // Only the space right of the keys; a message too long for it is cut at the end.
        let [_, rest] =
            Layout::horizontal([Constraint::Length(keys_w), Constraint::Fill(1)]).areas(area);
        let text = format!("{msg} ");
        let align = if text.chars().count() > rest.width as usize {
            Alignment::Left
        } else {
            Alignment::Right
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::new().fg(t.hi))
                .alignment(align),
            rest,
        );
    }
}

fn quiz_keys(qc: &QuizCard) -> Vec<(&'static str, String)> {
    let mut keys: Vec<(&str, String)> = match &qc.state {
        QuizState::Generating => Vec::new(),
        QuizState::Failed(_) => vec![("r", "try again".into()), ("x", "dismiss".into())],
        QuizState::Done => vec![("Enter", "send".into())],
        QuizState::Asking(_) if qc.code_gap().is_some() => vec![
            ("y", "fine as is".into()),
            ("c", "fix it".into()),
            ("m", "show the code".into()),
        ],
        QuizState::Asking(i) => match (qc.answers.get(*i), qc.questions[*i].kind) {
            (Some(a), _) if matches!(a.verdict, Verdict::JudgeFailed(_)) => vec![
                ("r", "judge again".into()),
                ("Enter", "skip question".into()),
            ],
            (Some(_), _) => vec![("m", "show code".into())],
            (None, QuestionKind::Choice) => {
                vec![("1-4", "pick".into()), ("m", "show code".into())]
            }
            (None, QuestionKind::Free) => {
                vec![("i", "type an answer".into()), ("m", "show code".into())]
            }
        },
    };
    keys.extend([("s", "skip".into()), ("M", "mode".into())]);
    keys
}
