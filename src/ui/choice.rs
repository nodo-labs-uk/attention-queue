//! The choice card: one question of Claude's option menu at a time, with
//! its options numbered.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::window_of;
use crate::app::{App, project_name};
use crate::choice::ChoiceCard;
use crate::model::Card;
use crate::visible::visible_code;

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

    let mut lines = Vec::new();
    // What was answered so far, so a second question is read in its light.
    for (earlier, answer) in cc.questions.iter().zip(&cc.answers) {
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", visible_code(&earlier.question)), dim),
            Span::styled(visible_code(answer), Style::new().fg(t.ok)),
        ]));
    }
    if at > 0 {
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(visible_code(&q.question), bold));
    if q.multi {
        lines.push(Line::styled("Pick as many as apply, then Enter.", dim));
    }
    lines.push(Line::raw(""));
    for (i, o) in q.options.iter().enumerate() {
        let on = i == cc.cursor;
        let tick = match (q.multi, cc.ticked.contains(&i)) {
            (false, _) => "",
            (true, true) => "[x] ",
            (true, false) => "[ ] ",
        };
        lines.push(Line::from(vec![
            Span::styled(if on { "▸ " } else { "  " }, Style::new().fg(t.acc)),
            Span::styled(format!("{}", i + 1), key),
            Span::styled(
                format!("  {tick}{}", visible_code(&o.label)),
                if on { bold } else { Style::new().fg(t.fg) },
            ),
        ]));
        if !o.description.is_empty() {
            lines.push(Line::styled(
                format!("      {}", visible_code(&o.description)),
                dim,
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

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
