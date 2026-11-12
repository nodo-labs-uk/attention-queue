use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use super::input;
use super::new_session::{SIZE, render_folder};
use crate::config::Workspace;
use crate::theme::Theme;
use crate::visible::push_visible;
use crate::workspace::{Stage, WorkspaceDraft};

/// `O`: the saved workspaces, the folder, or the repos to tick, in one box
/// of one size, so nothing moves between them.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    d: &WorkspaceDraft,
    saved: &std::collections::BTreeMap<String, Workspace>,
    t: &Theme,
) {
    let dim = Style::new().fg(t.dim);
    let picked = Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD);
    match &d.stage {
        Stage::Choose { cursor } => {
            let mut rows: Vec<Line> = saved
                .iter()
                .enumerate()
                .map(|(i, (name, w))| {
                    let n = w.repos.len();
                    let text = format!(
                        " {}  {n} repo{}  {} ",
                        shown(name),
                        if n == 1 { "" } else { "s" },
                        shown(&w.root.display().to_string())
                    );
                    row(text, i == *cursor, picked, t)
                })
                .collect();
            rows.push(row(
                " Other folder… ".to_string(),
                *cursor == saved.len(),
                picked,
                t,
            ));
            list_box(
                frame,
                area,
                "Open a workspace",
                rows,
                &[
                    ("Enter", "open"),
                    ("Ctrl-O", "other folder"),
                    ("Esc", "cancel"),
                ],
                t,
            );
        }
        Stage::Folder(pick) => render_folder(
            frame,
            area,
            pick,
            "Folder holding the workspace's git repos",
            "Not there: type a folder that is",
            t,
        ),
        Stage::Finding { asked } => list_box(
            frame,
            area,
            "Open a workspace",
            vec![Line::styled(
                format!(
                    "Looking for git repos in {}…",
                    shown(&asked.display().to_string())
                ),
                dim,
            )],
            &[("Esc", "back")],
            t,
        ),
        Stage::Repos(pick) => {
            let title = format!(
                "Workspace {}: {}",
                shown(&pick.name),
                shown(&pick.root.display().to_string())
            );
            let rows = pick
                .repos
                .iter()
                .enumerate()
                .map(|(i, (rel, on))| {
                    let path = match rel.as_os_str().is_empty() {
                        true => "(the folder itself)".to_string(),
                        false => rel.display().to_string(),
                    };
                    let text = format!(" [{}] {} ", if *on { "x" } else { " " }, shown(&path));
                    row(text, i == pick.cursor, picked, t)
                })
                .collect();
            list_box(
                frame,
                area,
                &title,
                rows,
                &[
                    ("Enter", "open ticked"),
                    ("Space", "tick"),
                    ("a", "all"),
                    ("Esc", "back"),
                ],
                t,
            );
        }
    }
}

fn row(text: String, on: bool, picked: Style, t: &Theme) -> Line<'static> {
    if on {
        Line::styled(text, picked)
    } else {
        Line::styled(text, Style::new().fg(t.fg))
    }
}

/// A folder or workspace name can hold anything, so it is shown as
/// visible text.
fn shown(s: &str) -> String {
    let mut out = String::new();
    s.chars().for_each(|c| push_visible(c, &mut out));
    out
}

/// The box with `rows`, scrolled to keep the highlighted row in view, and
/// `keys` along its bottom.
fn list_box(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: Vec<Line<'static>>,
    keys: &[(&str, &str)],
    t: &Theme,
) {
    let panel = input::panel(area, SIZE);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let mut hints = vec![Span::raw(" ")];
    for (k, what) in keys {
        hints.push(Span::styled(k.to_string(), key));
        hints.push(Span::styled(format!(" {what}  "), dim));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" {title} "))
        .title_bottom(Line::from(hints).right_aligned())
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    let room = inner.height as usize;
    let at = rows
        .iter()
        .position(|l| l.style.bg == Some(t.acc))
        .unwrap_or(0);
    let top = (at + 1).saturating_sub(room);
    let shown: Vec<Line> = rows.into_iter().skip(top).take(room).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}
