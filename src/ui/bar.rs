use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{kind_label, window_of};
use crate::app::{App, mode_label, project_name};
use crate::flag::{FlagKind, FlagState};
use crate::model::Mode;
use crate::model::{Card, CardKind};
use crate::permission::Held;

/// Kinds in the order the queue serves them.
const KIND_ORDER: [fn(&CardKind) -> bool; 9] = [
    |k| matches!(k, CardKind::Menu(_)),
    |k| matches!(k, CardKind::Permission(_)),
    |k| matches!(k, CardKind::Choice(_)),
    |k| matches!(k, CardKind::Step(_)),
    |k| matches!(k, CardKind::ShellChange(_)),
    |k| matches!(k, CardKind::Review(_)),
    |k| matches!(k, CardKind::Quiz(_)),
    |k| matches!(k, CardKind::Question(_)),
    |k| matches!(k, CardKind::Tell(_)),
];

/// The flag on the card, if it holds a flagged action.
fn flag_of(card: &Card) -> Option<&crate::flag::Flag> {
    match &card.kind {
        CardKind::Permission(pc) => match &pc.held {
            Some(Held::Flagged(f)) => Some(f),
            _ => None,
        },
        CardKind::Question(q) => match &q.flag {
            FlagState::Flagged(f) => Some(f),
            _ => None,
        },
        CardKind::Choice(c) => match &c.flag {
            FlagState::Flagged(f) => Some(f),
            _ => None,
        },
        _ => None,
    }
}

/// The card named once: its kind as a badge (in the flag's colour when it
/// holds a flagged action), its project, then where it is and how far on.
fn card_spans(app: &App, card: &Card) -> Vec<Span<'static>> {
    let t = app.theme();
    let kind = kind_label(&card.kind).to_uppercase();
    let badge = match flag_of(card) {
        Some(f) => {
            let mark = match f.kind {
                FlagKind::SomeoneElse => "▌▌",
                FlagKind::HighStakes => "▲",
            };
            Span::styled(
                format!(" {mark} {kind} "),
                Style::new()
                    .fg(t.bg)
                    .bg(t.flag_colour(f.kind))
                    .add_modifier(Modifier::BOLD),
            )
        }
        None => Span::styled(
            format!(" {kind} "),
            Style::new()
                .fg(t.acc)
                .bg(t.sel)
                .add_modifier(Modifier::BOLD),
        ),
    };
    let mut spans = vec![
        Span::raw(" "),
        badge,
        Span::raw(" "),
        Span::styled(
            project_name(&card.checkout),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ];
    let mut place = vec![format!("window {}", window_of(app, card))];
    // A step or quiz card already says its mode.
    let mode = app.config.mode_for(&card.checkout);
    let said = matches!(card.kind, CardKind::Step(_) | CardKind::Quiz(_));
    if mode != Mode::Review && !said {
        place.push(mode_label(mode).to_string());
    }
    match &card.kind {
        CardKind::Review(rc) => place.push(format!("round {}", rc.round)),
        CardKind::Step(sc) => place.push(format!("step {}", sc.step_no)),
        _ => {}
    }
    spans.push(Span::styled(
        format!(" · {}", place.join(" · ")),
        Style::new().fg(t.dim),
    ));
    if let CardKind::Quiz(qc) = &card.kind
        && let Some((which, score)) = super::quiz::progress(qc)
    {
        spans.push(Span::styled(format!("  {which}"), Style::new().fg(t.fg)));
        spans.push(Span::styled(
            format!("  {score}"),
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

/// What is queued, as one pill per kind: the count first, then the kind.
fn count_pills(app: &App) -> Vec<Span<'static>> {
    let t = app.theme();
    // Plain text, not dim: dim on the pill's fill is too faint in light themes.
    let pill = Style::new().bg(t.line).fg(t.fg);
    let cards: Vec<&Card> = app.queue.iter().collect();
    let mut spans = Vec::new();
    for is_kind in KIND_ORDER {
        let mut of_kind = cards.iter().filter(|c| is_kind(&c.kind));
        let Some(first) = of_kind.next() else {
            continue;
        };
        let n = 1 + of_kind.count();
        spans.extend([
            Span::styled(" ", pill),
            Span::styled(n.to_string(), pill.fg(t.hi).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {} ", kind_label(&first.kind)), pill),
            Span::raw(" "),
        ]);
    }
    spans
}

/// With nothing waiting: how many sessions there are, and how many wait on you.
fn session_count(app: &App) -> Vec<Span<'static>> {
    let t = app.theme();
    let n = app.agents.len();
    if n == 0 {
        return Vec::new();
    }
    let waiting = app.agents.iter().filter(|a| a.waiting).count();
    let mut spans = vec![Span::styled(
        format!("{n} session{}", if n == 1 { "" } else { "s" }),
        Style::new().fg(t.dim),
    )];
    if waiting > 0 {
        spans.push(Span::styled(" · ", Style::new().fg(t.dim)));
        spans.push(Span::styled(
            format!("{waiting} waiting"),
            Style::new().fg(t.hi),
        ));
    }
    spans.push(Span::raw("  "));
    spans
}

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
        Some(card) => left.extend(card_spans(app, card)),
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
        left.push(Span::raw(" "));
        left.push(Span::styled(
            flag,
            Style::new().fg(t.bg).bg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    let left = Line::from(left);
    let left_w = left.width() as u16;
    frame.render_widget(Paragraph::new(left).style(base), area);

    let mut right = if app.queue.current().is_some() {
        count_pills(app)
    } else {
        session_count(app)
    };
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
    let help = [
        Span::styled("F3", key),
        Span::styled(" keys  ", Style::new().fg(t.dim)),
        Span::styled("F1", key),
        Span::styled(" help ", Style::new().fg(t.dim)),
    ];
    let help_w: usize = help.iter().map(Span::width).sum();
    let right_w: usize = right.iter().map(Span::width).sum();
    // On a narrow screen the help keys go first, so the card and the
    // counts keep their room; F1 still opens the help.
    if usize::from(left_w) + right_w + help_w < usize::from(area.width) {
        right.extend(help);
    }
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

#[cfg(test)]
mod tests {
    use crate::flag::{Flag, FlagKind};
    use crate::model::CardKind;
    use crate::permission::{Held, PermissionCard};
    use crate::queue::tests::card;
    use crate::ui::tests::{app, render};

    #[test]
    fn queue_bar_marks_a_flagged_card() {
        let mut a = app();
        let flag = Flag {
            kind: FlagKind::HighStakes,
            label: "MAIN BRANCH".into(),
            target: "o/r main".into(),
            who: String::new(),
            happens: String::new(),
            as_you: String::new(),
            conflict: String::new(),
            pending: false,
        };
        a.queue.push(card(
            1,
            "/w/api",
            CardKind::Permission(PermissionCard {
                tool_name: "Bash".into(),
                held: Some(Held::Flagged(flag)),
                ..Default::default()
            }),
            5,
        ));
        let text = render(&a, 60, 6);
        assert!(text.lines().next().unwrap().contains("▲ PERMISSION"));
        insta::assert_snapshot!(text);
    }
}
