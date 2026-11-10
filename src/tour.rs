//! The tour: a few pages that walk a new user through aq, one idea each.
//! It opens by itself until it has been finished or skipped once; F2 opens
//! it again. Pure: no I/O.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Overlay};

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
            "This tour takes about a minute.",
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
        title: "Review",
        lines: &[
            "When a session finishes a turn, its whole diff is one card.",
            "",
            "key:c\tcomment on the line (V selects a range)",
            "key:s\tsend all comments as one message",
            "key:a\tapprove the whole change",
            "key:d\tmark a file done; it is ticked and skipped",
            "key:v\tsplit or unified view",
            "key:H L\tprevious or next file",
        ],
    },
    Page {
        title: "Step and quiz",
        lines: &[
            "In step mode each edit is its own card:",
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
            "key:P\task it to open a pull request",
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
            "key:T\tnext theme",
            "key:F1\tevery key",
            "",
            "That's it. F2 shows this tour again.",
        ],
    },
];

impl App {
    /// Opens the tour on a first run.
    pub fn start_tour_if_new(&mut self) {
        if !self.config.tutorial_done && self.overlay.is_none() {
            self.overlay = Some(Overlay::Tour { page: 0 });
        }
    }

    /// F2: the tour again, from the start.
    pub(crate) fn open_tour(&mut self) {
        self.overlay = Some(Overlay::Tour { page: 0 });
    }

    pub(crate) fn on_tour_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Tour { page }) = &mut self.overlay else {
            return Vec::new();
        };
        let last = PAGES.len() - 1;
        match key.code {
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
        if self.config.tutorial_done {
            return Vec::new();
        }
        self.config.tutorial_done = true;
        vec![Effect::SaveConfig]
    }
}

#[cfg(test)]
mod tests {
    use super::PAGES;
    use crate::app::tests::key;
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::time::Instant;

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    #[test]
    fn the_tour_opens_once_and_is_remembered() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.start_tour_if_new();
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        press(&mut a, KeyCode::Right);
        press(&mut a, KeyCode::Left);
        press(&mut a, KeyCode::Left);
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        for _ in 1..PAGES.len() {
            assert!(press(&mut a, KeyCode::Enter).is_empty());
        }
        assert_eq!(
            a.overlay,
            Some(Overlay::Tour {
                page: PAGES.len() - 1
            })
        );
        // Enter on the last page ends it, and that is saved.
        assert_eq!(press(&mut a, KeyCode::Enter), [Effect::SaveConfig]);
        assert_eq!(a.overlay, None);
        assert!(a.config.tutorial_done);
        a.start_tour_if_new();
        assert_eq!(a.overlay, None);
        // F2 shows it again; Esc closes it with nothing more to save.
        press(&mut a, KeyCode::F(2));
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert_eq!(a.overlay, None);
        // Keys do not reach the card or the session list while it shows.
        a.open_tour();
        assert!(a.update(key('x')).is_empty());
        assert_eq!(a.overlay, Some(Overlay::Tour { page: 0 }));
    }

    #[test]
    fn every_page_fits_the_panel() {
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
    }
}
