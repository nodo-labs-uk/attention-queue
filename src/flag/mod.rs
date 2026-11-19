use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use candidate::Action;
use facts::{Facts, Me, Runner};
use rules::Decision;

pub mod candidate;
pub mod facts;
pub mod log;
pub mod rules;

/// The look of a flagged action: it involves someone else, or it is high stakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    SomeoneElse,
    HighStakes,
}

/// What aq found for one tool call: its actions, the facts about each (in
/// the same order), the combined decision, and the PR conflicts for the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub actions: Vec<Action>,
    pub decision: Decision,
    pub facts: Vec<Facts>,
    pub conflicts: Vec<String>,
    /// The user the facts were judged against; `None` when `gh` could not
    /// say who they are.
    pub me: Option<Me>,
}

/// What a held card says about a flagged call: the band's look and label,
/// what it acts on, and the lines that explain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub kind: FlagKind,
    pub label: String,
    pub target: String,
    pub who: String,
    pub happens: String,
    pub as_you: String,
    pub conflict: String,
    /// Haiku's lines are not in yet.
    pub pending: bool,
}

/// What aq knows about a question or option-menu card that may lead to a
/// flagged action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FlagState {
    #[default]
    None,
    /// Haiku, then the facts, are checking it.
    Checking,
    Flagged(Flag),
}

/// Words that point at an action worth flagging. Matched whole and in any case.
const ACTION_WORDS: [&str; 11] = [
    "pr",
    "pull request",
    "issue",
    "merge",
    "push",
    "release",
    "tag",
    "force",
    "delete branch",
    "comment",
    "review",
];

/// Whether `text` mentions an action worth flagging: one of the action
/// words in any of its forms, a `#<number>` or a GitHub link. Cheap, so it
/// runs on every card.
pub fn mentions_action(text: &str) -> bool {
    let text = text.to_lowercase();
    let word_char = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let whole = |words: &str| {
        text.match_indices(words).any(|(i, _)| {
            !word_char(text[..i].chars().next_back())
                && !word_char(text[i + words.len()..].chars().next())
        })
    };
    let number = text.match_indices('#').any(|(i, _)| {
        let rest = &text[i + 1..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0
            && !word_char(text[..i].chars().next_back())
            && !word_char(rest[digits..].chars().next())
    });
    number
        || text.contains("github.com/")
        || ACTION_WORDS
            .iter()
            .flat_map(|w| forms(w))
            .any(|w| whole(&w))
}

/// `word` and its s, es, ed and ing forms: "merging", "pushed", "tagged".
/// A word of two letters takes only an s ("PRs").
fn forms(word: &str) -> Vec<String> {
    if word.len() <= 2 {
        return vec![word.to_string(), format!("{word}s")];
    }
    let mut out: Vec<String> = ["", "s", "es", "ed", "ing"]
        .iter()
        .map(|end| format!("{word}{end}"))
        .collect();
    match word.strip_suffix('e') {
        Some(stem) => out.extend([format!("{word}d"), format!("{stem}ing")]),
        // A doubled last letter: "tagged", "tagging".
        None => {
            let last = word.chars().last().unwrap_or_default();
            out.extend([format!("{word}{last}ed"), format!("{word}{last}ing")]);
        }
    }
    out
}

/// What a flag is about, from the first facts that name it: a PR or issue
/// with its author, marked when someone is outside the user's org, or a
/// pushed branch. `None` when no facts name it.
pub fn target(facts: &[Facts], me: Option<&Me>) -> Option<String> {
    let suffix = |inside: &dyn Fn(&Me) -> bool| {
        if me.is_some_and(|me| !inside(me)) {
            " (outside your org)"
        } else {
            ""
        }
    };
    facts.iter().find_map(|f| match f {
        Facts::Pr(p) => Some(format!(
            "{} PR #{} by {}{}",
            p.repo,
            p.number,
            p.author,
            suffix(&|me| p.inside(me))
        )),
        Facts::Issue(i) => Some(format!(
            "{} issue #{} by {}{}",
            i.repo,
            i.number,
            i.author,
            suffix(&|me| i.inside(me))
        )),
        Facts::Push(p) => Some(format!("{}/{} branch {}", p.owner, p.repo, p.branch)),
        Facts::Unknown => None,
    })
}

/// The user, from `cache` or else from `gh`. Only a success is kept, so a
/// failure is tried again on the next call.
pub fn me(cache: &Mutex<Option<Me>>, r: &dyn Runner, cwd: &Path) -> anyhow::Result<Me> {
    let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(me) = &*cached {
        return Ok(me.clone());
    }
    let me = facts::load_me(r, cwd)?;
    *cached = Some(me.clone());
    Ok(me)
}

/// Gathers the facts for each action and applies the rules. Without the
/// user's login, no facts can be trusted, so every action's are unknown.
pub fn check(
    actions: Vec<Action>,
    cwd: &Path,
    cache: &Mutex<Option<Me>>,
    r: &dyn Runner,
) -> Verdict {
    let (me, found, known) = match me(cache, r, cwd) {
        Ok(me) => {
            let found = actions
                .iter()
                .map(|a| facts::gather(a, cwd, &me, r))
                .collect();
            (me.clone(), found, Some(me))
        }
        Err(_) => {
            let nobody = Me {
                login: String::new(),
                orgs: Vec::new(),
            };
            (nobody, vec![Facts::Unknown; actions.len()], None)
        }
    };
    let decision = rules::decide_all(
        actions
            .iter()
            .zip(&found)
            .map(|(a, f)| rules::decide(a, f, &me)),
    );
    let mut conflicts = Vec::new();
    for line in found.iter().flat_map(|f| rules::conflicts(f, &me)) {
        if !conflicts.contains(&line) {
            conflicts.push(line);
        }
    }
    Verdict {
        actions,
        decision,
        facts: found,
        conflicts,
        me: known,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Knows the user when `knows_me`; every other command fails. Counts
    /// the calls for the user's login.
    struct Gh {
        knows_me: bool,
        asked: AtomicUsize,
    }

    impl Runner for Gh {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> anyhow::Result<String> {
            match (program, args) {
                ("gh", ["api", "user", ..]) => {
                    self.asked.fetch_add(1, Ordering::SeqCst);
                    if self.knows_me {
                        Ok("smnai\n".into())
                    } else {
                        anyhow::bail!("not logged in")
                    }
                }
                ("gh", ["api", "user/orgs", ..]) => Ok("nodo-labs-uk\n".into()),
                _ => anyhow::bail!("offline"),
            }
        }

        fn run_with(
            &self,
            cwd: &Path,
            program: &str,
            args: &[&str],
            _opts: facts::RunOpts,
        ) -> anyhow::Result<String> {
            self.run(cwd, program, args)
        }
    }

    fn actions(command: &str) -> Vec<Action> {
        candidate::candidates("Bash", &serde_json::json!({ "command": command }))
    }

    #[test]
    fn action_words_are_matched_whole_and_in_any_case() {
        for text in [
            "How should I merge PR #5?",
            "Shall I open a Pull Request?",
            "Reply on the issue?",
            "Push it?",
            "Tag v1.2 now?",
            "Force it?",
            "Delete branch fix-5?",
            "Post the comment?",
            "Fix #12 first?",
            "See https://github.com/nodo-labs-uk/socratui/pull/5 ok?",
            "Ready for review?",
            "Cut the release?",
        ] {
            assert!(mentions_action(text), "{text}");
        }
        for text in [
            "Which approach do you prefer?",
            "Should I prune the cache?",
            "Is colour #fff right?",
            "Step 2#5?",
            "Is the predicate right?",
            "Pressing on?",
        ] {
            assert!(!mentions_action(text), "{text}");
        }
    }

    #[test]
    fn action_words_match_their_s_es_ed_and_ing_forms() {
        for text in [
            "Merging now?",
            "Both PRs?",
            "Close the issues?",
            "Open two pull requests?",
            "Pushed it, carry on?",
            "Merged already, pushing on?",
            "Tagged it?",
            "Tagging v1.2?",
            "Force pushes?",
            "Commented there?",
            "Reviewed it?",
            "Releasing today?",
        ] {
            assert!(mentions_action(text), "{text}");
        }
    }

    #[test]
    fn a_failed_login_is_not_kept_and_leaves_every_fact_unknown() {
        let gh = Gh {
            knows_me: false,
            asked: AtomicUsize::new(0),
        };
        let cache = Mutex::new(None);
        let line = "git push --force origin fix && gh pr merge 5";
        for _ in 0..2 {
            let v = check(actions(line), Path::new("/w"), &cache, &gh);
            assert_eq!(v.actions.len(), 2);
            assert_eq!(v.facts, [Facts::Unknown, Facts::Unknown]);
            assert!(v.conflicts.is_empty());
            assert_eq!(
                v.decision,
                Decision::Flagged {
                    kind: FlagKind::HighStakes,
                    label: "FORCE PUSH"
                }
            );
        }
        assert_eq!(gh.asked.load(Ordering::SeqCst), 2, "tried again");
        assert!(cache.lock().unwrap().is_none());
    }

    #[test]
    fn the_user_is_looked_up_once() {
        let gh = Gh {
            knows_me: true,
            asked: AtomicUsize::new(0),
        };
        let cache = Mutex::new(None);
        for _ in 0..2 {
            let v = check(actions("git push origin fix"), Path::new("/w"), &cache, &gh);
            // The push's own lookups fail: grey, for Haiku.
            assert_eq!(
                (v.facts.as_slice(), v.decision),
                (&[Facts::Unknown][..], Decision::Grey)
            );
        }
        assert_eq!(gh.asked.load(Ordering::SeqCst), 1);
        assert_eq!(cache.lock().unwrap().as_ref().unwrap().login, "smnai");
    }
}
