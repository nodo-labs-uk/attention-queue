//! The choice card: one question of Claude's option menu at a time, with
//! its options numbered.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::quiz::wrap;
use super::window_of;
use crate::app::{App, project_name};
use crate::choice::ChoiceCard;
use crate::model::Card;
use crate::visible::visible_code;

/// The widest the card's text runs.
const MEASURE: usize = 76;

/// The narrowest the card is drawn, so short menus still read as a card.
const NARROWEST: usize = 44;

/// What every row of an option starts with: room for its cursor and number.
const INDENT: &str = "     ";

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, cc: &ChoiceCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    if cc.current().is_none() {
        return;
    }
    let at = cc.answers.len();
    let mut title = "Claude is asking".to_string();
    if cc.questions.len() > 1 {
        title.push_str(&format!(" · question {} of {}", at + 1, cc.questions.len()));
    }
    let head = Line::from(vec![
        Span::styled(project_name(&card.checkout), bold),
        Span::styled(format!("  window {}", window_of(app, card)), dim),
    ]);
    let mut keys = Vec::new();
    for (k, label) in key_hints(cc) {
        keys.extend(super::keys::cap(k, label, t));
    }
    let keys = Line::from(keys);

    // The box fits its widest text, and keeps one size for every question
    // of the menu, so the keys stay put from one question to the next.
    let band_head = match &cc.flag {
        crate::flag::FlagState::Flagged(f) => {
            format!("▌▌ {} · {}", f.label, f.target).chars().count()
        }
        _ => 0,
    };
    let width = natural_width(cc)
        .max(head.width())
        .max(keys.width().saturating_sub(2))
        .max(band_head)
        .clamp(NARROWEST, MEASURE)
        .min(usize::from(area.width.saturating_sub(4)))
        .max(1);
    // The keys wrap onto a second row on a narrow screen.
    let keys_rows = (keys.width().saturating_sub(2).div_ceil(width)).max(1) as u16;
    let band = super::permission::flag_band_height(&cc.flag, width);
    // Head, blank, band, the question, blank, keys.
    let frame_rows = 2 + band + 1 + keys_rows;
    let room = area.height.saturating_sub(2 + frame_rows);
    let tallest = |roomy| {
        (0..cc.questions.len())
            .map(|i| question_lines(app, cc, i, width, roomy).len())
            .max()
            .unwrap_or(0) as u16
    };
    let roomy = tallest(true) <= room;
    let height = frame_rows + tallest(roomy);
    let inner = super::confirm::card_box(frame, area, &title, (width as u16, height), t);

    let [head_area, _, body, _, keys_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(keys_rows),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(head), head_area);
    let body = super::permission::flag_band(frame, body, app, &cc.flag);
    frame.render_widget(
        Paragraph::new(question_lines(app, cc, at, width, roomy)),
        body,
    );
    frame.render_widget(Paragraph::new(keys).wrap(Wrap { trim: false }), keys_area);
}

/// The keys the card takes, and what each does.
fn key_hints(cc: &ChoiceCard) -> [(&'static str, &'static str); 6] {
    let multi = cc.current().is_some_and(|q| q.multi);
    [
        if multi {
            ("Space/1-9", "tick")
        } else {
            ("1-9", "pick")
        },
        ("Enter", "answer"),
        ("o", "own words"),
        ("T", "TL;DR"),
        ("u", "back"),
        ("x", "answer in the window"),
    ]
}

/// The widest any question of the menu runs unwrapped, in columns.
fn natural_width(cc: &ChoiceCard) -> usize {
    let cols = |s: &str| {
        visible_code(s)
            .chars()
            .map(crate::visible::cols)
            .sum::<usize>()
    };
    cc.questions
        .iter()
        .flat_map(|q| {
            let tick = if q.multi { 4 } else { 0 };
            std::iter::once(cols(&q.question)).chain(q.options.iter().flat_map(move |o| {
                [
                    INDENT.len() + tick + cols(&o.label),
                    INDENT.len() + cols(&o.description),
                ]
            }))
        })
        .max()
        .unwrap_or(0)
}

/// Question `at` of the menu as rows of `width`: the answers so far, the
/// question and its options, a blank row between options when `roomy`.
fn question_lines(
    app: &App,
    cc: &ChoiceCard,
    at: usize,
    width: usize,
    roomy: bool,
) -> Vec<Line<'static>> {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let q = &cc.questions[at];
    let mut lines = Vec::new();
    // What was answered so far, so a second question is read in its light:
    // the menu's short name for the question, then the answer.
    let name = |q: &crate::choice::ChoiceQuestion| {
        let short = if q.header.is_empty() {
            &q.question
        } else {
            &q.header
        };
        visible_code(short)
    };
    let names = cc.questions[..at]
        .iter()
        .map(|q| name(q).chars().count())
        .max()
        .unwrap_or(0)
        .min(width / 2);
    for (i, earlier) in cc.questions[..at].iter().enumerate() {
        let answer = cc.answers.get(i).map(String::as_str).unwrap_or_default();
        let short: String = name(earlier).chars().take(names).collect();
        lines.push(Line::from(vec![
            Span::styled("✓ ", Style::new().fg(t.ok)),
            Span::styled(format!("{short:names$}  "), dim),
            Span::styled(visible_code(answer), Style::new().fg(t.fg)),
        ]));
    }
    if at > 0 {
        lines.push(Line::raw(""));
    }
    for row in wrap(&visible_code(&q.question), width, "") {
        lines.push(Line::styled(row, bold));
    }
    if q.multi {
        for row in wrap(
            "Tick as many as apply with Space or a number, then Enter.",
            width,
            "",
        ) {
            lines.push(Line::styled(row, dim));
        }
    }
    let on_question = at == cc.answers.len();
    for (i, o) in q.options.iter().enumerate() {
        let on = on_question && i == cc.cursor;
        let tick = match (q.multi, on_question && cc.ticked.contains(&i)) {
            (false, _) => "",
            (true, true) => "[x] ",
            (true, false) => "[ ] ",
        };
        if i == 0 || roomy {
            lines.push(Line::raw(""));
        }
        // Every row of an option starts under its label.
        let label = wrap(&format!("{tick}{}", visible_code(&o.label)), width, INDENT);
        for (n, row) in label.iter().enumerate() {
            let text = Span::styled(
                row[INDENT.len()..].to_string(),
                if on { bold } else { Style::new().fg(t.fg) },
            );
            lines.push(if n == 0 {
                Line::from(vec![
                    Span::styled(if on { "▸ " } else { "  " }, Style::new().fg(t.acc)),
                    Span::styled(format!("{:<3}", i + 1), key),
                    text,
                ])
            } else {
                Line::from(vec![Span::raw(INDENT), text])
            });
        }
        if !o.description.is_empty() {
            for row in wrap(&visible_code(&o.description), width, INDENT) {
                lines.push(Line::styled(row, dim));
            }
        }
    }
    lines
}
