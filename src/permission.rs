//! Permission cards: Claude asks "allow this?" and its PermissionRequest hook
//! waits here for y or n. Pure apart from `go_to_window`.

use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use crate::app::{App, Effect, Overlay};
use crate::ipc::HookReply;
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::ui::input::{InputEvent, TextInput};

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
}

impl PermissionCard {
    /// What the card shows in full: the whole command, the path, or the
    /// input as pretty JSON.
    pub fn body(&self) -> String {
        match field(&self.tool_name, &self.tool_input) {
            Some(text) => text.to_string(),
            None => serde_json::to_string_pretty(&self.tool_input).unwrap_or_default(),
        }
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
                self.set_status("Too quick: look at this request first");
            }
            KeyCode::Char('y') => return self.decide_permission(id, HookReply::Allow),
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
        let what = pc.summary.replace('\n', " ");
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
    use crate::theme::builtin_themes;

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%5", 5, "api", "/w/api", false)];
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
        a.update(Msg::Ipc {
            req_id,
            req,
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(req_id),
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

    #[test]
    fn body_shows_the_full_command_path_or_pretty_json() {
        let long = "echo ".to_string() + &"a".repeat(300);
        let c = |tool: &str, input: Value| PermissionCard {
            summary: summary(tool, &input),
            tool_name: tool.into(),
            tool_input: input,
            ..Default::default()
        };
        assert_eq!(c("Bash", json!({"command": long})).body(), long);
        assert_eq!(c("Edit", json!({"file_path": "/a"})).body(), "/a");
        assert!(
            c("WebFetch", json!({"url": "u"}))
                .body()
                .contains("\n  \"url\": \"u\"")
        );
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
