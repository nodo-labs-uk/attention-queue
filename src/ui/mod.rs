mod ask;
mod bar;
mod choice;
mod confirm;
mod diffview;
pub mod empty;
pub mod fold;
pub mod help;
pub mod input;
mod keys;
mod new_session;
mod permission;
mod question;
mod quiz;
mod review;
mod shell;
mod step;
mod tour;
mod tree;
mod workspace;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;

use crate::app::{
    App, Overlay, Selection, Unit, mode_label, project_name, shared_checkout_warning,
};
use crate::highlight::Highlighter;
use crate::model::{Card, CardKind};

/// The text of `buf` under `sel`, either way round: the rest of the first
/// row, whole rows between, the start of the last, as a terminal selects.
/// Trailing spaces are dropped from each row.
pub fn selected_text(buf: &Buffer, sel: Selection) -> String {
    let (a, b) = span(buf, sel);
    let area = buf.area;
    let mut rows = Vec::new();
    for y in a.1..=b.1.min(area.bottom().saturating_sub(1)) {
        let first = if y == a.1 { a.0 } else { area.left() };
        let last = if y == b.1 {
            b.0
        } else {
            area.right().saturating_sub(1)
        };
        let mut row = String::new();
        for x in first..=last.min(area.right().saturating_sub(1)) {
            row.push_str(buf[(x, y)].symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

/// `from` and `to` with the earlier one (by row, then column) first.
fn order(from: (u16, u16), to: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if (from.1, from.0) <= (to.1, to.0) {
        (from, to)
    } else {
        (to, from)
    }
}

/// Characters that end a word for a double click: Ghostty's defaults.
const WORD_BREAKS: &str = "'\"│`|:;,()[]{}<>$";

fn breaks_word(buf: &Buffer, x: u16, y: u16) -> bool {
    let s = buf[(x, y)].symbol();
    s.trim().is_empty() || (s.chars().count() == 1 && WORD_BREAKS.contains(s))
}

/// Where `sel` starts and ends on screen, earlier end first, once each end
/// has grown to its whole word or line.
fn span(buf: &Buffer, sel: Selection) -> ((u16, u16), (u16, u16)) {
    let (mut a, mut b) = order(sel.from, sel.to);
    let area = buf.area;
    let (left, right) = (area.left(), area.right().saturating_sub(1));
    match sel.unit {
        Unit::Cell => {}
        Unit::Line => {
            a.0 = left;
            b.0 = right;
        }
        Unit::Word => {
            // A click on a space or a break takes just that cell.
            if !breaks_word(buf, a.0, a.1) {
                while a.0 > left && !breaks_word(buf, a.0 - 1, a.1) {
                    a.0 -= 1;
                }
            }
            if !breaks_word(buf, b.0, b.1) {
                while b.0 < right && !breaks_word(buf, b.0 + 1, b.1) {
                    b.0 += 1;
                }
            }
        }
    }
    (a, b)
}

/// The selection drawn reversed over whatever is under it.
fn draw_selection(frame: &mut Frame, sel: Selection) {
    let buf = frame.buffer_mut();
    let (a, b) = span(buf, sel);
    let area = buf.area;
    for y in a.1..=b.1.min(area.bottom().saturating_sub(1)) {
        let first = if y == a.1 { a.0 } else { area.left() };
        let last = if y == b.1 {
            b.0
        } else {
            area.right().saturating_sub(1)
        };
        for x in first..=last.min(area.right().saturating_sub(1)) {
            buf[(x, y)].modifier.toggle(Modifier::REVERSED);
        }
    }
}

pub fn draw(frame: &mut Frame, app: &App, hl: &mut Highlighter) {
    let t = app.theme();
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), area);
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(keys::height(app, area.width, area.height)),
    ])
    .areas(area);
    bar::render(frame, top, app);
    match app.queue.current() {
        Some(card) => render_card(frame, body, app, card, hl),
        None => empty::render(frame, body, app),
    }
    keys::render(frame, bottom, app);
    match &app.overlay {
        Some(Overlay::Ask(ask)) => ask::render(frame, body, ask, app.theme()),
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
        Some(Overlay::OwnAnswer(d)) => {
            input::render(frame, body, &d.input, "Answer in your own words", t);
        }
        Some(Overlay::Deny(d)) => {
            input::render(frame, body, &d.input, "Deny: tell Claude why", t);
        }
        Some(Overlay::Search(list)) => step::render_search(frame, body, list, t),
        Some(Overlay::QuizAnswer(d)) => quiz::render_answer(frame, body, app, d, hl),
        Some(Overlay::QuizCode { card, question }) => {
            quiz::render_code(frame, body, app, *card, *question, hl);
        }
        Some(Overlay::Help { scroll }) => help::render(frame, body, *scroll, t),
        Some(Overlay::Tour { page }) => tour::render(frame, body, *page, t),
        Some(Overlay::CloseAgent(id)) => {
            let name = app
                .agent(id)
                .map_or_else(|| id.pane.clone(), |a| a.window_name.clone());
            confirm::render(
                frame,
                body,
                "Close session",
                &[
                    format!("Close {name}? Its Claude session ends."),
                    "Its conversation is kept: claude --resume picks it up.".into(),
                ],
                &[("y", "yes"), ("n", "no")],
                t,
            );
        }
        Some(Overlay::Trust) => {
            if let Some((_, dir)) = app.pending_trust.front() {
                let n = app.pending_trust.len();
                let title = match n {
                    1 => "Trust this folder?".to_string(),
                    n => format!("Trust this folder? 1 of {n}"),
                };
                let mut hints = vec![("y", "yes"), ("n", "no")];
                if n > 1 {
                    hints.push(("a", "all"));
                }
                confirm::render(
                    frame,
                    body,
                    &title,
                    &[
                        "Claude Code starts here for the first time:".into(),
                        dir.display().to_string(),
                        "It will be able to read, edit and run files in it.".into(),
                        "No closes the new session.".into(),
                    ],
                    &hints,
                    t,
                );
            }
        }
        Some(Overlay::Model(id)) => {
            let name = app
                .agent(id)
                .map_or_else(|| id.pane.clone(), |a| a.window_name.clone());
            let keys: Vec<String> = crate::app::MODELS
                .iter()
                .map(|(k, _)| k.to_string())
                .collect();
            let hints: Vec<(&str, &str)> = keys
                .iter()
                .zip(crate::app::MODELS)
                .map(|(k, (_, model))| (k.as_str(), model))
                .collect();
            confirm::render(
                frame,
                body,
                &format!("Model for {name}"),
                &[
                    "Sends /model to the session. Claude Code also keeps it".into(),
                    "as the default for new sessions. A session with history".into(),
                    "re-reads it on the new model. Esc changes nothing.".into(),
                ],
                &hints,
                t,
            );
        }
        Some(Overlay::PermissionMode(id)) => {
            let name = app
                .agent(id)
                .map_or_else(|| id.pane.clone(), |a| a.window_name.clone());
            let keys: Vec<String> = crate::app::PERMISSION_KEYS
                .iter()
                .map(|(k, _)| k.to_string())
                .collect();
            let hints: Vec<(&str, &str)> = keys
                .iter()
                .zip(crate::app::PERMISSION_KEYS)
                .map(|(k, (_, mode))| (k.as_str(), mode))
                .collect();
            confirm::render(
                frame,
                body,
                &format!("Permission mode for {name}"),
                &[
                    "Presses Shift+Tab in the session until its footer shows".into(),
                    "the mode, as you would. Bypass permissions is left out.".into(),
                    "Esc changes nothing.".into(),
                ],
                &hints,
                t,
            );
        }
        Some(Overlay::Prompt(d)) => {
            let name = app
                .agent(&d.agent)
                .map_or_else(|| d.agent.pane.clone(), |a| a.window_name.clone());
            let title = match &d.group {
                Some(group) => format!("Message every session in {}", empty::visible(group)),
                None => format!("Message to {name}"),
            };
            let ghost = d.ghost();
            let more: &[(&str, &str)] = if ghost.is_some() {
                &[("Tab", "suggestion")]
            } else {
                &[]
            };
            input::render_with_ghost(
                frame,
                body,
                &d.input,
                &title,
                "send",
                more,
                ghost,
                t,
                (100, 12),
            );
        }
        Some(Overlay::NewSession(d)) => new_session::render(frame, body, d, t),
        Some(Overlay::Workspace(d)) => workspace::render(frame, body, d, &app.config.workspaces, t),
        None => {}
    }
    if let Some(step) = app.practice_step() {
        tour::render_coach(frame, body, step, app.theme());
    }
    if let Some(s) = app.selection {
        draw_selection(frame, s);
    }
}

pub fn kind_label(kind: &CardKind) -> &'static str {
    match kind {
        CardKind::Permission(_) => "permission",
        CardKind::Question(_) | CardKind::Choice(_) => "question",
        CardKind::Tell(_) => "message",
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
    if app.held.contains_key(&card.id) {
        lines.push(Line::styled(
            crate::app::QUEUED_STATUS,
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(q) = app.question_of(&card.agent) {
        // Sending from this card answers it; its own card has it in full.
        lines.push(Line::styled(
            format!("Claude asks: {}", q.ask_line()),
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(w) = shared_checkout_warning(card, &app.agents) {
        lines.push(Line::styled(w, Style::new().fg(t.hi)));
    }
    lines
}

fn render_card(frame: &mut Frame, area: Rect, app: &App, card: &Card, hl: &mut Highlighter) {
    match &card.kind {
        CardKind::Review(rc) => review::render(frame, area, app, card, rc, hl),
        CardKind::Step(sc) => step::render(frame, area, app, card, sc, hl),
        CardKind::ShellChange(sc) => shell::render(frame, area, app, card, sc, hl),
        CardKind::Permission(pc) => permission::render(frame, area, app, card, pc),
        CardKind::Question(qc) => question::render(frame, area, app, card, qc),
        CardKind::Tell(tc) => question::render_tell(frame, area, app, card, tc),
        CardKind::Choice(cc) => choice::render(frame, area, app, card, cc),
        CardKind::Quiz(qc) => quiz::render(frame, area, app, card, qc, hl),
    }
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
    fn a_selection_reads_like_a_terminal_selection() {
        let buf =
            ratatui::buffer::Buffer::with_lines(["first row   ", "second row  ", "third row   "]);
        let text = |from, to| super::selected_text(&buf, sel(from, to, Unit::Cell));
        // Rest of the first row, all of the middle one, start of the last.
        assert_eq!(text((6, 0), (4, 2)), "row\nsecond row\nthird");
        // Dragged backwards, the same text.
        assert_eq!(text((4, 2), (6, 0)), "row\nsecond row\nthird");
        assert_eq!(text((0, 1), (5, 1)), "second");
    }

    fn sel(from: (u16, u16), to: (u16, u16), unit: Unit) -> Selection {
        Selection { from, to, unit }
    }

    #[test]
    fn a_double_click_takes_the_word_and_a_triple_click_the_line() {
        let buf = ratatui::buffer::Buffer::with_lines([
            "│ src/app.rs:42 (ok)  ",
            "  let x = foo_bar(1); ",
        ]);
        let word = |x, y| super::selected_text(&buf, sel((x, y), (x, y), Unit::Word));
        // A path stops at the colon and the frame's bar, as in Ghostty.
        assert_eq!(word(5, 0), "src/app.rs");
        assert_eq!(word(13, 0), "42");
        assert_eq!(word(17, 0), "ok");
        assert_eq!(word(12, 1), "foo_bar");
        // On a space or a break, just that cell.
        assert_eq!(word(15, 0), "");
        assert_eq!(word(12, 0), ":");
        // Dragged on after a double click: whole words at both ends.
        assert_eq!(
            super::selected_text(&buf, sel((3, 1), (10, 1), Unit::Word)),
            "let x = foo_bar"
        );
        // Triple: the whole row, its indent kept and trailing spaces gone.
        assert_eq!(
            super::selected_text(&buf, sel((9, 1), (9, 1), Unit::Line)),
            "  let x = foo_bar(1);"
        );
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
        let mut perm = card(
            4,
            "/w/api",
            CardKind::Permission(PermissionCard {
                tool_name: "Bash".into(),
                tool_input: serde_json::json!({"command": "ls"}),
                summary: "ls".into(),
                ..Default::default()
            }),
            5,
        );
        perm.agent = a.agents[0].id.clone();
        a.queue.push(perm);
        insta::assert_snapshot!(render(&a, 90, 10));
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
    fn empty_state_tabs_flow_onto_more_rows_when_narrow() {
        let mut a = app();
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 4, "chezmoi", "/home/u/.local/share/chezmoi", false),
            agent("%3", 12, "web-redesign", "/w/web", true),
        ];
        insta::assert_snapshot!(render(&a, 34, 8));
    }

    #[test]
    fn empty_state_shows_the_picked_agents_screen() {
        let mut a = app();
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", false),
        ];
        let mut screen: Vec<String> = (1..=18).map(|i| format!("line {i}")).collect();
        screen.push(format!("long {}", "word ".repeat(15)));
        a.preview = Some(crate::app::Preview {
            pane: "%1".into(),
            text: Ok(screen.join("\n") + "\n> \n\n"),
        });
        insta::assert_snapshot!(render(&a, 60, 16));
    }

    #[test]
    fn scrolling_up_shows_the_conversation_and_g_comes_back() {
        use crate::app::{Effect, Msg};
        use crate::transcript::Said;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        a.update(Msg::Resize {
            width: 60,
            height: 16,
        });
        a.preview = Some(crate::app::Preview {
            pane: "%1".into(),
            text: Ok("live pane text".into()),
        });
        let key = |code, m| Msg::Key(KeyEvent::new(code, m), std::time::Instant::now());
        let effects = a.update(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            effects,
            [Effect::ReadConversation {
                session_id: "s1".into()
            }]
        );
        assert!(render(&a, 60, 16).contains("Reading the conversation"));
        let mut said = Vec::new();
        for i in 1..=30 {
            said.push(Said::User(format!("prompt {i}")));
            said.push(Said::Claude(format!("reply {i}")));
            said.push(Said::Tool(format!("Bash(step {i})")));
        }
        a.update(Msg::Conversation {
            session_id: "s1".into(),
            said: Ok(said),
        });
        let screen = render(&a, 60, 16);
        assert!(
            screen.contains("reply 30") && screen.contains("⎿ Bash(step 30)"),
            "{screen}"
        );
        assert!(!screen.contains("live pane text"), "{screen}");
        a.update(Msg::Wheel { up: true });
        let screen = render(&a, 60, 16);
        assert!(!screen.contains("step 30)"), "{screen}");
        assert!(screen.contains("3 more rows"), "{screen}");
        a.update(key(KeyCode::Home, KeyModifiers::NONE));
        assert!(render(&a, 60, 16).contains("❯ prompt 1"), "the top");
        a.update(key(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(a.live_scroll, 0);
        assert!(render(&a, 60, 16).contains("live pane text"));
        assert_eq!(a.picked_agent().unwrap().id.pane, "%1", "the pick stays");
    }

    #[test]
    fn empty_state_without_agents() {
        insta::assert_snapshot!(render(&app(), 50, 7));
    }
}
