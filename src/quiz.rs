//! Quiz cards: questions about what the agent's change does, checked locally
//! (multiple choice) or judged by the model (free text). Pure: no I/O.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::app::{App, Effect, Overlay};
use crate::git::{DiffLine, FileDiff, FileStatus, Hunk};
use crate::llm::{JUDGE_PROMPT, JobKind, LlmEvent, LlmJob, QUIZ_PROMPT};
use crate::model::{AgentId, Card, CardKind, Checkout};
use crate::review::{APPROVE_MESSAGE, LastRound, file_path};
use crate::ui::input::{InputEvent, TextInput};

/// Sent on Done when the user asked for fixes, one `- {fix}` line each.
pub const FIX_HEADING: &str = "Quiz follow-up. Please fix:";
/// The diff in a quiz prompt is cut to about this many bytes.
pub const DIFF_CAP: usize = 60 * 1024;
/// Lines of code either side of a question's `code_ref`.
pub const CONTEXT: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    Choice,
    Free,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodeRef {
    pub path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    pub kind: QuestionKind,
    pub question: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub answer_index: Option<usize>,
    pub explanation: String,
    #[serde(default)]
    pub must_cover: Vec<String>,
    pub code_ref: CodeRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeVerdict {
    Right,
    Partial,
    Wrong,
}

/// Where a judged answer's gap lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GapIn {
    Code,
    Understanding,
    #[serde(rename = "none")]
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Judgement {
    pub verdict: JudgeVerdict,
    pub gap: String,
    pub gap_in: GapIn,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Given {
    /// Index into the question's choices.
    Choice(usize),
    Free(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Right,
    Wrong,
    Judging,
    JudgeFailed(String),
    Judged(Judgement),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub given: Given,
    pub verdict: Verdict,
}

/// Lines of a file around a question's `code_ref`, as of the quiz's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub path: PathBuf,
    /// Line number of `lines[0]`.
    pub first: u32,
    pub lines: Vec<String>,
    pub target: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    Loading,
    Loaded(Snippet),
    Failed(String),
}

/// Asks the runtime for the code around a question's `code_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRequest {
    pub card: u64,
    pub question: usize,
    pub checkout: Checkout,
    pub tree: String,
    pub path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuizState {
    Generating,
    Failed(String),
    /// The question on screen. `answers.len()` is this index until it is
    /// answered, one more while its answer is judged or waits for `y`/`c`.
    Asking(usize),
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuizCard {
    pub state: QuizState,
    pub questions: Vec<Question>,
    pub answers: Vec<Answer>,
    pub fixes: Vec<String>,
    pub from_tree: String,
    pub to_tree: String,
    /// The diff text given to the model, kept for `r`.
    pub diff: String,
    /// Code around each question's `code_ref`, by question, once asked for.
    pub code: BTreeMap<usize, Code>,
}

/// A free-text answer being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerDraft {
    pub card: u64,
    pub question: usize,
    pub input: TextInput,
}

impl QuizCard {
    pub fn new(from_tree: String, to_tree: String, diff: String) -> Self {
        QuizCard {
            state: QuizState::Generating,
            questions: Vec::new(),
            answers: Vec::new(),
            fixes: Vec::new(),
            from_tree,
            to_tree,
            diff,
            code: BTreeMap::new(),
        }
    }

    /// Right answers, and answers with a verdict.
    pub fn score(&self) -> (usize, usize) {
        let mut right = 0;
        let mut total = 0;
        for a in &self.answers {
            match &a.verdict {
                Verdict::Right => (right, total) = (right + 1, total + 1),
                Verdict::Judged(j) if j.verdict == JudgeVerdict::Right => {
                    (right, total) = (right + 1, total + 1)
                }
                Verdict::Wrong | Verdict::Judged(_) => total += 1,
                Verdict::Judging | Verdict::JudgeFailed(_) => {}
            }
        }
        (right, total)
    }

    /// The current question's judgement when it found a gap in the code,
    /// so `y` or `c` is due.
    pub fn code_gap(&self) -> Option<&Judgement> {
        let QuizState::Asking(i) = self.state else {
            return None;
        };
        match &self.answers.get(i)?.verdict {
            Verdict::Judged(j) if j.gap_in == GapIn::Code => Some(j),
            _ => None,
        }
    }

    /// The question whose code `m` shows: the last answered one, or the
    /// current one before any answer.
    pub fn code_question(&self) -> Option<usize> {
        match self.state {
            QuizState::Asking(i) => Some(self.answers.len().saturating_sub(1).min(i)),
            QuizState::Done => self.answers.len().checked_sub(1),
            _ => None,
        }
    }
}

/// The model's questions, checked: 3 to 6, and every choice question with
/// 2 to 4 choices and a right one among them.
pub fn parse_questions(json: &str) -> Result<Vec<Question>, String> {
    #[derive(Deserialize)]
    struct Out {
        questions: Vec<Question>,
    }
    let out: Out = serde_json::from_str(json).map_err(|e| format!("unexpected questions: {e}"))?;
    let n = out.questions.len();
    if !(3..=6).contains(&n) {
        return Err(format!("expected 3 to 6 questions, got {n}"));
    }
    for (i, q) in out.questions.iter().enumerate() {
        if q.kind == QuestionKind::Choice {
            let ok = (2..=4).contains(&q.choices.len())
                && q.answer_index.is_some_and(|a| a < q.choices.len());
            if !ok {
                return Err(format!("question {} has no valid choices", i + 1));
            }
        }
    }
    Ok(out.questions)
}

pub fn parse_judgement(json: &str) -> Result<Judgement, String> {
    serde_json::from_str(json).map_err(|e| format!("unexpected judgement: {e}"))
}

pub fn quiz_prompt(diff: &str) -> String {
    format!("{QUIZ_PROMPT}\n## The change\n\n```diff\n{diff}```\n")
}

pub fn judge_prompt(q: &Question, code: Option<&Code>, answer: &str) -> String {
    let mut p = format!("{JUDGE_PROMPT}\n## Question\n\n{}\n", q.question);
    p.push_str("\n## Points a good answer makes\n\n");
    for m in &q.must_cover {
        p.push_str(&format!("- {m}\n"));
    }
    p.push_str(&format!(
        "\n## Code at {}:{}\n\n",
        q.code_ref.path.display(),
        q.code_ref.line
    ));
    match code {
        Some(Code::Loaded(s)) => {
            p.push_str("```\n");
            for (n, l) in (s.first..).zip(&s.lines) {
                p.push_str(&format!("{n:>5}  {l}\n"));
            }
            p.push_str("```\n");
        }
        Some(Code::Failed(e)) => p.push_str(&format!("(not available: {e})\n")),
        _ => p.push_str("(not available)\n"),
    }
    p.push_str(&format!("\n## The developer's answer\n\n{answer}\n"));
    p
}

pub fn fix_message(fixes: &[String]) -> String {
    let mut s = FIX_HEADING.to_string();
    for f in fixes {
        s.push_str("\n- ");
        s.push_str(f);
    }
    s
}

/// `CONTEXT` lines either side of `line` in `text`.
pub fn snippet(path: &Path, text: &str, line: u32) -> Snippet {
    let all: Vec<&str> = text.lines().collect();
    let target = line.clamp(1, (all.len() as u32).max(1));
    let first = target.saturating_sub(CONTEXT).max(1);
    let last = (target + CONTEXT).min(all.len() as u32);
    Snippet {
        path: path.to_path_buf(),
        first,
        lines: all
            .get(first as usize - 1..last as usize)
            .unwrap_or_default()
            .iter()
            .map(|l| l.to_string())
            .collect(),
        target,
    }
}

fn file_header(f: &FileDiff) -> String {
    let old = f.old_path.as_deref().unwrap_or(file_path(f));
    let new = file_path(f);
    let mut s = format!("diff --git a/{} b/{}\n", old.display(), new.display());
    let side = |p: Option<&Path>, pre: &str| {
        p.map_or_else(|| "/dev/null".into(), |p| format!("{pre}/{}", p.display()))
    };
    let (old_side, new_side) = match f.status {
        FileStatus::Added | FileStatus::Untracked => ("/dev/null".into(), side(Some(new), "b")),
        FileStatus::Deleted => (side(Some(old), "a"), "/dev/null".into()),
        _ => (side(Some(old), "a"), side(Some(new), "b")),
    };
    if f.binary {
        s.push_str("Binary file changed\n");
    } else {
        s.push_str(&format!("--- {old_side}\n+++ {new_side}\n"));
    }
    s
}

fn hunk_text(h: &Hunk) -> String {
    let mut s = format!(
        "@@ -{},{} +{},{} @@\n",
        h.old_start, h.old_len, h.new_start, h.new_len
    );
    for l in &h.lines {
        let (sigil, text) = match l {
            DiffLine::Context { text, .. } => (' ', text),
            DiffLine::Add { text, .. } => ('+', text),
            DiffLine::Del { text, .. } => ('-', text),
        };
        s.push(sigil);
        s.push_str(text);
        s.push('\n');
    }
    s
}

/// The diff as unified text. Over `cap` bytes, it is every changed file with
/// its line counts, then the largest hunks that fit, with a note saying so.
pub fn diff_text(files: &[FileDiff], cap: usize) -> String {
    let full: String = files
        .iter()
        .map(|f| file_header(f) + &f.hunks.iter().map(hunk_text).collect::<String>())
        .collect();
    if full.len() <= cap {
        return full;
    }
    let mut out = format!(
        "NOTE: the diff is {} KB, over the {} KB limit. Below is every changed \
         file, then only the largest hunks. Read the files for the rest.\n\nChanged files:\n",
        full.len() / 1024,
        cap / 1024
    );
    for f in files {
        let count = |add: bool| {
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| match l {
                    DiffLine::Add { .. } => add,
                    DiffLine::Del { .. } => !add,
                    DiffLine::Context { .. } => false,
                })
                .count()
        };
        out.push_str(&format!(
            "- {} (+{} -{})\n",
            file_path(f).display(),
            count(true),
            count(false)
        ));
    }
    out.push('\n');
    let mut hunks: Vec<(usize, usize, String)> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| f.hunks.iter().enumerate().map(move |(hi, h)| (fi, hi, h)))
        .map(|(fi, hi, h)| (fi, hi, hunk_text(h)))
        .collect();
    hunks.sort_by_key(|(_, _, t)| std::cmp::Reverse(t.len()));
    let mut room = cap.saturating_sub(out.len());
    let mut kept = Vec::new();
    for (fi, hi, text) in hunks {
        let need = text.len() + file_header(&files[fi]).len();
        if need <= room {
            room -= need;
            kept.push((fi, hi, text));
        }
    }
    kept.sort_by_key(|(fi, hi, _)| (*fi, *hi));
    let mut last_file = None;
    for (fi, _, text) in kept {
        if last_file != Some(fi) {
            out.push_str(&file_header(&files[fi]));
            last_file = Some(fi);
        }
        out.push_str(&text);
    }
    out
}

impl App {
    /// The diff a `ComputeQuiz` asked for: a quiz card for a non-empty one,
    /// unless the checkout already has a quiz waiting.
    pub(crate) fn on_quiz_diff(
        &mut self,
        checkout: Checkout,
        agent: AgentId,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        at: SystemTime,
    ) -> Vec<Effect> {
        let open = self
            .queue
            .iter()
            .any(|c| c.checkout == checkout && matches!(c.kind, CardKind::Quiz(_)));
        if files.is_empty() || open {
            return Vec::new();
        }
        let diff = diff_text(&files, DIFF_CAP);
        self.next_card_id += 1;
        let id = self.next_card_id;
        let job = quiz_job(id, &checkout, &diff);
        self.queue.push(Card {
            id,
            checkout,
            agent,
            created: at,
            kind: CardKind::Quiz(QuizCard::new(from_tree, to_tree, diff)),
        });
        vec![Effect::Llm(job)]
    }

    pub(crate) fn on_llm(&mut self, ev: LlmEvent) -> Vec<Effect> {
        let Some((_, qc)) = self.quiz_mut(ev.card) else {
            return Vec::new();
        };
        match ev.kind {
            JobKind::Quiz => {
                if qc.state != QuizState::Generating {
                    return Vec::new();
                }
                qc.state = match ev.result.and_then(|j| parse_questions(&j)) {
                    Ok(questions) => {
                        qc.questions = questions;
                        QuizState::Asking(0)
                    }
                    Err(e) => QuizState::Failed(e),
                };
                Vec::new()
            }
            JobKind::Judge { question } => {
                let Some(answer) = qc.answers.get_mut(question) else {
                    return Vec::new();
                };
                if answer.verdict != Verdict::Judging {
                    return Vec::new();
                }
                match ev.result.and_then(|j| parse_judgement(&j)) {
                    Err(e) => {
                        answer.verdict = Verdict::JudgeFailed(e);
                        Vec::new()
                    }
                    Ok(j) => {
                        let code_gap = j.gap_in == GapIn::Code;
                        answer.verdict = Verdict::Judged(j);
                        if code_gap {
                            Vec::new()
                        } else {
                            self.next_question(ev.card)
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn on_code_read(
        &mut self,
        card: u64,
        question: usize,
        result: Result<Snippet, String>,
    ) -> Vec<Effect> {
        let Some((checkout, qc)) = self.quiz_mut(card) else {
            return Vec::new();
        };
        qc.code.insert(
            question,
            match result {
                Ok(s) => Code::Loaded(s),
                Err(e) => Code::Failed(e),
            },
        );
        // A free answer waiting for this code to be judged.
        match qc.answers.get(question) {
            Some(Answer {
                given: Given::Free(text),
                verdict: Verdict::Judging,
            }) => vec![Effect::Llm(judge_job(card, &checkout, qc, question, text))],
            _ => Vec::new(),
        }
    }

    pub(crate) fn on_quiz_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let id = card.id;
        let checkout = card.checkout.clone();
        let CardKind::Quiz(qc) = &mut card.kind else {
            return Vec::new();
        };
        let i = match (&qc.state, key.code) {
            (QuizState::Failed(_), KeyCode::Char('r')) => {
                qc.state = QuizState::Generating;
                return vec![Effect::Llm(quiz_job(id, &checkout, &qc.diff))];
            }
            (QuizState::Generating, KeyCode::Char('r')) => {
                self.set_status("Still writing the questions");
                return Vec::new();
            }
            (QuizState::Done, KeyCode::Enter) => return self.finish_quiz(id),
            (_, KeyCode::Char('m')) => {
                let Some(q) = qc.code_question() else {
                    return Vec::new();
                };
                let effects = request_code(id, &checkout, qc, q);
                self.overlay = Some(Overlay::QuizCode {
                    card: id,
                    question: q,
                });
                return effects;
            }
            (QuizState::Asking(i), _) => *i,
            _ => return Vec::new(),
        };
        let q = &qc.questions[i];
        let answered = qc.answers.len() > i;
        match key.code {
            KeyCode::Char(d @ '1'..='4') if !answered && q.kind == QuestionKind::Choice => {
                let picked = d as usize - '1' as usize;
                if picked >= q.choices.len() {
                    return Vec::new();
                }
                let right = q.answer_index == Some(picked);
                qc.answers.push(Answer {
                    given: Given::Choice(picked),
                    verdict: if right {
                        Verdict::Right
                    } else {
                        Verdict::Wrong
                    },
                });
                let mut effects = if right {
                    Vec::new()
                } else {
                    request_code(id, &checkout, qc, i)
                };
                effects.extend(self.next_question(id));
                effects
            }
            KeyCode::Char('i') if !answered && q.kind == QuestionKind::Free => {
                self.overlay = Some(Overlay::QuizAnswer(AnswerDraft {
                    card: id,
                    question: i,
                    input: TextInput::multi_line(),
                }));
                Vec::new()
            }
            KeyCode::Char('c') if qc.code_gap().is_some() => {
                let gap = qc.code_gap().map(|j| j.gap.clone()).unwrap_or_default();
                qc.fixes.push(format!("Fix: {gap}"));
                self.next_question(id)
            }
            KeyCode::Char('y') if qc.code_gap().is_some() => self.next_question(id),
            KeyCode::Char('r') | KeyCode::Enter => {
                let Some(Answer {
                    given: Given::Free(text),
                    verdict: verdict @ Verdict::JudgeFailed(_),
                }) = qc.answers.get_mut(i)
                else {
                    return Vec::new();
                };
                if key.code == KeyCode::Enter {
                    // Skip it: the question stays unscored.
                    return self.next_question(id);
                }
                *verdict = Verdict::Judging;
                let text = text.clone();
                vec![Effect::Llm(judge_job(id, &checkout, qc, i, &text))]
            }
            _ => Vec::new(),
        }
    }

    /// Keys while typing a free-text answer. Ctrl-S, or Enter on an empty
    /// last line, submits.
    pub(crate) fn on_quiz_answer_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::QuizAnswer(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let input = &mut draft.input;
        let enter_submits = key.code == KeyCode::Enter
            && key.modifiers == KeyModifiers::NONE
            && input.on_empty_last_line()
            && !input.text().trim().is_empty();
        let event = if enter_submits {
            InputEvent::Submit(input.text())
        } else {
            input.handle_key(key)
        };
        let text = match event {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => {
                self.overlay = None;
                return Vec::new();
            }
            InputEvent::Submit(text) => text.trim().to_string(),
        };
        let Some(Overlay::QuizAnswer(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        let (card, i) = (draft.card, draft.question);
        let Some((checkout, qc)) = self.quiz_mut(card) else {
            return Vec::new();
        };
        if qc.state != QuizState::Asking(i) || qc.answers.len() != i {
            return Vec::new();
        }
        qc.answers.push(Answer {
            given: Given::Free(text.clone()),
            verdict: Verdict::Judging,
        });
        match qc.code.get(&i) {
            // The code comes first; `on_code_read` sends the judge job.
            Some(Code::Loading) => Vec::new(),
            Some(_) => vec![Effect::Llm(judge_job(card, &checkout, qc, i, &text))],
            None => request_code(card, &checkout, qc, i),
        }
    }

    /// Esc, `q` or `m` closes the code view.
    pub(crate) fn on_quiz_code_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('m')
        ) {
            self.overlay = None;
        }
    }

    /// Moves past the current question, finishing the quiz after the last.
    fn next_question(&mut self, id: u64) -> Vec<Effect> {
        let Some((_, qc)) = self.quiz_mut(id) else {
            return Vec::new();
        };
        let QuizState::Asking(i) = qc.state else {
            return Vec::new();
        };
        if i + 1 < qc.questions.len() {
            qc.state = QuizState::Asking(i + 1);
            return Vec::new();
        }
        qc.state = QuizState::Done;
        self.finish_quiz(id)
    }

    /// Sends the fixes, or approves. The card closes once the send lands,
    /// or at once when there is nothing to send: no fixes and the agent is
    /// working or gone.
    fn finish_quiz(&mut self, id: u64) -> Vec<Effect> {
        if self.sending.contains_key(&id) {
            self.set_status("Still sending");
            return Vec::new();
        }
        let Some(card) = self.queue.iter().find(|c| c.id == id) else {
            return Vec::new();
        };
        let CardKind::Quiz(qc) = &card.kind else {
            return Vec::new();
        };
        let agent = self.agent(&card.agent);
        let (text, done) = if qc.fixes.is_empty() {
            if !agent.is_some_and(|a| a.waiting) {
                return self.close_quiz(id, "Quiz done".into());
            }
            (APPROVE_MESSAGE.to_string(), "Approved".to_string())
        } else {
            let Some(agent) = agent else {
                self.set_status("agent gone");
                return Vec::new();
            };
            let n = qc.fixes.len();
            let s = if n == 1 { "" } else { "es" };
            (
                fix_message(&qc.fixes),
                format!("Sent {n} fix{s} to {}", agent.window_name),
            )
        };
        let agent = card.agent.clone();
        self.sending.insert(id, done);
        self.set_status("Sending");
        vec![Effect::Send {
            agent,
            text,
            card: Some(id),
        }]
    }

    /// Removes quiz card `id`; its tree is where the next round starts.
    pub(crate) fn close_quiz(&mut self, id: u64, status: String) -> Vec<Effect> {
        let Some(Card {
            checkout,
            kind: CardKind::Quiz(qc),
            ..
        }) = self.queue.remove(id)
        else {
            return Vec::new();
        };
        self.rounds
            .entry(checkout.0)
            .and_modify(|r| r.tree = qc.to_tree.clone())
            .or_insert(LastRound {
                tree: qc.to_tree,
                round: 0,
                comments: Vec::new(),
            });
        self.set_status(status);
        vec![Effect::SaveRounds]
    }

    fn quiz_mut(&mut self, id: u64) -> Option<(Checkout, &mut QuizCard)> {
        let card = self.queue.find_mut(|c| c.id == id)?;
        match &mut card.kind {
            CardKind::Quiz(qc) => Some((card.checkout.clone(), qc)),
            _ => None,
        }
    }
}

fn quiz_job(card: u64, checkout: &Checkout, diff: &str) -> LlmJob {
    LlmJob {
        card,
        kind: JobKind::Quiz,
        cwd: checkout.0.clone(),
        prompt: quiz_prompt(diff),
    }
}

fn judge_job(card: u64, checkout: &Checkout, qc: &QuizCard, i: usize, answer: &str) -> LlmJob {
    LlmJob {
        card,
        kind: JobKind::Judge { question: i },
        cwd: checkout.0.clone(),
        prompt: judge_prompt(&qc.questions[i], qc.code.get(&i), answer),
    }
}

/// Asks for question `i`'s code unless it is loaded or on its way.
fn request_code(card: u64, checkout: &Checkout, qc: &mut QuizCard, i: usize) -> Vec<Effect> {
    if qc.code.contains_key(&i) {
        return Vec::new();
    }
    qc.code.insert(i, Code::Loading);
    let r = &qc.questions[i].code_ref;
    vec![Effect::ReadCode(CodeRequest {
        card,
        question: i,
        checkout: checkout.clone(),
        tree: qc.to_tree.clone(),
        path: r.path.clone(),
        line: r.line,
    })]
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use anyhow::anyhow;

    use super::*;
    use crate::app::Msg;
    use crate::app::tests::agent;
    use crate::config::Config;
    use crate::llm::{FakeLlm, run_job};
    use crate::model::Mode;
    use crate::theme::builtin_themes;

    pub(crate) const QUESTIONS: &str = r#"{"questions":[
        {"kind":"choice","question":"If the token store is unreachable, what does a request get?",
         "choices":["it is let through","429 Too Many Requests","503 Service Unavailable"],
         "answer_index":2,
         "explanation":"limit.rs:40 maps a store error to StoreDown, which http.rs:87 turns into 503.",
         "code_ref":{"path":"src/limit.rs","line":40}},
        {"kind":"free","question":"Why does check() take a token only after the zero test?",
         "must_cover":["no negative count","rejected requests cost nothing"],
         "explanation":"limit.rs:44 tests before it takes.",
         "code_ref":{"path":"src/limit.rs","line":44}},
        {"kind":"choice","question":"What does a burst of 20 get?",
         "choices":["all pass","10 pass"],"answer_index":1,
         "explanation":"capacity is 10 at limit.rs:12.",
         "code_ref":{"path":"src/limit.rs","line":12}}
    ]}"#;

    pub(crate) const CODE_GAP: &str = r#"{"verdict":"partial","gap":"two requests can both pass the zero test","gap_in":"code","explanation":"no lock at limit.rs:44"}"#;
    const UNDERSTANDING_GAP: &str = r#"{"verdict":"partial","gap":"it is not about overflow","gap_in":"understanding","explanation":"limit.rs:44"}"#;

    fn key(c: char) -> Msg {
        Msg::Key(KeyEvent::from(KeyCode::Char(c)), Instant::now())
    }

    fn code(c: KeyCode) -> Msg {
        Msg::Key(KeyEvent::from(c), Instant::now())
    }

    fn quiz(a: &App) -> &QuizCard {
        match &a.queue.current().expect("a card").kind {
            CardKind::Quiz(qc) => qc,
            k => panic!("not a quiz card: {k:?}"),
        }
    }

    fn file() -> FileDiff {
        crate::git::parse_unified(crate::review::tests::FIXTURE.as_bytes()).remove(0)
    }

    /// An app with agent `%1` (window `api`) in quiz-mode `/a`, which just
    /// finished; returns the quiz job.
    pub(crate) fn generating() -> (App, LlmJob) {
        let mut a = App::new(Config::default(), builtin_themes());
        a.config.set_mode(&Checkout("/a".into()), Mode::Quiz);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let [
            Effect::ComputeQuiz {
                checkout,
                agent,
                from_tree,
            },
        ] = effects.as_slice()
        else {
            panic!("expected one ComputeQuiz, got {effects:?}");
        };
        assert_eq!(from_tree, &None);
        let effects = a.update(Msg::QuizDiff {
            checkout: checkout.clone(),
            agent: agent.clone(),
            from_tree: "base".into(),
            to_tree: "t1".into(),
            files: vec![file()],
            at: SystemTime::UNIX_EPOCH,
        });
        let [Effect::Llm(job)] = effects.as_slice() else {
            panic!("expected the quiz job, got {effects:?}");
        };
        assert_eq!(quiz(&a).state, QuizState::Generating);
        (a, job.clone())
    }

    /// A quiz with the three `QUESTIONS`, on the first.
    pub(crate) fn asking() -> App {
        let (mut a, job) = generating();
        let fake = FakeLlm::new(vec![Ok(QUESTIONS.into())]);
        assert!(a.update(Msg::Llm(run_job(&fake, job))).is_empty());
        assert_eq!(quiz(&a).state, QuizState::Asking(0));
        a
    }

    fn snippet_for(a: &App, q: usize) -> Snippet {
        let r = &quiz(a).questions[q].code_ref;
        let text: String = (1..=60).map(|n| format!("line {n}\n")).collect();
        snippet(&r.path, &text, r.line)
    }

    /// Types `text` as the answer to the current free question and submits
    /// it; returns the judge job once the code is in.
    pub(crate) fn answer_free(a: &mut App, text: &str) -> LlmJob {
        a.update(key('i'));
        assert!(matches!(a.overlay, Some(Overlay::QuizAnswer(_))));
        for c in text.chars() {
            a.update(key(c));
        }
        let effects = a.update(Msg::Key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Instant::now(),
        ));
        assert_eq!(a.overlay, None);
        let [Effect::ReadCode(req)] = effects.as_slice() else {
            panic!("expected a code read, got {effects:?}");
        };
        let s = snippet_for(a, req.question);
        let effects = a.update(Msg::CodeRead {
            card: req.card,
            question: req.question,
            result: Ok(s),
        });
        let [Effect::Llm(job)] = effects.as_slice() else {
            panic!("expected the judge job, got {effects:?}");
        };
        assert!(job.prompt.contains("   44  line 44"), "{}", job.prompt);
        assert!(job.prompt.contains(text));
        assert!(job.prompt.contains("- rejected requests cost nothing"));
        job.clone()
    }

    #[test]
    fn choice_wrong_shows_explanation() {
        let mut a = asking();
        let effects = a.update(key('1'));
        let [Effect::ReadCode(req)] = effects.as_slice() else {
            panic!("a wrong answer loads its code, got {effects:?}");
        };
        assert_eq!(
            (req.path.as_path(), req.line),
            (Path::new("src/limit.rs"), 40)
        );
        assert_eq!(req.tree, "t1");
        let qc = quiz(&a);
        assert_eq!(qc.answers[0].verdict, Verdict::Wrong);
        assert_eq!(qc.state, QuizState::Asking(1), "on to the next question");
        assert_eq!(qc.score(), (0, 1));
        let s = snippet_for(&a, 0);
        a.update(Msg::CodeRead {
            card: req.card,
            question: 0,
            result: Ok(s),
        });
        let screen = crate::ui::tests::render(&a, 100, 30);
        assert!(screen.contains("StoreDown"), "{screen}");
        assert!(screen.contains("line 40"), "{screen}");
        assert!(screen.contains("score so far 0/1"), "{screen}");
    }

    #[test]
    fn choice_right_scores_and_loads_nothing() {
        let mut a = asking();
        assert!(a.update(key('3')).is_empty());
        assert_eq!(quiz(&a).score(), (1, 1));
        // `4` is not a choice of a three-choice question.
        let mut b = asking();
        assert!(b.update(key('4')).is_empty());
        assert!(quiz(&b).answers.is_empty());
    }

    #[test]
    fn free_answer_code_gap_offers_fix() {
        let mut a = asking();
        a.update(key('3'));
        let job = answer_free(&mut a, "so it never goes negative");
        assert_eq!(quiz(&a).answers[1].verdict, Verdict::Judging);
        let fake = FakeLlm::new(vec![Ok(CODE_GAP.into())]);
        assert!(a.update(Msg::Llm(run_job(&fake, job))).is_empty());
        let qc = quiz(&a);
        assert_eq!(qc.state, QuizState::Asking(1), "waits for y or c");
        assert!(qc.code_gap().is_some());
        let screen = crate::ui::tests::render(&a, 100, 30);
        assert!(screen.contains("c fix it"), "{screen}");
        a.update(key('c'));
        let qc = quiz(&a);
        assert_eq!(qc.fixes, ["Fix: two requests can both pass the zero test"]);
        assert_eq!(qc.state, QuizState::Asking(2));
    }

    #[test]
    fn y_keeps_the_code_as_is() {
        let mut a = asking();
        a.update(key('3'));
        let job = answer_free(&mut a, "x");
        let fake = FakeLlm::new(vec![Ok(CODE_GAP.into())]);
        a.update(Msg::Llm(run_job(&fake, job)));
        a.update(key('y'));
        assert!(quiz(&a).fixes.is_empty());
        assert_eq!(quiz(&a).state, QuizState::Asking(2));
    }

    #[test]
    fn understanding_gap_offers_no_fix() {
        let mut a = asking();
        a.update(key('3'));
        let job = answer_free(&mut a, "overflow");
        let fake = FakeLlm::new(vec![Ok(UNDERSTANDING_GAP.into())]);
        a.update(Msg::Llm(run_job(&fake, job)));
        let qc = quiz(&a);
        assert_eq!(qc.state, QuizState::Asking(2), "straight on");
        assert_eq!(qc.code_gap(), None);
        assert!(a.update(key('c')).is_empty());
        assert!(quiz(&a).fixes.is_empty());
        let screen = crate::ui::tests::render(&a, 100, 30);
        assert!(screen.contains("it is not about overflow"), "{screen}");
    }

    #[test]
    fn judge_failure_retries_or_skips() {
        let mut a = asking();
        a.update(key('3'));
        let job = answer_free(&mut a, "x");
        let fake = FakeLlm::new(vec![Err(anyhow!("timed out"))]);
        a.update(Msg::Llm(run_job(&fake, job.clone())));
        assert!(matches!(
            quiz(&a).answers[1].verdict,
            Verdict::JudgeFailed(_)
        ));
        assert_eq!(a.update(key('r')), [Effect::Llm(job.clone())]);
        a.update(Msg::Llm(run_job(&fake, job)));
        a.update(code(KeyCode::Enter));
        assert_eq!(quiz(&a).state, QuizState::Asking(2));
        assert_eq!(quiz(&a).score(), (1, 1));
    }

    fn to_done(a: &mut App, fix: bool) -> Vec<Effect> {
        a.update(key('3'));
        let job = answer_free(a, "x");
        let fake = FakeLlm::new(vec![Ok(CODE_GAP.into())]);
        a.update(Msg::Llm(run_job(&fake, job)));
        a.update(key(if fix { 'c' } else { 'y' }));
        a.update(key('2'))
    }

    #[test]
    fn done_with_fixes_sends_list() {
        let mut a = asking();
        let id = a.queue.current().unwrap().id;
        let effects = to_done(&mut a, true);
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text:
                    "Quiz follow-up. Please fix:\n- Fix: two requests can both pass the zero test"
                        .into(),
                card: Some(id),
            }]
        );
        assert_eq!(quiz(&a).state, QuizState::Done);
        assert_eq!(a.queue.len(), 1, "the card waits for the send");
        assert_eq!(a.update(Msg::SendDone { card: id }), [Effect::SaveRounds]);
        assert!(a.queue.is_empty());
        assert_eq!(a.rounds[&PathBuf::from("/a")].tree, "t1");
        assert_eq!(a.status_text(), Some("Sent 1 fix to api"));
    }

    #[test]
    fn done_without_fixes_approves() {
        let mut a = asking();
        let id = a.queue.current().unwrap().id;
        let effects = to_done(&mut a, false);
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Approved, carry on.".into(),
                card: Some(id),
            }]
        );
        a.update(Msg::SendFailed {
            card: id,
            error: "agent gone".into(),
        });
        assert_eq!(a.queue.len(), 1, "a failed send keeps the card");
        assert!(matches!(
            a.update(code(KeyCode::Enter)).as_slice(),
            [Effect::Send { .. }]
        ));
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        let last = &a.rounds[&PathBuf::from("/a")];
        assert_eq!((last.tree.as_str(), last.round), ("t1", 0));
        // The next round diffs from here.
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ComputeQuiz { from_tree: Some(t), .. }] if t == "t1"
        ));
    }

    #[test]
    fn quiz_generation_failure_is_shown() {
        let (mut a, job) = generating();
        let fake = FakeLlm::new(vec![Ok("{\"questions\": \"nope\"}".into())]);
        a.update(Msg::Llm(run_job(&fake, job.clone())));
        assert!(matches!(quiz(&a).state, QuizState::Failed(_)));
        let screen = crate::ui::tests::render(&a, 100, 20);
        assert!(screen.contains("Could not write the questions"), "{screen}");
        assert_eq!(a.update(key('r')), [Effect::Llm(job)]);
        assert_eq!(quiz(&a).state, QuizState::Generating);
    }

    #[test]
    fn m_shows_the_code_of_the_last_answer() {
        let mut a = asking();
        let effects = a.update(key('m'));
        assert!(matches!(effects.as_slice(), [Effect::ReadCode(r)] if r.question == 0));
        assert!(matches!(
            a.overlay,
            Some(Overlay::QuizCode { question: 0, .. })
        ));
        a.update(code(KeyCode::Esc));
        assert_eq!(a.overlay, None);
        // Its code is on the way, so a wrong answer asks for nothing more.
        assert!(a.update(key('1')).is_empty());
        a.update(key('m'));
        assert!(matches!(
            a.overlay,
            Some(Overlay::QuizCode { question: 0, .. })
        ));
    }

    #[test]
    fn empty_diff_or_open_quiz_makes_no_card() {
        let (mut a, _) = generating();
        let msg = |files| Msg::QuizDiff {
            checkout: Checkout("/a".into()),
            agent: a.agents[0].id.clone(),
            from_tree: "t1".into(),
            to_tree: "t2".into(),
            files,
            at: SystemTime::UNIX_EPOCH,
        };
        let (empty, more) = (msg(Vec::new()), msg(vec![file()]));
        assert!(a.update(empty).is_empty());
        assert!(a.update(more).is_empty());
        assert_eq!(a.queue.len(), 1);
    }

    #[test]
    fn bad_choices_fail_the_parse() {
        let bad = QUESTIONS.replace("\"answer_index\":2", "\"answer_index\":3");
        assert!(parse_questions(&bad).is_err());
        assert_eq!(parse_questions(QUESTIONS).unwrap().len(), 3);
    }

    #[test]
    fn big_diff_keeps_file_list_and_largest_hunks() {
        let mut f = file();
        let hunk = |n: usize| Hunk {
            old_start: 1,
            old_len: 0,
            new_start: 1,
            new_len: n as u32,
            lines: (0..n)
                .map(|i| DiffLine::Add {
                    new: i as u32 + 1,
                    text: format!("big line {n} {i}"),
                })
                .collect(),
        };
        f.hunks = vec![hunk(5), hunk(400), hunk(50)];
        let full = diff_text(std::slice::from_ref(&f), usize::MAX);
        assert!(full.starts_with("diff --git a/src/limit.rs b/src/limit.rs\n--- a/src/limit.rs\n+++ b/src/limit.rs\n@@ -1,0 +1,5 @@\n+big line 5 0\n"));
        let cut = diff_text(std::slice::from_ref(&f), 3000);
        assert!(cut.len() <= 3000, "{}", cut.len());
        assert!(cut.starts_with("NOTE: the diff is"), "{cut}");
        assert!(cut.contains("- src/limit.rs (+455 -0)"), "{cut}");
        assert!(!cut.contains("big line 400"), "too big to fit");
        let (fifty, five) = (cut.find("big line 50 0"), cut.find("big line 5 0"));
        assert!(fifty.is_some() && five.is_some(), "{cut}");
        assert!(five < fifty, "kept in file order");
    }

    #[test]
    fn snippet_clamps_to_the_file() {
        let s = snippet(Path::new("a.rs"), "one\ntwo\nthree\n", 99);
        assert_eq!((s.first, s.target, s.lines.len()), (1, 3, 3));
        let text: String = (1..=100).map(|n| format!("{n}\n")).collect();
        let s = snippet(Path::new("a.rs"), &text, 50);
        assert_eq!((s.first, s.lines.len()), (30, 41));
    }
}
