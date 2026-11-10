//! Question cards: a session ended its turn by asking you something (to
//! approve a draft, to pick between two ways), so its last message is put
//! in front of you with keys to answer it. Pure: no I/O.

use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Overlay, PromptDraft};
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::review::APPROVE_MESSAGE;
use crate::ui::input::TextInput;
use crate::visible::{visible_lines, wrap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionCard {
    /// The session's last message, as it wrote it.
    pub text: String,
    /// First row on screen.
    pub scroll: usize,
}

impl QuestionCard {
    /// The line that does the asking: the last one with a question mark
    /// or asking words, else the last one.
    pub fn ask_line(&self) -> String {
        let said = spoken(&self.text);
        let line = said
            .iter()
            .rev()
            .take(TAIL)
            .find(|l| is_asking(l))
            .or(said.last())
            .cloned()
            .unwrap_or_default();
        visible_lines(&line).join(" ")
    }

    /// The message as rows of at most `width` characters.
    pub fn rows(&self, width: usize) -> Vec<String> {
        let mut rows = Vec::new();
        for line in visible_lines(&self.text) {
            if line.is_empty() {
                rows.push(String::new());
            } else {
                wrap(&line, width, &mut |row| rows.push(row));
            }
        }
        rows
    }
}

/// How much later than its turn's end a question card is dated.
const AFTER_ITS_REVIEW: Duration = Duration::from_secs(30);

/// Words that ask for a go-ahead without a question mark.
const ASKING: [&str; 12] = [
    "approve",
    "say the word",
    "let me know",
    "shall i",
    "should i",
    "want me to",
    "would you like",
    "do you want",
    "confirm",
    "go ahead",
    "your call",
    "which one",
];

/// Whether a message ends by asking for an answer: a question mark or
/// asking words in its last few lines. Code blocks are left out, since
/// code is full of question marks.
pub fn asks(text: &str) -> bool {
    spoken(text).iter().rev().take(TAIL).any(|l| is_asking(l))
}

/// How many of a message's last lines are looked at.
const TAIL: usize = 4;

/// The lines of a message that are not blank and not code.
fn spoken(text: &str) -> Vec<String> {
    let mut in_code = false;
    let mut said = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        } else if !in_code && !line.trim().is_empty() {
            said.push(line.trim().to_string());
        }
    }
    said
}

fn is_asking(line: &str) -> bool {
    let line = line.to_lowercase();
    line.contains('?') || ASKING.iter().any(|w| line.contains(w))
}

impl App {
    /// A session's last message, read when its turn ended. One that asks
    /// for an answer becomes a card, in place of the session's last one.
    pub(crate) fn on_last_message(
        &mut self,
        agent: AgentId,
        checkout: Checkout,
        text: String,
        at: SystemTime,
    ) {
        // The session has moved on, or has gone, since it was read.
        if !self.agent(&agent).is_some_and(|a| a.waiting) {
            return;
        }
        self.drop_questions(|a| *a == agent);
        if !asks(&text) {
            return;
        }
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout,
            agent,
            // Behind the review or quiz of the same turn, which is made a
            // moment later and says in its heading what was asked:
            // answering there answers this too.
            created: at + AFTER_ITS_REVIEW,
            kind: CardKind::Question(QuestionCard { text, scroll: 0 }),
        });
    }

    /// What `agent` asked at the end of its turn, while that is unanswered.
    pub fn question_of(&self, agent: &AgentId) -> Option<&QuestionCard> {
        self.queue.iter().find_map(|c| match &c.kind {
            CardKind::Question(q) if &c.agent == agent => Some(q),
            _ => None,
        })
    }

    /// Takes away the question cards of the sessions `gone` picks: a
    /// question is over once its session is answered or working again.
    pub(crate) fn drop_questions(&mut self, gone: impl Fn(&AgentId) -> bool) {
        let ids: Vec<u64> = self
            .queue
            .iter()
            .filter(|c| matches!(c.kind, CardKind::Question(_)) && gone(&c.agent))
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.queue.remove(id);
        }
    }

    /// `a` or `y` approves, Enter or `i` writes a reply, `x` puts the card
    /// away, `g` goes to the session's window; the rest scroll.
    pub(crate) fn on_question_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let (id, agent) = (card.id, card.agent.clone());
        let page = self.screen.map_or(10, |(_, h)| (h as usize / 2).max(1));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a' | 'A' | 'y') if !ctrl => {
                if !self.settled(id) {
                    self.too_quick("question");
                    return Vec::new();
                }
                return self.send_prompt(agent, APPROVE_MESSAGE.to_string());
            }
            KeyCode::Enter | KeyCode::Char('i' | 'c') => {
                self.overlay = Some(Overlay::Prompt(PromptDraft {
                    agent,
                    input: TextInput::single_line(),
                }));
            }
            KeyCode::Char('x') => {
                self.queue.remove(id);
                self.set_status("Question put away: it is still in the session's window");
            }
            KeyCode::Char('g') if !ctrl => return vec![Effect::GoToWindow { pane: agent.pane }],
            code => self.scroll_question(code, ctrl, page),
        }
        Vec::new()
    }

    fn scroll_question(&mut self, code: KeyCode, ctrl: bool, page: usize) {
        let Some(Card {
            kind: CardKind::Question(qc),
            ..
        }) = self.queue.current_mut()
        else {
            return;
        };
        // The view stops at the last row whatever this says.
        let end = qc.text.len();
        qc.scroll = match code {
            KeyCode::Char('j') | KeyCode::Down => qc.scroll + 1,
            KeyCode::Char('k') | KeyCode::Up => qc.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => qc.scroll + page,
            KeyCode::Char('u') if ctrl => qc.scroll.saturating_sub(page),
            KeyCode::PageDown => qc.scroll + page,
            KeyCode::PageUp => qc.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => end,
            _ => qc.scroll,
        }
        .min(end);
    }
}

#[cfg(test)]
mod tests {
    use super::asks;
    use crate::app::tests::{agent, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::model::{CardKind, Checkout};
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::time::{Instant, SystemTime};

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        a
    }

    fn said(a: &mut App, text: &str) {
        let effects = a.update(Msg::LastMessage {
            agent: a.agents[0].id.clone(),
            checkout: Checkout("/w/api".into()),
            text: text.into(),
            at: SystemTime::now(),
        });
        assert!(effects.is_empty());
    }

    fn question(a: &App) -> Option<&str> {
        match &a.queue.current()?.kind {
            CardKind::Question(q) => Some(&q.text),
            _ => None,
        }
    }

    #[test]
    fn a_message_asks_when_its_last_lines_do() {
        assert!(asks(
            "Here is the draft.\n\nApprove and I'll push the branch."
        ));
        assert!(asks("Two ways to do it.\n\nWhich do you prefer?"));
        assert!(asks("Done.\n\nSay the word and I'll commit."));
        assert!(!asks("Done: both tests pass."));
        // An early question that the message itself went on to settle.
        assert!(!asks(
            "Is it cached? Yes.\n\nSo:\n- a\n- b\n- c\n- d\n\nAll fixed."
        ));
        // Code is not the session talking.
        assert!(!asks("Fixed it:\n\n```rust\nlet x = y?;\n```"));
        assert!(!asks(""));
    }

    #[test]
    fn a_approves_and_the_card_goes() {
        let mut a = app();
        said(&mut a, "Draft below.\n\nApprove and I'll push.");
        assert_eq!(question(&a), Some("Draft below.\n\nApprove and I'll push."));
        let effects = a.update(key('a'));
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Approved, carry on.".into(),
                card: None,
            }]
        );
        assert!(a.queue.is_empty());
    }

    #[test]
    fn enter_replies_in_your_own_words() {
        let mut a = app();
        said(&mut a, "README or API reference?");
        a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now()));
        assert!(matches!(a.overlay, Some(Overlay::Prompt(_))));
        for c in "the README".chars() {
            a.update(key(c));
        }
        let effects = a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now()));
        assert!(matches!(&effects[..], [Effect::Send { text, .. }] if text == "the README"));
        assert!(a.queue.is_empty());
    }

    #[test]
    fn a_question_lasts_only_while_its_session_waits_on_it() {
        let mut a = app();
        said(&mut a, "Done.");
        assert!(a.queue.is_empty(), "nothing asked");
        said(&mut a, "Shall I go on?");
        // A finished turn asks for its last message, whatever the mode.
        let mut busy = a.agents.clone();
        busy[0].waiting = false;
        a.update(Msg::Agents(busy));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/w/api", true)]));
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ReadLastMessage { session_id, .. } if session_id == "s1"
        )));
        said(&mut a, "Shall I go on?");
        // The next turn's question takes its place.
        said(&mut a, "And now: tabs or spaces?");
        assert_eq!(a.queue.len(), 1);
        assert_eq!(question(&a), Some("And now: tabs or spaces?"));
        // x only puts it away.
        assert!(a.update(key('x')).is_empty());
        assert!(a.queue.is_empty());
        said(&mut a, "Shall I go on?");
        // Answered in its own window: the session is working again.
        let mut busy = a.agents.clone();
        busy[0].waiting = false;
        a.update(Msg::Agents(busy));
        assert!(a.queue.is_empty());
        // A message read after the session moved on is not shown.
        said(&mut a, "Shall I go on?");
        assert!(a.queue.is_empty());
    }

    #[test]
    fn a_long_question_scrolls() {
        let mut a = app();
        let text = format!("{}OK?", "line\n".repeat(50));
        said(&mut a, &text);
        a.update(key('j'));
        a.update(key('j'));
        a.update(key('k'));
        let Some(CardKind::Question(q)) = a.queue.current().map(|c| &c.kind) else {
            panic!("no question");
        };
        assert_eq!(q.scroll, 1);
        assert_eq!(q.rows(80).len(), 51);
    }
}
