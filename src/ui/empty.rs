use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::agents::Agent;
use crate::app::{App, project_name};
use crate::model::{AgentId, Mode};
use crate::transcript::Said;
use crate::visible::{push_visible, wrap};

/// With agents: a strip of tabs, one per agent, and the picked agent's
/// screen filling the rest. Without: a short note in the middle.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let Some(picked) = app.picked_agent() else {
        let lines = vec![
            Line::styled(
                "Nothing needs you.",
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Line::styled("No Claude agents found in tmux.", Style::new().fg(t.dim)),
        ];
        let w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let block = super::input::centred(area, (w, 2));
        frame.render_widget(Paragraph::new(lines), block);
        return;
    };
    if app.zen {
        // No tabs; the session sits in the cards' column, with its caption.
        let column = super::text_card_area(area);
        let [cap, live] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(column);
        frame.render_widget(Paragraph::new(caption(app, picked, cap.width)), cap);
        render_live(frame, live, app);
        return;
    }
    let rows = tab_rows(app, &picked.id, area.width);
    // A row of the strip's colour above and below the tabs sets them apart.
    let [strip, live] = Layout::vertical([
        Constraint::Length(rows.len() as u16 + 2),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Block::new().style(Style::new().bg(t.panel)), strip);
    frame.render_widget(Paragraph::new(rows), strip.inner(Margin::new(0, 1)));
    // A caption names the session; the live screen takes the rest, a row
    // shorter, so its last line stays in view.
    let [cap, live] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(live);
    frame.render_widget(Paragraph::new(caption(app, picked, cap.width)), cap);
    // Full width: agents' panes are usually as wide as this one, and their
    // lines are already wrapped to it, so any margin would wrap them again.
    render_live(frame, live, app);
}

/// One tab per agent in window order, flowed into as many rows of `width`
/// as they need. Each tab is a pill: the picked one filled in the accent
/// colour, the others on a quiet fill. Waiting is a filled dot, working a
/// hollow dimmed one, so the state does not rest on colour.
/// With agents in more than one row (a workspace, else a tmux session),
/// each starts a row of its own, led by its name in a column of one width,
/// so the tabs of every row start together.
fn tab_rows(app: &App, picked: &AgentId, width: u16) -> Vec<Line<'static>> {
    let t = app.theme();
    let agents = app.agents_in_order();
    let one_row = agents.iter().all(|a| a.row() == agents[0].row());
    // The group column: the widest name, then two spaces.
    let lead = if one_row {
        0
    } else {
        agents
            .iter()
            .map(|a| visible(a.row()).chars().count())
            .max()
            .unwrap_or(0)
            + 2
    };
    let labels: Vec<String> = agents.iter().map(|a| tab_label(app, a)).collect();
    // Every tab as wide as the widest, so the tabs of each row line up in
    // columns; a label too wide for the screen keeps its own width.
    let cell = labels
        .iter()
        .map(|l| Span::raw(l.as_str()).width())
        .max()
        .unwrap_or(0)
        .min((width as usize).saturating_sub(lead + 8));
    let mut rows: Vec<Line> = Vec::new();
    let mut group: Option<&str> = None;
    for (a, label) in agents.iter().zip(labels) {
        if group != Some(a.row()) {
            group = Some(a.row());
            let name = if one_row {
                String::new()
            } else {
                visible(a.row())
            };
            rows.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{name:<lead$}"),
                    Style::new().fg(t.dim).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        let on = &a.id == picked;
        let dot = if a.waiting { "●" } else { "○" };
        let pad = cell.saturating_sub(Span::raw(label.as_str()).width());
        let label = format!("{label}{} ", " ".repeat(pad));
        let spans = if on {
            let style = Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD);
            vec![
                Span::styled(format!(" {dot}"), style),
                Span::styled(label, style),
            ]
        } else {
            let pill = Style::new().bg(t.line);
            // The name stays readable on the fill in every theme; the
            // dot carries the state.
            let dot_fg = if a.waiting { t.hi } else { t.dim };
            vec![
                Span::styled(format!(" {dot}"), pill.fg(dot_fg)),
                Span::styled(label, pill.fg(t.fg)),
            ]
        };
        let w: usize = spans.iter().map(Span::width).sum();
        let row = rows.last_mut().expect("a row");
        if row.width() + w > width as usize && row.width() > 2 + lead {
            rows.push(Line::raw(" ".repeat(2 + lead)));
        }
        let row = rows.last_mut().expect("a row");
        row.spans.extend(spans);
        row.spans.push(Span::raw("  "));
    }
    rows
}

/// A tab's text: window number, name, project when it differs, a mode
/// other than review, a permission mode other than auto, and a queued
/// message.
fn tab_label(app: &App, a: &Agent) -> String {
    // A program can name its own window, so names are shown as visible text.
    let name = visible(&a.window_name);
    let project = visible(&agent_project(a));
    let mut label = format!(" {} {name}", a.window_index);
    if project != name {
        label.push_str(&format!(" {project}"));
    }
    // Review is the default; any other mode is worth knowing at a glance,
    // vibe most of all, since it never makes a card.
    let mode = a.checkout.as_ref().map(|c| app.config.mode_for(c));
    if let Some(m @ (Mode::Step | Mode::Quiz | Mode::Vibe)) = mode {
        label.push_str(match m {
            Mode::Step => " · step",
            Mode::Quiz => " · quiz",
            _ => " · vibe",
        });
    }
    // Auto is how sessions usually run: only another mode is shown.
    if a.permission_mode != Some("auto") {
        label.push_str(permission_mark(a));
    }
    if app.prompt_waiting(&a.id) {
        // Typed text in its box holds the message back: say so.
        label.push_str(if app.prompt_has_text(&a.id) {
            " queued: clear its prompt"
        } else {
            " queued"
        });
    }
    label
}

/// Claude Code's permission mode, with the mark its footer uses, after a
/// space; empty when it is not known.
fn permission_mark(a: &Agent) -> &'static str {
    match a.permission_mode {
        Some("auto") => " ⏵⏵ auto",
        Some("accept edits") => " ⏵⏵ edits",
        Some("plan") => " ⏸ plan",
        Some("manual") => " ⏸ manual",
        _ => "",
    }
}

/// The dim line over the live screen: which session it is, its window and
/// permission mode, and whether it waits on you, then a rule to the edge.
fn caption(app: &App, a: &Agent, width: u16) -> Line<'static> {
    let t = app.theme();
    let mut parts = vec![
        visible(&agent_project(a)),
        format!("window {}", a.window_index),
    ];
    let mark = permission_mark(a).trim_start();
    if !mark.is_empty() {
        parts.push(mark.to_string());
    }
    parts.push(
        if a.waiting {
            "waiting for you"
        } else {
            "working"
        }
        .to_string(),
    );
    let text = format!(" {} ", parts.join(" · "));
    let rule = (width as usize).saturating_sub(text.chars().count());
    Line::from(vec![
        Span::styled(text, Style::new().fg(t.dim)),
        Span::styled("─".repeat(rule), Style::new().fg(t.dim)),
    ])
}

/// The picked agent's screen as last read, its bottom rows, where its
/// latest output is. Scrolled up, its conversation instead.
fn render_live(frame: &mut Frame, area: Rect, app: &App) {
    if app.live_scroll > 0 {
        render_conversation(frame, area, app);
        return;
    }
    let t = app.theme();
    let lines: Vec<Line> = match app.picked_preview() {
        None => vec![Line::styled("Reading the screen…", Style::new().fg(t.dim))],
        Some(Err(e)) => vec![Line::styled(e.clone(), Style::new().fg(t.hi))],
        Some(Ok(text)) => {
            // Wrapped to the area, so the bottom rows are counted after wrapping.
            let mut rows = Vec::new();
            for line in without_input_box(text) {
                wrap(&live_line(line), area.width as usize, &mut |r| rows.push(r));
            }
            let skip = rows.len().saturating_sub(area.height as usize);
            rows.drain(skip..)
                .map(|r| Line::styled(r, Style::new().fg(t.fg)))
                .collect()
        }
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// What a row of the conversation view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    User,
    Claude,
    Tool,
    Gap,
}

/// The conversation as rows of `width`: a prompt marked `❯`, a reply `●`,
/// a tool call `⎿`, with a blank row before each prompt and reply.
pub fn conversation_rows(said: &[Said], width: usize) -> Vec<(RowKind, String)> {
    let mut rows = Vec::new();
    for s in said {
        let (kind, mark, text) = match s {
            Said::User(t) => (RowKind::User, "❯ ", t),
            Said::Claude(t) => (RowKind::Claude, "● ", t),
            Said::Tool(t) => (RowKind::Tool, "  ⎿ ", t),
        };
        if kind != RowKind::Tool && !rows.is_empty() {
            rows.push((RowKind::Gap, String::new()));
        }
        let indent = " ".repeat(mark.chars().count());
        let room = width.saturating_sub(indent.len()).max(1);
        let mut first = true;
        for line in text.lines() {
            wrap(&visible(line.trim_end()), room, &mut |r| {
                let lead = if first { mark } else { indent.as_str() };
                first = false;
                rows.push((kind, format!("{lead}{r}")));
            });
        }
    }
    rows
}

/// The picked agent's conversation, scrolled up `live_scroll - 1` rows from
/// its end, with a row saying how to get back to the live screen.
fn render_conversation(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let dim = Style::new().fg(t.dim);
    let said = match app.picked_conversation() {
        None => {
            let note = Line::styled("Reading the conversation…", dim);
            frame.render_widget(Paragraph::new(note), area);
            return;
        }
        Some(Err(e)) => {
            let note = Line::styled(e.clone(), Style::new().fg(t.hi));
            frame.render_widget(Paragraph::new(note), area);
            return;
        }
        Some(Ok(said)) => said,
    };
    let rows = conversation_rows(said, area.width as usize);
    // The last row is the way back.
    let height = (area.height as usize).saturating_sub(1);
    let back = (app.live_scroll - 1).min(rows.len().saturating_sub(height));
    let end = rows.len() - back;
    let mut lines: Vec<Line> = rows[end.saturating_sub(height)..end]
        .iter()
        .map(|(kind, text)| {
            let style = match kind {
                RowKind::User => Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
                RowKind::Claude => Style::new().fg(t.fg),
                RowKind::Tool | RowKind::Gap => dim,
            };
            Line::styled(text.clone(), style)
        })
        .collect();
    let more = if back > 0 {
        format!("↓ {back} more rows · G back to the live screen")
    } else {
        "↓ the live screen · G or scroll down".to_string()
    };
    lines.resize(height, Line::raw(""));
    lines.push(Line::styled(more, Style::new().fg(t.bg).bg(t.dim)));
    frame.render_widget(Paragraph::new(lines), area);
}

/// `s` with control and invisible characters spelled out, so text from an
/// agent cannot move the cursor, recolour the terminal or reorder what is
/// shown.
pub(crate) fn visible(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    s.chars().for_each(|c| push_visible(c, &mut out));
    out
}

/// A row of the session's screen as its terminal draws it: Claude Code
/// pads its marks with non-breaking spaces, which a terminal shows as
/// spaces. Other hidden characters are spelled out.
fn live_line(line: &str) -> String {
    visible(line.replace('\u{a0}', " ").trim_end())
}

/// The screen's lines down to Claude Code's input box: the rule above its
/// last `❯` line, the prompt, and the status lines under it are left out,
/// since the message box here takes their place. A screen without that box
/// (a menu open, another program) is kept whole.
fn without_input_box(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    match crate::agents::input_row(&lines) {
        Some(p) => lines[..p - 1].to_vec(),
        None => lines,
    }
}

fn agent_project(a: &Agent) -> String {
    match &a.checkout {
        Some(c) => project_name(c),
        None => a.cwd.file_name().map_or_else(
            || a.cwd.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::without_input_box;

    #[test]
    fn agent_text_is_shown_with_controls_spelled_out() {
        use crate::transcript::Said;
        let said = [Said::Claude("ok \u{1b}[2J\u{202e}gnp.exe".into())];
        let rows = super::conversation_rows(&said, 80);
        let all: String = rows.iter().map(|(_, r)| r.as_str()).collect();
        assert!(
            !all.contains('\u{1b}') && !all.contains('\u{202e}'),
            "{all:?}"
        );
        assert!(all.contains("^[") && all.contains("\\u{202e}"), "{all:?}");
    }

    #[test]
    fn the_live_screen_shows_a_non_breaking_space_as_the_terminal_does() {
        assert_eq!(
            super::live_line("  ⎿ \u{a0}Added 6 lines  "),
            "  ⎿  Added 6 lines"
        );
        assert_eq!(
            super::live_line("x\u{202e}y"),
            "x\\u{202e}y",
            "still spelled out"
        );
    }

    #[test]
    fn input_box_and_status_lines_are_left_out() {
        let screen = "● Done.\n\n❯ earlier prompt in the history\n● More.\n─────\n❯ half typed\n─────\n  ctx:11% Opus\n";
        assert_eq!(
            without_input_box(screen),
            ["● Done.", "", "❯ earlier prompt in the history", "● More."]
        );
        // No rule above the last ❯ (a menu, another program): all kept.
        let menu = "Allow this?\n❯ 1. Yes\n  2. No";
        assert_eq!(
            without_input_box(menu),
            ["Allow this?", "❯ 1. Yes", "  2. No"]
        );
    }
}
