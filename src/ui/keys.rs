use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::question::TELL_KEYS;
use crate::app::{App, Overlay};
use crate::model::{Card, CardKind};
use crate::quiz::{QuizCard, QuizState, Verdict};
use crate::review::ReviewCard;

/// The review card's keys.
fn review_keys(rc: &ReviewCard) -> Vec<(&'static str, String)> {
    vec![
        ("c", "comment".into()),
        ("s", format!("send {}", rc.current_comments().count())),
        ("a", "approve".into()),
        ("v", "view".into()),
        ("]c", "change".into()),
        ("H/L", "file".into()),
        ("d", "done".into()),
        ("u", "show done".into()),
        ("V", "range".into()),
        ("x", "delete".into()),
        ("e", "edit".into()),
        ("f", "files".into()),
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
        ("B", "move to back".into()),
    ]
}

/// The keys that work on any card, after the card's own: the least likely
/// last, so a short screen cuts these first.
fn every_card(app: &App) -> Vec<(&'static str, String)> {
    // On a step card b moves back a word.
    let step = matches!(
        app.queue.current().map(|c| &c.kind),
        Some(CardKind::Step(_))
    );
    let mut keys: Vec<(&'static str, String)> =
        (!step).then(|| ("b", "/btw".into())).into_iter().collect();
    keys.extend([
        ("W", "where are we".into()),
        ("T", "sum up last turn".into()),
    ]);
    keys.extend(stashed(app));
    keys.extend([
        ("M", "mode".into()),
        ("!", "shell".into()),
        ("E", "quick session".into()),
        ("N", "new session".into()),
    ]);
    keys
}

/// `S`, while any question is stashed.
fn stashed(app: &App) -> Option<(&'static str, String)> {
    (!app.stashed.is_empty()).then(|| ("S", format!("stashed {}", app.stashed.len())))
}

/// The keys that work right now, the likeliest first.
fn keys_for(app: &App) -> Vec<(&str, String)> {
    let mut keys = keys_here(app);
    if app.overlay.is_none() && app.queue.current().is_some() && app.closing_pr.is_none() {
        keys.extend(every_card(app));
    }
    keys
}

/// The keys of the window or card on screen.
fn keys_here(app: &App) -> Vec<(&str, String)> {
    match (&app.overlay, app.queue.current()) {
        (Some(Overlay::Ask(a)), _) if a.input.is_some() => {
            vec![("Enter", "ask".into()), ("Ctrl-Q", "close".into())]
        }
        (Some(Overlay::Ask(_)), _) => vec![
            ("j/k", "scroll".into()),
            ("PgUp/PgDn", "page".into()),
            ("b", "/btw".into()),
            ("W", "where are we".into()),
            ("T", "sum up last turn".into()),
            ("r", "retry".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::Comment(_)), _) => vec![
            ("Ctrl-S", "save".into()),
            ("Enter", "new line".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::NewSession(d)), _) if d.folder.is_some() => vec![
            ("Tab", "complete".into()),
            ("↑/↓", "choose".into()),
            ("Enter", "use it".into()),
            ("Esc", "back".into()),
        ],
        (Some(Overlay::Workspace(d)), _) => match &d.stage {
            crate::workspace::Stage::Choose { .. } => vec![
                ("Enter", "open".into()),
                ("j/k", "choose".into()),
                ("Ctrl-O", "other folder".into()),
                ("Ctrl-Q", "close".into()),
            ],
            crate::workspace::Stage::Folder(_) => vec![
                ("Tab", "complete".into()),
                ("↑/↓", "choose".into()),
                ("Enter", "find repos".into()),
                ("Esc", "back".into()),
            ],
            crate::workspace::Stage::Finding { .. } => vec![("Esc", "back".into())],
            crate::workspace::Stage::Repos(_) => vec![
                ("Enter", "open ticked".into()),
                ("Space", "tick".into()),
                ("a", "all".into()),
                ("Esc", "back".into()),
            ],
        },
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
            ("Ctrl-Q", "close".into()),
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
            vec![("Enter", "send".into()), ("Ctrl-Q", "close".into())]
        }
        (Some(Overlay::PrSubmit { .. }), _) => {
            vec![("Enter", "post".into()), ("Ctrl-Q", "close".into())]
        }
        (Some(Overlay::QuizAnswer(_)), _) => vec![
            ("Ctrl-S", "submit".into()),
            ("Enter Enter", "submit".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::QuizCode { .. }), _) => vec![("Ctrl-Q", "close".into())],
        (Some(Overlay::CloseAgent(_)), _) => {
            vec![("y", "close it".into()), ("n", "keep it".into())]
        }
        (Some(Overlay::Trust), _) => {
            let mut keys = vec![("y", "trust it".into()), ("n", "close it".into())];
            if app.pending_trust.len() > 1 {
                keys.push(("a", "trust all".into()));
            }
            keys
        }
        (Some(Overlay::Model(_) | Overlay::PermissionMode(_)), _) => {
            vec![("Ctrl-Q", "close".into())]
        }
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
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::Help { .. }), _) => vec![
            ("j/k", "scroll".into()),
            ("PgUp/PgDn", "page".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::Reader(r)), _) => vec![
            ("Space", if r.playing() { "pause" } else { "play" }.into()),
            ("↑/↓", "speed".into()),
            ("0-9 Enter", "set speed".into()),
            ("←/→", "back/on".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::Stash { .. }), _) => vec![
            ("Enter", "bring back".into()),
            ("j/k", "choose".into()),
            ("x", "drop".into()),
            ("Ctrl-Q", "close".into()),
        ],
        (Some(Overlay::PrFinder(_)), _) => super::pr::KEYS
            .iter()
            .map(|(k, what)| (*k, what.to_string()))
            .collect(),
        (Some(Overlay::Search(_)), _) => vec![
            ("j/k", "move".into()),
            ("Enter", "show".into()),
            ("Ctrl-Q", "close".into()),
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
            ("B", "move to back".into()),
        ],
        (None, Some(card)) if app.closing_pr == Some(card.id) => {
            vec![("y", "close it".into()), ("n", "keep it".into())]
        }
        (
            None,
            Some(Card {
                kind: CardKind::Review(rc),
                checkout,
                ..
            }),
        ) => {
            let mut keys = review_keys(rc);
            if let Some(pr) = app.pr_reviews.get(&checkout.0) {
                keys.insert(3, ("i", "description".into()));
                keys.insert(4, ("D", "close PR".into()));
                if !pr.info.mine {
                    keys[1] = ("s", "post".into());
                    keys.insert(3, ("X", "request changes".into()));
                    if let Some(r) = keys.iter_mut().find(|k| k.0 == "r") {
                        r.1 = "refresh".into();
                    }
                }
            }
            keys
        }
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
            ("B", "move to back".into()),
        ],
        (
            None,
            Some(Card {
                kind: CardKind::Choice(cc),
                ..
            }),
        ) => vec![
            if cc.multi() {
                ("Space/1-9", "tick".into())
            } else {
                ("1-9", "pick".into())
            },
            ("j/k", "move".into()),
            ("Enter", "answer".into()),
            ("o", "own words".into()),
            ("u", "back".into()),
            ("x", "answer in the window".into()),
            ("g", "go to window".into()),
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
                ("d", "done".into()),
                ("Enter", "reply".into()),
                ("x", "stash".into()),
                ("r", "read fast".into()),
                ("g", "go to window".into()),
                ("j/k", "scroll".into()),
                ("Space", "skip".into()),
                ("B", "move to back".into()),
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
            ("B", "move to back".into()),
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
            ("Tab", "last messaged".into()),
            ("Enter", "message".into()),
            ("a", "approve".into()),
            ("A", "message group".into()),
            ("z", "zen".into()),
            ("W", "where are we".into()),
            ("T", "sum up last turn".into()),
            ("r", "read reply fast".into()),
            ("b", "/btw".into()),
            ("c", "/mcp".into()),
            ("g", "go to window".into()),
            ("p", "open PR".into()),
            ("m", "model".into()),
            ("P", "permissions".into()),
            ("M", "mode".into()),
            ("Esc Esc", "stop".into()),
            ("x", "close".into()),
            ("Ctrl-U/D", "scroll back".into()),
            ("N", "new session".into()),
            ("R", "review PR".into()),
            ("O", "workspace".into()),
            ("!", "shell".into()),
            ("E", "quick session".into()),
        ]
        .into_iter()
        .chain(stashed(app))
        .chain([("t", "theme".into()), ("Q", "quit".into())])
        .collect(),
        (
            None,
            Some(Card {
                kind: CardKind::Tell(_),
                ..
            }),
        ) => TELL_KEYS.iter().map(|(k, l)| (*k, l.to_string())).collect(),
        (
            None,
            Some(Card {
                kind: CardKind::Menu(mc),
                ..
            }),
        ) => {
            let n = mc.menu.options.len().min(9);
            vec![
                (if n == 2 { "1 2" } else { "1-9" }, "pick".into()),
                ("↑↓ Enter", "move and pick".into()),
                ("g", "go to window".into()),
                ("x", "answer it there".into()),
            ]
        }
        (None, None) => vec![
            ("N", "new session".into()),
            ("O", "workspace".into()),
            ("t", "theme".into()),
            ("Q", "quit".into()),
        ],
    }
}

/// Key `k` as a small cap, so keys and words don't run together, then
/// what it does in dim. Every key hint in aq is drawn this way.
pub(crate) fn cap(k: &str, label: &str, t: &crate::theme::Theme) -> [Span<'static>; 2] {
    [
        Span::styled(
            format!(" {k} "),
            Style::new()
                .fg(t.key)
                .bg(t.line)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}  "), Style::new().fg(t.dim)),
    ]
}

/// The keys flowed into rows of `width`, a key never split from its label.
fn rows(app: &App, width: u16) -> Vec<Line<'static>> {
    let t = app.theme();
    let mut rows = vec![Line::raw(" ")];
    for (k, label) in keys_for(app) {
        let item = cap(k, &label, t);
        let w: usize = item.iter().map(Span::width).sum();
        let row = rows.last_mut().expect("a row");
        if row.width() + w > width as usize && row.width() > 1 {
            rows.push(Line::raw(" "));
        }
        rows.last_mut().expect("a row").spans.extend(item);
    }
    rows
}

/// How many rows the keys take in a screen of `width` and `height`: at
/// most a quarter of it, so a small window keeps room for the rest; the
/// keys cut are the least likely ones, listed last. It does not change
/// with the status message, so nothing moves when one shows.
pub fn height(app: &App, width: u16, height: u16) -> u16 {
    if !app.keys_shown() {
        return 0;
    }
    (rows(app, width).len() as u16).min((height / 4).max(1))
}

/// Bottom rows: the keys that work right now, and the latest status message
/// right of the last row.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let mut rows = rows(app, area.width);
    rows.truncate(area.height as usize);
    let last_w = rows.last().map_or(0, |r| r.width() as u16).min(area.width);
    let base = Style::new().bg(t.panel).fg(t.fg);
    frame.render_widget(Paragraph::new(rows).style(base), area);
    if let Some(msg) = app.status_text() {
        // Only the space right of the keys; a message too long for it is cut at the end.
        let last = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let [_, rest] =
            Layout::horizontal([Constraint::Length(last_w), Constraint::Fill(1)]).areas(last);
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
    keys.extend([("Space", "skip".into()), ("B", "move to back".into())]);
    keys
}
