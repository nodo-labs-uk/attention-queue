//! Held flagged calls: a candidate the rules flag gets a held permission
//! card at once; a grey one waits for Haiku, which clears it or holds it.
//! Also flagged questions: a question or option-menu card that may lead to
//! such a call is checked by Haiku and the facts, and flagged before it is
//! answered.

use std::time::SystemTime;

use serde::Deserialize;
use serde_json::Value;

use crate::app::{App, Effect};
use crate::flag::candidate::{Action, IssueVerb, PrVerb, valid_repo};
use crate::flag::facts::{Facts, Me};
use crate::flag::rules::{self, Decision};
use crate::flag::{Flag, FlagKind, FlagState, Verdict, mentions_action};
use crate::ipc::{HookReply, HookRequest};
use crate::llm::{FLAG_PROMPT, FLAG_QUESTION_PROMPT, JobKind, LlmEvent, LlmJob, valid_session_id};
use crate::model::{Card, CardKind, Checkout};
use crate::permission::{Held, PermissionCard, PermissionRequest, summary};

/// A grey call waiting for Haiku: the card it gets if Haiku flags it, the
/// target and the conflict from facts.
#[derive(Debug)]
pub(crate) struct PendingFlag {
    pub card: Card,
    pub target: String,
    pub conflict: String,
}

/// Haiku's answer for a flag job, as `schemas/flag.json` gives it.
#[derive(Debug, Deserialize)]
struct Said {
    flag: String,
    who: String,
    happens: String,
    as_you: String,
    conflict: String,
}

/// A flag log line for an outcome the rules alone do not give: what Haiku
/// said on a grey call, or the user's answer on a held card.
pub(crate) fn log_entry(tool: &str, input: &Value, decision: &str, label: Option<&str>) -> Value {
    let mut entry = crate::flag::log::entry(tool, input, &Decision::Grey);
    entry["decision"] = decision.into();
    entry["label"] = label.into();
    entry
}

/// What the call acts on, from the first action whose facts name it; else
/// the card's one-line summary.
fn target(v: &Verdict, summary: &str) -> String {
    crate::flag::target(&v.facts, v.me.as_ref()).unwrap_or_else(|| summary.to_string())
}

/// The lines joined by "; ", leaving out empty ones.
fn joined<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    lines
        .into_iter()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn label_of(kind: FlagKind) -> &'static str {
    match kind {
        FlagKind::SomeoneElse => "SOMEONE ELSE",
        FlagKind::HighStakes => "HIGH STAKES",
    }
}

fn permission_of(card: &mut Card) -> Option<&mut PermissionCard> {
    match &mut card.kind {
        CardKind::Permission(pc) => Some(pc),
        _ => None,
    }
}

impl App {
    /// A checked candidate: a cleared call carries on, a flagged one is held
    /// on a card at once, and a grey one waits for Haiku. Haiku is asked
    /// about both of the last two.
    pub(crate) fn on_flag(
        &mut self,
        req_id: u64,
        at: SystemTime,
        req: HookRequest,
        checkout: Option<Checkout>,
        verdict: Verdict,
    ) -> Vec<Effect> {
        let HookRequest::PreTool {
            session_id,
            cwd,
            tool_name,
            tool_input,
            pane,
            ..
        } = req
        else {
            return vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }];
        };
        let (kind, label) = match verdict.decision {
            Decision::Cleared => {
                return vec![Effect::Reply {
                    req_id,
                    decision: HookReply::NoDecision,
                }];
            }
            Decision::Flagged { kind, label } => (Some(kind), Some(label)),
            Decision::Grey => (None, None),
        };
        let target = target(&verdict, &summary(&tool_name, &tool_input));
        let conflict = verdict.conflicts.join("; ");
        let r = PermissionRequest {
            session_id: session_id.clone(),
            cwd,
            tool_name,
            tool_input,
            pane,
            checkout,
        };
        let held = kind.zip(label).map(|(kind, label)| {
            Held::Flagged(Flag {
                kind,
                label: label.into(),
                target: target.clone(),
                who: String::new(),
                happens: String::new(),
                as_you: String::new(),
                conflict: conflict.clone(),
                pending: true,
            })
        });
        let card = self.permission_card(req_id, at, r, held);
        let job = LlmJob {
            card: card.id,
            kind: JobKind::Flag {
                session_id: Some(session_id),
            },
            cwd: card.checkout.0.clone(),
            prompt: flag_prompt(&card, label, &verdict.facts),
        };
        if label.is_some() {
            self.queue.push(card);
        } else {
            self.pending_flags.insert(
                card.id,
                PendingFlag {
                    card,
                    target,
                    conflict,
                },
            );
        }
        vec![Effect::Llm(job)]
    }

    /// Haiku's answer for a flag job. A grey call is cleared, held with its
    /// lines, or held unchecked when Haiku failed; a held card gets its lines.
    pub(crate) fn on_flag_llm(&mut self, ev: LlmEvent) -> Vec<Effect> {
        let said = ev.result.and_then(|text| {
            serde_json::from_str::<Said>(&text).map_err(|e| format!("unreadable answer: {e}"))
        });
        if let Some(pending) = self.pending_flags.remove(&ev.card) {
            return self.on_grey_answer(pending, said);
        }
        let Some(pc) = self
            .queue
            .find_mut(|c| c.id == ev.card)
            .and_then(permission_of)
        else {
            return Vec::new();
        };
        if let Some(Held::Flagged(f)) = &mut pc.held
            && f.pending
        {
            // The rule's kind stands, whatever Haiku's flag.
            if let Ok(s) = said {
                f.who = s.who;
                f.happens = s.happens;
                f.as_you = s.as_you;
                f.conflict = joined([f.conflict.as_str(), s.conflict.as_str()]);
            }
            f.pending = false;
        }
        Vec::new()
    }

    fn on_grey_answer(&mut self, pending: PendingFlag, said: Result<Said, String>) -> Vec<Effect> {
        let PendingFlag {
            mut card,
            target,
            conflict,
        } = pending;
        let pc = permission_of(&mut card).expect("a permission card");
        // Only "none" clears: any other unknown value holds the call.
        let said = said.and_then(|s| match s.flag.as_str() {
            "none" | "someone_else" | "high_stakes" => Ok(s),
            _ => Err("unreadable answer".to_string()),
        });
        let (held, log) = match said {
            Err(why) => {
                let log = log_entry(&pc.tool_name, &pc.tool_input, "unchecked", None);
                (Held::Unchecked(why), log)
            }
            Ok(s) => {
                let kind = match s.flag.as_str() {
                    "someone_else" => FlagKind::SomeoneElse,
                    "high_stakes" => FlagKind::HighStakes,
                    _ => {
                        // "none", the only other value left.
                        let log = log_entry(&pc.tool_name, &pc.tool_input, "cleared", None);
                        return vec![
                            Effect::Reply {
                                req_id: pc.req_id,
                                decision: HookReply::NoDecision,
                            },
                            Effect::LogFlag(log),
                        ];
                    }
                };
                let label = label_of(kind);
                let log = log_entry(&pc.tool_name, &pc.tool_input, "flagged", Some(label));
                let flag = Flag {
                    kind,
                    label: label.into(),
                    target,
                    who: s.who,
                    happens: s.happens,
                    as_you: s.as_you,
                    conflict: joined([conflict.as_str(), s.conflict.as_str()]),
                    pending: false,
                };
                (Held::Flagged(flag), log)
            }
        };
        pc.held = Some(held);
        self.queue.push(card);
        vec![Effect::LogFlag(log)]
    }

    /// Whether the hook `req_id` waits on a held card or on Haiku: it must
    /// be refused, never let through, if aq stops.
    pub fn holds_flag(&self, req_id: u64) -> bool {
        self.awaits_haiku(req_id)
            || self.queue.iter().any(|c| {
                matches!(&c.kind, CardKind::Permission(p) if p.req_id == req_id && p.held.is_some())
            })
    }

    /// Whether the grey call `req_id` still waits for Haiku.
    pub fn awaits_haiku(&self, req_id: u64) -> bool {
        self.pending_flags
            .values()
            .any(|p| matches!(&p.card.kind, CardKind::Permission(pc) if pc.req_id == req_id))
    }

    /// The hook a grey call's Haiku job `card` is for.
    pub fn flag_job_req(&self, card: u64) -> Option<u64> {
        match &self.pending_flags.get(&card)?.card.kind {
            CardKind::Permission(pc) => Some(pc.req_id),
            _ => None,
        }
    }
}

/// Haiku's answer for a question flag job, as `schemas/flag-question.json`
/// gives it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct QuestionSaid {
    flag: String,
    repo: String,
    number: u64,
    kind: String,
    verb: String,
    who: String,
    happens: String,
    as_you: String,
}

impl QuestionSaid {
    /// Haiku's repo, if it is a safe `owner/name`. Anything else, such as
    /// `--repo=x`, never reaches `gh`.
    fn repo(&self) -> Option<&str> {
        Some(self.repo.trim()).filter(|r| valid_repo(r))
    }

    /// Whether Haiku named a PR or an issue by number, on no repo or a
    /// valid one.
    fn has_target(&self) -> bool {
        matches!(self.kind.as_str(), "pr" | "issue")
            && self.number > 0
            && (self.repo.trim().is_empty() || self.repo().is_some())
    }

    /// Whether the answer posts in the user's voice: a comment or a review
    /// always does; a close or a merge only when Haiku says what is said.
    fn speaks(&self) -> bool {
        let said = self.as_you.trim().trim_end_matches('.').to_lowercase();
        match self.verb.as_str() {
            "comment" | "review" => true,
            "close" | "merge" => !matches!(said.as_str(), "" | "none" | "nothing"),
            _ => false,
        }
    }

    /// The write Haiku named on its PR or issue, as an action the rules
    /// can judge. `None` without a target, without a write, or for a write
    /// the facts of a PR or issue do not decide (a push, a release).
    fn action(&self) -> Option<Action> {
        if !self.has_target() {
            return None;
        }
        let number = Some(self.number);
        let repo = self.repo().map(String::from);
        let speaks = self.speaks();
        match self.kind.as_str() {
            "pr" => {
                let verb = match self.verb.as_str() {
                    "merge" => PrVerb::Merge,
                    "close" => PrVerb::Close,
                    "comment" => PrVerb::Comment,
                    "review" => PrVerb::Review,
                    "edit" => PrVerb::Edit,
                    _ => return None,
                };
                Some(Action::Pr {
                    verb,
                    number,
                    repo,
                    selector: None,
                    speaks,
                })
            }
            _ => {
                let verb = match self.verb.as_str() {
                    "close" => IssueVerb::Close,
                    "comment" | "review" => IssueVerb::Comment,
                    "edit" => IssueVerb::Edit,
                    "delete" => IssueVerb::Delete,
                    _ => return None,
                };
                Some(Action::Issue {
                    verb,
                    number,
                    repo,
                    speaks,
                })
            }
        }
    }

    /// What Haiku named: `owner/name PR #5`, as much of it as it gave, or
    /// nothing.
    fn target(&self) -> String {
        let what = match self.kind.as_str() {
            "pr" => "PR",
            _ => "issue",
        };
        let number = if self.has_target() {
            format!("{what} #{}", self.number)
        } else {
            String::new()
        };
        format!("{} {number}", self.repo().unwrap_or_default())
            .trim()
            .to_string()
    }

    /// Haiku's own flag, with its lines. `None` for "none" or anything else.
    fn flag(&self) -> Option<Flag> {
        let kind = match self.flag.as_str() {
            "someone_else" => FlagKind::SomeoneElse,
            "high_stakes" => FlagKind::HighStakes,
            _ => return None,
        };
        Some(self.lines(Flag {
            kind,
            label: label_of(kind).into(),
            target: self.target(),
            who: String::new(),
            happens: String::new(),
            as_you: String::new(),
            conflict: String::new(),
            pending: false,
        }))
    }

    /// `flag` with Haiku's lines filled in.
    fn lines(&self, flag: Flag) -> Flag {
        Flag {
            who: self.who.clone(),
            happens: self.happens.clone(),
            as_you: self.as_you.clone(),
            ..flag
        }
    }
}

/// The text the word check reads: a question's message, or a menu's
/// questions, option labels and descriptions.
fn question_text(card: &Card) -> Option<String> {
    match &card.kind {
        CardKind::Question(q) => Some(q.text.clone()),
        CardKind::Choice(c) => Some(
            c.questions
                .iter()
                .flat_map(|q| {
                    std::iter::once(q.question.as_str()).chain(
                        q.options
                            .iter()
                            .flat_map(|o| [o.label.as_str(), o.description.as_str()]),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn flag_state(card: &mut Card) -> Option<&mut FlagState> {
    match &mut card.kind {
        CardKind::Question(q) => Some(&mut q.flag),
        CardKind::Choice(c) => Some(&mut c.flag),
        _ => None,
    }
}

/// Who decided a question card's flag, for the log.
#[derive(Debug, Clone, Copy)]
enum By {
    Facts,
    Haiku,
}

/// A flag log line for a question or option-menu card. A flagged line
/// says who flagged it.
fn question_log(card: &Card, decision: &str, flagged: Option<(&str, By)>) -> Value {
    let at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let card = match card.kind {
        CardKind::Choice(_) => "choice",
        _ => "question",
    };
    let mut entry = serde_json::json!({
        "at": at,
        "card": card,
        "decision": decision,
        "label": flagged.map(|(label, _)| label),
    });
    if let Some((_, by)) = flagged {
        entry["by"] = match by {
            By::Facts => "facts",
            By::Haiku => "haiku",
        }
        .into();
    }
    entry
}

impl App {
    /// A new question or option-menu card. One whose text mentions an
    /// action is marked as checking, and Haiku is asked whether answering
    /// it leads to a flagged action. It can be answered meanwhile.
    pub(crate) fn check_question(&mut self, id: u64, session_id: Option<String>) -> Vec<Effect> {
        let Some(card) = self.queue.find_mut(|c| c.id == id) else {
            return Vec::new();
        };
        if !question_text(card).is_some_and(|t| mentions_action(&t)) {
            return Vec::new();
        }
        let data = crate::ask::context_for(card);
        if let Some(state) = flag_state(card) {
            *state = FlagState::Checking;
        }
        vec![Effect::Llm(LlmJob {
            card: id,
            kind: JobKind::FlagQuestion {
                session_id: session_id.filter(|s| valid_session_id(s)),
            },
            cwd: card.checkout.0.clone(),
            prompt: format!(
                "{FLAG_QUESTION_PROMPT}\n## The question (data)\n\n{}",
                crate::quiz::fenced(&data, "")
            ),
        })]
    }

    /// The card `id` while it is being checked: still waiting, in the
    /// queue or the stash, and not answered elsewhere.
    fn checking(&mut self, id: u64) -> Option<&mut Card> {
        if self.stale == Some(id) {
            return None;
        }
        let card = match self.queue.find_mut(|c| c.id == id) {
            Some(card) => card,
            None => self.stashed.iter_mut().find(|c| c.id == id)?,
        };
        let checking = flag_state(card).is_some_and(|s| *s == FlagState::Checking);
        checking.then_some(card)
    }

    /// Haiku's answer on a question card. A write on a named PR or issue
    /// is looked up, so the facts decide. An answer that leads to no write
    /// is not flagged, target or not. Otherwise Haiku's flag stands. A
    /// failure flags nothing: each action that follows is still held on
    /// its own.
    pub(crate) fn on_flag_question_llm(&mut self, ev: LlmEvent) -> Vec<Effect> {
        let Some(card) = self.checking(ev.card) else {
            return Vec::new();
        };
        let cwd = card.checkout.0.clone();
        let said = ev
            .result
            .ok()
            .and_then(|text| serde_json::from_str::<QuestionSaid>(&text).ok());
        let Some(said) = said else {
            return self.settle_question(ev.card, None, "error");
        };
        if let Some(action) = said.action() {
            self.question_flags.insert(ev.card, said);
            return vec![Effect::FlagFacts {
                card: ev.card,
                cwd,
                action,
            }];
        }
        // An answer that leads to no write is never flagged.
        if said.verb == "none" {
            return self.settle_question(ev.card, None, "none");
        }
        let flag = said.flag().map(|f| (f, By::Haiku));
        self.settle_question(ev.card, flag, "none")
    }

    /// The facts for the PR or issue Haiku named on question card `card`.
    /// They decide by the same rules as a command; when they cannot, or
    /// the lookup failed, Haiku's flag stands.
    pub(crate) fn on_flag_facts(
        &mut self,
        card: u64,
        result: Result<(Facts, Me), String>,
    ) -> Vec<Effect> {
        let Some(said) = self.question_flags.remove(&card) else {
            return Vec::new();
        };
        if self.checking(card).is_none() {
            return Vec::new();
        }
        let from_facts = match (result, said.action()) {
            (Ok((facts, me)), Some(action)) if facts != Facts::Unknown => {
                match rules::decide(&action, &facts, &me) {
                    Decision::Cleared => return self.settle_question(card, None, "cleared"),
                    Decision::Flagged { kind, label } => Some(
                        said.lines(Flag {
                            kind,
                            label: label.into(),
                            target: crate::flag::target(std::slice::from_ref(&facts), Some(&me))
                                .unwrap_or_else(|| said.target()),
                            who: String::new(),
                            happens: String::new(),
                            as_you: String::new(),
                            conflict: rules::conflicts(&facts, &me).join("; "),
                            pending: false,
                        }),
                    ),
                    Decision::Grey => None,
                }
            }
            _ => None,
        };
        let flag = match from_facts {
            Some(f) => Some((f, By::Facts)),
            None => said.flag().map(|f| (f, By::Haiku)),
        };
        self.settle_question(card, flag, "none")
    }

    /// Puts `flag` on question card `id`, or clears it, and logs that as
    /// `unflagged` when there is no flag.
    fn settle_question(
        &mut self,
        id: u64,
        flag: Option<(Flag, By)>,
        unflagged: &str,
    ) -> Vec<Effect> {
        let Some(card) = self.checking(id) else {
            return Vec::new();
        };
        let log = match &flag {
            Some((f, by)) => question_log(card, "flagged", Some((&f.label, *by))),
            None => question_log(card, unflagged, None),
        };
        if let Some(state) = flag_state(card) {
            *state = flag.map_or(FlagState::None, |(f, _)| FlagState::Flagged(f));
        }
        vec![Effect::LogFlag(log)]
    }
}

/// The flag prompt, then the call, the rule's label and the facts as data.
fn flag_prompt(card: &Card, label: Option<&str>, facts: &[Facts]) -> String {
    let rule = label.map_or_else(
        || "No rule decided this call.".to_string(),
        |l| format!("Flagged by a rule: {l}"),
    );
    let facts = serde_json::to_string_pretty(facts).unwrap_or_default();
    let data = format!(
        "{}\n\n{rule}\n\nFacts from GitHub:\n{facts}\n",
        crate::ask::context_for(card)
    );
    format!(
        "{FLAG_PROMPT}\n## The request (data)\n\n{}",
        crate::quiz::fenced(&data, "")
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    use super::*;
    use crate::app::tests::agent;
    use crate::app::{Msg, Overlay};
    use crate::config::Config;
    use crate::flag::candidate::{Action, PrVerb};
    use crate::flag::facts::{Facts, Me, PrFacts, PushFacts, Review};
    use crate::flag::rules::Decision;
    use crate::flag::{Flag, FlagKind, FlagState};
    use crate::ipc::HookReply;
    use crate::llm::{FLAG_PROMPT, FLAG_QUESTION_PROMPT, JobKind, LlmJob};
    use crate::model::Checkout;
    use crate::model::{CardKind, Mode};
    use crate::permission::{FLAG_DENY_DEFAULT, Gist, Held, PermissionCard};
    use crate::theme::builtin_themes;

    const MERGE: &str = "gh pr merge 5 --squash";
    const ASKED: &str = "You requested changes on 2026-11-17; no commits since";

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%5", 5, "api", "/w/api", false)];
        a.update(Msg::Resize {
            width: 80,
            height: 24,
        });
        a
    }

    fn bash(command: &str) -> HookRequest {
        HookRequest::PreTool {
            session_id: "s5".into(),
            transcript_path: String::new(),
            cwd: "/w/api".into(),
            tool_name: "Bash".into(),
            tool_use_id: "t1".into(),
            tool_input: json!({ "command": command }),
            pane: Some("%5".into()),
        }
    }

    fn me() -> Me {
        Me {
            login: "smnai".into(),
            orgs: vec!["nodo-labs-uk".into()],
        }
    }

    /// The incident: Timonsc's PR from a fork, with changes requested.
    fn pr() -> Facts {
        Facts::Pr(PrFacts {
            repo: "nodo-labs-uk/socratui".into(),
            number: 5,
            author: "Timonsc".into(),
            author_is_bot: false,
            author_association: "CONTRIBUTOR".into(),
            base: "main".into(),
            default_branch: "main".into(),
            cross_repo: true,
            head_owner: "Timonsc".into(),
            draft: false,
            checks_failing: false,
            my_last_review: Some(Review {
                state: "CHANGES_REQUESTED".into(),
                at: "2026-11-17".into(),
            }),
            commits_since_my_review: Some(0),
        })
    }

    fn verdict(decision: Decision, facts: Vec<Facts>, conflicts: &[&str]) -> Verdict {
        Verdict {
            actions: Vec::new(),
            decision,
            facts,
            conflicts: conflicts.iter().map(|c| c.to_string()).collect(),
            me: Some(me()),
        }
    }

    const SOMEONE_ELSE: Decision = Decision::Flagged {
        kind: FlagKind::SomeoneElse,
        label: "SOMEONE ELSE",
    };

    fn flag(a: &mut App, req_id: u64, command: &str, v: Verdict) -> Vec<Effect> {
        a.update(Msg::Flag {
            req_id,
            req: bash(command),
            checkout: None,
            verdict: v,
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(req_id),
        })
    }

    /// The incident's merge, flagged by the rules.
    fn merge(a: &mut App) -> LlmJob {
        let fx = flag(a, 1, MERGE, verdict(SOMEONE_ELSE, vec![pr()], &[ASKED]));
        let [Effect::Llm(job)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        job.clone()
    }

    fn haiku(a: &mut App, card: u64, result: Result<String, String>) -> Vec<Effect> {
        a.update(Msg::Llm(LlmEvent {
            card,
            kind: JobKind::Flag {
                session_id: Some("s5".into()),
            },
            result,
        }))
    }

    fn says(flag: &str) -> Result<String, String> {
        Ok(json!({
            "flag": flag,
            "who": "Timonsc opened this PR from their fork",
            "happens": "merges PR #5 into main",
            "as_you": "the merge is recorded under your GitHub account",
            "conflict": "you said to wait for Timonsc",
        })
        .to_string())
    }

    fn perm(a: &App) -> &PermissionCard {
        match &a.queue.current().expect("a card").kind {
            CardKind::Permission(p) => p,
            k => panic!("{k:?}"),
        }
    }

    fn held(a: &App) -> &Held {
        perm(a).held.as_ref().expect("a held card")
    }

    fn flagged(a: &App) -> &Flag {
        match held(a) {
            Held::Flagged(f) => f,
            h => panic!("{h:?}"),
        }
    }

    /// The `decision` of each flag log line among `fx`.
    fn logged(fx: &[Effect]) -> Vec<String> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::LogFlag(v) => Some(v["decision"].as_str().unwrap_or("?").to_string()),
                _ => None,
            })
            .collect()
    }

    fn key(code: KeyCode, at: Instant) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE), at)
    }

    /// Puts the card on screen and returns a time long enough after.
    fn look(a: &mut App) -> Instant {
        let t0 = Instant::now();
        a.update(Msg::Tick(t0));
        t0 + Duration::from_millis(400)
    }

    #[test]
    fn a_cleared_call_gets_no_card_and_no_decision() {
        let mut a = app();
        let fx = flag(
            &mut a,
            1,
            "git push origin fix",
            verdict(Decision::Cleared, Vec::new(), &[]),
        );
        assert_eq!(
            fx,
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::NoDecision
            }]
        );
        assert!(a.queue.is_empty());
        assert!(!a.holds_flag(1));
    }

    #[test]
    fn a_flagged_call_shows_at_once_with_its_conflict_and_asks_haiku() {
        let mut a = app();
        let job = merge(&mut a);
        assert_eq!(
            job.kind,
            JobKind::Flag {
                session_id: Some("s5".into())
            }
        );
        assert_eq!(job.cwd, std::path::Path::new("/w/api"));
        assert!(job.prompt.starts_with(FLAG_PROMPT));
        for part in [MERGE, "SOMEONE ELSE", "\"author\": \"Timonsc\""] {
            assert!(job.prompt.contains(part), "{part}: {}", job.prompt);
        }
        assert_eq!(a.queue.current().unwrap().id, job.card);
        assert_eq!(
            *flagged(&a),
            Flag {
                kind: FlagKind::SomeoneElse,
                label: "SOMEONE ELSE".into(),
                target: "nodo-labs-uk/socratui PR #5 by Timonsc (outside your org)".into(),
                who: String::new(),
                happens: String::new(),
                as_you: String::new(),
                conflict: ASKED.into(),
                pending: true,
            }
        );
        // No summary job: Haiku's flag lines take its place.
        assert_eq!(perm(&a).gist, Gist::Pending);
        assert!(a.holds_flag(1));
        assert!(!a.awaits_haiku(1));
    }

    #[test]
    fn a_push_names_its_branch() {
        let mut a = app();
        let push = Facts::Push(PushFacts {
            owner: "smnai".into(),
            repo: "aq".into(),
            branch: "main".into(),
            default_branch: "main".into(),
            pushes_tag: false,
        });
        let main = Decision::Flagged {
            kind: FlagKind::HighStakes,
            label: "MAIN BRANCH",
        };
        flag(&mut a, 1, "git push", verdict(main, vec![push], &[]));
        assert_eq!(flagged(&a).target, "smnai/aq branch main");
        assert_eq!(flagged(&a).kind, FlagKind::HighStakes);
    }

    #[test]
    fn haiku_fills_the_lines_but_cannot_clear_a_rule_flag() {
        let mut a = app();
        let job = merge(&mut a);
        let fx = haiku(&mut a, job.card, says("none"));
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::Reply { .. })),
            "{fx:?}"
        );
        let f = flagged(&a);
        assert_eq!(f.kind, FlagKind::SomeoneElse);
        assert_eq!(f.label, "SOMEONE ELSE");
        assert_eq!(f.who, "Timonsc opened this PR from their fork");
        assert_eq!(f.happens, "merges PR #5 into main");
        assert_eq!(f.as_you, "the merge is recorded under your GitHub account");
        assert_eq!(f.conflict, format!("{ASKED}; you said to wait for Timonsc"));
        assert!(!f.pending);
        assert!(a.holds_flag(1));

        // A failed check keeps the lines from facts.
        let mut a = app();
        let job = merge(&mut a);
        haiku(&mut a, job.card, Err("took longer than 30 s".into()));
        let f = flagged(&a);
        assert_eq!((f.conflict.as_str(), f.who.as_str()), (ASKED, ""));
        assert!(!f.pending);
    }

    #[test]
    fn a_grey_call_waits_for_haiku_then_clears_or_shows() {
        let mut a = app();
        let grey = || verdict(Decision::Grey, vec![Facts::Unknown], &[]);
        let fx = flag(&mut a, 1, "git push origin fix", grey());
        let [Effect::Llm(first)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        assert!(matches!(first.kind, JobKind::Flag { .. }));
        assert!(a.queue.is_empty(), "no card before Haiku says");
        assert!(a.holds_flag(1) && a.awaits_haiku(1));
        assert_eq!(a.flag_job_req(first.card), Some(1));
        let fx = haiku(&mut a, first.card, says("none"));
        assert!(fx.contains(&Effect::Reply {
            req_id: 1,
            decision: HookReply::NoDecision
        }));
        assert_eq!(logged(&fx), ["cleared"]);
        assert!(a.queue.is_empty());
        assert!(!a.holds_flag(1) && !a.awaits_haiku(1));

        let fx = flag(&mut a, 2, "git push origin fix", grey());
        let [Effect::Llm(second)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        let fx = haiku(&mut a, second.card, says("someone_else"));
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::Reply { .. })),
            "{fx:?}"
        );
        assert_eq!(logged(&fx), ["flagged"]);
        assert_eq!(a.queue.current().unwrap().id, second.card);
        assert_eq!(perm(&a).req_id, 2);
        let f = flagged(&a);
        assert_eq!(f.kind, FlagKind::SomeoneElse);
        assert_eq!(f.label, "SOMEONE ELSE");
        assert_eq!(f.target, "git push origin fix");
        assert_eq!(f.who, "Timonsc opened this PR from their fork");
        assert_eq!(f.conflict, "you said to wait for Timonsc");
        assert!(!f.pending);
        assert!(a.holds_flag(2) && !a.awaits_haiku(2));
    }

    #[test]
    fn a_grey_call_whose_check_fails_is_held_unchecked() {
        let mut a = app();
        let v = verdict(Decision::Grey, vec![Facts::Unknown], &[]);
        let fx = flag(&mut a, 1, "git push origin fix", v);
        let [Effect::Llm(job)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        let fx = haiku(&mut a, job.card, Err("took longer than 30 s".into()));
        assert_eq!(logged(&fx), ["unchecked"]);
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::Reply { .. })),
            "{fx:?}"
        );
        assert_eq!(*held(&a), Held::Unchecked("took longer than 30 s".into()));
        let later = look(&mut a);
        let fx = a.update(key(KeyCode::Char('y'), later));
        assert_eq!(
            fx[0],
            Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow
            }
        );
        assert_eq!(logged(&fx), ["allowed"]);
        assert!(a.queue.is_empty());
    }

    #[test]
    fn an_unknown_flag_on_a_grey_call_is_held_unchecked() {
        let mut a = app();
        let v = verdict(Decision::Grey, vec![Facts::Unknown], &[]);
        let fx = flag(&mut a, 1, "git push origin fix", v);
        let [Effect::Llm(job)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        let fx = haiku(&mut a, job.card, says("maybe"));
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::Reply { .. })),
            "{fx:?}"
        );
        assert_eq!(logged(&fx), ["unchecked"]);
        assert_eq!(*held(&a), Held::Unchecked("unreadable answer".into()));
    }

    #[test]
    fn a_second_haiku_result_for_a_held_card_changes_nothing() {
        let mut a = app();
        let job = merge(&mut a);
        haiku(&mut a, job.card, says("none"));
        let first = flagged(&a).clone();
        let fx = haiku(&mut a, job.card, says("none"));
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(*flagged(&a), first);
        assert_eq!(
            first.conflict,
            format!("{ASKED}; you said to wait for Timonsc")
        );
    }

    #[test]
    fn y_allows_and_n_denies_with_the_flag_reason() {
        let mut a = app();
        merge(&mut a);
        let later = look(&mut a);
        // Too quick, as on any permission card.
        assert!(
            a.update(key(KeyCode::Char('y'), later - Duration::from_millis(300)))
                .is_empty()
        );
        let fx = a.update(key(KeyCode::Char('y'), later));
        assert_eq!(
            fx[0],
            Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow
            }
        );
        assert_eq!(logged(&fx), ["allowed"]);
        let entry = fx.iter().find_map(|e| match e {
            Effect::LogFlag(v) => Some(v.clone()),
            _ => None,
        });
        let entry = entry.unwrap();
        assert_eq!(
            (entry["tool"].as_str(), entry["command"].as_str()),
            (Some("Bash"), Some(MERGE))
        );
        assert_eq!(entry["label"], "SOMEONE ELSE");

        let mut a = app();
        merge(&mut a);
        let later = look(&mut a);
        a.update(key(KeyCode::Char('n'), later));
        assert!(matches!(a.overlay, Some(Overlay::Deny(_))));
        let fx = a.update(key(KeyCode::Enter, later));
        assert_eq!(
            fx[0],
            Effect::Reply {
                req_id: 1,
                decision: HookReply::Deny {
                    reason: FLAG_DENY_DEFAULT.into()
                }
            }
        );
        assert_eq!(logged(&fx), ["denied"]);
        assert!(a.queue.is_empty() && !a.holds_flag(1));
    }

    /// The incident's option menu, with its hook waiting as request 7.
    fn incident_menu(a: &mut App) -> Vec<Effect> {
        a.update(Msg::Ipc {
            req_id: 7,
            req: HookRequest::PreTool {
                session_id: "s5".into(),
                transcript_path: String::new(),
                cwd: "/w/api".into(),
                tool_name: "AskUserQuestion".into(),
                tool_use_id: "t1".into(),
                tool_input: json!({"questions": [
                    {"question": "How should I merge PR #5?", "header": "Merge", "multiSelect": false,
                     "options": [
                        {"label": "Merge as is", "description": ""},
                        {"label": "Fix, then merge", "description": ""},
                        {"label": "Wait", "description": ""},
                    ]},
                ]}),
                pane: Some("%5".into()),
            },
            checkout: None,
            at: SystemTime::UNIX_EPOCH,
            now: Instant::now(),
        })
    }

    fn question_haiku(a: &mut App, card: u64, result: Result<String, String>) -> Vec<Effect> {
        a.update(Msg::Llm(LlmEvent {
            card,
            kind: JobKind::FlagQuestion {
                session_id: Some("s5".into()),
            },
            result,
        }))
    }

    fn question_says(flag: &str, verb: &str, as_you: &str) -> Result<String, String> {
        Ok(json!({
            "flag": flag,
            "repo": "nodo-labs-uk/socratui",
            "number": 5,
            "kind": "pr",
            "verb": verb,
            "who": "Timonsc opened PR #5",
            "happens": "fixes and merges PR #5 into main",
            "as_you": as_you,
        })
        .to_string())
    }

    /// The flag log lines among `fx`.
    fn log_lines(fx: &[Effect]) -> Vec<Value> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::LogFlag(v) => Some(v.clone()),
                _ => None,
            })
            .collect()
    }

    fn state(a: &App) -> &FlagState {
        match &a.queue.current().expect("a card").kind {
            CardKind::Choice(c) => &c.flag,
            CardKind::Question(q) => &q.flag,
            k => panic!("{k:?}"),
        }
    }

    /// The FlagQuestion job among `fx`.
    fn question_job(fx: &[Effect]) -> LlmJob {
        let [Effect::Llm(job)] = fx else {
            panic!("{fx:?}");
        };
        assert_eq!(
            job.kind,
            JobKind::FlagQuestion {
                session_id: Some("s5".into())
            }
        );
        job.clone()
    }

    #[test]
    fn the_incident_menu_is_flagged_by_the_facts() {
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        assert!(job.prompt.starts_with(FLAG_QUESTION_PROMPT));
        assert!(job.prompt.contains("How should I merge PR #5?"));
        assert!(job.prompt.contains("Fix, then merge"));
        assert_eq!(*state(&a), FlagState::Checking);
        let fx = question_haiku(&mut a, job.card, question_says("someone_else", "merge", ""));
        assert_eq!(
            fx,
            [Effect::FlagFacts {
                card: job.card,
                cwd: "/w/api".into(),
                action: Action::Pr {
                    verb: PrVerb::Merge,
                    number: Some(5),
                    repo: Some("nodo-labs-uk/socratui".into()),
                    selector: None,
                    speaks: false,
                },
            }]
        );
        assert_eq!(*state(&a), FlagState::Checking, "until the facts are in");
        let fx = a.update(Msg::FlagFacts {
            card: job.card,
            result: Ok((pr(), me())),
        });
        let FlagState::Flagged(f) = state(&a) else {
            panic!("{:?}", state(&a));
        };
        assert_eq!(f.kind, FlagKind::SomeoneElse);
        assert_eq!(f.label, "SOMEONE ELSE");
        assert_eq!(
            f.target,
            "nodo-labs-uk/socratui PR #5 by Timonsc (outside your org)"
        );
        assert_eq!(f.conflict, ASKED);
        assert_eq!(f.who, "Timonsc opened PR #5");
        assert!(!f.pending);
        let [Effect::LogFlag(entry)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        assert_eq!(
            (
                &entry["card"],
                &entry["decision"],
                &entry["label"],
                &entry["by"]
            ),
            (
                &json!("choice"),
                &json!("flagged"),
                &json!("SOMEONE ELSE"),
                &json!("facts")
            )
        );
        let text = crate::ui::tests::render(&a, 100, 30);
        assert!(
            text.contains(" aq  ▌▌ question"),
            "the queue bar marks it: {text}"
        );
        assert!(
            text.contains("▌▌ SOMEONE ELSE · nodo-labs-uk/socratui PR #5"),
            "{text}"
        );
        assert!(text.contains(&format!("Conflict: {ASKED}")), "{text}");
        assert!(text.contains("How should I merge PR #5?"), "{text}");
        assert!(!text.contains("Checking whether"), "{text}");
    }

    #[test]
    fn facts_clear_a_wrong_haiku_flag() {
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        question_haiku(&mut a, job.card, question_says("someone_else", "merge", ""));
        let Facts::Pr(mine) = pr() else {
            unreachable!()
        };
        let mine = Facts::Pr(PrFacts {
            author: "smnai".into(),
            head_owner: "nodo-labs-uk".into(),
            cross_repo: false,
            base: "feat/x".into(),
            ..mine
        });
        let fx = a.update(Msg::FlagFacts {
            card: job.card,
            result: Ok((mine, me())),
        });
        assert_eq!(*state(&a), FlagState::None);
        assert_eq!(logged(&fx), ["cleared"]);
        let text = crate::ui::tests::render(&a, 100, 30);
        assert!(!text.contains("Checking whether"), "{text}");
        assert!(!text.contains("SOMEONE ELSE"), "{text}");
    }

    #[test]
    fn haiku_stands_when_there_are_no_facts_and_its_failure_clears() {
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        question_haiku(
            &mut a,
            job.card,
            question_says("someone_else", "comment", "a comment"),
        );
        let fx = a.update(Msg::FlagFacts {
            card: job.card,
            result: Err("gh is not logged in".into()),
        });
        assert_eq!(logged(&fx), ["flagged"]);
        assert_eq!(log_lines(&fx)[0]["by"], "haiku");
        let FlagState::Flagged(f) = state(&a) else {
            panic!("{:?}", state(&a));
        };
        assert_eq!(
            (f.kind, f.label.as_str(), f.target.as_str()),
            (
                FlagKind::SomeoneElse,
                "SOMEONE ELSE",
                "nodo-labs-uk/socratui PR #5"
            )
        );
        assert_eq!(f.as_you, "a comment");

        // Facts for a card that is gone change nothing.
        assert!(
            a.update(Msg::FlagFacts {
                card: 99,
                result: Ok((pr(), me()))
            })
            .is_empty()
        );

        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        let fx = question_haiku(&mut a, job.card, Err("took longer than 30 s".into()));
        assert_eq!(logged(&fx), ["error"]);
        assert_eq!(*state(&a), FlagState::None);
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        let fx = question_haiku(&mut a, job.card, Ok("not json".into()));
        assert_eq!(logged(&fx), ["error"]);
        assert_eq!(*state(&a), FlagState::None);
    }

    /// Haiku's answer about `kind` #`number` with `verb`.
    fn names(
        flag: &str,
        kind: &str,
        number: u64,
        verb: &str,
        happens: &str,
    ) -> Result<String, String> {
        Ok(json!({
            "flag": flag,
            "repo": if number > 0 { "nodo-labs-uk/socratui" } else { "" },
            "number": number,
            "kind": kind,
            "verb": verb,
            "who": "",
            "happens": happens,
            "as_you": "",
        })
        .to_string())
    }

    /// The incident menu, answered by Haiku with `said`.
    fn answered(said: Result<String, String>) -> (App, u64, Vec<Effect>) {
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        let fx = question_haiku(&mut a, job.card, said);
        (a, job.card, fx)
    }

    #[test]
    fn a_question_that_leads_to_no_write_is_not_flagged_or_looked_up() {
        // "Shall I fix issue #12?", from an outside reporter: a local fix.
        let (a, _, fx) = answered(names(
            "someone_else",
            "issue",
            12,
            "none",
            "fixes it locally",
        ));
        assert_eq!(logged(&fx), ["none"], "{fx:?}");
        assert_eq!(*state(&a), FlagState::None);

        // The user's own PR, already merged.
        let (a, _, fx) = answered(names("high_stakes", "pr", 5, "none", "already merged"));
        assert_eq!(logged(&fx), ["none"], "{fx:?}");
        assert_eq!(*state(&a), FlagState::None);
    }

    #[test]
    fn opening_a_pr_is_never_flagged_even_without_a_target() {
        // "Shall I open a PR?"
        let (a, _, fx) = answered(names("someone_else", "none", 0, "none", "opens a PR"));
        assert_eq!(logged(&fx), ["none"], "{fx:?}");
        assert_eq!(*state(&a), FlagState::None);
    }

    #[test]
    fn a_repo_that_is_not_owner_slash_name_never_reaches_gh() {
        for repo in ["--repo=x", "a/b c", "-o/r", "o/r/x", "o"] {
            let mut said: serde_json::Value =
                serde_json::from_str(&names("someone_else", "pr", 5, "merge", "").unwrap())
                    .unwrap();
            said["repo"] = repo.into();
            let (a, _, fx) = answered(Ok(said.to_string()));
            assert!(
                !fx.iter().any(|e| matches!(e, Effect::FlagFacts { .. })),
                "{repo}: {fx:?}"
            );
            // Haiku's flag stands, with no target named.
            let FlagState::Flagged(f) = state(&a) else {
                panic!("{repo}: {:?}", state(&a));
            };
            assert_eq!(f.target, "", "{repo}");
        }
    }

    #[test]
    fn a_named_write_is_judged_by_the_facts() {
        // An outsider's PR, merged.
        let (mut a, card, fx) = answered(names("none", "pr", 5, "merge", ""));
        let [Effect::FlagFacts { action, .. }] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        assert_eq!(
            *action,
            Action::Pr {
                verb: PrVerb::Merge,
                number: Some(5),
                repo: Some("nodo-labs-uk/socratui".into()),
                selector: None,
                speaks: false,
            }
        );
        a.update(Msg::FlagFacts {
            card,
            result: Ok((pr(), me())),
        });
        let FlagState::Flagged(f) = state(&a) else {
            panic!("{:?}", state(&a));
        };
        assert_eq!(f.label, "SOMEONE ELSE");

        // The user's own PR, merged into main.
        let (mut a, card, _) = answered(names("none", "pr", 5, "merge", ""));
        let Facts::Pr(theirs) = pr() else {
            unreachable!()
        };
        let mine = Facts::Pr(PrFacts {
            author: "smnai".into(),
            head_owner: "nodo-labs-uk".into(),
            cross_repo: false,
            my_last_review: None,
            commits_since_my_review: None,
            ..theirs
        });
        a.update(Msg::FlagFacts {
            card,
            result: Ok((mine, me())),
        });
        let FlagState::Flagged(f) = state(&a) else {
            panic!("{:?}", state(&a));
        };
        assert_eq!(
            (f.kind, f.label.as_str()),
            (FlagKind::HighStakes, "MAIN BRANCH")
        );
    }

    #[test]
    fn the_verb_says_whether_the_answer_speaks() {
        let speaks = |verb: &str, as_you: &str| {
            let mut said: serde_json::Value =
                serde_json::from_str(&names("none", "pr", 5, verb, "").unwrap()).unwrap();
            said["as_you"] = as_you.into();
            let (_, _, fx) = answered(Ok(said.to_string()));
            match fx.as_slice() {
                [
                    Effect::FlagFacts {
                        action: Action::Pr { speaks, .. },
                        ..
                    },
                ] => *speaks,
                fx => panic!("{fx:?}"),
            }
        };
        assert!(speaks("comment", ""));
        assert!(speaks("review", ""));
        assert!(!speaks("merge", ""));
        assert!(!speaks("merge", "None."));
        assert!(!speaks("close", "nothing"));
        assert!(speaks("close", "a closing note in your name"));
        assert!(!speaks("edit", "a new title"));
    }

    #[test]
    fn a_push_or_a_flag_without_a_target_keeps_haikus_flag() {
        let (a, _, fx) = answered(names("high_stakes", "none", 0, "push", "force pushes main"));
        assert_eq!(logged(&fx), ["flagged"]);
        assert_eq!(log_lines(&fx)[0]["by"], "haiku");
        let FlagState::Flagged(f) = state(&a) else {
            panic!("{:?}", state(&a));
        };
        assert_eq!((f.label.as_str(), f.target.as_str()), ("HIGH STAKES", ""));
        let text = crate::ui::tests::render(&a, 100, 30);
        assert!(text.contains("▲ HIGH STAKES "), "{text}");
        assert!(!text.contains("HIGH STAKES ·"), "{text}");

        // A release on a named PR is not looked up as a PR.
        let (a, _, fx) = answered(names("high_stakes", "pr", 5, "release", ""));
        assert_eq!(logged(&fx), ["flagged"], "{fx:?}");
        assert!(matches!(state(&a), FlagState::Flagged(_)));
    }

    #[test]
    fn a_question_without_action_words_runs_no_model() {
        let mut a = app();
        a.agents[0] = agent("%5", 5, "api", "/w/api", true);
        let fx = a.update(Msg::LastMessage {
            agent: a.agents[0].id.clone(),
            checkout: Checkout("/w/api".into()),
            text: "Which approach do you prefer?".into(),
            at: SystemTime::UNIX_EPOCH,
        });
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(*state(&a), FlagState::None);
        let text = crate::ui::tests::render(&a, 100, 30);
        assert!(!text.contains("Checking whether"), "{text}");

        // A question that names a PR is checked.
        let mut a = app();
        a.agents[0] = agent("%5", 5, "api", "/w/api", true);
        let fx = a.update(Msg::LastMessage {
            agent: a.agents[0].id.clone(),
            checkout: Checkout("/w/api".into()),
            text: "Shall I merge PR #5 now?".into(),
            at: SystemTime::UNIX_EPOCH,
        });
        let job = question_job(&fx);
        assert!(job.prompt.contains("Shall I merge PR #5 now?"));
        assert_eq!(*state(&a), FlagState::Checking);
        let text = crate::ui::tests::render(&a, 100, 30);
        assert!(
            text.contains("Checking whether this involves others…"),
            "{text}"
        );
        let fx = question_haiku(&mut a, job.card, question_says("none", "merge", ""));
        // A named target is looked up, whatever Haiku's flag.
        assert!(
            matches!(fx.as_slice(), [Effect::FlagFacts { .. }]),
            "{fx:?}"
        );
        let fx = a.update(Msg::FlagFacts {
            card: job.card,
            result: Ok((pr(), me())),
        });
        assert_eq!(logged(&fx), ["flagged"]);
        let entry = fx.iter().find_map(|e| match e {
            Effect::LogFlag(v) => Some(v["card"].clone()),
            _ => None,
        });
        assert_eq!(entry, Some(json!("question")));
    }

    #[test]
    fn answering_while_checking_works() {
        let mut a = app();
        let job = question_job(&incident_menu(&mut a));
        assert_eq!(*state(&a), FlagState::Checking);
        let later = look(&mut a);
        let fx = a.update(key(KeyCode::Char('2'), later));
        assert!(
            matches!(
                fx.as_slice(),
                [Effect::Reply {
                    req_id: 7,
                    decision: HookReply::Answer { .. }
                }]
            ),
            "{fx:?}"
        );
        assert!(a.queue.is_empty());
        // Haiku's late answer is for a card that is gone.
        assert!(
            question_haiku(&mut a, job.card, question_says("someone_else", "merge", "")).is_empty()
        );
        assert!(a.question_flags.is_empty());
    }

    #[test]
    fn flagged_cards_show_in_vibe_mode() {
        let mut a = app();
        a.config.set_mode(&Checkout("/w/api".into()), Mode::Vibe);
        merge(&mut a);
        assert!(matches!(held(&a), Held::Flagged(_)));
        let v = verdict(Decision::Grey, vec![Facts::Unknown], &[]);
        let fx = flag(&mut a, 2, "git push origin fix", v);
        let [Effect::Llm(job)] = fx.as_slice() else {
            panic!("{fx:?}");
        };
        haiku(&mut a, job.card, says("high_stakes"));
        assert_eq!(a.queue.len(), 2);
    }
}
