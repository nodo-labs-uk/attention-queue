//! Question cards: a session ended its turn by asking you something (to
//! approve a draft, to pick between two ways), so its last message is put
//! in front of you with keys to answer it. Pure: no I/O.

use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Overlay, PromptDraft};
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::review::APPROVE_MESSAGE;
use crate::ui::input::TextInput;
use crate::visible::visible_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionCard {
    /// The session's last message, as it wrote it.
    pub text: String,
    /// First row on screen.
    pub scroll: usize,
}

/// How a piece of a question's text is drawn. Claude writes Markdown, so
/// its marks are taken out and shown as style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Bold,
    /// Inline code, or a line of a code block.
    Code,
    Heading,
    /// A bullet, a quote bar or a rule: there to give shape.
    Mark,
}

/// One row on screen: pieces of text, each with its kind.
pub type Row = Vec<(Kind, String)>;

impl QuestionCard {
    /// Whether a yes answers the question: it asks for a go-ahead in its
    /// last lines. "How long did it take?" does not, and has no approve.
    pub fn approvable(&self) -> bool {
        spoken(&self.text)
            .iter()
            .rev()
            .take(TAIL)
            .any(|l| wants_go_ahead(l))
    }

    /// The line that does the asking: the last one with a question mark
    /// or asking words, else the last one. Without its Markdown marks.
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
        let line = visible_lines(&line).join(" ");
        inline(&line, Kind::Plain)
            .into_iter()
            .map(|(_, c)| c)
            .collect()
    }

    /// The message as rows of at most `width` characters, its Markdown
    /// read: headings, bold, code, bullets, quotes and rules. Anything
    /// else (a table, say) is shown as written.
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(8);
        let mut rows = Vec::new();
        let mut in_code = false;
        for line in visible_lines(&self.text) {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                // Code keeps its own line breaks; a long line is cut, not
                // flowed.
                let chars: Vec<(Kind, char)> = format!("  {line}")
                    .chars()
                    .map(|c| (Kind::Code, c))
                    .collect();
                rows.extend(chars.chunks(width).map(pieces));
                continue;
            }
            if trimmed.is_empty() {
                rows.push(Row::new());
                continue;
            }
            let indent = line.len() - trimmed.len();
            let hashes = trimmed.chars().take_while(|&c| c == '#').count();
            let rule = trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == '*');
            let (mark, body, kind) = if rule {
                rows.push(vec![(Kind::Mark, "─".repeat(width.min(40)))]);
                continue;
            } else if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                ("", trimmed[hashes..].trim(), Kind::Heading)
            } else if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                ("• ", rest, Kind::Plain)
            } else if let Some(rest) = trimmed.strip_prefix("> ") {
                ("│ ", rest, Kind::Plain)
            } else {
                ("", trimmed, Kind::Plain)
            };
            // Later rows of a bullet line up under its text.
            let first: Vec<(Kind, char)> = " "
                .repeat(indent)
                .chars()
                .map(|c| (Kind::Plain, c))
                .chain(mark.chars().map(|c| (Kind::Mark, c)))
                .collect();
            let hang = first.len().min(width / 2);
            flow(&first, &inline(body, kind), hang, width, &mut rows);
        }
        rows
    }
}

/// `text` with its inline marks read: `**bold**` and `` `code` ``, each
/// only when it is closed on the same line.
fn inline(text: &str, base: Kind) -> Vec<(Kind, char)> {
    let chars: Vec<char> = text.chars().collect();
    let closes = |from: usize, mark: &[char]| chars[from..].windows(mark.len()).any(|w| w == mark);
    let mut out = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let star = chars[i] == '*' && chars.get(i + 1) == Some(&'*');
        if chars[i] == '`' && (code || closes(i + 1, &['`'])) {
            code = !code;
            i += 1;
        } else if star && !code && (bold || closes(i + 2, &['*', '*'])) {
            bold = !bold;
            i += 2;
        } else {
            let kind = match (code, bold) {
                (true, _) => Kind::Code,
                (false, true) if base == Kind::Plain => Kind::Bold,
                _ => base,
            };
            out.push((kind, chars[i]));
            i += 1;
        }
    }
    out
}

/// Lays `first` then `body` out in rows of `width`, breaking at spaces
/// where it can; rows after the first start `hang` spaces in.
fn flow(
    first: &[(Kind, char)],
    body: &[(Kind, char)],
    hang: usize,
    width: usize,
    rows: &mut Vec<Row>,
) {
    let mut row: Vec<(Kind, char)> = first.to_vec();
    let mut rest = body;
    loop {
        let room = width.saturating_sub(row.len()).max(1);
        if rest.len() <= room {
            row.extend_from_slice(rest);
            rows.push(pieces(&row));
            return;
        }
        // The last space that fits, else a cut mid-word.
        let cut = rest[..=room.min(rest.len() - 1)]
            .iter()
            .rposition(|(_, c)| *c == ' ')
            .filter(|&at| at > 0);
        let (line, next) = match cut {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => rest.split_at(room),
        };
        row.extend_from_slice(line);
        rows.push(pieces(&row));
        row = vec![(Kind::Plain, ' '); hang];
        rest = next;
    }
}

/// Characters gathered into runs of one kind.
fn pieces(chars: &[(Kind, char)]) -> Row {
    let mut row: Row = Vec::new();
    for &(kind, c) in chars {
        match row.last_mut() {
            Some((k, text)) if *k == kind => text.push(c),
            _ => row.push((kind, c.to_string())),
        }
    }
    row
}

/// How much later than its turn's end a question card is dated.
const AFTER_ITS_REVIEW: Duration = Duration::from_secs(30);

/// Words that ask for a go-ahead: a yes answers them in full.
const GO_AHEAD: [&str; 9] = [
    "approve",
    "say the word",
    "shall i",
    "should i",
    "want me to",
    "would you like",
    "do you want",
    "confirm",
    "go ahead",
];

/// Words that ask for an answer in your own words.
const OPEN: [&str; 3] = ["let me know", "your call", "which one"];

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
    line.contains('?') || GO_AHEAD.iter().chain(&OPEN).any(|w| has_words(&line, w))
}

/// Whether a yes answers `line`.
fn wants_go_ahead(line: &str) -> bool {
    let line = line.to_lowercase();
    GO_AHEAD.iter().any(|w| has_words(&line, w))
}

/// Whether `words` is in `line` as whole words: "confirm" asks, but
/// "I confirmed the fix" reports.
fn has_words(line: &str, words: &str) -> bool {
    line.match_indices(words).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + words.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
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

    /// `a` or `y` approves a question that asks for a go-ahead, Enter or `i` writes a reply, `x` puts the card
    /// away, `g` goes to the session's window; the rest scroll.
    pub(crate) fn on_question_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let (id, agent) = (card.id, card.agent.clone());
        let page = self.screen.map_or(10, |(_, h)| (h as usize / 2).max(1));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a' | 'y') if !ctrl => {
                if !self
                    .question_of(&agent)
                    .is_some_and(QuestionCard::approvable)
                {
                    self.set_status("This question needs your own words: Enter to reply");
                    return Vec::new();
                }
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
        // Asking words inside longer words report, they do not ask.
        assert!(!asks(
            "I confirmed the problem was real before fixing it. If it had been a false alarm, I would have said so."
        ));
        assert!(!asks("Approved changes are merged."));
        assert!(asks("Please confirm and I'll push."));
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
    fn an_open_question_cannot_be_approved() {
        let mut a = app();
        said(
            &mut a,
            "How long did the bid take?\n\nOnce you tell me, I'll update today's entries.",
        );
        assert!(a.update(key('a')).is_empty());
        assert_eq!(a.queue.len(), 1, "the card stays");
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

    #[test]
    fn markdown_is_read_and_not_shown() {
        use super::{Kind, QuestionCard};
        let q = QuestionCard {
            text: "## PR draft\n\n**Title:** Use `Ctrl-O` here\n- one two three four five six\n\
                   ```\nlet x = a ** b;\n```\n2 * 3 and a lone ` stay\n\nApprove **this**?"
                .into(),
            scroll: 0,
        };
        let rows = q.rows(20);
        let text = |r: &super::Row| r.iter().map(|(_, t)| t.as_str()).collect::<String>();
        let lines: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(
            lines,
            [
                "PR draft",
                "",
                "Title: Use Ctrl-O",
                "here",
                "• one two three four",
                "  five six",
                "  let x = a ** b;",
                "2 * 3 and a lone `",
                "stay",
                "",
                "Approve this?",
            ]
        );
        assert_eq!(rows[0], [(Kind::Heading, "PR draft".to_string())]);
        assert_eq!(rows[2][0], (Kind::Bold, "Title:".to_string()));
        assert!(rows[2].contains(&(Kind::Code, "Ctrl-O".to_string())));
        assert_eq!(rows[4][0], (Kind::Mark, "• ".to_string()));
        assert_eq!(rows[6], [(Kind::Code, "  let x = a ** b;".to_string())]);
        // The line on a review card's heading loses its marks too.
        assert_eq!(q.ask_line(), "Approve this?");
    }
}
