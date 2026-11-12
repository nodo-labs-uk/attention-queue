use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{kind_label, window_of};
use crate::app::{App, project_name};
use crate::model::{Card, CardKind};

/// Kinds in the order the queue serves them.
const KIND_ORDER: [fn(&CardKind) -> bool; 8] = [
    |k| matches!(k, CardKind::Permission(_)),
    |k| matches!(k, CardKind::Choice(_)),
    |k| matches!(k, CardKind::Step(_)),
    |k| matches!(k, CardKind::ShellChange(_)),
    |k| matches!(k, CardKind::Review(_)),
    |k| matches!(k, CardKind::Quiz(_)),
    |k| matches!(k, CardKind::Question(_)),
    |k| matches!(k, CardKind::Tell(_)),
];

/// Top row: the current card on the left, counts of everything queued and
/// the keys and help keys on the right. With the keys row hidden, the
/// latest status message shows here, between the two.
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
    let flag = if app.on_stale() {
        Some(" answered elsewhere ")
    } else if app.preempted() {
        Some(" new card ")
    } else {
        None
    };
    if let Some(flag) = flag {
        left.push(Span::styled(
            flag,
            Style::new().fg(t.bg).bg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    let left = Line::from(left);
    let left_w = left.width() as u16;
    frame.render_widget(Paragraph::new(left).style(base), area);

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
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    if !app.stashed.is_empty() {
        right.extend([
            Span::styled("S", key),
            Span::styled(
                format!(" {} stashed  ", app.stashed.len()),
                Style::new().fg(t.dim),
            ),
        ]);
    }
    right.extend([
        Span::styled("F3", key),
        Span::styled(" keys  ", Style::new().fg(t.dim)),
        Span::styled("F1", key),
        Span::styled(" help ", Style::new().fg(t.dim)),
    ]);
    let right = Line::from(right);
    let right_w = right.width() as u16;
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);

    if let Some(msg) = app.status_text().filter(|_| !app.keys_shown()) {
        let [_, gap, _] = Layout::horizontal([
            Constraint::Length(left_w + 2),
            Constraint::Fill(1),
            Constraint::Length(right_w + 1),
        ])
        .areas(area);
        // Against the keys on the right; one too long is cut at its end.
        let align = if msg.chars().count() > gap.width as usize {
            Alignment::Left
        } else {
            Alignment::Right
        };
        frame.render_widget(
            Paragraph::new(msg)
                .style(Style::new().fg(t.hi))
                .alignment(align),
            gap,
        );
    }
}
