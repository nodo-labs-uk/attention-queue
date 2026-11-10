use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::input;
use crate::new_session::NewSessionDraft;
use crate::theme::Theme;
use crate::visible::push_visible;

/// The same box for the prompt and for the folder, so nothing moves
/// between them.
const SIZE: (u16, u16) = (100, 12);

/// `N`: the prompt box, or after Ctrl-O the folder box with its matches.
pub fn render(frame: &mut Frame, area: Rect, d: &NewSessionDraft, t: &Theme) {
    let Some(pick) = &d.folder else {
        let place = match (d.worktree, d.new) {
            (true, _) => "a new worktree of ",
            (false, true) => "the new folder ",
            (false, false) => "",
        };
        let title = format!(
            "New Claude session in {place}{} (empty starts it bare)",
            d.name
        );
        let tab = if d.worktree {
            "same folder"
        } else {
            "own worktree"
        };
        let more = [("Ctrl-O", "other folder"), ("Tab", tab)];
        input::render_with_keys(frame, area, &d.input, &title, "start", &more, t, SIZE);
        return;
    };
    let title = "Folder for the new session";
    let more = [("Tab", "complete"), ("↑/↓", "choose")];
    input::render_with_keys(frame, area, &pick.input, title, "use it", &more, t, SIZE);
    let rows = input::below_first_row(area, SIZE);
    if rows.height == 0 {
        return;
    }
    let dim = Style::new().fg(t.dim);
    let typed = pick.input.text();
    let (note, names) = match pick.listing() {
        None => (Line::styled("Reading…", dim), &[][..]),
        Some(l) => {
            let note = match (l.exists, typed.trim().is_empty(), l.names.is_empty()) {
                (_, true, _) => Line::raw(""),
                (false, ..) => Line::styled(
                    "Not there yet: Enter names it, and it is made when the session starts",
                    Style::new().fg(t.hi),
                ),
                (true, _, true) => Line::styled("No folders inside", dim),
                (true, _, false) => Line::raw(""),
            };
            (note, &l.names[..])
        }
    };
    let mut lines = vec![note];
    // Keeps the chosen match in view.
    let room = rows.height as usize - 1;
    let top = pick.cursor.map_or(0, |c| (c + 1).saturating_sub(room));
    for (i, name) in names.iter().enumerate().skip(top).take(room) {
        // A folder can be named anything, so its name is shown as visible text.
        let mut shown = String::from(" ");
        name.chars().for_each(|c| push_visible(c, &mut shown));
        shown.push_str("/ ");
        let style = if pick.cursor == Some(i) {
            Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.fg)
        };
        lines.push(Line::styled(shown, style));
    }
    frame.render_widget(Paragraph::new(lines), rows);
}
