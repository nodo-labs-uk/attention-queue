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
pub const TELL_KEYS: [(&str, &str); 6] = [
    ("y", "send"),
    ("e", "edit first"),
    ("n", "drop"),
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
            ("g", "go to window"),
            ("B", "move to back"),
            ("?", "ask"),
        ])
        .collect();
    render_text(frame, area, app, card, qc, "Claude is asking", &keys);
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
