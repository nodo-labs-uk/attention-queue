use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{kind_label, window_of};
use crate::app::{App, project_name};
use crate::model::{Card, CardKind};

/// Kinds in the order the queue serves them.
const KIND_ORDER: [fn(&CardKind) -> bool; 5] = [
    |k| matches!(k, CardKind::Permission(_)),
    |k| matches!(k, CardKind::Step(_)),
    |k| matches!(k, CardKind::ShellChange(_)),
    |k| matches!(k, CardKind::Review(_)),
    |k| matches!(k, CardKind::Quiz(_)),
];

/// Top row: the current card on the left, counts of everything queued on the right.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let base = Style::new().bg(t.panel).fg(t.fg);
    let mut left = vec![Span::styled(
        " aq ",
        Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD),
    )];
    match app.queue.current() {
        Some(card) => {
            left.push(Span::styled(
                format!(" {} ", kind_label(&card.kind)),
                Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
            ));
            left.push(Span::raw(project_name(&card.checkout)));
            left.push(Span::styled(
                format!(" · {}", window_of(app, card)),
                Style::new().fg(t.dim),
            ));
            if let CardKind::Quiz(qc) = &card.kind
                && let Some((which, score)) = super::quiz::progress(qc)
            {
                left.push(Span::styled(format!("  {which}"), Style::new().fg(t.fg)));
                left.push(Span::styled(
                    format!("  {score}"),
                    Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
                ));
            }
        }
        None => left.push(Span::styled(" nothing waiting", Style::new().fg(t.dim))),
    }
    if app.preempted() {
        left.push(Span::styled(
            " new card ",
            Style::new().fg(t.bg).bg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(left)).style(base), area);

    let cards: Vec<&Card> = app.queue.iter().collect();
    let mut right = Vec::new();
    for is_kind in KIND_ORDER {
        let mut of_kind = cards.iter().filter(|c| is_kind(&c.kind));
        let Some(first) = of_kind.next() else {
            continue;
        };
        let n = 1 + of_kind.count();
        right.push(Span::styled(
            n.to_string(),
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ));
        right.push(Span::styled(
            format!(" {}  ", kind_label(&first.kind)),
            Style::new().fg(t.dim),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        area,
    );
}
