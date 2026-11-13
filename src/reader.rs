//! Fast reading: `r` on a question, or on a session's last reply, shows
//! its words one at a time, at a speed you set, each placed so the eye
//! rests on the same column.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::{App, Effect, Overlay};
use crate::model::{Card, CardKind};

pub const MIN_WPM: u32 = 100;
pub const MAX_WPM: u32 = 1500;
/// What Up and Down change the speed by.
const WPM_STEP: u32 = 25;
/// How far Left and Right move.
const JUMP: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reader {
    pub words: Vec<String>,
    /// The word on screen.
    pub at: usize,
    pub wpm: u32,
    /// When the next word is due, while playing.
    pub due: Option<Instant>,
    /// Digits typed for a new speed, set with Enter.
    pub typed: String,
}

impl Reader {
    pub fn playing(&self) -> bool {
        self.due.is_some()
    }

    pub fn word(&self) -> &str {
        self.words.get(self.at).map_or("", String::as_str)
    }

    /// How long `word` stays: longer at the end of a sentence or clause,
    /// and for long words.
    fn shown_for(&self, word: &str) -> Duration {
        let base = 60_000.0 / self.wpm as f64;
        let factor = match word.chars().last() {
            Some('.' | '!' | '?' | ':') => 2.0,
            Some(',' | ';' | ')') => 1.5,
            _ if word.chars().count() > 8 => 1.3,
            _ => 1.0,
        };
        Duration::from_millis((base * factor) as u64)
    }

    fn play(&mut self, now: Instant) {
        if self.at + 1 >= self.words.len() {
            self.at = 0;
        }
        self.due = Some(now + self.shown_for(self.word()));
    }

    /// Moves on every word whose time is up by `now`; stops at the last.
    fn tick(&mut self, now: Instant) {
        while let Some(due) = self.due
            && now >= due
        {
            if self.at + 1 >= self.words.len() {
                self.due = None;
                return;
            }
            self.at += 1;
            self.due = Some(due + self.shown_for(self.word()));
        }
    }
}

/// The words of `text`, Markdown marks taken out, a code block standing
/// as one word.
pub fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !in_code {
                out.push("[code]".to_string());
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        for w in line.split_whitespace() {
            let w = w.replace(['*', '`'], "");
            let w = w.trim_matches(|c| matches!(c, '_' | '#' | '>'));
            if !w.is_empty() && w != "-" {
                out.push(w.to_string());
            }
        }
    }
    out
}

/// The letter the eye should rest on: a little left of the middle.
pub fn focus(word: &str) -> usize {
    match word.chars().count() {
        0..=1 => 0,
        2..=5 => 1,
        6..=9 => 2,
        10..=13 => 3,
        _ => 4,
    }
}

impl App {
    /// `r` on a question or tell card: its text, a word at a time.
    pub(crate) fn open_reader(&mut self) {
        let text = match self.queue.current().map(|c: &Card| &c.kind) {
            Some(CardKind::Question(q)) => q.text.clone(),
            Some(CardKind::Tell(t)) => t.message.text.clone(),
            _ => return,
        };
        self.read_fast(&text);
    }

    /// `r` on the sessions view: the picked session's last reply, once its
    /// conversation is read from the transcript.
    pub(crate) fn read_reply_fast(&mut self) -> Vec<Effect> {
        if !self.practice_agents.is_empty() {
            self.set_status("The tour's sessions have nothing to read");
            return Vec::new();
        }
        let Some(session_id) = self.picked_agent().map(|a| a.session_id.clone()) else {
            return Vec::new();
        };
        self.read_reply = Some(session_id);
        // Read afresh: the reply may have grown since the last read.
        self.reading_conversation = false;
        self.read_conversation()
    }

    /// A conversation came in: if `r` waits for it, read its last reply.
    pub(crate) fn on_conversation_read(&mut self) -> Vec<Effect> {
        let Some(want) = self.read_reply.clone() else {
            return Vec::new();
        };
        if self.picked_agent().map(|a| &a.session_id) != Some(&want) {
            self.read_reply = None;
            return Vec::new();
        }
        let reply = match self.picked_conversation() {
            Some(Ok(said)) => crate::transcript::last_reply(said),
            Some(Err(_)) => None,
            // Another session's conversation came first: ask again.
            None => return self.read_conversation(),
        };
        self.read_reply = None;
        match reply {
            Some(text) if self.overlay.is_none() => self.read_fast(&text),
            Some(_) => {}
            None => self.set_status("No reply from Claude to read yet"),
        }
        Vec::new()
    }

    /// Opens the reader on `text`, playing.
    fn read_fast(&mut self, text: &str) {
        let words = words(text);
        if words.is_empty() {
            self.set_status("Nothing to read");
            return;
        }
        let mut r = Reader {
            words,
            at: 0,
            wpm: self.config.read_wpm.clamp(MIN_WPM, MAX_WPM),
            due: None,
            typed: String::new(),
        };
        r.play(self.now.unwrap_or_else(Instant::now));
        self.overlay = Some(Overlay::Reader(Box::new(r)));
    }

    /// When the reader's next word is due, for the runtime to wake then.
    pub fn reader_due(&self) -> Option<Instant> {
        match &self.overlay {
            Some(Overlay::Reader(r)) => r.due,
            _ => None,
        }
    }

    pub(crate) fn on_reader_tick(&mut self, now: Instant) {
        if let Some(Overlay::Reader(r)) = &mut self.overlay {
            r.tick(now);
        }
    }

    /// Space plays or pauses; Up and Down, or digits and Enter, set the
    /// speed; Left and Right go back or on; Esc closes, keeping the speed.
    pub(crate) fn on_reader_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let now = self.now.unwrap_or_else(Instant::now);
        let Some(Overlay::Reader(r)) = &mut self.overlay else {
            return Vec::new();
        };
        let last = r.words.len() - 1;
        let mut speed = None;
        let jumped = matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::Char('h' | 'l' | 'g')
        );
        match key.code {
            KeyCode::Char(' ') if r.playing() => r.due = None,
            KeyCode::Char(' ') => r.play(now),
            KeyCode::Up | KeyCode::Char('k' | '+' | '=') => {
                speed = Some((r.wpm + WPM_STEP).min(MAX_WPM));
            }
            KeyCode::Down | KeyCode::Char('j' | '-') => {
                speed = Some(r.wpm.saturating_sub(WPM_STEP).max(MIN_WPM));
            }
            KeyCode::Char(c @ '0'..='9') if r.typed.len() < 4 => r.typed.push(c),
            KeyCode::Backspace => {
                r.typed.pop();
            }
            KeyCode::Enter if !r.typed.is_empty() => {
                speed = r
                    .typed
                    .parse::<u32>()
                    .ok()
                    .map(|w| w.clamp(MIN_WPM, MAX_WPM));
                r.typed.clear();
            }
            KeyCode::Left | KeyCode::Char('h') => r.at = r.at.saturating_sub(JUMP),
            KeyCode::Right | KeyCode::Char('l') => r.at = (r.at + JUMP).min(last),
            KeyCode::Home | KeyCode::Char('g') => r.at = 0,
            KeyCode::Esc | KeyCode::Char('q' | 'r') => {
                self.overlay = None;
                return Vec::new();
            }
            _ => {}
        }
        // After a jump, the new word gets its full time.
        if jumped && r.playing() {
            r.due = Some(now + r.shown_for(r.word()));
        }
        let Some(wpm) = speed else {
            return Vec::new();
        };
        r.wpm = wpm;
        if wpm == self.config.read_wpm {
            return Vec::new();
        }
        self.config.read_wpm = wpm;
        vec![Effect::SaveConfig]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(text: &str, wpm: u32) -> Reader {
        Reader {
            words: words(text),
            at: 0,
            wpm,
            due: None,
            typed: String::new(),
        }
    }

    #[test]
    fn words_leave_out_markdown_and_code() {
        assert_eq!(
            words("## Plan\n- **Ship** it `now`?\n```rust\nfn x() {}\n```\nDone."),
            ["Plan", "Ship", "it", "now?", "[code]", "Done."]
        );
    }

    #[test]
    fn focus_is_a_little_left_of_the_middle() {
        assert_eq!(focus("a"), 0);
        assert_eq!(focus("word"), 1);
        assert_eq!(focus("reading"), 2);
        assert_eq!(focus("concentration"), 3);
    }

    #[test]
    fn words_move_on_at_the_speed_set_and_stop_at_the_end() {
        let t0 = Instant::now();
        // 600 wpm: 100 ms a word, twice that after a full stop.
        let mut r = reader("one two. three", 600);
        r.play(t0);
        r.tick(t0 + Duration::from_millis(99));
        assert_eq!(r.word(), "one");
        r.tick(t0 + Duration::from_millis(100));
        assert_eq!(r.word(), "two.");
        r.tick(t0 + Duration::from_millis(299));
        assert_eq!(r.word(), "two.", "held longer at a full stop");
        r.tick(t0 + Duration::from_millis(300));
        assert_eq!(r.word(), "three");
        r.tick(t0 + Duration::from_secs(5));
        assert_eq!(r.word(), "three");
        assert!(!r.playing(), "stopped at the end");
        // Playing again from the end starts over.
        r.play(t0);
        assert_eq!(r.word(), "one");
    }
}
