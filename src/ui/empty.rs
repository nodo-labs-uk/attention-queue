use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::agents::Agent;
use crate::app::{App, project_name};

/// "Nothing needs you." and one line per agent: window, project, working or waiting.
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let mut agents: Vec<&Agent> = app.agents.iter().collect();
    agents.sort_by(|a, b| (&a.session, a.window_index).cmp(&(&b.session, b.window_index)));
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
        lines.push(Line::from(vec![
            Span::styled(format!("{:w0$}  ", row[0]), Style::new().fg(t.fg)),
            Span::styled(format!("{:w1$}  ", row[1]), Style::new().fg(t.dim)),
            Span::styled(row[2].clone(), Style::new().fg(state)),
        ]));
    }

    let text_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let [col] = Layout::horizontal([Constraint::Length(text_w)])
        .flex(Flex::Center)
        .areas(area);
    let [block] = Layout::vertical([Constraint::Length(lines.len() as u16)])
        .flex(Flex::Center)
        .areas(col);
    frame.render_widget(Paragraph::new(lines), block);
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
