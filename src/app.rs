use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::agents::Agent;
use crate::config::Config;
use crate::git::{FileDiff, GrepHit};
use crate::ipc::{HookReply, HookRequest};
use crate::llm::{LlmEvent, LlmJob};
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::queue::Queue;
use crate::review::{self, APPROVE_MESSAGE, Comment, LastRound, ReviewCard, Target};
use crate::step::{PendingEdit, ReasonDraft, SearchList, Steps};
use crate::theme::Theme;
use crate::ui::input::{InputEvent, TextInput};

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    /// Time passes; `now` comes from the runtime so `update` stays pure.
    Tick(Instant),
    Agents(Vec<Agent>),
    /// Listing agents failed; the last known list stays.
    AgentsFailed(String),
    /// A `Send` or `SaveState` effect failed in the runtime.
    EffectFailed(String),
    /// A hook is waiting for `Effect::Reply` with this `req_id`.
    Ipc {
        req_id: u64,
        req: HookRequest,
    },
    /// A PreToolUse hook is waiting for `Effect::Reply` with this `req_id`;
    /// the runtime has read the file and transcript for it.
    PreTool {
        req_id: u64,
        edit: PendingEdit,
    },
    /// The hook waiting on `req_id` went away without an answer.
    HookGone {
        req_id: u64,
    },
    /// The result of an `Effect::Grep`.
    GrepDone {
        ident: String,
        result: Result<Vec<GrepHit>, String>,
    },
    Llm(LlmEvent),
    /// A `Send` for a review card reached the agent.
    SendDone {
        card: u64,
    },
    /// A `Send` for a review card failed; the card stays.
    SendFailed {
        card: u64,
        error: String,
    },
    /// The diff a `ComputeReview` effect asked for.
    ReviewComputed {
        req: ReviewRequest,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        at: SystemTime,
    },
}

/// A diff for the review board, computed by the runtime: from `from_tree`
/// (the branch base when `None`) to a fresh snapshot of the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    pub checkout: Checkout,
    pub agent: AgentId,
    pub from_tree: Option<String>,
    pub full_branch: bool,
    /// The card to refresh (`R`). `None` means the agent finished a round:
    /// refresh the checkout's review card, or make one if the diff is not empty.
    pub card: Option<u64>,
}

/// Work for the runtime. `update` never does I/O itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Send {
        agent: AgentId,
        text: String,
        /// The review card this sends; it closes only once the send succeeds.
        card: Option<u64>,
    },
    Reply {
        req_id: u64,
        decision: HookReply,
    },
    Llm(LlmJob),
    /// Write the config (and, later, drafts and quiz progress) to disk.
    SaveState,
    ComputeReview(ReviewRequest),
    /// Write `App::rounds` to the state dir.
    SaveRounds,
    /// `git grep` for a whole word in the checkout (`gd` on a step card).
    Grep {
        checkout: Checkout,
        ident: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// `?`: a side question about the current card.
    Ask,
    /// `c` on the review board: typing a comment.
    Comment(CommentDraft),
    /// `n` or `e` on a step card: typing the reason.
    StepReason(ReasonDraft),
    /// `gd` on a step card: where the identifier appears.
    Search(SearchList),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentDraft {
    pub card: u64,
    pub target: Target,
    pub input: TextInput,
}

pub struct App {
    pub queue: Queue,
    pub agents: Vec<Agent>,
    pub config: Config,
    pub themes: Vec<Theme>,
    pub theme_idx: usize,
    pub overlay: Option<Overlay>,
    pub status_msg: Option<Status>,
    pub quit: bool,
    /// Last sent review round per checkout root.
    pub rounds: BTreeMap<PathBuf, LastRound>,
    /// Review cards whose message is on its way, with the status to show once it lands.
    sending: BTreeMap<u64, String>,
    pub steps: Steps,
    pub(crate) next_card_id: u64,
}

/// How long a status message stays on screen.
pub const STATUS_FOR: Duration = Duration::from_secs(5);

/// A status line message. Its clock starts at the first tick after it is set,
/// so `update` never reads the real clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    shown_at: Option<Instant>,
}

impl App {
    /// `themes` must not be empty.
    pub fn new(config: Config, themes: Vec<Theme>) -> Self {
        let theme_idx = themes
            .iter()
            .position(|t| t.name == config.theme)
            .unwrap_or(0);
        App {
            queue: Queue::new(),
            agents: Vec::new(),
            config,
            themes,
            theme_idx,
            overlay: None,
            status_msg: None,
            quit: false,
            rounds: BTreeMap::new(),
            sending: BTreeMap::new(),
            steps: Steps::default(),
            next_card_id: 0,
        }
    }

    pub fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status_msg.as_ref().map(|s| s.text.as_str())
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status_msg = Some(Status {
            text: text.into(),
            shown_at: None,
        });
    }

    fn expire_status(&mut self, now: Instant) {
        let Some(status) = &mut self.status_msg else {
            return;
        };
        let shown_at = *status.shown_at.get_or_insert(now);
        if now.duration_since(shown_at) >= STATUS_FOR {
            self.status_msg = None;
        }
    }

    pub fn agent(&self, id: &AgentId) -> Option<&Agent> {
        self.agents.iter().find(|a| &a.id == id)
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Tick(now) => {
                self.expire_status(now);
                Vec::new()
            }
            Msg::Agents(agents) => self.on_agents(agents),
            Msg::AgentsFailed(e) => {
                self.set_status(format!("Could not list agents: {e}"));
                Vec::new()
            }
            Msg::EffectFailed(e) => {
                self.set_status(e);
                Vec::new()
            }
            // Cards for hook requests come later; until then nothing waits.
            Msg::Ipc { req_id, .. } => vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }],
            Msg::PreTool { req_id, edit } => self.on_pre_tool(req_id, edit),
            Msg::HookGone { req_id } => {
                self.on_hook_gone(req_id);
                Vec::new()
            }
            Msg::GrepDone { ident, result } => {
                self.on_grep_done(ident, result);
                Vec::new()
            }
            Msg::Llm(LlmEvent::Noop) => Vec::new(),
            Msg::SendDone { card } => self.on_send_result(card, None),
            Msg::SendFailed { card, error } => self.on_send_result(card, Some(error)),
            Msg::ReviewComputed {
                req,
                from_tree,
                to_tree,
                files,
                at,
            } => {
                self.on_review_computed(req, from_tree, to_tree, files, at);
                Vec::new()
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        match &self.overlay {
            Some(Overlay::Comment(_)) => {
                self.on_comment_key(key);
                return Vec::new();
            }
            Some(Overlay::StepReason(_)) => return self.on_reason_key(key),
            Some(Overlay::Search(_)) => {
                self.on_search_key(key);
                return Vec::new();
            }
            _ => {}
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('Q') => {
                self.quit = true;
                Vec::new()
            }
            KeyCode::Char('q') => {
                self.overlay = None;
                Vec::new()
            }
            KeyCode::Char('?') => {
                if self.queue.current().is_some() {
                    self.overlay = Some(Overlay::Ask);
                }
                Vec::new()
            }
            KeyCode::Char('T') => self.next_theme(),
            KeyCode::Char('M') => self.cycle_mode(),
            KeyCode::Char('s') => {
                self.queue.skip();
                Vec::new()
            }
            _ if self.overlay.is_none() => match self.queue.current().map(|c| &c.kind) {
                Some(CardKind::Step(_)) => self.on_step_key(key),
                _ => self.on_review_key(key),
            },
            _ => Vec::new(),
        }
    }

    fn next_theme(&mut self) -> Vec<Effect> {
        self.theme_idx = (self.theme_idx + 1) % self.themes.len();
        self.config.theme = self.theme().name.clone();
        self.set_status(format!("Theme: {}", self.config.theme));
        vec![Effect::SaveState]
    }

    fn cycle_mode(&mut self) -> Vec<Effect> {
        let Some(checkout) = self.queue.current().map(|c| c.checkout.clone()) else {
            self.set_status("No card, so no project to set a mode for");
            return Vec::new();
        };
        let mode = match self.config.mode_for(&checkout) {
            Mode::Review => Mode::Step,
            Mode::Step => Mode::Quiz,
            Mode::Quiz => Mode::Review,
        };
        self.config.set_mode(&checkout, mode);
        self.set_status(format!("{}: {}", project_name(&checkout), mode_label(mode)));
        vec![Effect::SaveState]
    }

    fn on_agents(&mut self, agents: Vec<Agent>) -> Vec<Effect> {
        let started: Vec<Agent> = became_waiting(&self.agents, &agents)
            .into_iter()
            .cloned()
            .collect();
        self.agents = agents;
        started
            .iter()
            .flat_map(|a| self.on_agent_waiting(a))
            .collect()
    }

    /// An agent just went from working to waiting. Review cards and quiz
    /// cards (Task 11) are created here, by the mode of its checkout.
    fn on_agent_waiting(&mut self, agent: &Agent) -> Vec<Effect> {
        let Some(checkout) = &agent.checkout else {
            return Vec::new();
        };
        if self.config.mode_for(checkout) != Mode::Review {
            return Vec::new();
        }
        vec![Effect::ComputeReview(ReviewRequest {
            checkout: checkout.clone(),
            agent: agent.id.clone(),
            from_tree: self.rounds.get(&checkout.0).map(|r| r.tree.clone()),
            full_branch: false,
            card: None,
        })]
    }

    fn on_review_computed(
        &mut self,
        req: ReviewRequest,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        at: SystemTime,
    ) {
        let existing = match req.card {
            Some(id) => self.queue.find_mut(|c| c.id == id),
            None => self
                .queue
                .find_mut(|c| c.checkout == req.checkout && matches!(c.kind, CardKind::Review(_))),
        };
        if let Some(card) = existing {
            if let CardKind::Review(rc) = &mut card.kind {
                rc.replace_diff(from_tree, to_tree, files, req.full_branch);
            }
            card.agent = req.agent;
            return;
        }
        // A refreshed card that has gone since, or nothing to review.
        if req.card.is_some() || files.is_empty() {
            return;
        }
        let last = self.rounds.get(&req.checkout.0);
        let round = last.map_or(1, |l| l.round + 1);
        let comments = last.map(|l| l.comments.clone()).unwrap_or_default();
        self.next_card_id += 1;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout: req.checkout,
            agent: req.agent,
            created: at,
            kind: CardKind::Review(ReviewCard::new(
                round,
                from_tree,
                to_tree,
                files,
                comments,
                req.full_branch,
            )),
        });
    }

    fn on_review_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let id = card.id;
        let CardKind::Review(rc) = &mut card.kind else {
            return Vec::new();
        };
        match review::on_key(rc, key) {
            review::Action::None => Vec::new(),
            review::Action::Status(s) => {
                self.set_status(s);
                Vec::new()
            }
            review::Action::Comment(target) => {
                self.overlay = Some(Overlay::Comment(CommentDraft {
                    card: id,
                    target,
                    input: TextInput::multi_line(),
                }));
                Vec::new()
            }
            review::Action::Send => self.finish_review(false),
            review::Action::Approve => self.finish_review(true),
            review::Action::ToggleFullBranch => self.toggle_full_branch(),
        }
    }

    fn on_comment_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Comment(draft)) = &mut self.overlay else {
            return;
        };
        let body = match draft.input.handle_key(key) {
            InputEvent::Editing => return,
            InputEvent::Cancel => None,
            InputEvent::Submit(body) => Some(body),
        };
        let Some(Overlay::Comment(draft)) = self.overlay.take() else {
            return;
        };
        let Some(body) = body.filter(|b| !b.trim().is_empty()) else {
            return;
        };
        let card = self.queue.find_mut(|c| c.id == draft.card);
        if let Some(Card {
            kind: CardKind::Review(rc),
            ..
        }) = card
        {
            let t = draft.target;
            rc.comments.push(Comment {
                path: t.path,
                line: t.line,
                end_line: t.end_line,
                side: t.side,
                body: body.trim_end().to_string(),
                round: rc.round,
            });
        }
    }

    /// `S` (send this round's comments) or `A` (approve). The card closes,
    /// and its snapshot becomes the start of the next round, only once the
    /// message has reached the agent (`Msg::SendDone`), or at once when
    /// nothing is sent: approving an agent that is working or gone.
    fn finish_review(&mut self, approve: bool) -> Vec<Effect> {
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let CardKind::Review(rc) = &card.kind else {
            return Vec::new();
        };
        let id = card.id;
        if self.sending.contains_key(&id) {
            self.set_status("Still sending");
            return Vec::new();
        }
        let agent = self.agent(&card.agent);
        let (text, done) = if approve {
            if !agent.is_some_and(|a| a.waiting) {
                return self.close_review(id, "Approved".into());
            }
            (APPROVE_MESSAGE.to_string(), "Approved".to_string())
        } else {
            let Some(agent) = agent else {
                self.set_status("agent gone");
                return Vec::new();
            };
            let n = rc.current_comments().count();
            if n == 0 {
                self.set_status("No comments to send; A approves");
                return Vec::new();
            }
            let s = if n == 1 { "" } else { "s" };
            (
                review::compose_message(rc),
                format!("Sent {n} comment{s} to {}", agent.window_name),
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

    fn on_send_result(&mut self, card: u64, error: Option<String>) -> Vec<Effect> {
        let Some(done) = self.sending.remove(&card) else {
            return Vec::new();
        };
        match error {
            None => self.close_review(card, done),
            Some(e) => {
                self.set_status(e);
                Vec::new()
            }
        }
    }

    /// Removes review card `id` and records its round.
    fn close_review(&mut self, id: u64, status: String) -> Vec<Effect> {
        let Some(Card {
            checkout,
            kind: CardKind::Review(rc),
            ..
        }) = self.queue.remove(id)
        else {
            return Vec::new();
        };
        self.rounds.insert(
            checkout.0,
            LastRound {
                tree: rc.to_tree,
                round: rc.round,
                comments: rc.comments,
            },
        );
        self.set_status(status);
        vec![Effect::SaveRounds]
    }

    fn toggle_full_branch(&mut self) -> Vec<Effect> {
        let Some(card) = self.queue.current() else {
            return Vec::new();
        };
        let CardKind::Review(rc) = &card.kind else {
            return Vec::new();
        };
        let Some(last) = self.rounds.get(&card.checkout.0) else {
            self.set_status("Round 1 already shows the whole branch");
            return Vec::new();
        };
        let full_branch = !rc.show_full_branch;
        let req = ReviewRequest {
            checkout: card.checkout.clone(),
            agent: card.agent.clone(),
            from_tree: (!full_branch).then(|| last.tree.clone()),
            full_branch,
            card: Some(card.id),
        };
        self.set_status(if full_branch {
            "Loading the whole branch diff"
        } else {
            "Loading changes since the last round"
        });
        vec![Effect::ComputeReview(req)]
    }
}

/// Agents in `next` that were known and not waiting in `prev`, and are waiting now.
/// An agent first seen already waiting is not a transition.
pub fn became_waiting<'a>(prev: &[Agent], next: &'a [Agent]) -> Vec<&'a Agent> {
    next.iter()
        .filter(|a| a.waiting)
        .filter(|a| prev.iter().any(|p| p.id == a.id && !p.waiting))
        .collect()
}

/// Warning for a card whose checkout has more than one agent in it.
pub fn shared_checkout_warning(card: &Card, agents: &[Agent]) -> Option<String> {
    let windows: Vec<&str> = agents
        .iter()
        .filter(|a| a.checkout.as_ref() == Some(&card.checkout))
        .map(|a| a.window_name.as_str())
        .collect();
    let [rest @ .., last] = windows.as_slice() else {
        return None;
    };
    if rest.is_empty() {
        return None;
    }
    Some(format!(
        "shared checkout: diff may mix work from windows {} and {last}",
        rest.join(", ")
    ))
}

pub fn project_name(checkout: &Checkout) -> String {
    checkout.0.file_name().map_or_else(
        || checkout.0.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Review => "review board",
        Mode::Step => "step by step",
        Mode::Quiz => "quiz",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::agents::AgentStatus;
    use crate::model::{CardKind, ReviewCard};
    use crate::queue::tests::card;
    use crate::theme::builtin_themes;

    pub(crate) fn agent(
        pane: &str,
        window: u32,
        name: &str,
        checkout: &str,
        waiting: bool,
    ) -> Agent {
        Agent {
            id: AgentId {
                pane: pane.into(),
                claude_pid: window * 100,
            },
            session: "main".into(),
            window_index: window,
            window_name: name.into(),
            cwd: PathBuf::from(checkout),
            checkout: Some(Checkout(checkout.into())),
            session_id: format!("s{window}"),
            status: if waiting {
                AgentStatus::Idle
            } else {
                AgentStatus::Busy
            },
            waiting,
        }
    }

    fn app() -> App {
        App::new(Config::default(), builtin_themes())
    }

    fn key(c: char) -> Msg {
        Msg::Key(KeyEvent::from(KeyCode::Char(c)))
    }

    #[test]
    fn waiting_transition_detected_once_per_transition() {
        let states = [false, true, true, false, true, true];
        let mut prev: Vec<Agent> = Vec::new();
        let mut hits = Vec::new();
        for (i, w) in states.into_iter().enumerate() {
            let next = vec![
                agent("%1", 1, "api", "/a", w),
                agent("%2", 2, "web", "/b", false),
            ];
            for a in became_waiting(&prev, &next) {
                hits.push((i, a.id.pane.clone()));
            }
            prev = next;
        }
        assert_eq!(hits, [(1, "%1".to_string()), (4, "%1".to_string())]);
    }

    #[test]
    fn agent_first_seen_waiting_is_not_a_transition() {
        let next = vec![agent("%1", 1, "api", "/a", true)];
        assert!(became_waiting(&[], &next).is_empty());
    }

    #[test]
    fn update_keeps_latest_agents() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        assert!(a.agents[0].waiting);
    }

    #[test]
    fn agents_failure_shows_and_keeps_list() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        a.update(Msg::AgentsFailed("tmux not found".into()));
        assert_eq!(
            a.status_text(),
            Some("Could not list agents: tmux not found")
        );
        assert_eq!(a.agents.len(), 1);
    }

    #[test]
    fn effect_failure_shows() {
        let mut a = app();
        a.update(Msg::EffectFailed("Could not send: pane gone".into()));
        assert_eq!(a.status_text(), Some("Could not send: pane gone"));
    }

    #[test]
    fn status_expires_five_seconds_after_first_tick() {
        let mut a = app();
        let t0 = Instant::now();
        a.update(key('T'));
        a.update(Msg::Tick(t0));
        a.update(Msg::Tick(t0 + Duration::from_millis(4900)));
        assert!(a.status_text().is_some());
        a.update(Msg::Tick(t0 + Duration::from_secs(5)));
        assert_eq!(a.status_text(), None);
        // A new message starts its own five seconds.
        a.update(key('T'));
        a.update(Msg::Tick(t0 + Duration::from_secs(9)));
        assert!(a.status_text().is_some());
        a.update(Msg::Tick(t0 + Duration::from_secs(14)));
        assert_eq!(a.status_text(), None);
    }

    #[test]
    fn hook_request_gets_no_decision_for_now() {
        let mut a = app();
        let req = HookRequest::PostTool {
            session_id: "s1".into(),
            cwd: "/a".into(),
            tool_name: "Bash".into(),
            tool_input: serde_json::Value::Null,
        };
        assert_eq!(
            a.update(Msg::Ipc { req_id: 7, req }),
            [Effect::Reply {
                req_id: 7,
                decision: HookReply::NoDecision,
            }]
        );
    }

    #[test]
    fn theme_key_cycles_and_saves() {
        let mut a = app();
        assert_eq!(a.update(key('T')), [Effect::SaveState]);
        assert_eq!(a.theme().name, "catppuccin-mocha");
        assert_eq!(a.config.theme, "catppuccin-mocha");
    }

    #[test]
    fn starts_on_configured_theme() {
        let cfg = Config {
            theme: "gruvbox-dark".into(),
            ..Config::default()
        };
        assert_eq!(App::new(cfg, builtin_themes()).theme().name, "gruvbox-dark");
    }

    #[test]
    fn mode_key_cycles_current_checkout() {
        let mut a = app();
        assert!(a.update(key('M')).is_empty());
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 0));
        let ck = Checkout("/a".into());
        assert_eq!(a.update(key('M')), [Effect::SaveState]);
        assert_eq!(a.config.mode_for(&ck), Mode::Step);
        a.update(key('M'));
        assert_eq!(a.config.mode_for(&ck), Mode::Quiz);
        a.update(key('M'));
        assert_eq!(a.config.mode_for(&ck), Mode::Review);
    }

    #[test]
    fn ask_opens_overlay_and_q_closes_it() {
        let mut a = app();
        a.update(key('?'));
        assert_eq!(a.overlay, None, "nothing to ask about without a card");
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 0));
        a.update(key('?'));
        assert_eq!(a.overlay, Some(Overlay::Ask));
        a.update(key('q'));
        assert_eq!(a.overlay, None);
        assert!(!a.quit);
        a.update(key('Q'));
        assert!(a.quit);
    }

    #[test]
    fn skip_key_skips() {
        let mut a = app();
        a.queue
            .push(card(1, "/a", CardKind::Review(ReviewCard::default()), 10));
        a.queue
            .push(card(2, "/b", CardKind::Review(ReviewCard::default()), 5));
        a.update(key('s'));
        assert_eq!(a.queue.current().unwrap().id, 2);
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(a: &mut App, s: &str) {
        for c in s.chars() {
            a.update(key(c));
        }
    }

    /// The effect the agent finishing in `/a` asks for.
    fn finish_round(a: &mut App) -> ReviewRequest {
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        req.clone()
    }

    fn computed(req: ReviewRequest, from: &str, to: &str, files: Vec<FileDiff>) -> Msg {
        Msg::ReviewComputed {
            req,
            from_tree: from.into(),
            to_tree: to.into(),
            files,
            at: SystemTime::UNIX_EPOCH,
        }
    }

    fn fixture_files() -> Vec<FileDiff> {
        crate::git::parse_unified(crate::review::tests::FIXTURE.as_bytes())
    }

    fn review(a: &App) -> &ReviewCard {
        match &a.queue.current().expect("a card").kind {
            CardKind::Review(rc) => rc,
            k => panic!("not a review card: {k:?}"),
        }
    }

    #[test]
    fn finished_agent_gets_review_card() {
        let mut a = app();
        let req = finish_round(&mut a);
        assert_eq!(
            req,
            ReviewRequest {
                checkout: Checkout("/a".into()),
                agent: a.agents[0].id.clone(),
                from_tree: None,
                full_branch: false,
                card: None,
            }
        );
        assert!(a.queue.is_empty(), "no card until the diff is in");
        a.update(computed(req, "base", "t1", fixture_files()));
        let rc = review(&a);
        assert_eq!(
            (rc.round, rc.from_tree.as_str(), rc.to_tree.as_str()),
            (1, "base", "t1")
        );
        assert_eq!(a.queue.current().unwrap().agent, a.agents[0].id);
    }

    #[test]
    fn empty_diff_makes_no_card() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", Vec::new()));
        assert!(a.queue.is_empty());
    }

    #[test]
    fn non_review_checkout_computes_nothing() {
        let mut a = app();
        a.config.set_mode(&Checkout("/a".into()), Mode::Quiz);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(
            a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]))
                .is_empty()
        );
    }

    #[test]
    fn comment_then_send() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        assert!(matches!(a.overlay, Some(Overlay::Comment(_))));
        typed(&mut a, "store down?");
        // Keys go to the comment box, so this `S` is typed, not a send.
        typed(&mut a, "S");
        a.update(ctrl('s'));
        assert_eq!(a.overlay, None);
        assert_eq!(review(&a).comments[0].body, "store down?S");
        let id = a.queue.current().unwrap().id;
        let effects = a.update(key('S'));
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Review comments (round 1):\n\nsrc/limit.rs:40\n\
                       > let b = store.get(key).unwrap_or_default();\nstore down?S"
                    .into(),
                card: Some(id),
            }]
        );
        assert_eq!(a.queue.len(), 1, "the card waits for the send");
        assert!(a.rounds.is_empty());
        assert!(
            a.update(key('S')).is_empty(),
            "no second send while sending"
        );
        assert_eq!(a.update(Msg::SendDone { card: id }), [Effect::SaveRounds]);
        assert!(a.queue.is_empty());
        assert_eq!(a.rounds[&PathBuf::from("/a")].tree, "t1");
        assert_eq!(a.status_text(), Some("Sent 1 comment to api"));
    }

    #[test]
    fn send_without_comments_is_refused() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        assert!(a.update(key('S')).is_empty());
        assert_eq!(a.queue.len(), 1);
    }

    #[test]
    fn send_to_gone_agent_is_refused() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "x");
        a.update(ctrl('s'));
        // The agent's window closed.
        a.update(Msg::Agents(Vec::new()));
        let effects = a.update(key('S'));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Send { .. })),
            "{effects:?}"
        );
        assert_eq!(a.status_text(), Some("agent gone"));
        assert_eq!(a.queue.len(), 1, "the card stays");
    }

    #[test]
    fn approve_with_gone_agent_closes_without_sending() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(Msg::Agents(Vec::new()));
        assert_eq!(a.update(key('A')), [Effect::SaveRounds]);
        assert!(a.queue.is_empty());
        assert_eq!(a.rounds[&PathBuf::from("/a")].tree, "t1");
    }

    #[test]
    fn failed_send_keeps_card_comments_and_baseline() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "keep me");
        a.update(ctrl('s'));
        let id = a.queue.current().unwrap().id;
        a.update(key('S'));
        let effects = a.update(Msg::SendFailed {
            card: id,
            error: "agent gone".into(),
        });
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(a.status_text(), Some("agent gone"));
        assert_eq!(a.queue.len(), 1);
        assert_eq!(review(&a).comments[0].body, "keep me");
        assert_eq!(review(&a).current_comments().count(), 1);
        assert!(a.rounds.is_empty());
        // The user can try again.
        assert!(matches!(
            a.update(key('S')).as_slice(),
            [Effect::Send { .. }]
        ));
    }

    #[test]
    fn send_done_closes_card_and_records_round() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        let id = a.queue.current().unwrap().id;
        a.update(key('A'));
        assert_eq!(a.queue.len(), 1);
        assert_eq!(a.update(Msg::SendDone { card: id }), [Effect::SaveRounds]);
        assert!(a.queue.is_empty());
        let last = &a.rounds[&PathBuf::from("/a")];
        assert_eq!((last.tree.as_str(), last.round), ("t1", 1));
        assert_eq!(a.status_text(), Some("Approved"));
    }

    #[test]
    fn approve_sends_only_to_waiting_agent() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        let id = a.queue.current().unwrap().id;
        assert_eq!(
            a.update(key('A')),
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Approved, carry on.".into(),
                card: Some(id),
            }]
        );
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        // The agent is working again by the time the user approves the next round.
        a.update(computed(req, "t1", "t2", fixture_files()));
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert_eq!(a.update(key('A')), [Effect::SaveRounds]);
        assert!(a.queue.is_empty());
        assert_eq!(a.rounds[&PathBuf::from("/a")].round, 2);
    }

    #[test]
    fn round_two_uses_last_snapshot() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(&mut a, "first");
        a.update(ctrl('s'));
        let id = a.queue.current().unwrap().id;
        a.update(key('S'));
        a.update(Msg::SendDone { card: id });

        let req = finish_round(&mut a);
        assert_eq!(req.from_tree.as_deref(), Some("t1"));
        a.update(computed(req, "t1", "t2", fixture_files()));
        let rc = review(&a);
        assert_eq!(rc.round, 2);
        assert_eq!(rc.from_tree, "t1");
        assert_eq!(rc.comments.len(), 1, "round 1 comment shown as old");
        assert_eq!(rc.comments[0].round, 1);
        assert_eq!(rc.current_comments().count(), 0, "and not resent");
    }

    /// `A`, and the send (if any) succeeds.
    fn approve(a: &mut App) {
        let id = a.queue.current().unwrap().id;
        if !a.update(key('A')).is_empty() {
            a.update(Msg::SendDone { card: id });
        }
    }

    fn toggle(a: &mut App) -> ReviewRequest {
        match a.update(key('R')).as_slice() {
            [Effect::ComputeReview(r)] => r.clone(),
            e => panic!("expected one ComputeReview, got {e:?}"),
        }
    }

    #[test]
    fn full_branch_toggle_recomputes_from_base() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        assert!(
            a.update(key('R')).is_empty(),
            "round 1 is already the whole branch"
        );
        approve(&mut a);

        let req = finish_round(&mut a);
        a.update(computed(req, "t1", "t2", fixture_files()));
        let id = a.queue.current().unwrap().id;
        let r = toggle(&mut a);
        assert_eq!(
            (r.from_tree.as_deref(), r.full_branch, r.card),
            (None, true, Some(id))
        );
        a.update(computed(r, "base", "t3", fixture_files()));
        assert!(review(&a).show_full_branch);
        assert_eq!(review(&a).from_tree, "base");
        let r = toggle(&mut a);
        assert_eq!((r.from_tree.as_deref(), r.full_branch), (Some("t1"), false));
        // A recompute for a card closed meanwhile makes no new card.
        approve(&mut a);
        a.update(computed(r, "t1", "t4", fixture_files()));
        assert!(a.queue.is_empty());
    }

    #[test]
    fn shared_checkout_names_both_windows() {
        let c = card(1, "/a", CardKind::Review(ReviewCard::default()), 0);
        let one = [
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "web", "/b", true),
        ];
        assert_eq!(shared_checkout_warning(&c, &one), None);
        let two = [
            agent("%1", 1, "api", "/a", true),
            agent("%3", 3, "tests", "/a", false),
        ];
        assert_eq!(
            shared_checkout_warning(&c, &two).unwrap(),
            "shared checkout: diff may mix work from windows api and tests"
        );
    }
}
