use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Overlay};
use crate::model::{Card, CardKind};
use crate::quiz::{QuizCard, QuizState, Verdict};

/// Bottom row: the keys that work right now, and the latest status message.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let keys: Vec<(&str, String)> = match (&app.overlay, app.queue.current()) {
        (Some(Overlay::Ask(a)), _) if a.input.is_some() => {
            vec![("Enter", "ask".into()), ("Esc", "cancel".into())]
        }
        (Some(Overlay::Ask(_)), _) => vec![
            ("j/k", "scroll".into()),
            ("PgUp/PgDn", "page".into()),
            ("?", "ask again".into()),
            ("W", "where are we".into()),
            ("r", "retry".into()),
            ("q", "close".into()),
        ],
        (Some(Overlay::Comment(_)), _) => vec![
            ("Ctrl-S", "save".into()),
            ("Enter", "new line".into()),
            ("Esc", "cancel".into()),
        ],
        (Some(Overlay::NewSession(d)), _) if d.folder.is_some() => vec![
            ("Tab", "complete".into()),
            ("↑/↓", "choose".into()),
            ("Enter", "use it".into()),
            ("Esc", "back".into()),
        ],
        (Some(Overlay::NewSession(d)), _) => vec![
            ("Enter", "start".into()),
            ("Ctrl-O", "other folder".into()),
            (
                "Tab",
                if d.worktree {
                    "same folder".into()
                } else {
                    "own worktree".into()
                },
            ),
            ("Esc", "cancel".into()),
        ],
        (
            Some(
                Overlay::StepReason(_)
                | Overlay::Deny(_)
                | Overlay::Prompt(_)
                | Overlay::OwnAnswer(_),
            ),
            _,
        ) => {
            vec![("Enter", "send".into()), ("Esc", "cancel".into())]
        }
        (Some(Overlay::QuizAnswer(_)), _) => vec![
            ("Ctrl-S", "submit".into()),
            ("Enter Enter", "submit".into()),
            ("Esc", "cancel".into()),
        ],
        (Some(Overlay::QuizCode { .. }), _) => vec![("Esc", "close".into())],
        (Some(Overlay::CloseAgent(_)), _) => {
            vec![("y", "close it".into()), ("n", "keep it".into())]
        }
        (Some(Overlay::Model(_)), _) => vec![("Esc", "cancel".into())],
        (Some(Overlay::Tour { page }), _) => vec![
            (
                "→",
                match crate::tour::PAGES.get(*page).and_then(|p| p.practice) {
                    Some(_) => "start".into(),
                    None if page + 1 == crate::tour::PAGES.len() => "finish".into(),
                    None => "next".into(),
                },
            ),
            ("←", "back".into()),
            ("Esc", "skip".into()),
        ],
        (Some(Overlay::Help { .. }), _) => vec![
            ("j/k", "scroll".into()),
            ("PgUp/PgDn", "page".into()),
            ("q", "close".into()),
        ],
        (Some(Overlay::Search(_)), _) => vec![
            ("j/k", "move".into()),
            ("Enter", "show".into()),
            ("Esc", "close".into()),
        ],
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
            ("H/L", "change".into()),
            ("za", "fold".into()),
            ("zR", "unfold all".into()),
            (
                "gd",
                sc.word()
                    .map_or_else(|| "find".into(), |w| format!("find {w}")),
            ),
            ("w/b", "word".into()),
            ("m", if sc.view.raw { "file" } else { "raw" }.into()),
            ("Space", "skip".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Review(rc),
                ..
            }),
        ) => vec![
            ("c", "comment".into()),
            ("s", format!("send {}", rc.current_comments().count())),
            ("a", "approve".into()),
            ("v", "view".into()),
            ("]c", "change".into()),
            ("H/L", "file".into()),
            ("d", "done".into()),
            ("V", "range".into()),
            ("x", "delete".into()),
            (
                "r",
                if rc.show_full_branch {
                    "this round"
                } else {
                    "whole branch"
                }
                .into(),
            ),
            ("j/k", "line".into()),
            ("Space", "skip".into()),
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
            ("Space", "skip".into()),
            ("?", "ask".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Choice(_),
                ..
            }),
        ) => vec![
            ("1-9", "pick".into()),
            ("j/k", "move".into()),
            ("Enter", "answer".into()),
            ("o", "own words".into()),
            ("u", "back".into()),
            ("x", "answer in the window".into()),
            ("g", "go to window".into()),
            ("Space", "skip".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Question(qc),
                ..
            }),
        ) => qc
            .approvable()
            .then_some(("a", "approve".into()))
            .into_iter()
            .chain([
                ("Enter", "reply".into()),
                ("x", "put away".into()),
                ("g", "go to window".into()),
                ("j/k", "scroll".into()),
                ("Space", "skip".into()),
                ("?", "ask".into()),
            ])
            .collect(),
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
            ("H/L", "file".into()),
            ("j/k", "line".into()),
            ("Space", "skip".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Quiz(qc),
                ..
            }),
        ) => quiz_keys(qc),
        (None, None) if app.picked_agent().is_some() => vec![
            ("h/l", "pick".into()),
            ("Enter", "message".into()),
            ("g", "go to window".into()),
            ("p", "open PR".into()),
            ("x", "close".into()),
            ("M", "mode".into()),
            ("W", "where are we".into()),
            ("t", "theme".into()),
            ("Q", "quit".into()),
        ],
        (None, None) => vec![("t", "theme".into()), ("Q", "quit".into())],
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
        QuizState::Asking(i) => match qc.answers.get(*i) {
            Some(a) if matches!(a.verdict, Verdict::JudgeFailed(_)) => vec![
                ("r", "judge again".into()),
                ("Enter", "skip question".into()),
            ],
            Some(_) => vec![("m", "whole file".into())],
            None if !qc.tries(*i).is_empty() => vec![
                ("Enter", "try again".into()),
                ("e", "show the answer".into()),
                ("m", "whole file".into()),
            ],
            None => vec![("Enter", "answer".into()), ("m", "whole file".into())],
        },
    };
    keys.extend([("Space", "skip".into()), ("M", "mode".into())]);
    keys
}
