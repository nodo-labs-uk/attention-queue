//! The choice card: one question of Claude's option menu at a time, with
//! its options numbered.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::quiz::wrap;
use super::window_of;
use crate::app::{App, project_name};
use crate::choice::ChoiceCard;
use crate::model::Card;
use crate::visible::visible_code;

/// The widest the card's text runs.
const MEASURE: usize = 100;

/// What every row of an option starts with: room for its cursor and number.
const INDENT: &str = "     ";

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, cc: &ChoiceCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let Some(q) = cc.current() else {
        return;
    };
    let at = cc.answers.len();
    let mut title = "Claude is asking".to_string();
    if cc.questions.len() > 1 {
        title.push_str(&format!(" · question {} of {}", at + 1, cc.questions.len()));
    }
    let head = vec![
        Line::styled(title, Style::new().fg(t.acc).add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(format!("  window {}", window_of(app, card)), dim),
        ]),
    ];
    let [head_area, body, _, keys] = crate::permission::areas(area);
    frame.render_widget(Paragraph::new(head), head_area);

    // Text this wide reads in one sweep; a wider screen leaves the rest blank.
    let width = usize::from(body.width).min(MEASURE);
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
    for (earlier, answer) in cc.questions.iter().zip(&cc.answers) {
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
        lines.push(Line::styled("Pick as many as apply, then Enter.", dim));
    }
    let mut options: Vec<Vec<Line>> = Vec::new();
    for (i, o) in q.options.iter().enumerate() {
        let on = i == cc.cursor;
        let tick = match (q.multi, cc.ticked.contains(&i)) {
            (false, _) => "",
            (true, true) => "[x] ",
            (true, false) => "[ ] ",
        };
        // Every row of an option starts under its label.
        let label = wrap(&format!("{tick}{}", visible_code(&o.label)), width, INDENT);
        let mut rows: Vec<Line> = Vec::new();
        for (n, row) in label.iter().enumerate() {
            let text = Span::styled(
                row[INDENT.len()..].to_string(),
                if on { bold } else { Style::new().fg(t.fg) },
            );
            rows.push(if n == 0 {
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
                rows.push(Line::styled(row, dim));
            }
        }
        options.push(rows);
    }
    // A blank row before the options, and between them while there is room.
    let tight = lines.len() + options.iter().map(Vec::len).sum::<usize>();
    let roomy = tight + options.len() <= usize::from(body.height);
    for (n, rows) in options.into_iter().enumerate() {
        if n == 0 || roomy {
            lines.push(Line::raw(""));
        }
        lines.extend(rows);
    }
    frame.render_widget(Paragraph::new(lines), body);

    let mut spans = Vec::new();
    for (k, label) in [
        ("1-9", if q.multi { "tick" } else { "pick" }),
        ("Enter", "answer"),
        ("o", "own words"),
        ("u", "back"),
        ("x", "answer in the window"),
    ] {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}  "), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
}
