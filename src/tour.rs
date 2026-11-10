//! The tour: a few pages that walk a new user through aq, one idea each.
//! It opens by itself until it has been finished or skipped once; F2 opens
//! it again. Pure: no I/O.

use std::path::Path;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Overlay};
use crate::model::{AgentId, Card, CardKind, Checkout, ReviewCard};

/// One page: a heading, then lines. A line starting with `key:` shows that
/// key, then a tab, then what it does.
pub struct Page {
    pub title: &'static str,
    pub lines: &'static [&'static str],
}

pub const PAGES: &[Page] = &[
    Page {
        title: "Welcome to aq",
        lines: &[
            "aq watches every Claude Code session in your tmux and puts",
            "whatever needs you in one queue, shown one card at a time:",
            "a diff to review, an edit to allow, a permission prompt.",
            "",
            "This tour takes about two minutes, and you get to try it.",
            "",
            "key:→ or Enter\tnext page",
            "key:←\tprevious page",
            "key:Esc\tskip the tour (F2 brings it back)",
        ],
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
    },
    Page {
        title: "The queue",
        lines: &[
            "The top bar counts what is waiting behind the card on screen.",
            "Decide on a card and the next one comes up.",
            "",
            "key:Space\tskip a card for now",
            "key:j k\tmove, as in vim (also Ctrl-d, Ctrl-u, gg, G)",
            "",
            "A card that jumps in ignores deciding keys for 1.5 s, so a",
            "key meant for the last card never lands on the new one.",
        ],
    },
    Page {
        title: "Modes",
        lines: &[
            "Each project (git checkout) has its own mode.",
            "",
            "key:review\twhen a session finishes, review its diff",
            "key:step\tevery edit waits for you before it is applied",
            "key:quiz\texplain parts of the change in your own words",
            "key:vibe\tnothing is checked",
            "",
            "key:M\tmove the project to the next mode",
        ],
    },
    Page {
        title: "Try it",
        lines: &[
            "Next comes a practice review card, made up for this tour.",
            "It is what you get when a session finishes a turn.",
            "",
            "The tour tells you which key to press, one at a time.",
            "Nothing you do on it reaches a real session.",
            "",
            "key:→ or Enter\tstart the practice",
            "key:Esc\tleave the tour, during the practice too",
        ],
    },
    Page {
        title: "Step and quiz",
        lines: &[
            "That was review mode. In step mode each edit is its own card:",
            "key:y\tlet it through",
            "key:n\tsend it back with a reason Claude sees",
            "",
            "In quiz mode a box opens on each question:",
            "key:Ctrl-S\tsubmit your answer",
            "key:y c\tafter the verdict: keep as is, or ask for a fix",
        ],
    },
    Page {
        title: "When nothing is waiting",
        lines: &[
            "You get a tab per session, with the picked one's screen live.",
            "",
            "key:h l\tpick a session",
            "key:Enter\tmessage it (sent once it is idle)",
            "key:W\task where it is up to, without interrupting it",
            "key:p\task it to open a pull request",
            "key:Esc Esc\tstop what it is doing",
            "key:wheel\tscroll back through its conversation",
        ],
    },
    Page {
        title: "Anywhere",
        lines: &[
            "key:?\task the session a side question; a copy answers",
            "key:N\tstart a new Claude session in this project",
            "key:!\ta shell in a popup, with Claude's last ! command typed in",
            "key:drag\tselect text; letting go copies it",
            "key:t\tnext theme",
            "key:F1\tevery key",
            "",
            "That's it. F2 shows this tour again.",
        ],
    },
];

/// The page whose "next" starts the practice.
pub const TRY_IT: usize = 4;

/// One thing to press on the practice card.
pub struct Step {
    /// The key as shown.
    pub key: &'static str,
    pub what: &'static str,
}

pub const STEPS: &[Step] = &[
    Step {
        key: "j",
        what: "to move down a line (k moves up)",
    },
    Step {
        key: "L",
        what: "to go to the next file (H goes back)",
    },
    Step {
        key: "v",
        what: "to switch between the split and unified views",
    },
    Step {
        key: "c",
        what: "to comment on the line under the cursor",
    },
    Step {
        key: "Ctrl-S",
        what: "to save it, once you have typed a few words",
    },
    Step {
        key: "d",
        what: "to mark this file done: it is ticked and skipped from now on",
    },
    Step {
        key: "s",
        what: "to send your comments to the session as one message",
    },
    Step {
        key: "a",
        what: "to approve the change, which closes the card",
    },
];

/// The step where the comment box is open.
const TYPING: usize = 4;

/// The made-up project the practice card belongs to.
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

/// Whether `card` is the tour's practice card.
pub fn is_practice(card: &Card) -> bool {
    card.checkout.0 == Path::new(PRACTICE_PROJECT)
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
        match key.code {
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ' | 'l' | 'j') if *page == TRY_IT => {
                self.start_practice();
                Vec::new()
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ' | 'l' | 'j') if *page < last => {
                *page += 1;
                Vec::new()
            }
            KeyCode::Left | KeyCode::Char('h' | 'k') => {
                *page = page.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Right
            | KeyCode::Enter
            | KeyCode::Char(' ' | 'l' | 'j' | 'q')
            | KeyCode::Esc => self.end_tour(),
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

    /// Puts the practice card at the front of the reviews and starts the
    /// steps. It is dated first so it comes before real reviews; a
    /// permission or step card still goes ahead of it, as always.
    fn start_practice(&mut self) {
        self.drop_practice();
        self.overlay = None;
        let files = crate::git::parse_unified(PRACTICE_DIFF.as_bytes());
        let rc = ReviewCard::new(1, String::new(), String::new(), files, Vec::new(), false);
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout: Checkout(PRACTICE_PROJECT.into()),
            agent: AgentId {
                pane: String::new(),
                claude_pid: 0,
            },
            created: SystemTime::UNIX_EPOCH,
            kind: CardKind::Review(rc),
        });
        self.practice = Some(0);
    }

    fn drop_practice(&mut self) {
        self.practice = None;
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

    /// The practice step to show: only while the practice card is the one
    /// on screen, so a real card that jumps ahead is handled as usual.
    pub fn practice_step(&self) -> Option<usize> {
        self.practice
            .filter(|_| self.queue.current().is_some_and(is_practice))
    }

    /// A key during the practice: the one asked for does what it really
    /// does, any other only repeats the ask. Sending and approving stay
    /// inside aq, since no session is behind the card.
    pub(crate) fn on_practice_key(&mut self, key: KeyEvent, step: usize) -> Vec<Effect> {
        // The comment box takes every key until it closes.
        if self.overlay.is_some() {
            let effects = self.handle_key(key);
            if self.overlay.is_none() {
                let made = self.practice_comments() > 0;
                self.practice = Some(if made { TYPING + 1 } else { TYPING - 1 });
            }
            return effects;
        }
        if key.code == KeyCode::Esc {
            return self.end_tour();
        }
        let Some(want) = STEPS.get(step) else {
            return self.end_tour();
        };
        let pressed = match key.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Tab => "L".into(),
            _ => String::new(),
        };
        if pressed != want.key {
            self.set_status(format!(
                "Tour: press {} {}. Esc leaves the tour",
                want.key, want.what
            ));
            return Vec::new();
        }
        match want.key {
            "s" => {
                let n = self.practice_comments();
                let s = if n == 1 { "" } else { "s" };
                self.set_status(format!(
                    "Practice: a real session would now get your {n} comment{s} as one message"
                ));
                self.practice = Some(step + 1);
                Vec::new()
            }
            "a" => {
                self.drop_practice();
                self.set_status("Practice: approved, and the card is gone".to_string());
                self.overlay = Some(Overlay::Tour { page: TRY_IT + 1 });
                Vec::new()
            }
            _ => {
                let effects = self.handle_key(key);
                // `c` only counts once the box is open.
                if want.key != "c" || matches!(self.overlay, Some(Overlay::Comment(_))) {
                    self.practice = Some(step + 1);
                }
                effects
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::{PAGES, STEPS, TRY_IT, is_practice};
    use crate::app::tests::{ctrl, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::time::Instant;

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    fn to_practice(a: &mut App) {
        a.start_tour_if_new();
        for _ in 0..=TRY_IT {
            assert!(press(a, KeyCode::Enter).is_empty());
        }
        assert_eq!(a.practice_step(), Some(0));
    }

    #[test]
    fn the_tour_has_you_press_each_key_on_a_practice_card() {
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
        // c opens the real comment box; Esc there goes back to asking for c.
        a.update(key('c'));
        assert!(matches!(a.overlay, Some(Overlay::Comment(_))));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.practice_step(), Some(3));
        a.update(key('c'));
        for c in "why".chars() {
            a.update(key(c));
        }
        a.update(ctrl('s'));
        assert_eq!(a.overlay, None);
        assert_eq!(a.practice_step(), Some(5));
        // The practice card's comment is never saved.
        assert!(a.saved_state().drafts.is_empty());
        a.update(key('d'));
        // Sending and approving reach no session.
        assert!(a.update(key('s')).is_empty());
        assert!(a.status_text().unwrap().contains("1 comment as"));
        assert!(a.update(key('a')).is_empty());
        assert_eq!(a.practice_step(), None);
        assert!(a.queue.is_empty());
        assert_eq!(a.overlay, Some(Overlay::Tour { page: TRY_IT + 1 }));
        for _ in TRY_IT + 1..PAGES.len() - 1 {
            assert!(press(&mut a, KeyCode::Enter).is_empty());
        }
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
