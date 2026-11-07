//! Permission cards: Claude asks "allow this?" and its PermissionRequest hook
//! waits here for y or n. Pure apart from `go_to_window`.

use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use serde_json::Value;

use crate::app::{App, Effect, Overlay};
use crate::ipc::HookReply;
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::ui::input::{InputEvent, TextInput};
use crate::visible::{push_visible, visible_lines, wrap};

/// What `n` sends when nothing is typed.
pub const DENY_DEFAULT: &str = "Denied in aq.";
/// Longest summary, in characters.
const SUMMARY_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionCard {
    /// The hook waiting on this card.
    pub req_id: u64,
    pub tool_name: String,
    pub tool_input: Value,
    /// One line for the status message: the command, the path, or compact JSON.
    pub summary: String,
    /// `$TMUX_PANE` of the hook, for `g`.
    pub pane: Option<String>,
    /// First body row on screen.
    pub scroll: usize,
    /// How far down the body the user has had on screen, in rows.
    pub seen: usize,
}

/// How a body line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Plain,
    Del,
    Add,
}

/// The body rows and marker row the card gets on a screen of this size:
/// `(width, height)` of the body.
pub fn body_size(screen: (u16, u16)) -> (usize, usize) {
    let area = Rect::new(0, 1, screen.0, screen.1.saturating_sub(2));
    let body = areas(area)[1];
    (body.width as usize, body.height as usize)
}

/// Head, body, "more" marker and key strip of the card inside the card area.
pub fn areas(area: Rect) -> [Rect; 4] {
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area.inner(Margin::new(1, 1)))
}

impl PermissionCard {
    /// What the card shows in full, as unwrapped lines: the whole command, the
    /// path with the change as -/+ lines, or the input as pretty JSON.
    /// Control characters are made visible and long blank runs collapsed.
    pub fn body_lines(&self) -> Vec<(LineKind, String)> {
        let input = &self.tool_input;
        let str_of = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
        let mut out: Vec<(LineKind, String)> = Vec::new();
        let mut put = |kind: LineKind, mark: &str, text: &str| {
            out.extend(
                visible_lines(text)
                    .into_iter()
                    .map(|l| (kind, format!("{mark}{l}"))),
            );
        };
        // Keys of the input whose value the lines above show in full.
        let shown: std::cell::RefCell<Vec<&str>> = Default::default();
        let show = |key: &'static str,
                    kind: LineKind,
                    mark: &str,
                    put: &mut dyn FnMut(LineKind, &str, &str)| {
            if let Some(text) = str_of(input, key) {
                put(kind, mark, &text);
                shown.borrow_mut().push(key);
            }
        };
        let tool = self.tool_name.as_str();
        match tool {
            "Bash" => show("command", LineKind::Plain, "", &mut put),
            "Edit" => {
                show("file_path", LineKind::Plain, "", &mut put);
                show("old_string", LineKind::Del, "- ", &mut put);
                show("new_string", LineKind::Add, "+ ", &mut put);
            }
            "Write" => {
                show("file_path", LineKind::Plain, "", &mut put);
                show("content", LineKind::Add, "+ ", &mut put);
            }
            "MultiEdit" => {
                show("file_path", LineKind::Plain, "", &mut put);
                let edits = input.get("edits").and_then(Value::as_array);
                // Only edits made of two strings are shown as -/+ lines.
                let plain = |e: &Value| {
                    e.as_object().is_some_and(|m| {
                        m.len() == 2
                            && str_of(e, "old_string").is_some()
                            && str_of(e, "new_string").is_some()
                    })
                };
                if let Some(edits) = edits.filter(|e| e.iter().all(plain)) {
                    for e in edits {
                        put(
                            LineKind::Del,
                            "- ",
                            &str_of(e, "old_string").unwrap_or_default(),
                        );
                        put(
                            LineKind::Add,
                            "+ ",
                            &str_of(e, "new_string").unwrap_or_default(),
                        );
                    }
                    shown.borrow_mut().push("edits");
                }
            }
            "NotebookEdit" => {
                show("notebook_path", LineKind::Plain, "", &mut put);
                for key in ["cell_type", "edit_mode"] {
                    if let Some(v) = str_of(input, key) {
                        put(LineKind::Plain, &format!("{key}: "), &v);
                        shown.borrow_mut().push(key);
                    }
                }
                show("new_source", LineKind::Add, "+ ", &mut put);
            }
            _ => {}
        }
        // Whatever else the input holds, so the whole of it is on the card.
        let rest: serde_json::Map<String, Value> = match input {
            Value::Object(m) => m
                .iter()
                .filter(|(k, _)| !shown.borrow().contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            _ => serde_json::Map::new(),
        };
        let all_extra = shown.borrow().is_empty();
        if !rest.is_empty() || (all_extra && !input.is_object()) {
            let json = if all_extra {
                serde_json::to_string_pretty(input)
            } else {
                serde_json::to_string_pretty(&rest)
            }
            .unwrap_or_default();
            put(LineKind::Plain, "", &json);
        }
        out
    }

    /// `body_lines` cut into rows of at most `width` columns.
    pub fn body_rows(&self, width: usize) -> Vec<(LineKind, String)> {
        let mut rows = Vec::new();
        for (kind, line) in self.body_lines() {
            wrap(&line, width, &mut |row| rows.push((kind, row)));
        }
        rows
    }
}

/// The input field that says what the tool will do: the command of Bash, the
/// path of an edit tool.
fn field<'a>(tool: &str, input: &'a Value) -> Option<&'a str> {
    let key = match tool {
        "Bash" => "command",
        "Edit" | "Write" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    input.get(key)?.as_str()
}

/// Bash: the command; edit tools: the path; others: compact JSON; at most 200 characters.
pub fn summary(tool: &str, input: &Value) -> String {
    let text = field(tool, input).map_or_else(|| input.to_string(), str::to_string);
    text.chars().take(SUMMARY_MAX).collect()
}

/// The fields of `HookRequest::Permission`.
#[derive(Debug)]
pub struct PermissionRequest {
    pub session_id: String,
    pub cwd: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub pane: Option<String>,
}

/// `n` on a permission card: typing what Claude reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyDraft {
    pub card: u64,
    pub input: TextInput,
}

impl App {
    /// A permission request becomes a card for the agent in its pane, else
    /// the one with its session; with neither, the window shows as "?".
    pub(crate) fn on_permission(
        &mut self,
        req_id: u64,
        at: SystemTime,
        r: PermissionRequest,
    ) -> Vec<Effect> {
        let agent = self
            .agents
            .iter()
            .find(|a| r.pane.as_deref() == Some(a.id.pane.as_str()))
            .or_else(|| self.agents.iter().find(|a| a.session_id == r.session_id));
        let checkout = agent
            .and_then(|a| a.checkout.clone())
            .unwrap_or_else(|| Checkout(r.cwd.clone().into()));
        let agent = agent.map_or_else(
            || AgentId {
                pane: "?".into(),
                claude_pid: 0,
            },
            |a| a.id.clone(),
        );
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout,
            agent,
            created: at,
            kind: CardKind::Permission(PermissionCard {
                req_id,
                summary: summary(&r.tool_name, &r.tool_input),
                tool_name: r.tool_name,
                tool_input: r.tool_input,
                pane: r.pane,
                scroll: 0,
                seen: 0,
            }),
        });
        Vec::new()
    }

    /// Removes the card whose hook went away. Returns whether there was one.
    pub(crate) fn close_permission(&mut self, req_id: u64) -> bool {
        let Some(id) = self
            .queue
            .iter()
            .find(|c| matches!(&c.kind, CardKind::Permission(p) if p.req_id == req_id))
            .map(|c| c.id)
        else {
            return false;
        };
        self.queue.remove(id);
        if matches!(&self.overlay, Some(Overlay::Deny(d)) if d.card == id) {
            self.overlay = None;
        }
        self.set_status("Permission request closed: answered in the window or timed out");
        true
    }

    pub(crate) fn on_permission_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let CardKind::Permission(pc) = &card.kind else {
            return Vec::new();
        };
        let id = card.id;
        match key.code {
            KeyCode::Char('y' | 'n') if !self.looked_at(id) => {
                self.too_quick("request");
            }
            KeyCode::Char('y') if !self.read_to_end() => {
                self.set_status("Scroll to the end before allowing");
            }
            KeyCode::Char('y') => return self.decide_permission(id, HookReply::Allow),
            KeyCode::Char('j' | 'k')
            | KeyCode::Down
            | KeyCode::Up
            | KeyCode::PageDown
            | KeyCode::PageUp => {
                self.scroll_permission(key.code);
            }
            KeyCode::Char('n') => {
                self.overlay = Some(Overlay::Deny(DenyDraft {
                    card: id,
                    input: TextInput::single_line(),
                }));
            }
            KeyCode::Char('g') => {
                let known = (card.agent.pane != "?").then(|| card.agent.pane.clone());
                match pc.pane.clone().or(known) {
                    Some(pane) => return vec![Effect::GoToWindow { pane }],
                    None => self.set_status("No window known for this request"),
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Whether the whole body of the current permission card has been on
    /// screen: it fits, or it was scrolled to the end.
    fn read_to_end(&self) -> bool {
        let Some(Card {
            kind: CardKind::Permission(pc),
            ..
        }) = self.queue.current()
        else {
            return false;
        };
        let Some((width, height)) = self.screen.map(body_size) else {
            return false;
        };
        let total = pc.body_rows(width).len();
        pc.seen.max((pc.scroll + height).min(total)) >= total && (height > 0 || total == 0)
    }

    fn scroll_permission(&mut self, code: KeyCode) {
        let Some((width, height)) = self.screen.map(body_size) else {
            return;
        };
        let Some(Card {
            kind: CardKind::Permission(pc),
            ..
        }) = self.queue.current_mut()
        else {
            return;
        };
        let total = pc.body_rows(width).len();
        let last = total.saturating_sub(height);
        let page = height.max(1);
        let top = pc.scroll.min(last);
        pc.scroll = match code {
            KeyCode::Char('j') | KeyCode::Down => top + 1,
            KeyCode::PageDown => top + page,
            KeyCode::Char('k') | KeyCode::Up => top.saturating_sub(1),
            _ => top.saturating_sub(page),
        }
        .min(last);
        pc.seen = pc.seen.max((pc.scroll + height).min(total));
    }

    pub(crate) fn on_deny_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Deny(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let text = match draft.input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => {
                return {
                    self.overlay = None;
                    Vec::new()
                };
            }
            InputEvent::Submit(text) => text,
        };
        let Some(Overlay::Deny(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        let text = text.trim();
        let reason = if text.is_empty() { DENY_DEFAULT } else { text };
        self.decide_permission(
            draft.card,
            HookReply::Deny {
                reason: reason.into(),
            },
        )
    }

    fn decide_permission(&mut self, id: u64, decision: HookReply) -> Vec<Effect> {
        let Some(Card {
            kind: CardKind::Permission(pc),
            ..
        }) = self.queue.remove(id)
        else {
            return Vec::new();
        };
        let verb = if decision == HookReply::Allow {
            "Allowed"
        } else {
            "Denied"
        };
        let mut what = String::new();
        pc.summary
            .replace('\n', " ")
            .chars()
            .for_each(|c| push_visible(c, &mut what));
        self.set_status(format!("{verb} {}: {what}", pc.tool_name));
        vec![Effect::Reply {
            req_id: pc.req_id,
            decision,
        }]
    }
}

/// Switches tmux to the window of `pane` and selects it. `server` are the
/// tmux arguments that pick the server; empty means the default one.
/// Switching the client is best effort: it fails when no client is attached.
pub fn go_to_window(pane: &str, server: &[&str]) -> Result<()> {
    if pane.trim().is_empty() {
        bail!("no pane to go to");
    }
    let tmux = |args: &[&str]| -> Result<()> {
        let out = Command::new("tmux")
            .args(server)
            .args(args)
            .output()
            .context("running tmux")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    };
    let _ = tmux(&["switch-client", "-t", pane]);
    tmux(&["select-window", "-t", pane])?;
    tmux(&["select-pane", "-t", pane])
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime};

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use serde_json::json;

    use super::*;
    use crate::app::tests::agent;
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::ipc::{HookReply, HookRequest};
    use crate::model::CardKind;
    use crate::queue::tests::card;
    use crate::step::PREEMPT_FOR;
    use crate::theme::builtin_themes;

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%5", 5, "api", "/w/api", false)];
        a.update(Msg::Resize {
            width: 80,
            height: 24,
        });
        a
    }

    fn ask(tool: &str, input: Value, pane: Option<&str>, session: &str) -> HookRequest {
        HookRequest::Permission {
            session_id: session.into(),
            cwd: "/w/api".into(),
            tool_name: tool.into(),
            tool_input: input,
            pane: pane.map(Into::into),
        }
    }

    fn ipc(a: &mut App, req_id: u64, req: HookRequest) -> Vec<Effect> {
        ipc_at(a, req_id, req, Instant::now())
    }

    fn ipc_at(a: &mut App, req_id: u64, req: HookRequest, now: Instant) -> Vec<Effect> {
        a.update(Msg::Ipc {
            req_id,
            req,
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(req_id),
            now,
        })
    }

    fn key(c: char, at: Instant) -> Msg {
        Msg::Key(
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: crossterm::event::KeyEventState::NONE,
            },
            at,
        )
    }

    fn perm(a: &App) -> &PermissionCard {
        match &a.queue.current().unwrap().kind {
            CardKind::Permission(p) => p,
            k => panic!("{k:?}"),
        }
    }

    /// An app showing a Bash permission card that has been on screen long enough.
    fn seen() -> (App, Instant) {
        let mut a = app();
        let t0 = Instant::now();
        ipc(
            &mut a,
            1,
            ask("Bash", json!({"command": "ls"}), Some("%5"), "s5"),
        );
        a.update(Msg::Tick(t0));
        (a, t0 + Duration::from_millis(400))
    }

    #[test]
    fn bash_summary_is_command() {
        let cmd = "cargo test --all";
        assert_eq!(summary("Bash", &json!({"command": cmd})), cmd);
        let long = "x".repeat(500);
        assert_eq!(
            summary("Bash", &json!({"command": long})).chars().count(),
            200
        );
    }

    #[test]
    fn edit_summary_is_path_and_others_are_compact_json() {
        assert_eq!(
            summary("Edit", &json!({"file_path": "/a/b.rs", "old_string": "x"})),
            "/a/b.rs"
        );
        assert_eq!(
            summary("Write", &json!({"file_path": "/a/c.rs"})),
            "/a/c.rs"
        );
        assert_eq!(
            summary("NotebookEdit", &json!({"notebook_path": "/a/n.ipynb"})),
            "/a/n.ipynb"
        );
        assert_eq!(
            summary("WebFetch", &json!({"url": "http://x"})),
            r#"{"url":"http://x"}"#
        );
        let big = json!({"k": "y".repeat(500)});
        assert_eq!(summary("Other", &big).chars().count(), 200);
    }

    #[test]
    fn permission_card_goes_first() {
        let mut a = app();
        a.queue
            .push(card(1, "/w/api", CardKind::Step(Box::default()), 500));
        ipc(
            &mut a,
            9,
            ask("Bash", json!({"command": "ls"}), Some("%5"), "s5"),
        );
        let first = a.queue.current().unwrap();
        assert!(matches!(first.kind, CardKind::Permission(_)));
        assert_eq!(first.agent.pane, "%5");
        assert_eq!(perm(&a).req_id, 9);
        assert_eq!(perm(&a).summary, "ls");
    }

    #[test]
    fn a_card_that_jumps_ahead_ignores_decision_keys_for_a_while() {
        use crate::step::tests::{input, pending};
        let mut a = app();
        a.config
            .set_mode(&Checkout("/w/api".into()), crate::model::Mode::Step);
        let t0 = Instant::now();
        a.update(Msg::PreTool {
            req_id: 1,
            edit: pending("s1", "/w/api", input()),
            at: t0,
        });
        assert!(matches!(a.queue.current().unwrap().kind, CardKind::Step(_)));
        // The user is on the step card and about to press `y` when a
        // permission request takes the screen.
        let t1 = t0 + Duration::from_secs(5);
        ipc_at(
            &mut a,
            2,
            ask("Bash", json!({"command": "rm -rf /"}), Some("%5"), "s5"),
            t1,
        );
        assert_eq!(perm(&a).req_id, 2);
        assert!(a.preempted(), "the bar flags the new card");
        assert!(
            a.update(key('y', t1 + Duration::from_millis(400)))
                .is_empty()
        );
        assert_eq!(a.queue.len(), 2, "neither card decided");
        assert!(
            a.status_text().unwrap().contains("New card"),
            "{:?}",
            a.status_text()
        );
        assert!(
            a.update(key('n', t1 + Duration::from_millis(1400)))
                .is_empty()
        );
        assert!(a.overlay.is_none());
        let late = t1 + PREEMPT_FOR;
        a.update(Msg::Tick(late));
        assert!(!a.preempted(), "the flag goes after a while");
        assert_eq!(
            a.update(key('y', late)),
            [Effect::Reply {
                req_id: 2,
                decision: HookReply::Allow
            }]
        );
        // Back on the step card by the user's own key: the usual short look.
        let back = late + Duration::from_millis(300);
        assert!(!a.preempted());
        assert_eq!(a.update(key('y', back)).len(), 1);
    }

    #[test]
    fn closed_hook_removes_card() {
        let (mut a, _) = seen();
        assert_eq!(a.queue.len(), 1);
        a.update(Msg::HookGone { req_id: 1 });
        assert!(a.queue.is_empty());
    }

    #[test]
    fn card_matches_by_pane_then_session_then_shows_unknown() {
        let mut a = app();
        ipc(&mut a, 1, ask("Bash", json!({}), Some("%5"), "other"));
        assert_eq!(a.queue.current().unwrap().agent.pane, "%5");
        a.queue.remove(a.queue.current().unwrap().id);
        // The agent helper names session ids after the window: "s5".
        ipc(&mut a, 2, ask("Bash", json!({}), Some("%99"), "s5"));
        assert_eq!(a.queue.current().unwrap().agent.pane, "%5");
        a.queue.remove(a.queue.current().unwrap().id);
        ipc(&mut a, 3, ask("Bash", json!({}), Some("%99"), "nobody"));
        let c = a.queue.current().unwrap();
        assert_eq!(c.agent.pane, "?");
        assert_eq!(perm(&a).pane.as_deref(), Some("%99"));
        assert!(crate::ui::window_of(&a, c) == "?");
    }

    #[test]
    fn y_allows_after_the_card_was_seen_and_not_before() {
        let (mut a, later) = seen();
        // Too early: the card appeared at t0 and 100 ms is not enough.
        let early = later - Duration::from_millis(300);
        assert!(a.update(key('y', early)).is_empty());
        assert_eq!(a.queue.len(), 1);
        assert!(a.status_text().unwrap().contains("Too quick"));
        assert_eq!(
            a.update(key('y', later)),
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow
            }]
        );
        assert!(a.queue.is_empty());
    }

    #[test]
    fn n_asks_for_a_reason_and_sends_it_or_the_default() {
        for (typed, want) in [("too risky", "too risky"), ("", "Denied in aq.")] {
            let (mut a, later) = seen();
            assert!(a.update(key('n', later)).is_empty());
            assert!(matches!(a.overlay, Some(Overlay::Deny(_))));
            for c in typed.chars() {
                a.update(key(c, later));
            }
            let fx = a.update(Msg::Key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                later,
            ));
            assert_eq!(
                fx,
                [Effect::Reply {
                    req_id: 1,
                    decision: HookReply::Deny {
                        reason: want.into()
                    }
                }]
            );
            assert!(a.queue.is_empty());
            assert!(a.overlay.is_none());
        }
    }

    #[test]
    fn n_too_early_opens_nothing_and_esc_keeps_the_card() {
        let (mut a, later) = seen();
        a.update(key('n', later - Duration::from_millis(300)));
        assert!(a.overlay.is_none());
        a.update(key('n', later));
        a.update(Msg::Key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            later,
        ));
        assert!(a.overlay.is_none());
        assert_eq!(a.queue.len(), 1);
    }

    #[test]
    fn hook_gone_closes_the_reason_box_for_that_card() {
        let (mut a, later) = seen();
        a.update(key('n', later));
        a.update(Msg::HookGone { req_id: 1 });
        assert!(a.overlay.is_none());
    }

    #[test]
    fn g_goes_to_the_window_and_keeps_the_card() {
        let (mut a, later) = seen();
        assert_eq!(
            a.update(key('g', later)),
            [Effect::GoToWindow { pane: "%5".into() }]
        );
        assert_eq!(a.queue.len(), 1);
    }

    #[test]
    fn g_without_any_pane_says_so() {
        let mut a = app();
        ipc(&mut a, 1, ask("Bash", json!({}), None, "nobody"));
        let t = Instant::now();
        a.update(Msg::Tick(t));
        assert!(a.update(key('g', t)).is_empty());
        assert!(a.status_text().unwrap().contains("No window"));
    }

    fn card_of(tool: &str, input: Value) -> PermissionCard {
        PermissionCard {
            summary: summary(tool, &input),
            tool_name: tool.into(),
            tool_input: input,
            ..Default::default()
        }
    }

    fn text(pc: &PermissionCard) -> Vec<String> {
        pc.body_lines().into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn body_shows_the_full_command_path_or_pretty_json() {
        let long = "echo ".to_string() + &"a".repeat(300);
        assert_eq!(
            text(&card_of("Bash", json!({"command": long}))),
            std::slice::from_ref(&long)
        );
        let rows = card_of("Bash", json!({"command": long})).body_rows(80);
        assert_eq!(rows.len(), 4);
        assert!(
            card_of("WebFetch", json!({"url": "u"}))
                .body_lines()
                .iter()
                .any(|(_, t)| t == "  \"url\": \"u\"")
        );
    }

    #[test]
    fn control_characters_render_visibly() {
        let pc = card_of("Bash", json!({"command": "a\tb\rc\u{1b}[31m\u{7f}\u{85}"}));
        assert_eq!(text(&pc), ["a→b⏎c^[[31m^?\\u{85}"]);
    }

    #[test]
    fn blank_runs_collapse() {
        let pc = card_of("Bash", json!({"command": "a\n\n\n\n   \nb\n\nc"}));
        assert_eq!(text(&pc), ["a", "[4 blank lines]", "b", "", "c"]);
    }

    #[test]
    fn edit_shows_the_change_as_minus_and_plus_lines() {
        let pc = card_of(
            "Edit",
            json!({"file_path": "/a/b.rs", "old_string": "x\ny", "new_string": "z"}),
        );
        assert_eq!(text(&pc), ["/a/b.rs", "- x", "- y", "+ z"]);
        let kinds: Vec<LineKind> = pc.body_lines().iter().map(|l| l.0).collect();
        assert_eq!(
            kinds,
            [LineKind::Plain, LineKind::Del, LineKind::Del, LineKind::Add]
        );
        let w = card_of("Write", json!({"file_path": "/f", "content": "hi"}));
        assert_eq!(text(&w), ["/f", "+ hi"]);
        let m = card_of(
            "MultiEdit",
            json!({"file_path": "/f", "edits": [{"old_string": "a", "new_string": "b"}]}),
        );
        assert_eq!(text(&m), ["/f", "- a", "+ b"]);
    }

    /// An app on a 11-row screen (body of 2 rows) showing a command of `lines` lines.
    fn long_command(lines: usize) -> (App, Instant) {
        let mut a = app();
        a.update(Msg::Resize {
            width: 80,
            height: 11,
        });
        let t0 = Instant::now();
        let cmd = (0..lines)
            .map(|i| format!("echo {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        ipc(
            &mut a,
            1,
            ask("Bash", json!({"command": cmd}), Some("%5"), "s5"),
        );
        a.update(Msg::Tick(t0));
        (a, t0 + Duration::from_millis(400))
    }

    #[test]
    fn long_command_is_allowed_only_after_scrolling_to_the_end() {
        let (mut a, t) = long_command(5);
        assert_eq!(body_size((80, 11)), (78, 2));
        assert!(a.update(key('y', t)).is_empty());
        assert!(a.status_text().unwrap().contains("Scroll to the end"));
        a.update(key('j', t));
        a.update(key('j', t));
        assert!(a.update(key('y', t)).is_empty(), "rows 0-3 of 5 seen");
        a.update(key('j', t));
        assert_eq!(
            a.update(key('y', t)),
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow
            }]
        );
    }

    #[test]
    fn scrolling_back_up_keeps_what_was_seen_and_k_clamps_at_the_top() {
        let (mut a, t) = long_command(5);
        for _ in 0..5 {
            a.update(key('j', t));
        }
        for _ in 0..9 {
            a.update(key('k', t));
        }
        assert_eq!(perm(&a).scroll, 0);
        assert_eq!(a.update(key('y', t)).len(), 1);
    }

    #[test]
    fn short_command_is_allowed_at_once_but_a_hidden_screen_is_not() {
        let (mut a, t) = long_command(2);
        assert_eq!(a.update(key('y', t)).len(), 1);
        let (mut a, t) = long_command(1);
        a.update(Msg::Resize {
            width: 80,
            height: 4,
        });
        assert!(a.update(key('y', t)).is_empty(), "no body rows on screen");
    }

    #[test]
    fn widening_forgets_what_was_seen() {
        let mut a = app();
        a.update(Msg::Resize {
            width: 40,
            height: 11,
        });
        let t0 = Instant::now();
        let line = |c: char| c.to_string().repeat(60);
        let cmd = ['a', 'b', 'c', 'd'].map(line).join("\n");
        ipc(
            &mut a,
            1,
            ask("Bash", json!({"command": cmd}), Some("%5"), "s5"),
        );
        a.update(Msg::Tick(t0));
        let t = t0 + Duration::from_millis(400);
        // 8 rows at width 40, 2 on screen: scroll to rows 2-3 only.
        a.update(key('j', t));
        a.update(key('j', t));
        // At width 80 the body is 4 rows, and the top 2 are the only ones seen
        // by that count; lines c and d were never shown.
        a.update(Msg::Resize {
            width: 80,
            height: 11,
        });
        assert!(a.update(key('y', t)).is_empty());
        assert!(a.status_text().unwrap().contains("Scroll to the end"));
        a.update(key('j', t));
        a.update(key('j', t));
        assert_eq!(a.update(key('y', t)).len(), 1);
    }

    #[test]
    fn format_characters_are_spelled_out() {
        let cmd = "ls \u{202e}gnihton\u{202c}\u{200b}\u{2066}\u{feff}\u{2028}x";
        let pc = card_of("Bash", json!({"command": cmd}));
        assert_eq!(
            text(&pc),
            ["ls \\u{202e}gnihton\\u{202c}\\u{200b}\\u{2066}\\u{feff}\\u{2028}x"]
        );
    }

    #[test]
    fn invisible_characters_are_escaped_by_property() {
        for c in [
            '\u{3000}',
            '\u{2003}',
            '\u{fe0f}',
            '\u{3164}',
            '\u{2800}',
            '\u{e0100}',
            '\u{34f}',
            '\u{a0}',
            '\u{ffa0}',
        ] {
            let pc = card_of("Bash", json!({"command": format!("a{c}b")}));
            assert_eq!(text(&pc), [format!("a\\u{{{:x}}}b", c as u32)]);
        }
        let pc = card_of("Bash", json!({"command": "a b"}));
        assert_eq!(text(&pc), ["a b"]);
    }

    #[test]
    fn unassigned_and_private_use_characters_are_escaped() {
        for c in [
            '\u{e0080}',
            '\u{2065}',
            '\u{fff0}',
            '\u{180b}',
            '\u{17b4}',
            '\u{e000}',
        ] {
            let pc = card_of("Bash", json!({"command": format!("a{c}b")}));
            assert_eq!(text(&pc), [format!("a\\u{{{:x}}}b", c as u32)]);
        }
    }

    #[test]
    fn a_line_of_wide_spaces_is_not_blank() {
        let pc = card_of(
            "Bash",
            json!({"command": "a\n\u{3000}\n\u{3000}\n\u{3000}\nb\n \t \n \n \nc"}),
        );
        assert_eq!(
            text(&pc),
            [
                "a",
                "\\u{3000}",
                "\\u{3000}",
                "\\u{3000}",
                "b",
                "[3 blank lines]",
                "c"
            ]
        );
    }

    #[test]
    fn notebook_edit_shows_source_and_mode() {
        let pc = card_of(
            "NotebookEdit",
            json!({"notebook_path": "/n.ipynb", "new_source": "a = 1\nb = 2", "cell_type": "code", "edit_mode": "insert"}),
        );
        assert_eq!(
            text(&pc),
            [
                "/n.ipynb",
                "cell_type: code",
                "edit_mode: insert",
                "+ a = 1",
                "+ b = 2"
            ]
        );
    }

    #[test]
    fn unknown_input_keys_are_listed_below_the_body() {
        let pc = card_of(
            "Edit",
            json!({"file_path": "/f", "old_string": "a", "new_string": "b", "sneaky": "rm -rf /"}),
        );
        let t = text(&pc);
        assert_eq!(&t[..3], ["/f", "- a", "+ b"]);
        assert!(t.iter().any(|l| l.contains("\"sneaky\": \"rm -rf /\"")));
        assert!(!t.iter().any(|l| l.contains("old_string")));
        // A non-string value is not hidden either.
        let pc = card_of("Bash", json!({"command": "ls", "timeout": 5}));
        assert!(text(&pc).iter().any(|l| l.contains("\"timeout\": 5")));
        // A MultiEdit with an extra field on an edit shows the edits as JSON.
        let pc = card_of(
            "MultiEdit",
            json!({"file_path": "/f", "edits": [{"old_string": "a", "new_string": "b", "replace_all": true}]}),
        );
        assert!(text(&pc).iter().any(|l| l.contains("replace_all")));
    }

    #[test]
    fn empty_pane_is_refused() {
        assert!(go_to_window("", &[]).is_err());
    }

    #[test]
    fn go_to_window_selects_the_pane_on_a_private_server() {
        use std::process::Command;
        let name = format!("aqtest-{}", std::process::id());
        let tmux = |args: &[&str]| {
            let out = Command::new("tmux")
                .args(["-L", &name])
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        tmux(&["new-session", "-d", "-s", "t", "-x", "80", "-y", "24"]);
        let first = tmux(&["display-message", "-p", "-t", "t", "#{pane_id}"]);
        tmux(&["new-window", "-t", "t"]);
        let before = tmux(&["display-message", "-p", "-t", "t", "#{pane_id}"]);
        let result = go_to_window(&first, &["-L", &name]);
        let active = tmux(&["display-message", "-p", "-t", "t", "#{pane_id}"]);
        tmux(&["kill-server"]);
        result.unwrap();
        assert_ne!(before, first, "the second window starts active");
        assert_eq!(active, first);
    }
}
