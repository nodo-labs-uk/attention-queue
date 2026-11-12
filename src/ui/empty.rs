use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
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
        let [col] = Layout::horizontal([Constraint::Length(w)])
            .flex(Flex::Center)
            .areas(area);
        let [block] = Layout::vertical([Constraint::Length(2)])
            .flex(Flex::Center)
            .areas(col);
        frame.render_widget(Paragraph::new(lines), block);
        return;
    };
    let rows = tab_rows(app, &picked.id, area.width);
    // A row of the strip's colour above and below the tabs sets them apart.
    let [strip, live] = Layout::vertical([
        Constraint::Length(rows.len() as u16 + 2),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Block::new().style(Style::new().bg(t.panel)), strip);
    frame.render_widget(Paragraph::new(rows), strip.inner(Margin::new(0, 1)));
    // Full width: agents' panes are usually as wide as this one, and their
    // lines are already wrapped to it, so any margin would wrap them again.
    render_live(frame, live, app);
}

/// One tab per agent in window order, flowed into as many rows of `width`
/// as they need. Waiting is a filled dot, working a hollow one, so the state
/// does not rest on colour; the picked tab is a solid block.
fn tab_rows(app: &App, picked: &AgentId, width: u16) -> Vec<Line<'static>> {
    let t = app.theme();
    let agents = app.agents_in_order();
    let one_session = agents.iter().all(|a| a.session == agents[0].session);
    let mut rows: Vec<Line> = vec![Line::raw(" ")];
    for a in agents {
        let on = &a.id == picked;
        let (dot, dot_fg) = if a.waiting {
            ("●", t.hi)
        } else {
            ("○", t.dim)
        };
        // A program can name its own window, so names are shown as visible text.
        let name = visible(&a.window_name);
        let window = if one_session {
            format!("{} {}", a.window_index, name)
        } else {
            format!("{}:{} {}", visible(&a.session), a.window_index, name)
        };
        let project = visible(&agent_project(a));
        let mut label = format!(" {dot} {window}");
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
        // Claude Code's permission mode, with the mark its footer uses.
        label.push_str(match a.permission_mode {
            Some("auto") => " ⏵⏵ auto",
            Some("accept edits") => " ⏵⏵ edits",
            Some("plan") => " ⏸ plan",
            Some("manual") => " ⏸ manual",
            _ => "",
        });
        if app.prompt_waiting(&a.id) {
            // Typed text in its box holds the message back: say so.
            label.push_str(if app.prompt_has_text(&a.id) {
                " queued: clear its prompt"
            } else {
                " queued"
            });
        }
        label.push(' ');
        let spans = if on {
            vec![Span::styled(
                label,
                Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD),
            )]
        } else {
            let rest = label[1 + dot.len()..].to_string();
            vec![
                Span::raw(" "),
                Span::styled(dot, Style::new().fg(dot_fg)),
                Span::styled(rest, Style::new().fg(t.fg)),
            ]
        };
        let w: usize = spans.iter().map(Span::width).sum();
        let row = rows.last_mut().expect("a row");
        if row.width() + w > width as usize && row.width() > 1 {
            rows.push(Line::raw(" "));
        }
        let row = rows.last_mut().expect("a row");
        row.spans.extend(spans);
        row.spans.push(Span::raw(" "));
    }
    rows
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
fn visible(s: &str) -> String {
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
