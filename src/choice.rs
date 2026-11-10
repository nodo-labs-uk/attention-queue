//! Choice cards: Claude's own option menu (its AskUserQuestion tool),
//! caught by the PreToolUse hook before it shows and answered here. The
//! answer goes back through the hook, so nothing is typed into the pane.
//! Pure: no I/O.

use std::collections::BTreeSet;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Map, Value};

use crate::app::{App, Effect, Overlay};
use crate::ipc::HookReply;
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::ui::input::{InputEvent, TextInput};

/// The tool whose input is a menu of questions.
pub const TOOL: &str = "AskUserQuestion";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceQuestion {
    pub question: String,
    pub header: String,
    pub options: Vec<Choice>,
    /// Several options may be picked.
    pub multi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceCard {
    /// The hook waiting on the answer.
    pub req_id: u64,
    /// The tool's input as it came, handed back with the answers added.
    pub input: Value,
    pub questions: Vec<ChoiceQuestion>,
    /// Answers so far, one per question in order; the question on screen
    /// is the next.
    pub answers: Vec<String>,
    /// The option under the cursor.
    pub cursor: usize,
    /// Options ticked so far on a question that takes several.
    pub ticked: BTreeSet<usize>,
}

impl ChoiceCard {
    pub fn current(&self) -> Option<&ChoiceQuestion> {
        self.questions.get(self.answers.len())
    }
}

/// `o` on a choice card: an answer in your own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnAnswerDraft {
    pub card: u64,
    pub input: TextInput,
}

/// The questions in the tool's input. Empty when it is not shaped as
/// expected, or a question has nothing to pick from.
pub fn questions(input: &Value) -> Vec<ChoiceQuestion> {
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_string();
    let parsed: Vec<ChoiceQuestion> = input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| ChoiceQuestion {
            question: text(q, "question"),
            header: text(q, "header"),
            options: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| Choice {
                    label: text(o, "label"),
                    description: text(o, "description"),
                })
                .filter(|o| !o.label.is_empty())
                .collect(),
            multi: q["multiSelect"] == true,
        })
        .collect();
    let usable = parsed
        .iter()
        .all(|q| !q.question.is_empty() && !q.options.is_empty());
    if usable { parsed } else { Vec::new() }
}

impl App {
    /// The hook of an option menu is waiting. A menu aq can show becomes a
    /// card; any other is let through to the session's window.
    pub(crate) fn on_choice(
        &mut self,
        req_id: u64,
        at: SystemTime,
        session_id: &str,
        cwd: &str,
        checkout: Option<Checkout>,
        input: Value,
    ) -> Vec<Effect> {
        let questions = questions(&input);
        if questions.is_empty() {
            return vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }];
        }
        let agent = self.agents.iter().find(|a| a.session_id == session_id);
        let checkout = agent
            .and_then(|a| a.checkout.clone())
            .or(checkout)
            .unwrap_or_else(|| Checkout(cwd.into()));
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
            kind: CardKind::Choice(ChoiceCard {
                req_id,
                input,
                questions,
                answers: Vec::new(),
                cursor: 0,
                ticked: BTreeSet::new(),
            }),
        });
        Vec::new()
    }

    /// The hook went away: the menu was answered elsewhere or timed out.
    pub(crate) fn close_choice(&mut self, req_id: u64) -> bool {
        let Some(id) = self
            .queue
            .iter()
            .find(|c| matches!(&c.kind, CardKind::Choice(c) if c.req_id == req_id))
            .map(|c| c.id)
        else {
            return false;
        };
        self.queue.remove(id);
        if matches!(&self.overlay, Some(Overlay::OwnAnswer(d)) if d.card == id) {
            self.overlay = None;
        }
        self.set_status("Question closed: its session stopped waiting for the answer");
        true
    }

    /// A number picks that option (or ticks it, where several may be
    /// picked); j/k and Enter do the same with the cursor; `o` answers in
    /// your own words; `u` goes back a question; `x` or `g` hands the menu
    /// to the session's window.
    pub(crate) fn on_choice_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let (id, pane) = (card.id, card.agent.pane.clone());
        let settled = self.settled(id);
        let Some(Card {
            kind: CardKind::Choice(cc),
            ..
        }) = self.queue.current_mut()
        else {
            return Vec::new();
        };
        let Some(q) = cc.current().cloned() else {
            return Vec::new();
        };
        let n = q.options.len();
        let digit = match key.code {
            KeyCode::Char(c @ '1'..='9') => Some(c as usize - '1' as usize).filter(|&i| i < n),
            _ => None,
        };
        let picked = match (key.code, digit) {
            (_, Some(i)) if q.multi => {
                if !cc.ticked.remove(&i) {
                    cc.ticked.insert(i);
                }
                cc.cursor = i;
                return Vec::new();
            }
            (_, Some(i)) => vec![i],
            (KeyCode::Enter, _) if q.multi && !cc.ticked.is_empty() => {
                cc.ticked.iter().copied().collect()
            }
            (KeyCode::Enter, _) => vec![cc.cursor],
            (KeyCode::Char('j') | KeyCode::Down, _) => {
                cc.cursor = (cc.cursor + 1).min(n - 1);
                return Vec::new();
            }
            (KeyCode::Char('k') | KeyCode::Up, _) => {
                cc.cursor = cc.cursor.saturating_sub(1);
                return Vec::new();
            }
            (KeyCode::Char('u') | KeyCode::Backspace, _) => {
                cc.answers.pop();
                cc.cursor = 0;
                cc.ticked.clear();
                return Vec::new();
            }
            (KeyCode::Char('o' | 'i'), _) => {
                self.overlay = Some(Overlay::OwnAnswer(OwnAnswerDraft {
                    card: id,
                    input: TextInput::single_line(),
                }));
                return Vec::new();
            }
            (KeyCode::Char('x' | 'g'), _) => {
                let req_id = cc.req_id;
                self.queue.remove(id);
                self.set_status("The question is now a menu in the session's window");
                let mut effects = vec![Effect::Reply {
                    req_id,
                    decision: HookReply::NoDecision,
                }];
                if key.code == KeyCode::Char('g') {
                    effects.push(Effect::GoToWindow { pane });
                }
                return effects;
            }
            _ => return Vec::new(),
        };
        if !settled {
            self.too_quick("question");
            return Vec::new();
        }
        // Several labels go back joined with commas, as the menu does it.
        let labels: Vec<&str> = picked
            .iter()
            .filter_map(|&i| q.options.get(i))
            .map(|o| o.label.as_str())
            .collect();
        self.answer_choice(id, labels.join(", "))
    }

    pub(crate) fn on_own_answer_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::OwnAnswer(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let text = match draft.input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => None,
            InputEvent::Submit(text) => Some(text.trim().to_string()),
        };
        let Some(Overlay::OwnAnswer(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        match text.filter(|t| !t.is_empty()) {
            Some(text) => self.answer_choice(draft.card, text),
            None => Vec::new(),
        }
    }

    /// Records the answer to the question on screen. After the last one
    /// the hook gets the tool's input back with every answer in it, keyed
    /// by its question, and the card closes.
    fn answer_choice(&mut self, id: u64, answer: String) -> Vec<Effect> {
        let Some(Card {
            kind: CardKind::Choice(cc),
            ..
        }) = self.queue.find_mut(|c| c.id == id)
        else {
            return Vec::new();
        };
        cc.answers.push(answer);
        cc.cursor = 0;
        cc.ticked.clear();
        if cc.answers.len() < cc.questions.len() {
            return Vec::new();
        }
        let answers: Map<String, Value> = cc
            .questions
            .iter()
            .zip(&cc.answers)
            .map(|(q, a)| (q.question.clone(), Value::String(a.clone())))
            .collect();
        let mut input = cc.input.clone();
        if let Some(fields) = input.as_object_mut() {
            fields.insert("answers".into(), Value::Object(answers));
        }
        let req_id = cc.req_id;
        self.queue.remove(id);
        self.set_status("Answered");
        vec![Effect::Reply {
            req_id,
            decision: HookReply::Answer { input },
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::questions;
    use crate::app::tests::{agent, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::ipc::{HookReply, HookRequest};
    use crate::model::CardKind;
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use serde_json::{Value, json};
    use std::time::{Instant, SystemTime};

    fn menu() -> Value {
        json!({"questions": [
            {"question": "Which first?", "header": "Order", "multiSelect": false, "options": [
                {"label": "Menus", "description": "Option menus on a card"},
                {"label": "Review", "description": "The question on the review card"},
            ]},
            {"question": "Which checks?", "header": "Checks", "multiSelect": true, "options": [
                {"label": "Tests", "description": ""},
                {"label": "Clippy", "description": ""},
                {"label": "Docs", "description": ""},
            ]},
        ]})
    }

    fn asked(input: Value) -> (App, Vec<Effect>) {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", false)];
        let effects = a.update(Msg::Ipc {
            req_id: 7,
            req: HookRequest::PreTool {
                session_id: "s1".into(),
                transcript_path: String::new(),
                cwd: "/w/api".into(),
                tool_name: "AskUserQuestion".into(),
                tool_use_id: "t1".into(),
                tool_input: input,
            },
            checkout: None,
            at: SystemTime::now(),
            now: Instant::now(),
        });
        (a, effects)
    }

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    fn answer(effects: &[Effect]) -> &Value {
        match effects {
            [
                Effect::Reply {
                    req_id: 7,
                    decision: HookReply::Answer { input },
                },
            ] => input,
            other => panic!("no answer: {other:?}"),
        }
    }

    #[test]
    fn numbers_answer_each_question_and_the_hook_gets_them_all() {
        let (mut a, effects) = asked(menu());
        assert!(effects.is_empty(), "the hook waits");
        let card = a.queue.current().unwrap();
        assert!(matches!(card.kind, CardKind::Choice(_)));
        assert_eq!(card.agent.pane, "%1", "found by its session");
        // One pick answers the first; the second takes several.
        assert!(a.update(key('2')).is_empty());
        assert!(a.update(key('1')).is_empty());
        assert!(a.update(key('3')).is_empty());
        assert!(a.update(key('1')).is_empty(), "a second press unticks");
        assert!(a.update(key('2')).is_empty());
        let effects = press(&mut a, KeyCode::Enter);
        let input = answer(&effects);
        assert_eq!(
            input["answers"],
            json!({"Which first?": "Review", "Which checks?": "Clippy, Docs"})
        );
        assert_eq!(input["questions"], menu()["questions"], "handed back whole");
        assert!(a.queue.is_empty());
    }

    #[test]
    fn the_cursor_your_own_words_and_going_back_all_answer() {
        let (mut a, _) = asked(menu());
        a.update(key('j'));
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        // Back to the first question, answered in other words this time.
        a.update(key('u'));
        a.update(key('o'));
        assert!(matches!(a.overlay, Some(Overlay::OwnAnswer(_))));
        for c in "both at once".chars() {
            a.update(key(c));
        }
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        // Enter with nothing ticked takes the option under the cursor.
        let effects = press(&mut a, KeyCode::Enter);
        assert_eq!(
            answer(&effects)["answers"],
            json!({"Which first?": "both at once", "Which checks?": "Tests"})
        );
    }

    #[test]
    fn x_hands_the_menu_to_the_window_and_a_gone_hook_closes_the_card() {
        let (mut a, _) = asked(menu());
        assert_eq!(
            a.update(key('x')),
            [Effect::Reply {
                req_id: 7,
                decision: HookReply::NoDecision
            }]
        );
        assert!(a.queue.is_empty());
        let (mut a, _) = asked(menu());
        a.update(Msg::HookGone { req_id: 7 });
        assert!(a.queue.is_empty());
    }

    #[test]
    fn a_menu_aq_cannot_read_is_left_to_the_window() {
        assert!(questions(&json!({"questions": "?"})).is_empty());
        assert!(questions(&json!({"questions": [{"question": "Q", "options": []}]})).is_empty());
        let (a, effects) = asked(json!({}));
        assert_eq!(
            effects,
            [Effect::Reply {
                req_id: 7,
                decision: HookReply::NoDecision
            }]
        );
        assert!(a.queue.is_empty());
    }
}
