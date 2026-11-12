//! The tour: pages that walk a new user through aq, with practice cards in
//! between where it asks for each key in turn. It opens by itself until it
//! has been finished or skipped once; F2 opens it again. Pure: no I/O.

use std::path::Path;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use crate::agents::{Agent, AgentStatus, InputBox};
use crate::app::{App, Effect, NewSessionDraft, Overlay, PromptDraft, mode_label};
use crate::ask::{AskState, Entry, NO_CARD, WHERE_ARE_WE};
use crate::edit::apply_edit;
use crate::model::{AgentId, Card, CardKind, Checkout, Mode, ReviewCard};
use crate::quiz::{
    Answer, CodeRef, GapIn, JudgeVerdict, Judgement, Question, QuizCard, QuizState, Scope, Verdict,
};
use crate::step::{EditKey, PendingEdit, ReasonDraft, StepCard};
use crate::ui::input::{InputEvent, TextInput};

/// A practice: one mode's card, and the keys asked for on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Review,
    Step,
    Quiz,
    /// No card: made-up sessions on the screen for when nothing waits.
    Sessions,
}

/// One page: a heading, then lines. A line starting with `key:` shows that
/// key, then a tab, then what it does. Going on from a page with a
/// `practice` starts it; the tour comes back at the page after.
pub struct Page {
    pub title: &'static str,
    pub lines: &'static [&'static str],
    pub practice: Option<Act>,
}

pub const PAGES: &[Page] = &[
    Page {
        title: "Welcome to aq",
        lines: &[
            "aq watches every Claude Code session in your tmux and puts",
            "whatever needs you in one queue, shown one card at a time:",
            "a diff to review, an edit to allow, a permission prompt.",
            "",
            "This tour takes about four minutes, and most of it is you",
            "pressing the keys on made-up cards.",
            "",
            "key:→ or Enter\tnext page",
            "key:←\tprevious page",
            "key:Esc\tskip the tour (F2 brings it back)",
        ],
        practice: None,
    },
    Page {
        title: "Before you start",
        lines: &[
            "aq hears from Claude through three hooks in",
            "~/.claude/settings.json; the README shows how to add them,",
            "or has a prompt you can paste to Claude to do it for you.",
            "",
            "Sessions started before the hooks were added need a restart.",
            "Without aq running, the hooks do nothing at all.",
        ],
        practice: None,
    },
    Page {
        title: "The queue",
        lines: &[
            "The top bar counts what is waiting behind the card on screen.",
            "Decide on a card and the next one comes up. A new card waits",
            "its turn: it never takes the screen from the one you are on.",
            "",
            "key:Space\tskip a card for now",
            "key:j k\tmove, as in vim (also Ctrl-d, Ctrl-u, gg, G)",
            "",
            "A card answered in its own window stays, marked \"answered",
            "elsewhere\", and the next key only closes it. A card whose edit",
            "changes under you ignores deciding keys for 1.5 s.",
        ],
        practice: None,
    },
    Page {
        title: "Try it: review",
        lines: &[
            "Next comes a practice review card, made up for this tour.",
            "It is what you get when a session finishes a turn.",
            "The tour tells you which key to press, one at a time.",
            "Nothing you do on it reaches a real session.",
            "",
            "Also there, and not in the practice:",
            "key:e\topen the file at this line in your editor",
            "key:f\thide the file list for a wider diff",
            "key:m\tMarkdown as written, or as read",
            "",
            "key:→ or Enter\tstart the practice",
            "key:Esc\tleave the tour, during the practice too",
        ],
        practice: Some(Act::Review),
    },
    Page {
        title: "Try it: step mode",
        lines: &[
            "That was review mode, the default. Each project (git",
            "checkout) has its own mode, and M moves it to the next.",
            "",
            "In step mode every edit waits for you before it is applied.",
            "You let it through, or send it back with a reason.",
            "",
            "key:M\tmove the practice project to step mode",
        ],
        practice: Some(Act::Step),
    },
    Page {
        title: "Try it: quiz mode",
        lines: &[
            "In quiz mode, when a session finishes, aq picks the parts of",
            "the change that matter and asks you to explain them in your",
            "own words. A judge marks each answer, and a gap it finds in",
            "the code can go back to Claude as a fix.",
            "",
            "In the practice the verdict is made up.",
            "",
            "key:M\tmove the practice project to quiz mode",
        ],
        practice: Some(Act::Quiz),
    },
    Page {
        title: "Modes, in short",
        lines: &[
            "key:review\twhen a session finishes, review its diff",
            "key:step\tevery edit waits for you before it is applied",
            "key:quiz\texplain parts of the change in your own words",
            "key:vibe\tnothing is checked",
            "",
            "key:M\tmove the project to the next mode",
            "",
            "A script can run in vibe while a payments service sits in",
            "step: the mode is kept per project.",
        ],
        practice: None,
    },
    Page {
        title: "Try it: sessions",
        lines: &[
            "When nothing is waiting you get a tab per session, with the",
            "picked one's screen live underneath.",
            "Next come three made-up sessions to try that on.",
            "",
            "Also there, and not in the practice:",
            "key:Tab\tback to the session shown before",
            "key:p\task the session to open a pull request",
            "key:m P\tswitch its model, or its permission mode",
            "key:Esc Esc x\tstop what it is doing, or close it",
            "key:wheel\tscroll back through its conversation",
            "",
            "key:→ or Enter\tstart the practice",
        ],
        practice: Some(Act::Sessions),
    },
    Page {
        title: "Anywhere",
        lines: &[
            "W and N work on a card too, for the card's session. So do:",
            "",
            "key:?\task the session a side question; a copy answers",
            "key:!\ta shell in a popup, with Claude's last ! command typed in",
            "key:drag\tselect text; letting go copies it",
            "key:2 clicks\tcopy the word; a third click copies the line",
            "key:t\tnext theme",
            "key:F1\tevery key",
            "",
            "That's it. F2 shows this tour again.",
        ],
        practice: None,
    },
];

/// One thing to press on a practice card.
pub struct Step {
    pub act: Act,
    /// The key as shown.
    pub key: &'static str,
    pub what: &'static str,
}

const fn step(act: Act, key: &'static str, what: &'static str) -> Step {
    Step { act, key, what }
}

/// In order. A step whose key opens a box, a list or an answer is followed
/// by the step that closes it.
pub const STEPS: &[Step] = &[
    step(Act::Review, "j", "to move down a line (k moves up)"),
    step(Act::Review, "L", "to go to the next file (H goes back)"),
    step(
        Act::Review,
        "v",
        "to switch between the split and unified views",
    ),
    step(Act::Review, "F1", "to list every key that works here"),
    step(Act::Review, "Esc", "to close the list"),
    step(Act::Review, "c", "to comment on the line under the cursor"),
    step(
        Act::Review,
        "Ctrl-S",
        "to save it, once you have typed a few words",
    ),
    step(
        Act::Review,
        "d",
        "to mark this file done: it is ticked and skipped from now on",
    ),
    step(
        Act::Review,
        "s",
        "to send your comments to the session as one message",
    ),
    step(
        Act::Review,
        "a",
        "to approve the change, which closes the card",
    ),
    step(
        Act::Step,
        "y",
        "to let this edit through; Claude is waiting on you",
    ),
    step(
        Act::Step,
        "n",
        "to refuse this one: raising an error is not what you asked for",
    ),
    step(
        Act::Step,
        "Enter",
        "to send Claude your reason, once you have typed it",
    ),
    step(Act::Quiz, "i", "to answer the question in your own words"),
    step(
        Act::Quiz,
        "Ctrl-S",
        "to hand in your answer, once you have typed it",
    ),
    step(
        Act::Quiz,
        "c",
        "to have Claude fix the gap the judge found (y keeps the code)",
    ),
    step(
        Act::Sessions,
        "l",
        "to pick the next session, web (h goes back)",
    ),
    step(Act::Sessions, "Enter", "to write web a message"),
    step(
        Act::Sessions,
        "Enter",
        "to send it, once typed: web is busy, so it waits its turn",
    ),
    step(
        Act::Sessions,
        "W",
        "to ask web where it is up to, without interrupting it",
    ),
    step(Act::Sessions, "Esc", "to close the answer"),
    step(Act::Sessions, "g", "to jump to web's tmux window"),
    step(
        Act::Sessions,
        "N",
        "to start another Claude session in web's project",
    ),
    step(
        Act::Sessions,
        "Enter",
        "to start it (typed text is its first prompt, Tab adds a worktree)",
    ),
    step(
        Act::Sessions,
        "M",
        "to move web's project on from quiz mode: watch its tab",
    ),
    step(
        Act::Sessions,
        "M",
        "once more, and it is back in review, the default",
    ),
];

/// The made-up project the practice cards belong to.
pub const PRACTICE_PROJECT: &str = "/practice (aq tour)";

const PRACTICE_DIFF: &str = "\
diff --git a/greet.py b/greet.py
--- a/greet.py
+++ b/greet.py
@@ -1,5 +1,8 @@
 def greet(name):
-    return 'Hello ' + name
+    if not name:
+        return 'Hello, stranger'
+    return f'Hello, {name.strip()}'
 
 
 def main():
+    print(greet(''))
     print(greet('Ada'))
diff --git a/test_greet.py b/test_greet.py
--- a/test_greet.py
+++ b/test_greet.py
@@ -2,3 +2,7 @@ from greet import greet
 
 def test_greets_by_name():
-    assert greet('Ada') == 'Hello Ada'
+    assert greet(' Ada ') == 'Hello, Ada'
+
+
+def test_greets_a_stranger():
+    assert greet('') == 'Hello, stranger'
";

/// greet.py after that diff, for the step cards' edits.
const GREET_PY: &str = "\
def greet(name):
    if not name:
        return 'Hello, stranger'
    return f'Hello, {name.strip()}'


def main():
    print(greet(''))
    print(greet('Ada'))
";

/// The step cards' edits to greet.py, as (description, old, new): one to
/// allow, then one to refuse.
const EDITS: [(&str, &str, &str); 2] = [
    (
        "Greet Grace as well",
        "    print(greet('Ada'))",
        "    print(greet('Ada'))\n    print(greet('Grace'))",
    ),
    (
        "Raise when no name is given",
        "        return 'Hello, stranger'",
        "        raise ValueError('no name given')",
    ),
];

const QUESTION: &str = "What does greet return for a name made only of spaces?";
const GAP: &str = "a name of only spaces gets past the `not name` check, so greet returns \
                   'Hello, ' with no name after it";

/// The made-up sessions: window name, whether it waits for you, its screen.
const SESSIONS: [(&str, bool, &str); 3] = [
    (
        "api",
        true,
        "\
❯ make the rate limit a token bucket per key

● Update(limiter.py)
  ⎿  Updated limiter.py with 14 additions and 6 removals

● Done: each key now has its own bucket, and both tests pass.

❯ ",
    ),
    (
        "web",
        false,
        "\
❯ add a dark mode toggle to the settings page

● Update(settings.tsx)
  ⎿  Updated settings.tsx with 22 additions and 3 removals

● Bash(npm test)
  ⎿  Running…

✻ Working… (41s · esc to interrupt)",
    ),
    (
        "docs",
        true,
        "\
❯ document the new rate limit headers

● I can write this for the README or for the API reference. Which one?

❯ ",
    ),
];

/// The session the practice has you pick and work with.
const PICKED: &str = "web";

const WHERE_WEB_IS: &str = "\
Done: the toggle is on the settings page and the choice is saved.
In progress: running the tests.
Left: dark colours for the charts.

(Made up for the tour. A real answer comes from a copy of the session, so the session itself \
 is never interrupted.)";

const PANE_PREFIX: &str = "practice:";

fn practice_agents() -> Vec<Agent> {
    SESSIONS
        .iter()
        .zip(1..)
        .map(|(&(name, waiting, _), window_index)| {
            let dir = Path::new(PRACTICE_PROJECT).join(name);
            Agent {
                id: AgentId {
                    pane: format!("{PANE_PREFIX}{name}"),
                    claude_pid: 0,
                },
                session: "main".into(),
                window_index,
                window_name: name.into(),
                cwd: dir.clone(),
                checkout: Some(Checkout(dir)),
                session_id: String::new(),
                status: if waiting {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Busy
                },
                waiting,
                input: InputBox::Unseen,
                permission_mode: None,
            }
        })
        .collect()
}

/// The screen of a made-up session's pane; `None` for a real pane.
pub fn practice_screen(pane: &str) -> Option<&'static str> {
    let name = pane.strip_prefix(PANE_PREFIX)?;
    SESSIONS
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|&(.., screen)| screen)
}

/// Whether `card` is one of the tour's practice cards.
pub fn is_practice(card: &Card) -> bool {
    card.checkout.0 == Path::new(PRACTICE_PROJECT)
}

fn review_card() -> CardKind {
    let files = crate::git::parse_unified(PRACTICE_DIFF.as_bytes());
    CardKind::Review(ReviewCard::new(
        1,
        String::new(),
        String::new(),
        files,
        Vec::new(),
        false,
    ))
}

/// Edit `n` of `EDITS` waiting on a step card. No hook waits on it.
fn step_card(n: usize) -> CardKind {
    let (description, old, new) = EDITS[n];
    let input = json!({"file_path": "greet.py", "old_string": old, "new_string": new});
    let edit = PendingEdit {
        session_id: String::new(),
        checkout: None,
        file_checkout: None,
        tool_name: "Edit".into(),
        preview: apply_edit("Edit", &input, Some(GREET_PY)),
        tool_input: input,
        path: "greet.py".into(),
        description: Some(description.into()),
        at: SystemTime::UNIX_EPOCH,
    };
    let mut sc = StepCard::new(0, EditKey::default(), n as u32 + 1, edit);
    sc.req_ids.clear();
    CardKind::Step(Box::new(sc))
}

/// A quiz with its one question already written, on the review's diff.
fn quiz_card() -> CardKind {
    let files = crate::git::parse_unified(PRACTICE_DIFF.as_bytes());
    let mut qc = QuizCard::new(String::new(), String::new(), files);
    qc.state = QuizState::Asking(0);
    qc.questions = vec![Question {
        scope: Scope::Code,
        question: QUESTION.into(),
        explanation: String::new(),
        must_cover: Vec::new(),
        code_ref: CodeRef {
            path: "greet.py".into(),
            line: 2,
            end_line: Some(4),
        },
    }];
    CardKind::Quiz(qc)
}

impl App {
    /// Opens the tour on a first run.
    pub fn start_tour_if_new(&mut self) {
        if !self.config.tutorial_done && self.overlay.is_none() {
            self.overlay = Some(Overlay::Tour { page: 0 });
        }
    }

    /// F2: the tour again, from the start.
    pub(crate) fn open_tour(&mut self) {
        self.drop_practice();
        self.overlay = Some(Overlay::Tour { page: 0 });
    }

    pub(crate) fn on_tour_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Tour { page }) = &mut self.overlay else {
            return Vec::new();
        };
        let last = PAGES.len() - 1;
        let practice = PAGES.get(*page).and_then(|p| p.practice);
        match (key.code, practice) {
            (KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ' | 'l' | 'j' | 'M'), Some(act)) => {
                self.start_practice(act);
                Vec::new()
            }
            (KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ' | 'l' | 'j'), _)
                if *page < last =>
            {
                *page += 1;
                Vec::new()
            }
            (KeyCode::Left | KeyCode::Char('h' | 'k'), _) => {
                *page = page.saturating_sub(1);
                Vec::new()
            }
            (
                KeyCode::Right
                | KeyCode::Enter
                | KeyCode::Char(' ' | 'l' | 'j' | 'q')
                | KeyCode::Esc,
                _,
            ) => self.end_tour(),
            _ => Vec::new(),
        }
    }

    fn end_tour(&mut self) -> Vec<Effect> {
        self.overlay = None;
        self.drop_practice();
        if self.config.tutorial_done {
            return Vec::new();
        }
        self.config.tutorial_done = true;
        vec![Effect::SaveConfig]
    }

    /// Puts `act`'s practice card in the queue, or its sessions on the
    /// empty screen, and starts its steps. The practice project is in the
    /// card's mode while it lasts, so the bar says so.
    fn start_practice(&mut self, act: Act) {
        self.drop_practice();
        self.overlay = None;
        self.practice = STEPS.iter().position(|s| s.act == act);
        let (mode, kind) = match act {
            Act::Review => (Mode::Review, review_card()),
            Act::Step => (Mode::Step, step_card(0)),
            Act::Quiz => (Mode::Quiz, quiz_card()),
            Act::Sessions => {
                self.practice_agents = practice_agents();
                self.picked = self.practice_agents.first().map(|a| a.id.clone());
                self.live_scroll = 0;
                // In quiz mode, where the last practice left off.
                let dir = Path::new(PRACTICE_PROJECT).join(PICKED);
                self.config.set_mode(&Checkout(dir), Mode::Quiz);
                self.capture_picked();
                return;
            }
        };
        self.config
            .set_mode(&Checkout(PRACTICE_PROJECT.into()), mode);
        self.push_practice(kind);
    }

    /// Dated first, so a review or quiz comes before real ones; a
    /// permission or step card still goes ahead of those, as always.
    fn push_practice(&mut self, kind: CardKind) {
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout: Checkout(PRACTICE_PROJECT.into()),
            agent: AgentId {
                pane: String::new(),
                claude_pid: 0,
            },
            created: SystemTime::UNIX_EPOCH,
            kind,
        });
    }

    fn drop_practice_card(&mut self) {
        let ids: Vec<u64> = self
            .queue
            .iter()
            .filter(|c| is_practice(c))
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.queue.remove(id);
        }
    }

    fn drop_practice(&mut self) {
        self.practice = None;
        self.config
            .modes
            .retain(|dir, _| !dir.starts_with(PRACTICE_PROJECT));
        self.drop_practice_card();
        if !self.practice_agents.is_empty() {
            self.practice_agents.clear();
            self.prompts
                .retain(|agent, _| !agent.pane.starts_with(PANE_PREFIX));
            // Back to the real sessions, and whichever of them comes first.
            self.picked = None;
            self.preview = None;
        }
    }

    /// The practice step to show: only while its practice card is the one
    /// on screen (or, for the sessions, no card is), so a real card that
    /// comes up between practice cards is handled as usual.
    pub fn practice_step(&self) -> Option<usize> {
        let step = self.practice?;
        let on_screen = match STEPS.get(step)?.act {
            Act::Sessions => self.queue.current().is_none(),
            _ => self.queue.current().is_some_and(is_practice),
        };
        on_screen.then_some(step)
    }

    /// A key during a practice: the one asked for does what it really
    /// does, any other only repeats the ask. Whatever would reach a session
    /// or the judge stays inside aq, since neither is behind the card.
    pub(crate) fn on_practice_key(&mut self, key: KeyEvent, step: usize) -> Vec<Effect> {
        if self.overlay.is_some() {
            self.on_practice_box_key(key, step);
            return Vec::new();
        }
        if key.code == KeyCode::Esc {
            return self.end_tour();
        }
        let Some(want) = STEPS.get(step) else {
            return self.end_tour();
        };
        let held = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let pressed = match key.code {
            _ if held => String::new(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Tab => "L".into(),
            KeyCode::F(1) => "F1".into(),
            KeyCode::Enter => "Enter".into(),
            _ => String::new(),
        };
        if pressed != want.key {
            self.set_status(format!(
                "Tour: press {} {}. Esc leaves the tour",
                want.key, want.what
            ));
            return Vec::new();
        }
        let id = self.queue.current().map_or(0, |c| c.id);
        // What the last step said no longer describes the screen.
        self.status_msg = None;
        match (want.act, want.key) {
            (Act::Review, "s") => {
                let n = self.practice_comments();
                let s = if n == 1 { "" } else { "s" };
                self.set_status(format!(
                    "Practice: a session would get your {n} comment{s} as one message"
                ));
            }
            (Act::Review, "a") => {
                self.set_status("Practice: approved, and the card is gone");
            }
            (Act::Step, "y") => {
                self.drop_practice_card();
                self.push_practice(step_card(1));
                self.set_status("Practice: allowed, and here is its next edit");
            }
            (Act::Step, "n") => {
                self.overlay = Some(Overlay::StepReason(ReasonDraft {
                    card: id,
                    change: false,
                    input: TextInput::single_line(),
                }));
            }
            (Act::Quiz, "c") => {
                self.set_status("Practice: Claude would be asked to fix it");
            }
            (Act::Sessions, key) if key != "l" => {
                if !self.on_sessions_step(key) {
                    return Vec::new();
                }
            }
            // `c`, `i` and F1 only count once their box or list is open.
            (_, "c" | "i" | "F1") => {
                self.handle_key(key);
                if self.overlay.is_none() {
                    return Vec::new();
                }
            }
            _ => {
                self.handle_key(key);
            }
        }
        self.advance(step);
        Vec::new()
    }

    /// A step on the made-up sessions, none of which has anything behind
    /// it: each key opens its real box or only says what it would do.
    /// False when there is no session to do it to.
    fn on_sessions_step(&mut self, key: &str) -> bool {
        let Some(agent) = self.picked_agent().cloned() else {
            return false;
        };
        let name = &agent.window_name;
        let checkout = Checkout(agent.cwd.clone());
        match key {
            "Enter" => {
                self.overlay = Some(Overlay::Prompt(PromptDraft {
                    agent: agent.id,
                    input: TextInput::single_line(),
                    group: None,
                }));
            }
            "W" => {
                self.overlay = Some(Overlay::Ask(Box::new(AskState {
                    card: NO_CARD,
                    checkout,
                    session_id: None,
                    context: String::new(),
                    entries: vec![Entry {
                        question: WHERE_ARE_WE.into(),
                        answer: crate::ask::Answer::Done(WHERE_WEB_IS.into()),
                        fallback: false,
                    }],
                    input: None,
                    scroll: 0,
                })));
            }
            "g" => self.set_status(format!("Practice: tmux would switch to {name}'s window")),
            "N" => {
                self.overlay = Some(Overlay::NewSession(NewSessionDraft::new(
                    agent.cwd,
                    name.clone(),
                )));
            }
            "M" => {
                let mode = match self.config.mode_for(&checkout) {
                    Mode::Quiz => Mode::Vibe,
                    _ => Mode::Review,
                };
                self.config.set_mode(&checkout, mode);
                let what = match mode {
                    Mode::Vibe => "nothing is checked",
                    _ => "the default",
                };
                self.set_status(format!("{name}: {}, {what}", mode_label(mode)));
            }
            _ => {}
        }
        true
    }

    /// A key while a practice step's box, list or answer is open. Closing
    /// it with the thing done goes on to the next step; backing out goes
    /// back to the step that opens it.
    fn on_practice_box_key(&mut self, key: KeyEvent, step: usize) {
        // Not the real handlers for these: what they send would go to a
        // hook, a session or tmux.
        let input = match &mut self.overlay {
            Some(Overlay::StepReason(d)) => Some(&mut d.input),
            Some(Overlay::Prompt(d)) => Some(&mut d.input),
            Some(Overlay::NewSession(d)) => {
                if key.code == KeyCode::Tab {
                    d.worktree = !d.worktree;
                    return;
                }
                Some(&mut d.input)
            }
            _ => None,
        };
        let done = if let Some(input) = input {
            let text = match input.handle_key(key) {
                InputEvent::Editing => return,
                InputEvent::Cancel => None,
                InputEvent::Submit(text) => Some(text.trim().to_string()),
            };
            match (self.overlay.take(), text) {
                (Some(Overlay::StepReason(_)), Some(text)) if !text.is_empty() => {
                    self.set_status("Practice: Claude would read your reason and try again");
                    true
                }
                (Some(Overlay::Prompt(d)), Some(text)) if !text.is_empty() => {
                    self.prompts.insert(d.agent, text);
                    self.set_status("Practice: queued, as its tab says, until it is idle");
                    true
                }
                (Some(Overlay::NewSession(d)), Some(_)) => {
                    let place = if d.worktree { "a new worktree of " } else { "" };
                    self.set_status(format!("Practice: Claude would start in {place}{}", d.name));
                    true
                }
                _ => false,
            }
        } else if matches!(self.overlay, Some(Overlay::Ask(_))) {
            if !matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                return;
            }
            self.overlay = None;
            true
        } else {
            // The effects would read the code and call the judge.
            self.handle_key(key);
            if self.overlay.is_some() || self.practice.is_none() {
                return;
            }
            match STEPS[step].act {
                Act::Review if STEPS[step].key == "Ctrl-S" => self.practice_comments() > 0,
                Act::Quiz => self.judge_practice(),
                _ => true,
            }
        };
        if done {
            self.advance(step);
        } else {
            self.practice = Some(step.saturating_sub(1));
        }
    }

    /// Goes on from `step`: to the next one, or after its act's last, back
    /// to the tour at the page that follows.
    fn advance(&mut self, step: usize) {
        let act = STEPS[step].act;
        if STEPS.get(step + 1).is_some_and(|s| s.act == act) {
            self.practice = Some(step + 1);
            return;
        }
        self.drop_practice();
        let page = PAGES
            .iter()
            .position(|p| p.practice == Some(act))
            .map_or(0, |p| p + 1);
        self.overlay = Some(Overlay::Tour { page });
    }

    fn practice_comments(&self) -> usize {
        self.queue
            .iter()
            .find(|c| is_practice(c))
            .map_or(0, |c| match &c.kind {
                CardKind::Review(rc) => rc.comments.len(),
                _ => 0,
            })
    }

    /// Gives the practice quiz's answer its made-up verdict, in place of
    /// the judge. False when nothing was answered.
    fn judge_practice(&mut self) -> bool {
        let Some(Card {
            kind: CardKind::Quiz(qc),
            ..
        }) = self.queue.find_mut(is_practice)
        else {
            return false;
        };
        let Some(Answer { verdict, .. }) = qc.answers.first_mut() else {
            return false;
        };
        *verdict = Verdict::Judged(Judgement {
            verdict: JudgeVerdict::Right,
            gap: GAP.into(),
            gap_in: GapIn::Code,
            hint: String::new(),
            explanation: format!(
                "The tour marks any answer right; a real judge reads yours. \
                 It also found a gap in the code: {GAP}."
            ),
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Act, PAGES, STEPS, is_practice};
    use crate::app::tests::{ctrl, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::model::{CardKind, Checkout, Mode};
    use crate::quiz::{QuizCard, Verdict};
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::time::Instant;

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    fn page_of(act: Act) -> usize {
        PAGES.iter().position(|p| p.practice == Some(act)).unwrap()
    }

    fn first_step(act: Act) -> usize {
        STEPS.iter().position(|s| s.act == act).unwrap()
    }

    fn to_practice(a: &mut App) {
        a.start_tour_if_new();
        for _ in 0..=page_of(Act::Review) {
            assert!(press(a, KeyCode::Enter).is_empty());
        }
        assert_eq!(a.practice_step(), Some(0));
    }

    fn type_in(a: &mut App, text: &str) {
        for c in text.chars() {
            assert!(a.update(key(c)).is_empty());
        }
    }

    fn mode(a: &App) -> Mode {
        a.config.mode_for(&Checkout(super::PRACTICE_PROJECT.into()))
    }

    fn quiz(a: &App) -> &QuizCard {
        match &a.queue.current().unwrap().kind {
            CardKind::Quiz(qc) => qc,
            other => panic!("not a quiz: {other:?}"),
        }
    }

    #[test]
    fn the_tour_has_you_press_each_key_on_practice_cards() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.screen = Some((120, 30));
        a.start_tour_if_new();
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        press(&mut a, KeyCode::Right);
        press(&mut a, KeyCode::Left);
        press(&mut a, KeyCode::Left);
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        a.overlay = None;
        to_practice(&mut a);
        assert!(a.queue.current().is_some_and(is_practice));
        // Any other key only repeats the ask.
        assert!(a.update(key('a')).is_empty());
        assert_eq!(a.practice_step(), Some(0));
        assert!(a.status_text().unwrap().contains("press j"));
        for (i, c) in ['j', 'L', 'v'].into_iter().enumerate() {
            a.update(key(c));
            assert_eq!(a.practice_step(), Some(i + 1), "{c}");
        }
        // F1 lists the keys; closing the list is the next step.
        press(&mut a, KeyCode::F(1));
        assert!(matches!(a.overlay, Some(Overlay::Help { .. })));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.overlay, None);
        assert_eq!(a.practice_step(), Some(5));
        // c opens the real comment box; Esc there goes back to asking for c.
        a.update(key('c'));
        assert!(matches!(a.overlay, Some(Overlay::Comment(_))));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.practice_step(), Some(5));
        a.update(key('c'));
        type_in(&mut a, "why");
        a.update(ctrl('s'));
        assert_eq!(a.overlay, None);
        assert_eq!(a.practice_step(), Some(7));
        // The practice card's comment is never saved.
        assert!(a.saved_state().drafts.is_empty());
        a.update(key('d'));
        // Sending and approving reach no session.
        assert!(a.update(key('s')).is_empty());
        assert!(a.status_text().unwrap().contains("1 comment as"));
        assert!(a.update(key('a')).is_empty());
        assert_eq!(a.practice_step(), None);
        assert!(a.queue.is_empty());
        assert_eq!(
            a.overlay,
            Some(Overlay::Tour {
                page: page_of(Act::Step)
            })
        );

        // M starts the step practice: an edit to allow, then one to refuse.
        assert!(a.update(key('M')).is_empty());
        assert_eq!(a.practice_step(), Some(first_step(Act::Step)));
        assert_eq!(mode(&a), Mode::Step);
        let first = a.queue.current().unwrap().id;
        assert!(a.update(key('y')).is_empty());
        let second = a.queue.current().unwrap().id;
        assert_ne!(first, second);
        assert!(matches!(a.queue.current().unwrap().kind, CardKind::Step(_)));
        // An empty reason goes back to asking for n.
        a.update(key('n'));
        assert!(matches!(a.overlay, Some(Overlay::StepReason(_))));
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(a.overlay, None);
        assert_eq!(a.practice_step(), Some(first_step(Act::Step) + 1));
        a.update(key('n'));
        type_in(&mut a, "return the greeting");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert!(a.queue.is_empty());
        assert!(a.steps.history.is_empty(), "no step was really decided");
        assert_eq!(
            a.overlay,
            Some(Overlay::Tour {
                page: page_of(Act::Quiz)
            })
        );

        // M again: a question, answered, with a made-up verdict.
        assert!(a.update(key('M')).is_empty());
        assert_eq!(a.practice_step(), Some(first_step(Act::Quiz)));
        assert_eq!(mode(&a), Mode::Quiz);
        a.update(key('i'));
        assert!(matches!(a.overlay, Some(Overlay::QuizAnswer(_))));
        type_in(&mut a, "Hello, with nothing after it");
        // Neither the code is read nor the judge called.
        assert!(a.update(ctrl('s')).is_empty());
        assert_eq!(a.overlay, None);
        assert!(matches!(quiz(&a).answers[0].verdict, Verdict::Judged(_)));
        assert!(quiz(&a).code_gap().is_some());
        // The practice quiz is never saved.
        assert!(a.saved_state().quizzes.is_empty());
        assert!(a.update(key('c')).is_empty());
        assert!(a.queue.is_empty());
        assert_eq!(mode(&a), Mode::Review, "the practice project is forgotten");
        assert!(a.config.modes.is_empty());
        let after = page_of(Act::Quiz) + 1;
        assert_eq!(a.overlay, Some(Overlay::Tour { page: after }));
        for _ in after..page_of(Act::Sessions) {
            assert!(press(&mut a, KeyCode::Enter).is_empty());
        }

        // Made-up sessions take the place of the real one.
        a.agents = vec![crate::app::tests::agent("%1", 1, "real", "/w/real", true)];
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(a.practice_step(), Some(first_step(Act::Sessions)));
        let names = |a: &App| -> Vec<String> {
            a.agents_in_order()
                .iter()
                .map(|x| x.window_name.clone())
                .collect()
        };
        assert_eq!(names(&a), ["api", "web", "docs"]);
        let screen = |a: &App| a.picked_preview().unwrap().clone().unwrap();
        assert!(screen(&a).contains("token bucket"));
        // No pane is read, no message sent, no window opened.
        assert!(a.update(key('l')).is_empty());
        assert_eq!(a.picked_agent().unwrap().window_name, "web");
        assert!(screen(&a).contains("dark mode"));
        assert!(a.update(Msg::Tick(Instant::now())).is_empty());
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert!(matches!(a.overlay, Some(Overlay::Prompt(_))));
        type_in(&mut a, "also the charts");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        let web = a.picked_agent().unwrap().id.clone();
        assert!(a.prompt_waiting(&web));
        assert!(a.update(key('W')).is_empty());
        assert!(matches!(a.overlay, Some(Overlay::Ask(_))));
        // The answer takes no question of your own.
        assert!(a.update(key('x')).is_empty());
        assert!(matches!(a.overlay, Some(Overlay::Ask(_))));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.update(key('g')).is_empty());
        assert!(a.update(key('N')).is_empty());
        assert!(press(&mut a, KeyCode::Tab).is_empty());
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert!(a.status_text().unwrap().contains("a new worktree of web"));
        let web_dir = Checkout(a.picked_agent().unwrap().cwd.clone());
        assert_eq!(a.config.mode_for(&web_dir), Mode::Quiz);
        assert!(a.update(key('M')).is_empty());
        assert_eq!(a.config.mode_for(&web_dir), Mode::Vibe);
        assert!(a.update(key('M')).is_empty());
        // Done: the real session is back, and nothing of the practice is left.
        assert_eq!(names(&a), ["real"]);
        assert!(a.config.modes.is_empty());
        assert!(!a.prompt_waiting(&web));
        assert_eq!(
            a.overlay,
            Some(Overlay::Tour {
                page: PAGES.len() - 1
            })
        );
        // Enter on the last page ends it, and that is saved.
        assert_eq!(press(&mut a, KeyCode::Enter), [Effect::SaveConfig]);
        assert!(a.config.tutorial_done);
        a.start_tour_if_new();
        assert_eq!(a.overlay, None);
        // F2 shows it again; Esc closes it with nothing more to save.
        press(&mut a, KeyCode::F(2));
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn esc_leaves_the_practice_and_takes_its_card_away() {
        let mut a = App::new(Config::default(), builtin_themes());
        to_practice(&mut a);
        a.update(key('j'));
        assert_eq!(press(&mut a, KeyCode::Esc), [Effect::SaveConfig]);
        assert!(a.queue.is_empty());
        assert_eq!(a.practice_step(), None);
        assert_eq!(a.overlay, None);
        assert!(a.config.modes.is_empty());
    }

    #[test]
    fn every_page_and_step_fits() {
        for p in PAGES {
            assert!(p.lines.len() <= 12, "{}", p.title);
            for l in p.lines {
                let width = l
                    .replace("key:", "")
                    .replace('\t', "            ")
                    .chars()
                    .count();
                assert!(width <= 72, "{}: {l}", p.title);
            }
        }
        for s in STEPS {
            assert!(s.key.len() + s.what.len() <= 70, "{}", s.what);
        }
    }
}
