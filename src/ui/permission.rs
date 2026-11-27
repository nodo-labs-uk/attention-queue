//! The permission card: which tool wants what, and the keys that answer it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::flag::{Flag, FlagKind, FlagState};
use crate::permission::{GIST_ROWS, Gist, Held, LineKind, PermissionCard};
use crate::theme::Theme;
use crate::visible::{cols, visible_code, visible_lines, wrap_words};

/// How a row of the band is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandRow {
    /// The first row, on a strip tinted in the kind's colour: the mark and
    /// label (its first `label` characters) in that colour, then what is
    /// involved, and from character `aside` on, a quieter aside.
    Head {
        kind: FlagKind,
        label: usize,
        aside: Option<usize>,
    },
    /// A fact: a dim label in the first `LABEL` columns, then its text.
    Text,
    /// The conflict, label and text in the colour of removed lines.
    Conflict,
    Dim,
    /// aq could not check the call; on a strip tinted in the warning colour.
    Unchecked,
}

/// Width of the labels (`Conflict` and two spaces); wrapped text lines up
/// under them.
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
        row.insert_str(0, &format!("{lead:>width$}  ", width = LABEL - 2));
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
    let label = visible_lines(&format!("{mark} {}", flag.label)).join(" ");
    let target = visible_lines(&flag.target).join(" ");
    // "PR #5 by Timonsc (outside your org)": the bracketed end is an aside.
    let (subject, aside) = match target.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) {
        Some((subject, aside)) => (subject.to_string(), Some(aside.to_string())),
        None => (target, None),
    };
    let mut head = label.clone();
    if !subject.is_empty() {
        head.push_str("  ");
        head.push_str(&subject);
    }
    let aside_at = aside.as_ref().map(|a| {
        head.push_str("  ");
        let at = head.chars().count();
        head.push_str(a);
        at
    });
    if head.chars().map(cols).sum::<usize>() > width {
        head = cut(head, width);
    }
    let mut others: Vec<(BandRow, String)> = Vec::new();
    if flag.pending {
        others.push((BandRow::Dim, "Haiku: checking…".into()));
    } else {
        for (label, text) in [
            ("Who", &flag.who),
            ("Happens", &flag.happens),
            ("As you", &flag.as_you),
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
        field("Conflict", &flag.conflict, width)
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
    let head_row = BandRow::Head {
        kind: flag.kind,
        label: label.chars().count(),
        aside: aside_at,
    };
    let mut rows = vec![(head_row, head)];
    rows.extend(others);
    rows.extend(conflict.into_iter().map(|r| (BandRow::Conflict, r)));
    rows
}

/// `text` split after its first `n` characters.
fn split_at_char(text: &str, n: usize) -> (String, String) {
    let at = text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
    (text[..at].to_string(), text[at..].to_string())
}

/// A band row as styled spans.
fn band_line(kind: BandRow, text: String, t: &Theme) -> Line<'static> {
    let dim = Style::new().fg(t.dim);
    match kind {
        BandRow::Head { kind, label, aside } => {
            let (mark, rest) = split_at_char(&text, label);
            let (subject, aside) = match aside {
                Some(at) => split_at_char(&rest, at.saturating_sub(label)),
                None => (rest, String::new()),
            };
            Line::from(vec![
                Span::styled(
                    mark,
                    Style::new()
                        .fg(t.flag_colour(kind))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(subject, Style::new().fg(t.fg)),
                Span::styled(aside, dim),
            ])
        }
        BandRow::Text => {
            let (label, rest) = split_at_char(&text, LABEL);
            Line::from(vec![
                Span::styled(label, dim),
                Span::styled(rest, Style::new().fg(t.fg)),
            ])
        }
        BandRow::Conflict => {
            let (label, rest) = split_at_char(&text, LABEL);
            Line::from(vec![
                Span::styled(label, Style::new().fg(t.del).add_modifier(Modifier::BOLD)),
                Span::styled(rest, Style::new().fg(t.del)),
            ])
        }
        BandRow::Dim => Line::styled(text, dim),
        BandRow::Unchecked => {
            Line::styled(text, Style::new().fg(t.hi).add_modifier(Modifier::BOLD))
        }
    }
}

/// Draws the band's `rows` from the top of `text`, each strip row tinted
/// across `strip` (its column and width), so the strip can run wider than
/// the text. Returns how many rows it drew.
fn draw_band(
    frame: &mut Frame,
    text: Rect,
    strip: (u16, u16),
    rows: Vec<(BandRow, String)>,
    t: &Theme,
) -> u16 {
    let n = (rows.len() as u16).min(text.height);
    for (i, (kind, _)) in rows.iter().enumerate().take(n as usize) {
        let tint = match kind {
            BandRow::Head { kind, .. } => t.tint(t.flag_colour(*kind)),
            BandRow::Unchecked => t.tint(t.hi),
            _ => continue,
        };
        let row = Rect::new(strip.0, text.y + i as u16, strip.1, 1);
        frame.render_widget(Block::new().style(Style::new().bg(tint)), row);
    }
    let lines: Vec<Line> = rows
        .into_iter()
        .take(n as usize)
        .map(|(kind, text)| band_line(kind, text, t))
        .collect();
    frame.render_widget(Paragraph::new(lines), Rect { height: n, ..text });
    n
}

/// The rows `flag_band` takes at most at `width`, for sizing a card.
pub fn flag_band_height(state: &FlagState, width: usize) -> u16 {
    match state {
        FlagState::None => 0,
        FlagState::Checking => 1,
        FlagState::Flagged(flag) => {
            flag_rows(flag, width, usize::from(GIST_ROWS) + 1).len() as u16 + 1
        }
    }
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
            let rows = flag_rows(flag, usize::from(body.width), room);
            let n = draw_band(frame, body, (body.x, body.width), rows, app.theme());
            // A blank row between the band and the text.
            let used = (n + 1).min(body.height);
            return Rect {
                y: body.y + used,
                height: body.height - used,
                ..body
            };
        }
    };
    let [band, rest] =
        Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Fill(1)]).areas(body);
    frame.render_widget(Paragraph::new(lines), band);
    rest
}

/// The card: a held card's band or Haiku's summary, then the call on a
/// panel (the tool's name, then what it would do), and the keys. Under a
/// summary the panel holds the tool's name alone until `v` shows the rest.
pub fn render(frame: &mut Frame, area: Rect, app: &App, pc: &PermissionCard) {
    let t = app.theme();
    let [band, caption, body, gist, more, keys] =
        crate::permission::card_areas(area, pc.held.is_some(), pc.gist_max());
    // The panel starts under the band's or the summary's rows and a blank
    // row; rows they leave unused go below the body, which keeps the
    // height the scrolling counts on.
    let mut lift = 0;
    if let Some(held) = &pc.held {
        let room = if band.height >= 2 { band.height - 1 } else { 1 };
        let rows = band_rows(held, band.width as usize, room as usize);
        let n = draw_band(frame, band, (area.x, area.width), rows, t);
        lift = band.height.saturating_sub(n + 1);
    } else if gist.height > 0 {
        let style = match pc.gist {
            Gist::Done(_) => Style::new().fg(t.fg).add_modifier(Modifier::ITALIC),
            Gist::Pending | Gist::Failed(_) => Style::new().fg(t.dim),
        };
        let gist_rows: Vec<Line> = pc
            .gist_rows(body.width as usize, usize::from(gist.height - 1))
            .into_iter()
            .map(Line::from)
            .collect();
        let n = gist_rows.len() as u16;
        lift = gist.height - 1 - n;
        frame.render_widget(
            Paragraph::new(gist_rows).style(style),
            Rect::new(body.x, gist.y, body.width, n),
        );
    }
    let caption = Rect {
        y: caption.y - lift,
        ..caption
    };
    let body = Rect {
        y: body.y - lift,
        ..body
    };
    let folded = pc.folds() && gist.height > 0;
    let rows = if folded {
        Vec::new()
    } else {
        pc.body_rows(body.width as usize)
    };
    let top = pc
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let shown_rows = (rows.len() - top).min(body.height as usize) as u16;
    if caption.height > 0 {
        let panel = Rect::new(
            caption.x.saturating_sub(1),
            caption.y,
            caption.width + 2,
            caption.height + shown_rows,
        );
        frame.render_widget(Block::new().style(Style::new().bg(t.panel)), panel);
        frame.render_widget(
            Paragraph::new(visible_code(&pc.caption())).style(Style::new().fg(t.dim)),
            caption,
        );
    }
    // A command reads as code; the other fields as text.
    let plain = if pc.tool_name == "Bash" { t.key } else { t.fg };
    let shown: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(body.height as usize)
        .map(|(kind, text)| {
            let colour = match kind {
                LineKind::Plain => plain,
                LineKind::Del => t.del,
                LineKind::Add => t.add,
            };
            Line::styled(text.clone(), Style::new().fg(colour))
        })
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    let hidden = rows.len().saturating_sub(top + body.height as usize);
    if folded {
        frame.render_widget(
            Paragraph::new("▸ the request is hidden (v shows it)").style(Style::new().fg(t.dim)),
            more,
        );
    } else if hidden > 0 {
        let s = if hidden == 1 { "" } else { "s" };
        frame.render_widget(
            Paragraph::new(format!("▾ {hidden} more line{s} (j to scroll)"))
                .style(Style::new().fg(t.hi).add_modifier(Modifier::BOLD)),
            more,
        );
    }
    let mut spans = Vec::new();
    // A held card always shows its request.
    let request = pc.held.is_none().then_some(("v", "request"));
    for (k, label) in [("y", "allow"), ("n", "deny")]
        .into_iter()
        .chain(request)
        .chain([("g", "go to window"), ("c", "copy"), ("b", "/btw")])
    {
        spans.extend(super::keys::cap(k, label, t));
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
        assert!(text.contains("Conflict  you requested changes"));
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
        assert!(matches!(
            rows[0].0,
            BandRow::Head {
                kind: FlagKind::SomeoneElse,
                ..
            }
        ));
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
    fn the_band_is_a_tinted_strip_and_the_command_sits_on_a_panel() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        // The dark default and the light theme.
        for theme in [0, 3] {
            let mut a = app();
            a.theme_idx = theme;
            a.queue.push(held_card(Held::Flagged(incident())));
            let t = a.theme().clone();
            let mut hl = crate::highlight::Highlighter::new(&t);
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| crate::ui::draw(f, &a, &mut hl)).unwrap();
            let buf = term.backend().buffer();
            let row = |text: &str| {
                (0..buf.area.height)
                    .find(|&y| {
                        (0..buf.area.width)
                            .map(|x| buf[(x, y)].symbol())
                            .collect::<String>()
                            .contains(text)
                    })
                    .unwrap_or_else(|| panic!("{text} not on screen"))
            };
            let band = row("SOMEONE ELSE");
            let tint = t.tint(t.others);
            assert_ne!(tint, t.bg, "{}", t.name);
            // Across the card's column (two columns in on this screen),
            // past the end of the text.
            assert_eq!(buf[(2, band)].bg, tint, "{}", t.name);
            assert_eq!(buf[(97, band)].bg, tint, "{}", t.name);
            assert_eq!(buf[(1, band)].bg, t.bg, "the margin stays clear");
            assert_eq!(buf[(3, band)].fg, t.others, "{}", t.name);
            let conflict = row("Conflict");
            assert_eq!(buf[(3, conflict)].fg, t.del, "{}", t.name);
            assert_eq!(buf[(32, conflict)].fg, t.del, "{}", t.name);
            assert_eq!(buf[(3, row("Who"))].bg, t.bg, "the facts are off the strip");
            let command = row("gh pr merge");
            assert_eq!(buf[(3, row("Bash"))].bg, t.panel, "{}", t.name);
            assert_eq!(buf[(4, command)].bg, t.panel, "{}", t.name);
            assert_eq!(buf[(4, command)].fg, t.key, "{}", t.name);
        }
    }

    #[test]
    fn an_unheld_card_shows_haiku_and_hides_the_command_until_v() {
        let mut a = app();
        a.queue.push(card(
            1,
            "/w/api",
            CardKind::Permission(PermissionCard {
                tool_name: "Bash".into(),
                tool_input: serde_json::json!({"command": "cargo test"}),
                summary: "cargo test".into(),
                gist: crate::permission::Gist::Done("Runs the tests.".into()),
                ..Default::default()
            }),
            10,
        ));
        let screen = render(&a, 80, 24);
        let at = |screen: &str, text: &str| screen.lines().position(|l| l.contains(text)).unwrap();
        assert_eq!(
            at(&screen, "Bash"),
            at(&screen, "Haiku: Runs the tests.") + 2,
            "{screen}"
        );
        assert!(!screen.contains("Permission:"), "the bar says what it is");
        // The request is hidden until v shows it under its tool's name.
        assert!(!screen.contains("cargo test"), "{screen}");
        assert!(
            screen.contains("the request is hidden (v shows it)"),
            "{screen}"
        );
        a.update(crate::app::Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('v')),
            std::time::Instant::now(),
        ));
        let screen = render(&a, 80, 24);
        assert_eq!(
            at(&screen, "cargo test"),
            at(&screen, "Bash") + 1,
            "{screen}"
        );
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
        assert!(text.contains("Conflict  you requested changes"));
        assert!(text.contains('…'));
        insta::assert_snapshot!(text);
    }
}
