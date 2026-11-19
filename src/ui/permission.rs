//! The permission card: which tool wants what, and the keys that answer it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::window_of;
use crate::app::{App, project_name};
use crate::flag::{Flag, FlagKind, FlagState};
use crate::model::Card;
use crate::permission::{GIST_ROWS, Gist, Held, LineKind, PermissionCard};
use crate::theme::Theme;
use crate::visible::{cols, visible_code, visible_lines, wrap_words};

/// How a row of the band is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandRow {
    /// The label row, in the kind's colour.
    Head(FlagKind),
    Text,
    /// The conflict line, in the kind's colour and bold.
    Conflict(FlagKind),
    Dim,
    /// aq could not check the call.
    Unchecked,
}

/// Width of the labels (`Conflict: `); wrapped text lines up under them.
const LABEL: usize = 10;

/// `row` cut to fit `width` columns, ending in `…`.
fn cut(mut row: String, width: usize) -> String {
    while row.chars().map(cols).sum::<usize>() >= width.max(1) {
        row.pop();
    }
    row.push('…');
    row
}

/// `label` and `text` as rows of at most `width` columns, the later ones
/// indented under the text.
fn field(label: &str, text: &str, width: usize) -> Vec<String> {
    let line = visible_lines(text).join(" ");
    let mut rows = Vec::new();
    wrap_words(&line, width.saturating_sub(LABEL), &mut |row| {
        rows.push(row)
    });
    for (i, row) in rows.iter_mut().enumerate() {
        let lead = if i == 0 { label } else { "" };
        row.insert_str(0, &format!("{lead:<LABEL$}"));
    }
    rows
}

/// The band of a held card as at most `GIST_ROWS + 1` rows of `width`
/// columns, and never fewer than one, even when `room` is 0. When there
/// are too many rows, the first and the conflict stay, then the others in
/// order; a row that is cut ends in `…`.
pub fn band_rows(held: &Held, width: usize, room: usize) -> Vec<(BandRow, String)> {
    match held {
        Held::Unchecked(why) => {
            let room = room.clamp(1, GIST_ROWS as usize + 1);
            let mut rows = Vec::new();
            let text = visible_lines(&format!("aq could not check this: {why}")).join(" ");
            wrap_words(&text, width, &mut |row| rows.push(row));
            if rows.len() > room {
                rows.truncate(room);
                if let Some(last) = rows.pop() {
                    rows.push(cut(last, width));
                }
            }
            rows.into_iter().map(|r| (BandRow::Unchecked, r)).collect()
        }
        Held::Flagged(flag) => flag_rows(flag, width, room),
    }
}

/// The band of a flagged card, as `band_rows` lays it out.
pub fn flag_rows(flag: &Flag, width: usize, room: usize) -> Vec<(BandRow, String)> {
    let room = room.clamp(1, GIST_ROWS as usize + 1);
    let mark = match flag.kind {
        FlagKind::SomeoneElse => "▌▌",
        FlagKind::HighStakes => "▲",
    };
    let head = if flag.target.is_empty() {
        format!("{mark} {}", flag.label)
    } else {
        format!("{mark} {} · {}", flag.label, flag.target)
    };
    let mut head = visible_lines(&head).join(" ");
    if head.chars().map(cols).sum::<usize>() > width {
        head = cut(head, width);
    }
    let mut others: Vec<(BandRow, String)> = Vec::new();
    if flag.pending {
        others.push((BandRow::Dim, "Haiku: checking…".into()));
    } else {
        for (label, text) in [
            ("Who:", &flag.who),
            ("Happens:", &flag.happens),
            ("As you:", &flag.as_you),
        ] {
            if !text.is_empty() {
                others.extend(
                    field(label, text, width)
                        .into_iter()
                        .map(|r| (BandRow::Text, r)),
                );
            }
        }
    }
    let mut conflict = if flag.conflict.is_empty() {
        Vec::new()
    } else {
        field("Conflict:", &flag.conflict, width)
    };
    // The head and the conflict's first row stay; the others share the rest.
    let spare = (room.saturating_sub(1)).saturating_sub(conflict.len().min(1));
    let others_cut = others.len() > spare;
    others.truncate(spare);
    if others_cut {
        match others.last_mut() {
            Some((_, last)) => *last = cut(std::mem::take(last), width),
            None => head = cut(head, width),
        }
    }
    let left = room.saturating_sub(1 + others.len());
    let conflict_cut = conflict.len() > left;
    conflict.truncate(left);
    if conflict_cut && let Some(last) = conflict.last_mut() {
        *last = cut(std::mem::take(last), width);
    }
    let mut rows = vec![(BandRow::Head(flag.kind), head)];
    rows.extend(others);
    rows.extend(
        conflict
            .into_iter()
            .map(|r| (BandRow::Conflict(flag.kind), r)),
    );
    rows
}

/// The band's rows, styled.
fn band_lines(rows: Vec<(BandRow, String)>, t: &Theme) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|(kind, text)| {
            let style = match kind {
                BandRow::Head(k) | BandRow::Conflict(k) => Style::new()
                    .fg(t.flag_colour(k))
                    .add_modifier(Modifier::BOLD),
                BandRow::Text => Style::new().fg(t.fg),
                BandRow::Dim => Style::new().fg(t.dim),
                BandRow::Unchecked => Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
            };
            Line::styled(text, style)
        })
        .collect()
}

/// The top of a question or option-menu card's `body`: one dim row while
/// it is checked, the band and a blank row once it is flagged. Returns the
/// rest of `body`.
pub fn flag_band(frame: &mut Frame, body: Rect, app: &App, state: &FlagState) -> Rect {
    let lines = match state {
        FlagState::None => return body,
        FlagState::Checking => vec![Line::styled(
            "Checking whether this involves others…",
            Style::new().fg(app.theme().dim),
        )],
        FlagState::Flagged(flag) => {
            let room = usize::from(body.height.saturating_sub(2));
            let mut lines = band_lines(flag_rows(flag, usize::from(body.width), room), app.theme());
            // A blank row between the band and the text.
            lines.push(Line::raw(""));
            lines
        }
    };
    let [band, rest] =
        Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Fill(1)]).areas(body);
    frame.render_widget(Paragraph::new(lines), band);
    rest
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, card: &Card, pc: &PermissionCard) {
    let t = app.theme();
    let bold = Style::new().fg(t.fg).add_modifier(Modifier::BOLD);
    let head = vec![
        Line::styled(
            format!("Permission: {}", visible_code(&pc.tool_name)),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(project_name(&card.checkout), bold),
            Span::styled(
                format!("  window {}", window_of(app, card)),
                Style::new().fg(t.dim),
            ),
        ]),
    ];
    let [head_area, gist, body, more, keys] =
        crate::permission::card_areas(area, pc.held.is_some());
    frame.render_widget(Paragraph::new(head), head_area);
    if let Some(held) = &pc.held {
        let band = band_lines(
            band_rows(held, gist.width as usize, gist.height as usize),
            t,
        );
        frame.render_widget(Paragraph::new(band), gist);
    } else {
        let gist_style = match pc.gist {
            Gist::Done(_) => Style::new().fg(t.fg).add_modifier(Modifier::ITALIC),
            Gist::Pending | Gist::Failed(_) => Style::new().fg(t.dim),
        };
        let gist_rows: Vec<Line> = pc
            .gist_rows(gist.width as usize)
            .into_iter()
            .map(Line::from)
            .collect();
        frame.render_widget(Paragraph::new(gist_rows).style(gist_style), gist);
    }
    let rows = pc.body_rows(body.width as usize);
    let top = pc
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let shown: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(body.height as usize)
        .map(|(kind, text)| {
            let colour = match kind {
                LineKind::Plain => t.fg,
                LineKind::Del => t.del,
                LineKind::Add => t.add,
            };
            Line::styled(text.clone(), Style::new().fg(colour))
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
    let dim = Style::new().fg(t.dim);
    let mut spans = Vec::new();
    for (k, label) in [
        ("y", "allow"),
        ("n", "deny"),
        ("g", "go to window"),
        ("?", "ask"),
    ] {
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}  "), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
}

#[cfg(test)]
mod tests {
    use crate::flag::{Flag, FlagKind};
    use crate::model::{Card, CardKind};
    use crate::permission::{Held, PermissionCard};
    use crate::queue::tests::card;
    use crate::ui::tests::{app, render};

    fn incident() -> Flag {
        Flag {
            kind: FlagKind::SomeoneElse,
            label: "SOMEONE ELSE".into(),
            target: "nodo-labs-uk/socratui PR #5 by Timonsc (outside your org)".into(),
            who: "Timonsc opened this PR from their fork".into(),
            happens: "merges PR #5 into main".into(),
            as_you: "the merge is recorded under your GitHub account".into(),
            conflict: "you requested changes on 17 Nov; no commits since".into(),
            pending: false,
        }
    }

    fn held_card(held: Held) -> Card {
        card(
            1,
            "/w/socratui",
            CardKind::Permission(PermissionCard {
                tool_name: "Bash".into(),
                tool_input: serde_json::json!({"command": "gh pr merge 5 --squash"}),
                summary: "gh pr merge 5 --squash".into(),
                held: Some(held),
                ..Default::default()
            }),
            10,
        )
    }

    fn shown(held: Held) -> String {
        let mut a = app();
        a.queue.push(held_card(held));
        render(&a, 100, 30)
    }

    #[test]
    fn flagged_someone_else_card() {
        insta::assert_snapshot!(shown(Held::Flagged(incident())));
    }

    #[test]
    fn flagged_high_stakes_card() {
        let flag = Flag {
            kind: FlagKind::HighStakes,
            label: "MAIN BRANCH".into(),
            target: "nodo-labs-uk/aq PR #5 into main".into(),
            who: "nobody else is involved".into(),
            happens: "merges PR #5 into main".into(),
            as_you: "the push is recorded under your GitHub account".into(),
            conflict: String::new(),
            pending: false,
        };
        insta::assert_snapshot!(shown(Held::Flagged(flag)));
    }

    #[test]
    fn unchecked_card() {
        insta::assert_snapshot!(shown(Held::Unchecked("gh is not logged in".into())));
    }

    #[test]
    fn a_short_screen_keeps_the_band_to_its_first_row_and_the_conflict() {
        let mut a = app();
        a.queue.push(held_card(Held::Flagged(incident())));
        let text = render(&a, 60, 12);
        assert!(text.contains("▌▌ SOMEONE ELSE"));
        assert!(text.contains("Conflict: you requested changes"));
        assert!(text.contains("gh pr merge"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn a_short_screen_keeps_the_unchecked_row() {
        let mut a = app();
        a.queue
            .push(held_card(Held::Unchecked("gh is not logged in".into())));
        let text = render(&a, 60, 12);
        assert!(text.contains("aq could not check this"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn no_room_still_leaves_the_first_band_row() {
        use super::{BandRow, band_rows};
        let rows = band_rows(&Held::Flagged(incident()), 40, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, BandRow::Head(FlagKind::SomeoneElse));
        assert!(rows[0].1.starts_with("▌▌ SOMEONE ELSE"));
        let rows = band_rows(&Held::Unchecked("offline".into()), 40, 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.starts_with("aq could not check"));
        // A screen so short that the body has one row left.
        let mut a = app();
        a.queue.push(held_card(Held::Flagged(incident())));
        assert!(render(&a, 60, 10).contains("▌▌ SOMEONE ELSE"));
    }

    #[test]
    fn a_pending_band_says_haiku_is_checking() {
        let flag = Flag {
            who: String::new(),
            happens: String::new(),
            as_you: String::new(),
            pending: true,
            ..incident()
        };
        insta::assert_snapshot!(shown(Held::Flagged(flag)));
    }

    #[test]
    fn a_long_band_keeps_its_first_row_and_the_conflict() {
        let long = "word ".repeat(60);
        let flag = Flag {
            who: long.clone(),
            happens: long,
            ..incident()
        };
        let text = shown(Held::Flagged(flag));
        assert!(text.contains("▌▌ SOMEONE ELSE"));
        assert!(text.contains("Conflict: you requested changes"));
        assert!(text.contains('…'));
        insta::assert_snapshot!(text);
    }
}
