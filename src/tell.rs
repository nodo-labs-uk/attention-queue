//! `aq tell`: one Claude session asks another to do something. Each
//! message is a card you approve before it is sent, so nothing crosses
//! between repos without you. Pure: no I/O.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agents::Agent;
use crate::app::{App, Effect, Overlay, PromptDraft, project_name};
use crate::ipc::HookReply;
use crate::model::{AgentId, Card, CardKind, Checkout, Mode, QuestionCard};
use crate::ui::input::TextInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TellCard {
    /// The session that asks; the card's agent is the one it asks.
    pub from: AgentId,
    pub from_name: String,
    /// The message, read as Markdown, as on a question card.
    pub message: QuestionCard,
}

/// The name a session goes by in `aq tell`: its project, which is also
/// what its tab shows.
pub fn tell_name(a: &Agent) -> String {
    match &a.checkout {
        Some(c) => project_name(c),
        None => a.cwd.file_name().map_or_else(
            || a.window_name.clone(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

/// What `aq tell` says it does, for a session's system prompt.
pub fn how_to_tell(workspace: &str, names: &[String]) -> String {
    format!(
        "You are one of several Claude Code sessions in workspace {workspace}, one per git \
         repo: {}. To ask another session to do something in its repo, run \
         `aq tell <name> \"<message>\"`; `aq tell` alone lists the sessions. The developer \
         approves each message before it is sent. Write it so the other session can act on it \
         without your context.",
        names.join(", ")
    )
}

fn note(ok: bool, text: impl Into<String>) -> HookReply {
    HookReply::Note {
        ok,
        text: text.into(),
    }
}

impl App {
    /// The sessions a session in `pane` can tell, its own tmux session's
    /// first.
    fn tellable(&self, from: &Agent) -> Vec<&Agent> {
        let mut others: Vec<&Agent> = self
            .agents_in_order()
            .into_iter()
            .filter(|a| a.id != from.id)
            .collect();
        others.sort_by_key(|a| a.session != from.session);
        others
    }

    fn sender(&self, pane: Option<&str>) -> Result<&Agent, HookReply> {
        pane.and_then(|p| self.agents.iter().find(|a| a.id.pane == p))
            .ok_or_else(|| {
                note(
                    false,
                    "aq can't tell which session this is: run aq tell from a Claude session in tmux",
                )
            })
    }

    /// `aq tell` with no arguments: who can be told.
    pub(crate) fn on_tellable(&self, pane: Option<&str>) -> HookReply {
        let from = match self.sender(pane) {
            Ok(a) => a,
            Err(reply) => return reply,
        };
        let others = self.tellable(from);
        if others.is_empty() {
            return note(true, "No other Claude sessions are running.");
        }
        let lines: Vec<String> = others
            .iter()
            .map(|a| {
                let place = if a.session == from.session {
                    "same tmux session"
                } else {
                    "tmux session "
                };
                let session = if a.session == from.session {
                    String::new()
                } else {
                    a.session.clone()
                };
                format!("{}  ({place}{session})", tell_name(a))
            })
            .collect();
        note(true, lines.join("\n"))
    }

    /// `aq tell <to> <text>`: a card for you to approve, or in vibe mode
    /// the message sent at once.
    pub(crate) fn on_tell(
        &mut self,
        pane: Option<&str>,
        to: &str,
        text: &str,
    ) -> (HookReply, Vec<Effect>) {
        let (from, from_name) = match self.sender(pane) {
            Ok(a) => (a.id.clone(), tell_name(a)),
            Err(reply) => return (reply, Vec::new()),
        };
        let text = text.trim();
        if text.is_empty() {
            return (
                note(false, "Say what to tell it: aq tell <name> \"<message>\""),
                Vec::new(),
            );
        }
        let target = match self.resolve_tell(&from, to) {
            Ok(a) => a,
            Err(e) => return (note(false, e), Vec::new()),
        };
        let to_name = tell_name(target);
        let to_id = target.id.clone();
        let checkout = target
            .checkout
            .clone()
            .unwrap_or_else(|| Checkout(target.cwd.clone()));
        if self.config.mode_for(&checkout) == Mode::Vibe {
            let effects = self.send_prompt(to_id, framed(&from_name, text));
            return (note(true, format!("Sent to {to_name}.")), effects);
        }
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout,
            agent: to_id,
            created: SystemTime::now(),
            kind: CardKind::Tell(TellCard {
                from,
                from_name,
                message: QuestionCard {
                    text: text.to_string(),
                    scroll: 0,
                },
            }),
        });
        (
            note(
                true,
                format!(
                    "Waiting for the developer to approve it in aq; it reaches {to_name} once approved. \
                     Carry on with your own work meanwhile."
                ),
            ),
            Vec::new(),
        )
    }

    /// `to` as a session: by project name, window name, then window index,
    /// first in the sender's tmux session, then anywhere.
    fn resolve_tell(&self, from: &AgentId, to: &str) -> Result<&Agent, String> {
        let me = self.agent(from).ok_or("this session is gone")?;
        let others = self.tellable(me);
        let to = to.trim();
        // By project name, by window name, and (in its own tmux session
        // only) by window index.
        let named = |a: &Agent, by: usize| match by {
            0 => tell_name(a) == to,
            1 => a.window_name == to,
            _ => a.window_index.to_string() == to,
        };
        for (same_only, ways) in [(true, 0..3), (false, 0..2)] {
            for by in ways {
                let found: Vec<&&Agent> = others
                    .iter()
                    .filter(|a| (!same_only || a.session == me.session) && named(a, by))
                    .collect();
                match found[..] {
                    [one] => return Ok(one),
                    [] => {}
                    _ => {
                        return Err(format!(
                            "{to} is more than one session: {}",
                            found
                                .iter()
                                .map(|a| format!("{}:{}", a.session, a.window_index))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
        }
        let names: Vec<String> = others.iter().map(|a| tell_name(a)).collect();
        Err(match names.is_empty() {
            true => format!("No session called {to}: no other Claude sessions are running"),
            false => format!("No session called {to}. Sessions: {}", names.join(", ")),
        })
    }

    /// y sends the message, e opens it to edit before sending, n drops it.
    pub(crate) fn on_tell_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Card {
            id,
            agent,
            kind: CardKind::Tell(tc),
            ..
        }) = self.queue.current()
        else {
            return Vec::new();
        };
        let (id, agent, tc) = (*id, agent.clone(), tc.clone());
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('y') if !ctrl => {
                if !self.settled(id) {
                    self.too_quick("message");
                    return Vec::new();
                }
                self.queue.remove(id);
                self.send_prompt(agent, framed(&tc.from_name, &tc.message.text))
            }
            KeyCode::Char('e') if !ctrl => {
                self.queue.remove(id);
                self.overlay = Some(Overlay::Prompt(PromptDraft {
                    agent,
                    input: TextInput::single_line_with(&framed(&tc.from_name, &tc.message.text)),
                    group: None,
                }));
                Vec::new()
            }
            KeyCode::Char('n' | 'x') if !ctrl => {
                self.queue.remove(id);
                self.set_status(format!("Dropped the message from {}", tc.from_name));
                Vec::new()
            }
            code => {
                let page = self.screen.map_or(10, |(_, h)| (h as usize / 2).max(1));
                self.scroll_question(code, ctrl, page);
                Vec::new()
            }
        }
    }
}

/// The message as the other session reads it: who it is from first.
fn framed(from: &str, text: &str) -> String {
    format!("From the {from} session: {text}")
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{agent, key};
    use crate::app::{App, Effect, Msg};
    use crate::config::Config;
    use crate::ipc::{HookReply, HookRequest};
    use crate::model::{CardKind, Checkout, Mode};
    use crate::theme::builtin_themes;
    use std::time::{Instant, SystemTime};

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        let mut other = agent("%3", 1, "web", "/x/web", true);
        other.session = "other".into();
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", true),
            other,
        ];
        a
    }

    fn tell(a: &mut App, pane: &str, to: &str, text: &str) -> (Option<HookReply>, Vec<Effect>) {
        let effects = a.update(Msg::Ipc {
            req_id: 7,
            req: HookRequest::Tell {
                pane: Some(pane.into()),
                to: to.into(),
                text: text.into(),
            },
            checkout: None,
            at: SystemTime::now(),
            now: Instant::now(),
        });
        let reply = effects.iter().find_map(|e| match e {
            Effect::Reply { decision, .. } => Some(decision.clone()),
            _ => None,
        });
        (reply, effects)
    }

    fn ok_text(reply: Option<HookReply>) -> (bool, String) {
        match reply {
            Some(HookReply::Note { ok, text }) => (ok, text),
            other => panic!("not a note: {other:?}"),
        }
    }

    #[test]
    fn a_message_is_a_card_for_the_session_it_names_in_its_own_tmux_session_first() {
        let mut a = app();
        let (ok, text) = ok_text(tell(&mut a, "%1", "web", "add the field").0);
        assert!(ok && text.contains("approve"), "{text}");
        let card = a.queue.current().expect("a card");
        assert_eq!(card.agent.pane, "%2", "web in the same tmux session");
        let CardKind::Tell(tc) = &card.kind else {
            panic!("not a tell card");
        };
        assert_eq!(tc.from_name, "api");
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(
            screen.contains("api asks web") && screen.contains("add the field"),
            "{screen}"
        );
    }

    #[test]
    fn y_sends_it_framed_e_edits_it_n_drops_it() {
        let mut a = app();
        tell(&mut a, "%1", "web", "add the field");
        let effects = a.update(key('y'));
        let sent = effects.iter().any(
            |e| matches!(e, Effect::Send { text, .. } if text == "From the api session: add the field"),
        ) || a.prompts.values().any(|t| t == "From the api session: add the field");
        assert!(sent, "{effects:?}");
        assert!(a.queue.current().is_none());
        tell(&mut a, "%1", "web", "again");
        a.update(key('n'));
        assert!(a.queue.current().is_none());
        assert!(
            a.status_text()
                .unwrap()
                .contains("Dropped the message from api")
        );
    }

    #[test]
    fn unknown_ambiguous_and_unsaid_messages_are_refused_with_the_reason() {
        let mut a = app();
        let (ok, text) = ok_text(tell(&mut a, "%1", "db", "x").0);
        assert!(!ok && text.contains("Sessions: web, web"), "{text}");
        let (ok, text) = ok_text(tell(&mut a, "%1", "web", "  ").0);
        assert!(!ok && text.contains("Say what"), "{text}");
        let (ok, text) = ok_text(tell(&mut a, "%9", "web", "x").0);
        assert!(!ok && text.contains("which session"), "{text}");
        // From the other tmux session, two sessions are called api? No: one.
        let (ok, _) = ok_text(tell(&mut a, "%3", "api", "x").0);
        assert!(ok);
        assert!(a.queue.len() == 1);
    }

    #[test]
    fn in_vibe_mode_the_message_goes_without_a_card() {
        let mut a = app();
        a.config.set_mode(&Checkout("/w/web".into()), Mode::Vibe);
        let (ok, text) = ok_text(tell(&mut a, "%1", "web", "go").0);
        assert!(ok && text.contains("Sent to web"), "{text}");
        assert!(a.queue.current().is_none());
    }

    #[test]
    fn aq_tell_alone_lists_the_others_its_own_tmux_session_first() {
        let a = app();
        let HookReply::Note { text, .. } = a.on_tellable(Some("%3")) else {
            panic!("not a note");
        };
        assert_eq!(text, "api  (tmux session main)\nweb  (tmux session main)");
        let HookReply::Note { text, .. } = a.on_tellable(Some("%1")) else {
            panic!("not a note");
        };
        assert_eq!(text, "web  (same tmux session)\nweb  (tmux session other)");
    }
}
