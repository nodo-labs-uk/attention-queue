mod bar;
mod diffview;
mod empty;
pub mod fold;
pub mod input;
mod keys;
mod review;
mod shell;
mod step;
mod tree;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::app::{App, Overlay, mode_label, project_name, shared_checkout_warning};
use crate::highlight::Highlighter;
use crate::model::{Card, CardKind};

pub fn draw(frame: &mut Frame, app: &App, hl: &mut Highlighter) {
    let t = app.theme();
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), area);
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    bar::render(frame, top, app);
    match app.queue.current() {
        Some(card) => render_card(frame, body, app, card, hl),
        None => empty::render(frame, body, app),
    }
    keys::render(frame, bottom, app);
    match &app.overlay {
        Some(Overlay::Ask) => render_ask(frame, body, app),
        Some(Overlay::Comment(d)) => {
            let target = &d.target;
            let lines = if target.end_line == target.line {
                target.line.to_string()
            } else {
                format!("{}-{}", target.line, target.end_line)
            };
            let title = format!("Comment on {}:{lines}", target.path.display());
            input::render(frame, body, &d.input, &title, t);
        }
        Some(Overlay::StepReason(d)) => {
            let title = if d.change {
                "Ask Claude for a different change"
            } else {
                "Reject: tell Claude why"
            };
            input::render(frame, body, &d.input, title, t);
        }
        Some(Overlay::Search(list)) => step::render_search(frame, body, list, t),
        None => {}
    }
}

pub fn kind_label(kind: &CardKind) -> &'static str {
    match kind {
        CardKind::Permission(_) => "permission",
        CardKind::Step(_) => "step",
        CardKind::ShellChange(_) => "shell change",
        CardKind::Review(_) => "review",
        CardKind::Quiz(_) => "quiz",
    }
}

/// Window name of the card's agent, or its pane id if the agent is gone.
pub fn window_of(app: &App, card: &Card) -> String {
    app.agent(&card.agent)
        .map_or_else(|| card.agent.pane.clone(), |a| a.window_name.clone())
}

/// Card heading shared by every card kind: what, where, which mode, and any
/// shared-checkout warning.
pub fn card_header(app: &App, card: &Card) -> Vec<Line<'static>> {
    let t = app.theme();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            kind_label(&card.kind),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::new()),
        Span::styled(project_name(&card.checkout), Style::new().fg(t.fg)),
        Span::styled(
            format!(
                "  window {}  {}",
                window_of(app, card),
                mode_label(app.config.mode_for(&card.checkout))
            ),
            Style::new().fg(t.dim),
        ),
    ])];
    if let Some(w) = shared_checkout_warning(card, &app.agents) {
        lines.push(Line::styled(w, Style::new().fg(t.hi)));
    }
    lines
}

fn render_card(frame: &mut Frame, area: Rect, app: &App, card: &Card, hl: &mut Highlighter) {
    match &card.kind {
        CardKind::Review(rc) => return review::render(frame, area, app, card, rc, hl),
        CardKind::Step(sc) => return step::render(frame, area, app, card, sc, hl),
        CardKind::ShellChange(sc) => return shell::render(frame, area, app, card, sc, hl),
        _ => {}
    }
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    frame.render_widget(Paragraph::new(card_header(app, card)), inner);
}

fn render_ask(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.theme();
    let [h] = Layout::horizontal([Constraint::Percentage(70)])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Length(5)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.line))
        .title(" Ask about this card ")
        .style(Style::new().bg(t.panel).fg(t.fg));
    frame.render_widget(
        Paragraph::new("Side questions are not available yet.")
            .style(Style::new().fg(t.dim))
            .wrap(Wrap { trim: true })
            .block(block),
        panel,
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::tests::agent;
    use crate::config::Config;
    use crate::model::{PermissionCard, ReviewCard};
    use crate::queue::tests::card;
    use crate::theme::builtin_themes;

    pub(crate) fn render(app: &App, w: u16, h: u16) -> String {
        let mut hl = Highlighter::new(app.theme());
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app, &mut hl)).unwrap();
        term.backend().to_string()
    }

    pub(crate) fn app() -> App {
        App::new(Config::default(), builtin_themes())
    }

    #[test]
    fn bar_shows_counts() {
        let mut a = app();
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "api-tests", "/w/api", false),
            agent("%3", 3, "web", "/w/web", true),
        ];
        a.queue.push(card(
            1,
            "/w/web",
            CardKind::Review(ReviewCard::default()),
            30,
        ));
        a.queue.push(card(
            2,
            "/w/web",
            CardKind::Review(ReviewCard::default()),
            20,
        ));
        a.queue
            .push(card(3, "/w/web", CardKind::Step(Box::default()), 10));
        let mut perm = card(4, "/w/api", CardKind::Permission(PermissionCard), 5);
        perm.agent = a.agents[0].id.clone();
        a.queue.push(perm);
        insta::assert_snapshot!(render(&a, 90, 6));
    }

    #[test]
    fn empty_state_lists_agents() {
        let mut a = app();
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 4, "chezmoi", "/home/u/.local/share/chezmoi", false),
            agent("%3", 12, "web-redesign", "/w/web", true),
        ];
        insta::assert_snapshot!(render(&a, 70, 12));
    }

    #[test]
    fn long_status_leaves_keys_visible() {
        let mut a = app();
        a.update(crate::app::Msg::AgentsFailed(
            "running tmux list-panes: No such file or directory (os error 2)".into(),
        ));
        let screen = render(&a, 60, 3);
        let last = screen.lines().last().unwrap();
        assert!(last.contains("Q quit"), "{last}");
        assert!(last.contains("Could not list agents"), "{last}");
    }

    #[test]
    fn empty_state_without_agents() {
        insta::assert_snapshot!(render(&app(), 50, 7));
    }
}
