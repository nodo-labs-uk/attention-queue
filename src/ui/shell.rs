//! The shell change card: the command, and the diff of what it changed.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{card_header, diffview, tree};
use crate::app::App;
use crate::highlight::Highlighter;
use crate::model::Card;
use crate::shell::ShellCard;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: &Card,
    sc: &ShellCard,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let mut head = card_header(app, card);
    let mut lines = sc.command.lines();
    let sigil = if sc.outside { "" } else { "$ " };
    let mut title = format!("{sigil}{}", lines.next().unwrap_or_default());
    if lines.next().is_some() {
        title.push_str(" ...");
    }
    head.push(Line::from(Span::styled(
        title,
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
    )));
    if let Some(w) = sc.warning() {
        head.push(Line::styled(
            w,
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    head.push(Line::styled(
        "Already ran. y keeps the changes; u asks Claude to undo them.",
        Style::new().fg(t.dim),
    ));
    let [head_area, body] =
        Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Fill(1)]).areas(area);
    frame.render_widget(Paragraph::new(head), head_area.inner(Margin::new(1, 0)));
    let tree_w = (body.width / 4).clamp(12, 30);
    let [tree_area, diff_area] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(body);
    tree::render(frame, tree_area, &sc.diff, t);
    diffview::render(frame, diff_area, &sc.diff, t, hl);
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::app::tests::agent;
    use crate::model::CardKind;
    use crate::queue::tests::card;
    use crate::review::tests::fixture_card;
    use crate::shell::ShellCard;
    use crate::ui::tests::{app, render};

    #[test]
    fn shows_command_and_diff() {
        let mut a: App = app();
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        let rc = fixture_card(1);
        let mut sc = ShellCard::new(
            "sed -i s/a/b/ src/limit.rs".into(),
            rc.from_tree,
            rc.to_tree,
            rc.files,
        );
        sc.diff.view = crate::review::DiffView::Unified;
        let mut c = card(1, "/w/api", CardKind::ShellChange(sc), 0);
        c.agent = a.agents[0].id.clone();
        a.queue.push(c);
        let screen = render(&a, 80, 20);
        assert!(screen.contains("shell change"), "{screen}");
        assert!(screen.contains("$ sed -i s/a/b/ src/limit.rs"), "{screen}");
        assert!(screen.contains("limit.rs"), "{screen}");
        assert!(screen.contains("u undo"), "{screen}");
    }
}
