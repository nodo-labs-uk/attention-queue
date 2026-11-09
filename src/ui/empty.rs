use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::agents::Agent;
use crate::app::{App, project_name};

/// "Nothing needs you." and one line per agent: window, project, working or
/// waiting. The picked idle agent is marked, to go to with Enter, and its
/// screen shows below the list.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let agents = app.agents_in_order();
    let picked = app.picked_agent().map(|a| &a.id);
    let rows: Vec<[String; 3]> = agents
        .iter()
        .map(|a| {
            [
                format!("{}:{} {}", a.session, a.window_index, a.window_name),
                agent_project(a),
                if a.waiting { "waiting" } else { "working" }.to_string(),
            ]
        })
        .collect();
    let width = |col: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0), width(1));

    let mut lines = vec![
        Line::styled(
            "Nothing needs you.",
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    if rows.is_empty() {
        lines.push(Line::styled(
            "No Claude agents found in tmux.",
            Style::new().fg(t.dim),
        ));
    }
    for (row, agent) in rows.iter().zip(&agents) {
        let state = if agent.waiting { t.hi } else { t.dim };
        let (mark, name) = if picked == Some(&agent.id) {
            ("› ", Style::new().fg(t.acc).add_modifier(Modifier::BOLD))
        } else {
            ("  ", Style::new().fg(t.fg))
        };
        lines.push(Line::from(vec![
            Span::styled(mark, Style::new().fg(t.acc).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:w0$}  ", row[0]), name),
            Span::styled(format!("{:w1$}  ", row[1]), Style::new().fg(t.dim)),
            Span::styled(row[2].clone(), Style::new().fg(state)),
        ]));
    }

    let text_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let list_h = lines.len() as u16;
    let Some(agent) = app.picked_agent() else {
        let [col] = Layout::horizontal([Constraint::Length(text_w)])
            .flex(Flex::Center)
            .areas(area);
        let [block] = Layout::vertical([Constraint::Length(list_h)])
            .flex(Flex::Center)
            .areas(col);
        frame.render_widget(Paragraph::new(lines), block);
        return;
    };
    // With an agent to pick, the list sits on top and its screen fills the rest.
    let [_, list, _, live] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(list_h),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [list] = Layout::horizontal([Constraint::Length(text_w)])
        .flex(Flex::Center)
        .areas(list);
    frame.render_widget(Paragraph::new(lines), list);
    render_live(frame, live, app, agent);
}

/// The picked agent's screen as last read: its bottom rows, where the prompt is.
fn render_live(frame: &mut Frame, area: Rect, app: &App, agent: &Agent) {
    let t = app.theme();
    let block = Block::bordered()
        .border_style(Style::new().fg(t.dim))
        .title(Span::styled(
            format!(
                " {}:{} {} ",
                agent.session, agent.window_index, agent.window_name
            ),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = match app.picked_preview() {
        None => vec![Line::styled("Reading the screen…", Style::new().fg(t.dim))],
        Some(Err(e)) => vec![Line::styled(e.clone(), Style::new().fg(t.hi))],
        Some(Ok(text)) => {
            let rows: Vec<&str> = text.trim_end().lines().collect();
            let skip = rows.len().saturating_sub(inner.height as usize);
            rows[skip..]
                .iter()
                .map(|r| Line::styled(r.to_string(), Style::new().fg(t.fg)))
                .collect()
        }
    };
    frame.render_widget(Paragraph::new(lines), inner);
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
