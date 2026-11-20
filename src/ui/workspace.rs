use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::new_session::render_folder;
use super::window::{self, Height};
use crate::config::Workspace;
use crate::theme::Theme;
use crate::visible::push_visible;
use crate::workspace::{Stage, WorkspaceDraft};

/// `O`: the saved workspaces, the folder, or the repos to tick, in one tall
/// window, so nothing moves between them.
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
                &[("Enter", "open"), ("Ctrl-O", "other folder")],
                t,
            );
        }
        Stage::Folder(pick) => render_folder(
            frame,
            area,
            pick,
            "Folder holding the workspace's git repos",
            "Not there: type a folder that is",
            Height::Tall,
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

pub(crate) fn row(text: String, on: bool, picked: Style, t: &Theme) -> Line<'static> {
    if on {
        Line::styled(text, picked)
    } else {
        Line::styled(text, Style::new().fg(t.fg))
    }
}

/// A folder or workspace name can hold anything, so it is shown as
/// visible text.
pub(crate) fn shown(s: &str) -> String {
    let mut out = String::new();
    s.chars().for_each(|c| push_visible(c, &mut out));
    out
}

/// A tall window with `rows`, scrolled to keep the highlighted row in
/// view, and `keys` along its bottom.
pub(crate) fn list_box(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: Vec<Line<'static>>,
    keys: &[(&str, &str)],
    t: &Theme,
) {
    let panel = window::place(area, Height::Tall);
    let inner = window::draw(frame, panel, title, keys, t);
    let room = inner.height as usize;
    let at = rows
        .iter()
        .position(|l| l.style.bg == Some(t.acc))
        .unwrap_or(0);
    let top = (at + 1).saturating_sub(room);
    let shown: Vec<Line> = rows.into_iter().skip(top).take(room).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}
