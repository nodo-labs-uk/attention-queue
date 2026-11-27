//! Side questions, in one window. `W` and `T` ask a fork of the live
//! session where it is up to and what its latest turn did, so the session
//! never sees them. `b` asks anything with Claude Code's own `/btw` in the
//! session's window, which answers without adding to the conversation.
//! Pure: no I/O.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Effect, Overlay};
use crate::edit::EditPreview;
use crate::llm::{FALLBACK_NOTE, JobKind, LlmEvent, LlmJob, Speed, valid_session_id};
use crate::model::{Card, CardKind, Checkout};
use crate::pr::PrInfo;
use crate::quiz::{QuizState, diff_text, fenced};
use crate::ui::input::{InputEvent, TextInput};
use crate::visible::{visible_lines, wrap_words};

pub const ASK_PROMPT: &str = include_str!("../prompts/ask.md");

/// The card context in a prompt is cut to about this many bytes.
pub const CONTEXT_CAP: usize = 30 * 1024;
/// Lines kept either side of a changed range when a file is cut down.
const AROUND: usize = 30;
/// Lines moved by PgUp and PgDn when the screen size is not known.
const PAGE: usize = 10;
/// The card id of a question asked from the session list, with no card open.
/// Real cards count from 1.
pub const NO_CARD: u64 = 0;
/// What `W` asks.
pub const WHERE_ARE_WE: &str = "Where are we with this? In a few short lines: what is done, \
what is still left to do, and anything that needs me.";
/// What `T` asks, on Haiku.
pub const SUMMARISE: &str = "Give me a TL;DR of your latest message to me, for a reader in \
a hurry. Start with the one thing I most need to know or decide, in one sentence. Then add \
at most four short lines with the facts I need to act on it: what changed, what you found, \
what is left, the options if I must choose. Keep names, numbers and commands exact. Plain \
text, one point per line, no markdown, no headings and no labels. Leave out earlier turns, \
how you got there, and anything the first sentence already says.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Pending,
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub question: String,
    pub answer: Answer,
    /// Answered by a plain call because the session could not be resumed.
    pub fallback: bool,
    /// Asked with `/btw` in the session's window.
    pub btw: bool,
}

/// The ask overlay: the questions so far, newest last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskState {
    /// Which ask window this is; see `App::asks_opened`.
    pub id: u64,
    pub card: u64,
    pub checkout: Checkout,
    /// The live session to fork; `None` asks a plain `claude -p` instead.
    pub session_id: Option<String>,
    /// The session's tmux pane, where `/btw` is asked.
    pub pane: Option<String>,
    /// The card as text, made once when the overlay opened.
    pub context: String,
    pub entries: Vec<Entry>,
    /// A question being typed.
    pub input: Option<TextInput>,
    /// First text row on screen.
    pub scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Question,
    Answer,
    Pending,
    Error,
}

/// Width and height of the text inside the overlay, for a screen of this
/// size.
pub fn text_size(screen: (u16, u16)) -> (usize, usize) {
    let body = Rect::new(0, 0, screen.0, screen.1.saturating_sub(2));
    let p = crate::ui::window::place(body, crate::ui::window::Height::Tall);
    let (w, h) = crate::ui::window::INSET;
    (
        usize::from(p.width.saturating_sub(w)).max(1),
        usize::from(p.height.saturating_sub(h)).max(1),
    )
}

impl AskState {
    /// The text rows for a panel `width` characters wide: each question, then
    /// its answer as visible, wrapped text. Also where each entry starts.
    pub fn lines(&self, width: usize) -> (Vec<(LineKind, String)>, Vec<usize>) {
        let mut out = Vec::new();
        let mut starts = Vec::new();
        for e in &self.entries {
            starts.push(out.len());
            let question = if e.btw {
                format!("/btw {}", e.question)
            } else {
                shown_question(&e.question).to_string()
            };
            wrap_into(
                &mut out,
                LineKind::Question,
                &visible_text(&question),
                width,
            );
            out.push((LineKind::Answer, String::new()));
            match &e.answer {
                Answer::Pending if e.btw => {
                    out.push((
                        LineKind::Pending,
                        "Claude is answering in its window…".into(),
                    ));
                }
                Answer::Pending => out.push((LineKind::Pending, "Asking…".into())),
                Answer::Done(a) => {
                    wrap_into(&mut out, LineKind::Answer, &visible_text(a), width);
                    if e.fallback {
                        out.push((LineKind::Pending, FALLBACK_NOTE.trim_start().into()));
                    }
                }
                Answer::Failed(why) => {
                    wrap_into(&mut out, LineKind::Error, &visible_text(why), width);
                    out.push((LineKind::Error, "r to retry".into()));
                }
            }
            out.push((LineKind::Answer, String::new()));
        }
        (out, starts)
    }

    /// What asks entry `index` again: `/btw` in the window, or a fork.
    fn ask(&mut self, index: usize) -> Vec<Effect> {
        if !self.entries[index].btw {
            return vec![Effect::Llm(self.job(index))];
        }
        let Some(pane) = self.pane.clone() else {
            self.entries[index].answer = Answer::Failed("aq has no window for this session".into());
            return Vec::new();
        };
        vec![Effect::Btw {
            window: self.id,
            index,
            pane,
            question: self.entries[index].question.clone(),
        }]
    }

    fn job(&self, index: usize) -> LlmJob {
        LlmJob {
            card: self.card,
            kind: JobKind::Ask {
                index,
                session_id: self.session_id.clone(),
                speed: if self.entries[index].question == SUMMARISE {
                    Speed::Haiku
                } else {
                    Speed::LowEffort
                },
            },
            cwd: self.checkout.0.clone(),
            prompt: ask_prompt(&self.context, &self.entries[index].question),
        }
    }
}

/// A question as the panel shows it: `W`'s long wording is for the model.
fn shown_question(question: &str) -> &str {
    match question {
        WHERE_ARE_WE => "Where are we?",
        SUMMARISE => "The latest turn, summed up by Haiku",
        _ => question,
    }
}

fn visible_text(text: &str) -> String {
    visible_lines(text).join("\n")
}

/// `text` wrapped to `width` columns at spaces.
fn wrap_into(out: &mut Vec<(LineKind, String)>, kind: LineKind, text: &str, w: usize) {
    for line in text.split('\n') {
        wrap_words(line, w, &mut |row| out.push((kind, row)));
    }
}

pub fn ask_prompt(context: &str, question: &str) -> String {
    format!(
        "{ASK_PROMPT}## The card (data)\n\n{}\n## Question\n\n{}\n",
        fenced(context, ""),
        question.trim()
    )
}

/// `s` cut to at most `max` bytes on a character boundary, with a note.
fn cap_text(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[cut: {} of {} KB shown]",
        &s[..end],
        end / 1024,
        s.len() / 1024
    )
}

/// Lines of `after` near the changed ranges, in order, with a marker for
/// each gap; the whole text when it fits in `cap`.
fn changed_window(after: &str, changed: &[Range<u32>], cap: usize) -> String {
    if after.len() <= cap {
        return after.to_string();
    }
    let lines: Vec<&str> = after.lines().collect();
    let mut keep = vec![false; lines.len()];
    for r in changed {
        let (a, b) = (r.start as usize, (r.end as usize).max(r.start as usize + 1));
        for k in keep
            .iter_mut()
            .take((b + AROUND).min(lines.len()))
            .skip(a.saturating_sub(AROUND))
        {
            *k = true;
        }
    }
    let mut out =
        String::from("NOTE: the file is large, so only the lines around the change are shown.\n");
    let mut gap = 0;
    for (i, line) in lines.iter().enumerate() {
        if !keep[i] {
            gap += 1;
            continue;
        }
        if out.len() + line.len() + 40 > cap {
            break;
        }
        if gap > 0 {
            out.push_str(&format!("[{gap} lines not shown]\n"));
            gap = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What the card shows, as text for the model: the diff, the step's file or
/// edit, the permission's tool input, or the quiz question. About
/// `CONTEXT_CAP` bytes at most.
pub fn context_for(card: &Card) -> String {
    context_with_pr(card, None)
}

/// `context_for`, with the PR's title, number, author and description
/// before the diff when the card reviews a PR.
pub fn context_with_pr(card: &Card, pr: Option<&PrInfo>) -> String {
    match &card.kind {
        CardKind::Review(rc) if let Some(pr) = pr => {
            let head = format!(
                "A review of PR #{} of {}, \"{}\" by {}. Its description:\n\n{}\n\nDiff:\n\n",
                pr.number,
                pr.repo,
                pr.title,
                pr.author,
                cap_text(&pr.body, 4096)
            );
            let room = CONTEXT_CAP.saturating_sub(head.len());
            format!("{head}{}", diff_text(&rc.files, room))
        }
        CardKind::Review(rc) => format!(
            "A review of the agent's changes. Diff:\n\n{}",
            diff_text(&rc.files, CONTEXT_CAP)
        ),
        CardKind::ShellChange(sc) => format!(
            "The agent ran this shell command:\n{}\n\nIt changed files. Diff:\n\n{}",
            cap_text(&sc.command, 2048),
            diff_text(&sc.diff.files, CONTEXT_CAP - 2560)
        ),
        CardKind::Step(sc) => {
            let mut s = format!(
                "The agent wants to run {} on {}.\n",
                sc.tool_name,
                sc.path.display()
            );
            if let Some(d) = &sc.description {
                s.push_str(&format!("Its description: {}\n", cap_text(d, 1024)));
            }
            match &sc.preview {
                EditPreview::Placed { after, changed, .. } => {
                    let lines: Vec<String> = changed
                        .iter()
                        .map(|r| format!("{}-{}", r.start + 1, r.end.max(r.start + 1)))
                        .collect();
                    s.push_str(&format!(
                        "Changed lines of the file: {}\n",
                        lines.join(", ")
                    ));
                    s.push_str("\nThe file after the edit:\n\n");
                    s.push_str(&changed_window(after, changed, CONTEXT_CAP - 4096));
                }
                EditPreview::Unplaced { old, new, why } => {
                    let part = (CONTEXT_CAP - 4096) / 2;
                    s.push_str(&format!(
                        "The edit could not be placed in the file ({}).\n\nOld text:\n{}\n\nNew text:\n{}",
                        cap_text(why, 512),
                        cap_text(old, part),
                        cap_text(new, part)
                    ));
                }
            }
            s
        }
        CardKind::Choice(cc) => format!(
            "The agent is asking the developer to choose, with this input to its question tool:\n\n{}",
            cap_text(
                &serde_json::to_string_pretty(&cc.input).unwrap_or_default(),
                CONTEXT_CAP
            )
        ),
        CardKind::Question(qc) => format!(
            "The agent ended its turn by asking the developer this:\n\n{}",
            cap_text(&qc.text, CONTEXT_CAP)
        ),
        CardKind::Menu(mc) => format!(
            "Claude Code shows this menu in the session and waits for a pick:\n\n{}\n\n{}",
            mc.menu.question.join("\n"),
            mc.menu.options.join("\n")
        ),
        CardKind::Tell(tc) => format!(
            "The {} session asks to send this message to this session, for the developer to approve:\n\n{}",
            tc.from_name,
            cap_text(&tc.message.text, CONTEXT_CAP)
        ),
        CardKind::Permission(pc) => format!(
            "The agent asks permission to use the {} tool with this input:\n\n{}",
            pc.tool_name,
            cap_text(
                &serde_json::to_string_pretty(&pc.tool_input).unwrap_or_default(),
                CONTEXT_CAP
            )
        ),
        CardKind::Quiz(qc) => {
            let mut s = String::from("A quiz about the agent's change.\n");
            if let QuizState::Asking(i) = qc.state
                && let Some(q) = qc.questions.get(i)
            {
                s.push_str(&format!("\nThe question on screen:\n{}\n", q.question));
                let piece: String = qc
                    .excerpt(i)
                    .iter()
                    .map(|l| crate::quiz::diff_line(l) + "\n")
                    .collect();
                if !piece.is_empty() {
                    s.push_str(&format!("\nThe piece of the diff it is about:\n{piece}"));
                }
            }
            s.push_str("\nThe change:\n\n");
            let room = CONTEXT_CAP.saturating_sub(s.len());
            s.push_str(&cap_text(&qc.diff, room));
            s
        }
    }
}

impl App {
    /// Opens the window for the current card, or with no card for the
    /// session picked on the session list.
    fn open_ask(&mut self) {
        let Some(card) = self.queue.current() else {
            self.open_ask_picked();
            return;
        };
        let from_card = match &card.kind {
            CardKind::Step(sc) => Some(sc.session_id.clone()),
            _ => None,
        };
        // The hook's session is the exact one that raised the card.
        let session_id = from_card.filter(|s| valid_session_id(s)).or_else(|| {
            self.agent(&card.agent)
                .map(|a| a.session_id.clone())
                .filter(|s| valid_session_id(s))
        });
        let pane = Some(card.agent.pane.clone()).filter(|p| p != "?");
        self.asks_opened += 1;
        self.overlay = Some(Overlay::Ask(Box::new(AskState {
            id: self.asks_opened,
            card: card.id,
            checkout: card.checkout.clone(),
            session_id,
            pane,
            context: context_with_pr(
                card,
                self.pr_reviews.get(&card.checkout.0).map(|pr| &pr.info),
            ),
            entries: Vec::new(),
            input: None,
            scroll: 0,
        })));
    }

    fn open_ask_picked(&mut self) {
        let Some(agent) = self.picked_agent() else {
            return;
        };
        let checkout = agent
            .checkout
            .clone()
            .unwrap_or_else(|| Checkout(agent.cwd.clone()));
        let session_id = Some(agent.session_id.clone()).filter(|s| valid_session_id(s));
        let pane = Some(agent.id.pane.clone());
        self.asks_opened += 1;
        self.overlay = Some(Overlay::Ask(Box::new(AskState {
            id: self.asks_opened,
            card: NO_CARD,
            checkout,
            session_id,
            pane,
            context: "No card is open: the developer is looking at this session in the \
session list."
                .into(),
            entries: Vec::new(),
            input: None,
            scroll: 0,
        })));
    }

    /// `b`: a question to ask with `/btw`, typed in the window, which opens
    /// for the card on screen or the picked session if it is not open.
    pub(crate) fn open_btw(&mut self) {
        if !self.practice_agents.is_empty() {
            self.set_status("The tour's sessions are made up: /btw works on real ones");
            return;
        }
        if !matches!(self.overlay, Some(Overlay::Ask(_))) {
            self.open_ask();
        }
        if let Some(Overlay::Ask(ask)) = &mut self.overlay {
            ask.input = Some(TextInput::single_line());
        }
    }

    /// `W`: ask a fork of the session where the work stands.
    pub(crate) fn ask_where_are_we(&mut self) -> Vec<Effect> {
        self.ask_now(WHERE_ARE_WE)
    }

    /// `T`: Haiku sums up the session's latest turn, in the same window.
    pub(crate) fn summarise_turn(&mut self) -> Vec<Effect> {
        self.ask_now(SUMMARISE)
    }

    /// Opens the window, if it is not open, with `question` already sent.
    fn ask_now(&mut self, question: &str) -> Vec<Effect> {
        if !matches!(self.overlay, Some(Overlay::Ask(_))) {
            self.open_ask();
        }
        let Some(Overlay::Ask(ask)) = &mut self.overlay else {
            return Vec::new();
        };
        ask.input = None;
        ask.entries.push(Entry {
            question: question.into(),
            answer: Answer::Pending,
            fallback: false,
            btw: false,
        });
        let index = ask.entries.len() - 1;
        let width = self.screen_text_size().0;
        let Some(Overlay::Ask(ask)) = &mut self.overlay else {
            return Vec::new();
        };
        ask.scroll = ask.lines(width).1[index];
        vec![Effect::Llm(ask.job(index))]
    }

    fn screen_text_size(&self) -> (usize, usize) {
        text_size(self.screen.unwrap_or((80, 24)))
    }

    pub(crate) fn on_ask_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let size = self.screen_text_size();
        let Some(Overlay::Ask(ask)) = &mut self.overlay else {
            return Vec::new();
        };
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if let Some(input) = &mut ask.input {
            match input.handle_key(key) {
                InputEvent::Editing => {}
                InputEvent::Cancel if ask.entries.is_empty() => self.overlay = None,
                InputEvent::Cancel => ask.input = None,
                InputEvent::Submit(text) => {
                    let question = text.trim();
                    if question.is_empty() {
                        return Vec::new();
                    }
                    ask.input = None;
                    ask.entries.push(Entry {
                        question: question.to_string(),
                        answer: Answer::Pending,
                        fallback: false,
                        btw: true,
                    });
                    let index = ask.entries.len() - 1;
                    let effects = ask.ask(index);
                    let (_, starts) = ask.lines(size.0);
                    ask.scroll = starts[index];
                    return effects;
                }
            }
            return Vec::new();
        }
        let (rows, _) = ask.lines(size.0);
        let max = rows.len().saturating_sub(size.1);
        let page = size.1.saturating_sub(1).max(1).min(PAGE.max(size.1 / 2));
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.overlay = None,
            KeyCode::Char('b') => self.open_btw(),
            KeyCode::Char('W') => return self.ask_where_are_we(),
            KeyCode::Char('T') => return self.summarise_turn(),
            KeyCode::Char('j') | KeyCode::Down => ask.scroll = (ask.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => ask.scroll = ask.scroll.min(max).saturating_sub(1),
            KeyCode::PageDown => ask.scroll = (ask.scroll + page).min(max),
            KeyCode::PageUp => ask.scroll = ask.scroll.min(max).saturating_sub(page),
            KeyCode::Char('r') => {
                let failed = ask
                    .entries
                    .iter()
                    .rposition(|e| matches!(e.answer, Answer::Failed(_)));
                if let Some(i) = failed {
                    ask.entries[i].answer = Answer::Pending;
                    return ask.ask(i);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// The `/btw` answer for entry `index` of window `window`; dropped if
    /// that window is closed.
    pub(crate) fn on_btw_result(
        &mut self,
        window: u64,
        index: usize,
        pane: &str,
        result: Result<String, String>,
    ) {
        let Some(Overlay::Ask(ask)) = &mut self.overlay else {
            return;
        };
        if ask.id != window || ask.pane.as_deref() != Some(pane) {
            return;
        }
        if let Some(e) = ask
            .entries
            .get_mut(index)
            .filter(|e| e.btw && e.answer == Answer::Pending)
        {
            e.answer = match result {
                Ok(text) => Answer::Done(text.trim().to_string()),
                Err(why) => Answer::Failed(format!("/btw failed: {why}")),
            };
        }
    }

    /// The model's answer to a side question; dropped if the overlay is gone.
    pub(crate) fn on_ask_result(&mut self, ev: LlmEvent) -> Vec<Effect> {
        let JobKind::Ask {
            index, session_id, ..
        } = ev.kind
        else {
            return Vec::new();
        };
        let Some(Overlay::Ask(ask)) = &mut self.overlay else {
            return Vec::new();
        };
        // Every ask from the session list has no card: the session tells
        // one session's answer from another's.
        if ask.card != ev.card || ask.session_id != session_id {
            return Vec::new();
        }
        if let Some(e) = ask
            .entries
            .get_mut(index)
            .filter(|e| e.answer == Answer::Pending)
        {
            e.fallback = false;
            e.answer = match ev.result {
                Ok(text) => match text.strip_suffix(FALLBACK_NOTE) {
                    Some(plain) => {
                        e.fallback = true;
                        Answer::Done(plain.to_string())
                    }
                    None => Answer::Done(text),
                },
                Err(why) => Answer::Failed(why),
            };
        }
        Vec::new()
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::agent;
    use crate::config::Config;
    use crate::edit::EditPreview;
    use crate::llm::{FakeLlm, run_job};
    use crate::model::{PermissionCard, ReviewCard, StepCard};
    use crate::queue::tests::card;
    use crate::theme::builtin_themes;
    use anyhow::anyhow;
    use crossterm::event::KeyModifiers;
    use std::time::Instant;

    fn app() -> App {
        App::new(Config::default(), builtin_themes())
    }

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(
            KeyEvent::new(code, KeyModifiers::NONE),
            Instant::now(),
        ))
    }

    fn type_text(a: &mut App, text: &str) {
        for c in text.chars() {
            press(a, KeyCode::Char(c));
        }
    }

    fn ask_state(a: &App) -> &AskState {
        match &a.overlay {
            Some(Overlay::Ask(s)) => s,
            o => panic!("not asking: {o:?}"),
        }
    }

    fn review_with_cursor() -> Card {
        let files = crate::git::parse_unified(crate::review::tests::FIXTURE.as_bytes());
        let mut rc = ReviewCard::new(1, "a".into(), "b".into(), files, Vec::new(), false);
        rc.cursor = crate::review::Cursor::default();
        card(1, "/a", CardKind::Review(rc), 0)
    }

    #[test]
    fn ask_preserves_card_cursor() {
        let mut a = app();
        a.screen = Some((100, 30));
        let mut c = review_with_cursor();
        c.agent = agent("%1", 1, "api", "/a", true).id;
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        a.queue.push(c);
        // Move the cursor off its start.
        press(&mut a, KeyCode::Char('j'));
        press(&mut a, KeyCode::Char('j'));
        let before = a.queue.current().unwrap().clone();

        // j and k inside the window do not reach the card.
        let effects = press(&mut a, KeyCode::Char('W'));
        assert!(matches!(&effects[..], [Effect::Llm(j)] if matches!(
            &j.kind, JobKind::Ask { index: 0, session_id: Some(s), .. } if s == "s1")));
        let Effect::Llm(job) = &effects[0] else {
            panic!()
        };
        a.update(Msg::Llm(run_job(
            &FakeLlm::new(vec![Ok("because".into())]),
            job.clone(),
        )));
        for code in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            press(&mut a, code);
        }
        assert_eq!(
            ask_state(&a).entries[0].answer,
            Answer::Done("because".into())
        );
        press(&mut a, KeyCode::Char('q'));
        assert_eq!(a.overlay, None);
        assert_eq!(a.queue.current().unwrap(), &before);
    }

    #[test]
    fn answers_list_in_order_and_each_forks_the_live_session() {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        let mut c = review_with_cursor();
        c.agent = a.agents[0].id.clone();
        a.queue.push(c);
        let e1 = press(&mut a, KeyCode::Char('W'));
        let e2 = press(&mut a, KeyCode::Char('T'));
        for (e, i, speed) in [(&e1, 0, Speed::LowEffort), (&e2, 1, Speed::Haiku)] {
            let Effect::Llm(j) = &e[0] else { panic!() };
            assert_eq!(
                j.kind,
                JobKind::Ask {
                    index: i,
                    session_id: Some("s1".into()),
                    speed,
                }
            );
        }
        let ev = |index, r: Result<String, String>| {
            Msg::Llm(LlmEvent {
                card: 1,
                kind: JobKind::Ask {
                    index,
                    session_id: Some("s1".into()),
                    speed: Speed::LowEffort,
                },
                result: r,
            })
        };
        // Answers may land out of order.
        a.update(ev(1, Ok("second".into())));
        a.update(ev(0, Err("boom".into())));
        let s = ask_state(&a);
        assert_eq!(s.entries[1].answer, Answer::Done("second".into()));
        assert_eq!(s.entries[0].answer, Answer::Failed("boom".into()));
        let (rows, _) = s.lines(60);
        let text: Vec<&str> = rows.iter().map(|(_, l)| l.as_str()).collect();
        assert!(
            text.contains(&"r to retry") && text.contains(&"second"),
            "{text:?}"
        );
        // `r` asks the failed one again.
        let retry = press(&mut a, KeyCode::Char('r'));
        assert!(matches!(&retry[..], [Effect::Llm(j)] if j.prompt.contains(WHERE_ARE_WE)));
        assert_eq!(ask_state(&a).entries[0].answer, Answer::Pending);
        let (rows, _) = ask_state(&a).lines(60);
        assert!(
            rows.iter()
                .any(|(k, l)| *k == LineKind::Pending && l == "Asking…")
        );
    }

    #[test]
    fn b_asks_with_btw_in_the_session_s_window_and_r_asks_again() {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        assert!(press(&mut a, KeyCode::Char('b')).is_empty());
        assert!(ask_state(&a).input.is_some());
        type_text(&mut a, "why tokio?");
        let asked = press(&mut a, KeyCode::Enter);
        let btw = Effect::Btw {
            window: 1,
            index: 0,
            pane: "%1".into(),
            question: "why tokio?".into(),
        };
        assert_eq!(asked, std::slice::from_ref(&btw));
        let (rows, _) = ask_state(&a).lines(60);
        let text: Vec<&str> = rows.iter().map(|(_, l)| l.as_str()).collect();
        assert!(text.contains(&"/btw why tokio?"), "{text:?}");
        // An answer for an earlier window is dropped.
        a.update(Msg::Btw {
            window: 0,
            index: 0,
            pane: "%1".into(),
            result: Ok("no".into()),
        });
        assert_eq!(ask_state(&a).entries[0].answer, Answer::Pending);
        a.update(Msg::Btw {
            window: 1,
            index: 0,
            pane: "%1".into(),
            result: Err("its box is not empty".into()),
        });
        assert_eq!(
            ask_state(&a).entries[0].answer,
            Answer::Failed("/btw failed: its box is not empty".into())
        );
        assert_eq!(press(&mut a, KeyCode::Char('r')), [btw]);
        a.update(Msg::Btw {
            window: 1,
            index: 0,
            pane: "%1".into(),
            result: Ok("For the runtime.\n".into()),
        });
        assert_eq!(
            ask_state(&a).entries[0].answer,
            Answer::Done("For the runtime.".into())
        );
    }

    #[test]
    fn answer_text_is_made_visible() {
        let mut a = app();
        a.queue.push(review_with_cursor());
        press(&mut a, KeyCode::Char('W'));
        a.update(Msg::Llm(LlmEvent {
            card: 1,
            kind: JobKind::Ask {
                index: 0,
                session_id: None,
                speed: Speed::LowEffort,
            },
            result: Ok("run \u{1b}[2J\u{202e}now".into()),
        }));
        let (rows, _) = ask_state(&a).lines(60);
        let all: String = rows.iter().map(|(_, l)| l.as_str()).collect();
        assert!(
            !all.contains('\u{1b}') && !all.contains('\u{202e}'),
            "{all:?}"
        );
        assert!(all.contains("^[") && all.contains("\\u{202e}"), "{all:?}");
    }

    #[test]
    fn an_answer_for_one_session_does_not_land_under_another() {
        let mut a = app();
        a.agents = vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "web", "/b", true),
        ];
        press(&mut a, KeyCode::Char('W'));
        press(&mut a, KeyCode::Esc);
        press(&mut a, KeyCode::Char('l'));
        let e = press(&mut a, KeyCode::Char('W'));
        assert!(matches!(&e[..], [Effect::Llm(j)]
            if matches!(&j.kind, JobKind::Ask { session_id: Some(s), .. } if s == "s2")));
        a.update(Msg::Llm(LlmEvent {
            card: NO_CARD,
            kind: JobKind::Ask {
                index: 0,
                session_id: Some("s1".into()),
                speed: Speed::LowEffort,
            },
            result: Ok("api is half done".into()),
        }));
        assert_eq!(ask_state(&a).entries[0].answer, Answer::Pending);
    }

    #[test]
    fn t_has_haiku_sum_up_the_latest_turn_and_a_retry_stays_on_haiku() {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        let haiku_fork = |e: &[Effect]| {
            matches!(e, [Effect::Llm(j)] if j.prompt.contains(SUMMARISE)
                && matches!(&j.kind, JobKind::Ask { index: 0, session_id: Some(s), speed: Speed::Haiku } if s == "s1"))
        };
        assert!(haiku_fork(&press(&mut a, KeyCode::Char('T'))));
        a.update(Msg::Llm(LlmEvent {
            card: NO_CARD,
            kind: JobKind::Ask {
                index: 0,
                session_id: Some("s1".into()),
                speed: Speed::Haiku,
            },
            result: Err("timed out".into()),
        }));
        assert!(haiku_fork(&press(&mut a, KeyCode::Char('r'))));
        let screen = crate::ui::tests::render(&a, 100, 30);
        assert!(
            screen.contains("The latest turn, summed up by Haiku"),
            "{screen}"
        );
    }

    #[test]
    fn w_asks_where_we_are_on_a_card_and_on_the_session_list() {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        // The session list: the picked session is forked, with no card.
        let e = press(&mut a, KeyCode::Char('W'));
        assert!(matches!(&e[..], [Effect::Llm(j)] if j.card == NO_CARD
            && j.prompt.contains(WHERE_ARE_WE)
            && matches!(&j.kind, JobKind::Ask { index: 0, session_id: Some(s), .. } if s == "s1")));
        assert!(ask_state(&a).input.is_none());
        a.update(Msg::Llm(LlmEvent {
            card: NO_CARD,
            kind: JobKind::Ask {
                index: 0,
                session_id: Some("s1".into()),
                speed: Speed::LowEffort,
            },
            result: Ok("half done".into()),
        }));
        assert_eq!(
            ask_state(&a).entries[0].answer,
            Answer::Done("half done".into())
        );
        // W again in the panel asks again below.
        let e = press(&mut a, KeyCode::Char('W'));
        assert!(
            matches!(&e[..], [Effect::Llm(j)] if matches!(j.kind, JobKind::Ask { index: 1, .. }))
        );
        press(&mut a, KeyCode::Char('q'));
        // On a card: the card's own context goes with it.
        let mut c = review_with_cursor();
        c.agent = a.agents[0].id.clone();
        a.queue.push(c);
        let e = press(&mut a, KeyCode::Char('W'));
        assert!(matches!(&e[..], [Effect::Llm(j)] if j.card == 1 && j.prompt.contains("Diff")));
    }

    #[test]
    fn card_without_a_session_is_asked_plainly() {
        let mut a = app();
        a.queue.push(review_with_cursor());
        press(&mut a, KeyCode::Char('W'));
        assert_eq!(ask_state(&a).session_id, None);
    }

    #[test]
    fn esc_cancels_the_input_and_keeps_earlier_answers() {
        let mut a = app();
        a.queue.push(review_with_cursor());
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.overlay, None);
        press(&mut a, KeyCode::Char('W'));
        press(&mut a, KeyCode::Char('b'));
        press(&mut a, KeyCode::Esc);
        assert_eq!(ask_state(&a).entries.len(), 1);
        assert!(ask_state(&a).input.is_none());
    }

    fn ask_job(session: Option<&str>) -> LlmJob {
        LlmJob {
            card: 1,
            kind: JobKind::Ask {
                index: 0,
                session_id: session.map(Into::into),
                speed: Speed::LowEffort,
            },
            cwd: "/".into(),
            prompt: "p".into(),
        }
    }

    fn gone() -> anyhow::Error {
        crate::llm::SessionGone("No conversation found with session ID: x".into()).into()
    }

    #[test]
    fn fork_failure_falls_back() {
        let job = ask_job(Some("sid"));
        let mut seen = Vec::new();
        let r = crate::llm::ask_with_fallback(&job, crate::llm::FALLBACK_NOTE, |j| {
            seen.push(j.kind.clone());
            match &j.kind {
                JobKind::Ask {
                    session_id: Some(_),
                    ..
                } => Err(gone()),
                _ => Ok("plain answer".into()),
            }
        });
        assert_eq!(r.unwrap(), format!("plain answer{FALLBACK_NOTE}"));
        assert_eq!(
            seen,
            vec![
                JobKind::Ask {
                    index: 0,
                    session_id: Some("sid".into()),
                    speed: Speed::LowEffort,
                },
                JobKind::Ask {
                    index: 0,
                    session_id: None,
                    speed: Speed::LowEffort,
                },
            ]
        );
        // Both failing reports both.
        let e = crate::llm::ask_with_fallback(&job, crate::llm::FALLBACK_NOTE, |j| match &j.kind {
            JobKind::Ask {
                session_id: Some(_),
                ..
            } => Err(gone()),
            _ => Err(anyhow!("plain bad")),
        })
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("plain bad") && e.contains("No conversation"),
            "{e}"
        );
        // No session: one plain attempt only.
        let mut n = 0;
        let _ = crate::llm::ask_with_fallback(&ask_job(None), crate::llm::FALLBACK_NOTE, |_| {
            n += 1;
            Err(anyhow!("x"))
        });
        assert_eq!(n, 1);
    }

    #[test]
    fn timeout_and_other_failures_do_not_fall_back() {
        for why in [
            "took longer than 180 s",
            "Not logged in",
            "rate limit reached",
        ] {
            let mut n = 0;
            let r = crate::llm::ask_with_fallback(
                &ask_job(Some("sid")),
                crate::llm::FALLBACK_NOTE,
                |_| {
                    n += 1;
                    Err(anyhow!(why))
                },
            );
            assert_eq!(r.unwrap_err().to_string(), why);
            assert_eq!(n, 1, "{why}");
        }
    }

    #[test]
    fn fallback_answer_is_marked_in_the_overlay() {
        let mut a = app();
        a.queue.push(review_with_cursor());
        press(&mut a, KeyCode::Char('W'));
        let ev = |text: String| {
            Msg::Llm(LlmEvent {
                card: 1,
                kind: JobKind::Ask {
                    index: 0,
                    session_id: None,
                    speed: Speed::LowEffort,
                },
                result: Ok(text),
            })
        };
        a.update(ev(format!("fine{FALLBACK_NOTE}")));
        let s = ask_state(&a);
        assert!(s.entries[0].fallback);
        assert_eq!(s.entries[0].answer, Answer::Done("fine".into()));
        let (rows, _) = s.lines(80);
        assert!(rows.iter().any(
            |(k, l)| *k == LineKind::Pending && l == "(answered without the session's history)"
        ));
    }

    #[test]
    fn hook_session_beats_agent_session_and_bad_ids_are_dropped() {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        let sc = StepCard {
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            ..Default::default()
        };
        let mut c = card(1, "/a", CardKind::Step(Box::new(sc)), 0);
        c.agent = a.agents[0].id.clone();
        a.queue.push(c);
        press(&mut a, KeyCode::Char('W'));
        assert_eq!(
            ask_state(&a).session_id.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        // A crafted id on the agent is dropped.
        let mut b = app();
        b.agents = vec![agent("%1", 1, "api", "/a", true)];
        b.agents[0].session_id = "--dangerously-skip-permissions".into();
        let mut c = review_with_cursor();
        c.agent = b.agents[0].id.clone();
        b.queue.push(c);
        press(&mut b, KeyCode::Char('W'));
        assert_eq!(ask_state(&b).session_id, None);
    }

    #[test]
    fn context_for_step_card_contains_after_text() {
        let sc = StepCard {
            tool_name: "Edit".into(),
            path: "/a/lib.rs".into(),
            description: Some("bump the limit".into()),
            preview: EditPreview::Placed {
                before: "let n = 1;\n".into(),
                after: "let n = 2;\nlet kiwi = 3;\n".into(),
                changed: vec![0..1],
            },
            ..Default::default()
        };
        let c = card(1, "/a", CardKind::Step(Box::new(sc)), 0);
        let ctx = context_for(&c);
        assert!(
            ctx.contains("let kiwi = 3;") && ctx.contains("/a/lib.rs"),
            "{ctx}"
        );
        assert!(ctx.contains("bump the limit"));
    }

    #[test]
    fn big_step_file_is_cut_to_the_change_and_under_the_cap() {
        let after: String = (0..20_000)
            .map(|i| format!("line {i} of the file\n"))
            .collect();
        let sc = StepCard {
            preview: EditPreview::Placed {
                before: String::new(),
                after,
                changed: vec![10_000..10_002],
            },
            ..Default::default()
        };
        let ctx = context_for(&card(1, "/a", CardKind::Step(Box::new(sc)), 0));
        assert!(ctx.len() <= CONTEXT_CAP, "{}", ctx.len());
        assert!(ctx.contains("line 10001 of the file") && ctx.contains("lines not shown"));
        assert!(!ctx.contains("line 5 of the file"));
    }

    #[test]
    fn permission_context_has_the_tool_input_and_prompt_fences_it() {
        let pc = PermissionCard {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "rm -rf ```x"}),
            ..Default::default()
        };
        let ctx = context_for(&card(1, "/a", CardKind::Permission(pc), 0));
        assert!(ctx.contains("rm -rf"));
        let p = ask_prompt(&ctx, "is this safe?");
        assert!(p.contains("data") && p.contains("````"), "{p}");
        assert!(p.trim_end().ends_with("is this safe?"));
    }

    #[test]
    fn huge_context_stays_near_the_cap() {
        let pc = PermissionCard {
            tool_name: "Write".into(),
            tool_input: serde_json::json!({"content": "é".repeat(100_000)}),
            ..Default::default()
        };
        let ctx = context_for(&card(1, "/a", CardKind::Permission(pc), 0));
        assert!(
            ctx.len() < CONTEXT_CAP + 512 && ctx.contains("[cut:"),
            "{}",
            ctx.len()
        );
    }
}
