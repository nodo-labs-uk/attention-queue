//! Saved state, kept across restarts in `state.json` in the state dir: each
//! checkout's last review round, draft review comments and quiz progress.
//! Cards are not saved. A saved draft or quiz goes back on its card when
//! the same card (same checkout, from tree and to tree) is made again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::config::{Paths, write_atomic};
use crate::model::{CardKind, Checkout, Mode};
use crate::quiz::{Answer, Question, QuizCard, QuizState, Verdict};
use crate::review::{Comment, LastRound};

/// What an answer the judge was still on when aq closed shows after a restart.
pub const NOT_JUDGED: &str = "Not judged: aq closed first. r judges it again.";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    /// Last sent review round per checkout root.
    pub rounds: BTreeMap<PathBuf, LastRound>,
    pub drafts: Vec<Draft>,
    pub quizzes: Vec<QuizProgress>,
}

/// The card a draft or quiz belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardKey {
    pub checkout: PathBuf,
    pub from_tree: String,
    pub to_tree: String,
}

/// A review card's comments for its round, not sent yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub card: CardKey,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuizProgress {
    pub card: CardKey,
    pub questions: Vec<Question>,
    pub answers: Vec<Answer>,
    pub fixes: Vec<String>,
    /// The question on screen; `None` once every question is answered.
    pub asking: Option<usize>,
}

/// Saved drafts and quizzes whose card has not been made again yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unclaimed {
    pub drafts: Vec<Draft>,
    pub quizzes: Vec<QuizProgress>,
}

impl Unclaimed {
    pub fn has(&self, checkout: &Checkout) -> bool {
        let here = |k: &CardKey| k.checkout == checkout.0;
        self.drafts.iter().any(|d| here(&d.card)) || self.quizzes.iter().any(|q| here(&q.card))
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty() && self.quizzes.is_empty()
    }

    /// Drops every save for `checkout`.
    pub fn forget(&mut self, checkout: &Path) {
        self.drafts.retain(|d| d.card.checkout != checkout);
        self.quizzes.retain(|q| q.card.checkout != checkout);
    }

    /// The comments saved for the review card `key`. Every other save for
    /// its checkout is dropped: a new card there means the work moved on.
    pub fn take_draft(&mut self, key: &CardKey) -> Option<Vec<Comment>> {
        let found = self.drafts.iter().find(|d| d.card == *key);
        let found = found.map(|d| d.comments.clone());
        self.forget(&key.checkout);
        found
    }

    /// The progress saved for the quiz card `key`, dropping every other
    /// save for its checkout.
    pub fn take_quiz(&mut self, key: &CardKey) -> Option<QuizProgress> {
        let found = self.quizzes.iter().find(|q| q.card == *key).cloned();
        self.forget(&key.checkout);
        found
    }
}

impl Draft {
    /// The review card's comments of its round, if it has any, keyed by
    /// where the round starts whatever view is on screen.
    fn of(checkout: &Checkout, rc: &crate::review::ReviewCard) -> Option<Draft> {
        let comments: Vec<Comment> = rc.current_comments().cloned().collect();
        (!comments.is_empty()).then(|| Draft {
            card: CardKey {
                checkout: checkout.0.clone(),
                from_tree: rc.round_base.clone(),
                to_tree: rc.to_tree.clone(),
            },
            comments,
        })
    }
}

impl QuizProgress {
    /// The quiz's progress once it has questions.
    fn of(checkout: &Checkout, qc: &QuizCard) -> Option<QuizProgress> {
        let asking = match qc.state {
            QuizState::Asking(i) => Some(i),
            QuizState::Done => None,
            QuizState::Generating | QuizState::Failed(_) => return None,
        };
        Some(QuizProgress {
            card: CardKey {
                checkout: checkout.0.clone(),
                from_tree: qc.from_tree.clone(),
                to_tree: qc.to_tree.clone(),
            },
            questions: qc.questions.clone(),
            answers: qc.answers.clone(),
            fixes: qc.fixes.clone(),
            asking,
        })
    }

    /// The quiz card again. An answer that was being judged can be judged
    /// again with `r`.
    pub fn into_card(self, diff: String) -> QuizCard {
        let mut qc = QuizCard::new(self.card.from_tree, self.card.to_tree, diff);
        qc.questions = self.questions;
        qc.answers = self.answers;
        for a in &mut qc.answers {
            if a.verdict == Verdict::Judging {
                a.verdict = Verdict::JudgeFailed(NOT_JUDGED.into());
            }
        }
        qc.fixes = self.fixes;
        qc.state = match self.asking {
            Some(i) if i < qc.questions.len() => QuizState::Asking(i),
            _ => QuizState::Done,
        };
        qc
    }
}

impl App {
    /// Takes back what an earlier run saved. A draft or quiz whose
    /// checkout is in another mode now is dropped: no card would claim it.
    pub fn restore(&mut self, mut state: SavedState) {
        let mode = |k: &CardKey| self.config.mode_for(&Checkout(k.checkout.clone()));
        state.drafts.retain(|d| mode(&d.card) == Mode::Review);
        state.quizzes.retain(|q| mode(&q.card) == Mode::Quiz);
        self.rounds = state.rounds.clone();
        self.unclaimed = Unclaimed {
            drafts: state.drafts.clone(),
            quizzes: state.quizzes.clone(),
        };
        self.saved = Some(state);
    }

    /// What is worth keeping across a restart now, unclaimed saves included.
    pub fn saved_state(&self) -> SavedState {
        let mut state = SavedState {
            rounds: self.rounds.clone(),
            drafts: self.unclaimed.drafts.clone(),
            quizzes: self.unclaimed.quizzes.clone(),
        };
        for card in self.queue.iter() {
            match &card.kind {
                CardKind::Review(rc) => state.drafts.extend(Draft::of(&card.checkout, rc)),
                CardKind::Quiz(qc) => state.quizzes.extend(QuizProgress::of(&card.checkout, qc)),
                _ => {}
            }
        }
        state
    }

    /// The state to write, when it changed since it was last handed out.
    pub fn state_to_save(&mut self) -> Option<SavedState> {
        let state = self.saved_state();
        if self.saved.as_ref() == Some(&state) {
            return None;
        }
        self.saved = Some(state.clone());
        Some(state)
    }
}

pub fn state_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("state.json")
}

/// A missing file means nothing was saved yet.
pub fn load(path: &Path) -> Result<SavedState> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SavedState::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, state: &SavedState) -> Result<()> {
    let text = serde_json::to_string_pretty(state).context("serialising saved state")?;
    write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::app::tests::{
        agent, app, computed, ctrl, finish_round, fixture_files, key, review, typed,
    };
    use crate::app::{Effect, Msg, ReviewRequest};
    use crate::quiz::tests::{answer_free, asking, file, quiz};
    use crate::review::Side;

    /// aq starts again with the state `a` saved, and sees agent `%1` in
    /// `/a` already waiting. Returns the new app and what it asked for.
    fn restart(a: &mut App, mode: Mode) -> (App, Vec<Effect>) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/state.json");
        save(&file, &a.state_to_save().expect("something to save")).unwrap();
        let mut b = app();
        b.config.set_mode(&Checkout("/a".into()), mode);
        b.restore(load(&file).unwrap());
        let effects = b.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        (b, effects)
    }

    #[test]
    fn draft_comment_survives_restart() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "store down?");
        a.update(ctrl('s'));
        let (mut b, effects) = restart(&mut a, Mode::Review);
        assert_eq!(effects, [Effect::ComputeReview(req.clone())]);
        assert_eq!(b.state_to_save(), None, "nothing new to write yet");
        b.update(computed(req, "base", "t1", fixture_files()));
        let bodies: Vec<&str> = review(&b)
            .current_comments()
            .map(|c| c.body.as_str())
            .collect();
        assert_eq!(bodies, ["store down?"]);
        assert!(b.unclaimed.drafts.is_empty());
    }

    #[test]
    fn draft_for_another_snapshot_is_dropped() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "old");
        a.update(ctrl('s'));
        let (mut b, _) = restart(&mut a, Mode::Review);
        // Kept while unclaimed, so another restart still has it.
        assert_eq!(b.saved_state().drafts.len(), 1);
        // The tree moved on before the card came back.
        b.update(computed(req, "base", "t2", fixture_files()));
        assert_eq!(review(&b).current_comments().count(), 0);
        assert!(b.saved_state().drafts.is_empty());
    }

    #[test]
    fn sent_comments_are_no_draft() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "x");
        a.update(ctrl('s'));
        assert_eq!(a.saved_state().drafts.len(), 1);
        let id = a.queue.current().unwrap().id;
        a.update(key('S'));
        a.update(Msg::SendDone { card: id });
        let state = a.saved_state();
        assert!(state.drafts.is_empty());
        let round = &state.rounds[&PathBuf::from("/a")];
        assert_eq!((round.tree.as_str(), round.comments.len()), ("t1", 1));
    }

    #[test]
    fn quiz_progress_survives_restart() {
        let mut a = asking();
        // Question 1 is a choice; its right answer is the third.
        a.update(key('3'));
        // Question 2 is free text; the judge is still on it at exit.
        answer_free(&mut a, "no negative count");
        let before = quiz(&a).clone();
        assert_eq!(before.answers[1].verdict, Verdict::Judging);
        let (mut b, effects) = restart(&mut a, Mode::Quiz);
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::ComputeQuiz {
                    from_tree: None,
                    ..
                }]
            ),
            "{effects:?}"
        );
        let effects = b.update(Msg::QuizDiff {
            checkout: Checkout("/a".into()),
            agent: b.agents[0].id.clone(),
            from_tree: "base".into(),
            to_tree: "t1".into(),
            files: vec![file()],
            at: SystemTime::UNIX_EPOCH,
        });
        assert!(effects.is_empty(), "no new questions asked: {effects:?}");
        let qc = quiz(&b);
        assert_eq!(qc.questions, before.questions);
        assert_eq!(qc.state, QuizState::Asking(1));
        assert_eq!(qc.answers[0], before.answers[0]);
        assert_eq!(
            qc.answers[1].verdict,
            Verdict::JudgeFailed(NOT_JUDGED.into())
        );
    }

    /// A review card for `/a` with the comment `body`, in round 1.
    fn draft(body: &str) -> (App, ReviewRequest) {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, body);
        a.update(ctrl('s'));
        (a, req)
    }

    #[test]
    fn draft_written_in_full_branch_view_survives_restart() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        let id = a.queue.current().unwrap().id;
        a.update(key('A'));
        a.update(Msg::SendDone { card: id });
        // Round 2, shown as the whole branch while the comment is written.
        let req = finish_round(&mut a);
        a.update(computed(req.clone(), "t1", "t2", fixture_files()));
        let effects = a.update(key('R'));
        let [Effect::ComputeReview(full)] = effects.as_slice() else {
            panic!("expected the full-branch diff, got {effects:?}");
        };
        a.update(computed(full.clone(), "base", "t2", fixture_files()));
        assert!(review(&a).show_full_branch);
        a.update(key('c'));
        typed(&mut a, "seen whole");
        a.update(ctrl('s'));
        let (mut b, effects) = restart(&mut a, Mode::Review);
        assert_eq!(effects, [Effect::ComputeReview(req.clone())]);
        b.update(computed(req, "t1", "t2", fixture_files()));
        let bodies: Vec<&str> = review(&b)
            .current_comments()
            .map(|c| c.body.as_str())
            .collect();
        assert_eq!(bodies, ["seen whole"]);
    }

    #[test]
    fn save_for_another_mode_is_dropped_on_restart() {
        let (mut a, _) = draft("x");
        let (b, effects) = restart(&mut a, Mode::Quiz);
        assert!(
            effects.is_empty(),
            "the stale draft starts nothing: {effects:?}"
        );
        assert_eq!(b.saved_state().drafts, []);
    }

    #[test]
    fn empty_recompute_drops_the_save() {
        let (mut a, req) = draft("x");
        let (mut b, _) = restart(&mut a, Mode::Review);
        b.update(computed(req, "base", "t1", Vec::new()));
        assert!(b.queue.is_empty());
        assert_eq!(b.saved_state().drafts, []);

        let mut a = asking();
        a.update(key('3'));
        let (mut b, _) = restart(&mut a, Mode::Quiz);
        let effects = b.update(Msg::QuizDiff {
            checkout: Checkout("/a".into()),
            agent: b.agents[0].id.clone(),
            from_tree: "base".into(),
            to_tree: "t1".into(),
            files: Vec::new(),
            at: SystemTime::UNIX_EPOCH,
        });
        assert!(effects.is_empty());
        assert_eq!(b.saved_state().quizzes, []);
    }

    #[test]
    fn any_card_for_the_checkout_drops_the_save() {
        let (mut a, _) = draft("x");
        let (mut b, _) = restart(&mut a, Mode::Review);
        b.queue.push(crate::queue::tests::card(
            99,
            "/a",
            CardKind::Review(Default::default()),
            0,
        ));
        b.update(Msg::Tick(std::time::Instant::now()));
        assert_eq!(b.unclaimed, Unclaimed::default());
    }

    #[test]
    fn failed_write_is_tried_again() {
        let (mut a, _) = draft("x");
        assert!(a.state_to_save().is_some());
        assert_eq!(a.state_to_save(), None);
        a.update(Msg::StateSaveFailed("disk full".into()));
        assert_eq!(a.status_text(), Some("Could not save state: disk full"));
        assert!(a.state_to_save().is_some(), "written again");
    }

    #[test]
    fn missing_file_is_empty_and_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/state.json");
        assert_eq!(load(&file).unwrap(), SavedState::default());
        let mut state = SavedState::default();
        state.rounds.insert(
            PathBuf::from("/w/api"),
            LastRound {
                tree: "abc".into(),
                round: 2,
                comments: vec![Comment {
                    path: "src/a.rs".into(),
                    line: 3,
                    end_line: 4,
                    side: Side::Old,
                    body: "why?".into(),
                    round: 2,
                }],
            },
        );
        save(&file, &state).unwrap();
        assert_eq!(load(&file).unwrap(), state);
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.json"], "no temp file left behind");
        std::fs::write(&file, "{not json").unwrap();
        assert!(load(&file).is_err());
    }
}
