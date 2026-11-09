use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::agents::{Agent, AgentStatus};
use crate::config::Config;
use crate::git::{FileDiff, GrepHit};
use crate::ipc::{HookReply, HookRequest};
use crate::llm::{LlmEvent, LlmJob};
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::permission::{DenyDraft, PermissionRequest};
use crate::queue::Queue;
use crate::quiz::{AnswerDraft, CodeRequest, Snippet};
use crate::review::{self, APPROVE_MESSAGE, Comment, LastRound, ReviewCard, Target};
use crate::shell::{Shell, Snapshot, SnapshotRequest};
use crate::step::{PendingEdit, ReasonDraft, SearchList, Steps};
use crate::theme::Theme;
use crate::ui::input::{InputEvent, TextInput};

#[derive(Debug)]
pub enum Msg {
    /// A key, stamped by the runtime when it was read.
    Key(KeyEvent, Instant),
    /// Time passes; `now` comes from the runtime so `update` stays pure.
    Tick(Instant),
    Agents(Vec<Agent>),
    /// Listing agents failed; the last known list stays.
    AgentsFailed(String),
    /// A `Send` or `SaveConfig` effect failed in the runtime.
    EffectFailed(String),
    /// Writing the saved state failed.
    StateSaveFailed(String),
    /// A hook is waiting for `Effect::Reply` with this `req_id`.
    Ipc {
        req_id: u64,
        req: HookRequest,
        /// The checkout of the request's cwd, found by the runtime.
        checkout: Option<Checkout>,
        /// When the runtime received it.
        at: SystemTime,
        now: Instant,
    },
    /// A PreToolUse hook is waiting for `Effect::Reply` with this `req_id`;
    /// the runtime has read the file and transcript for it.
    PreTool {
        req_id: u64,
        edit: PendingEdit,
        /// When the runtime handed it over.
        at: Instant,
    },
    /// A tool finished (PostToolUse). `checkout` is the one the tool worked
    /// in: the edited file's for an edit tool, the cwd's otherwise.
    PostTool {
        session_id: String,
        checkout: Option<Checkout>,
        tool_name: String,
        tool_input: serde_json::Value,
        /// The file an edit tool touched, absolute.
        edited: Option<PathBuf>,
    },
    /// The result of an `Effect::Snapshot`.
    Snapshot {
        req: SnapshotRequest,
        result: Snapshot,
        at: SystemTime,
    },
    /// The hook waiting on `req_id` went away without an answer.
    HookGone {
        req_id: u64,
    },
    /// The terminal size, from the runtime before it draws.
    Resize {
        width: u16,
        height: u16,
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
    /// The diff a `ComputeQuiz` effect asked for.
    QuizDiff {
        checkout: Checkout,
        agent: AgentId,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        at: SystemTime,
    },
    /// The code an `Effect::ReadCode` asked for.
    CodeRead {
        card: u64,
        question: usize,
        result: Result<Snippet, String>,
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
    /// Write the config to disk. (Saved state is written by the runtime
    /// whenever `App::state_to_save` has something.)
    SaveConfig,
    ComputeReview(ReviewRequest),
    /// Diff a quiz-mode checkout from `from_tree` (the branch base when
    /// `None`) to a fresh snapshot, for a quiz card.
    ComputeQuiz {
        checkout: Checkout,
        agent: AgentId,
        from_tree: Option<String>,
    },
    /// Read the code around a quiz question's `code_ref`.
    ReadCode(CodeRequest),
    /// Snapshot a step-mode checkout's working tree, and diff it from the
    /// request's tree.
    Snapshot(SnapshotRequest),
    /// `git grep` for a whole word in the checkout (`gd` on a step card).
    Grep {
        checkout: Checkout,
        ident: String,
    },
    /// `g` on a permission card: switch tmux to the pane's window.
    GoToWindow {
        pane: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// `?`: side questions about the current card.
    Ask(Box<crate::ask::AskState>),
    /// `c` on the review board: typing a comment.
    Comment(CommentDraft),
    /// `n` or `e` on a step card: typing the reason.
    StepReason(ReasonDraft),
    /// `gd` on a step card: where the identifier appears.
    Search(SearchList),
    /// `n` on a permission card: typing the reason Claude reads.
    Deny(DenyDraft),
    /// `i` on a quiz card: typing a free-text answer.
    QuizAnswer(AnswerDraft),
    /// `m` on a quiz card: the code behind a question.
    QuizCode { card: u64, question: usize },
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
    pub(crate) sending: BTreeMap<u64, String>,
    /// Messages of cards in `sending` held until their agent is at its
    /// prompt, with the agent.
    pub(crate) held: BTreeMap<u64, (AgentId, String)>,
    pub steps: Steps,
    /// Terminal size as last reported by the runtime.
    pub(crate) screen: Option<(u16, u16)>,
    pub shell: Shell,
    /// The latest time a message carried.
    pub(crate) now: Option<Instant>,
    /// The card on screen, and when and how it got there.
    pub(crate) shown: Option<Shown>,
    pub(crate) next_card_id: u64,
    /// Saved drafts and quizzes waiting for their card to come back.
    pub(crate) unclaimed: crate::persist::Unclaimed,
    /// The state as last handed out to be written; `None` after a failed
    /// write, so the next look writes it again.
    pub(crate) saved: Option<crate::persist::SavedState>,
}

/// The card on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shown {
    pub id: u64,
    /// When it got there; `None` until a message with a time arrives after
    /// the card changed.
    pub at: Option<Instant>,
    /// It took the screen from another card without a key of the user's.
    pub preempted: bool,
}

/// Shown while a message waits for its agent to be at its prompt.
pub const QUEUED_STATUS: &str = "Queued: will send when the agent is idle";

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
            held: BTreeMap::new(),
            steps: Steps::default(),
            screen: None,
            shell: Shell::default(),
            now: None,
            shown: None,
            next_card_id: 0,
            unclaimed: Default::default(),
            saved: Some(Default::default()),
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
        // The user's own doing: a key, or the send a key started landing.
        let by_user = matches!(msg, Msg::Key(..) | Msg::SendDone { .. });
        let at = match &msg {
            Msg::Key(_, t)
            | Msg::Tick(t)
            | Msg::PreTool { at: t, .. }
            | Msg::Ipc { now: t, .. } => Some(*t),
            _ => None,
        };
        if let Some(t) = at {
            self.now = Some(t);
            if let Some(Shown { at: at @ None, .. }) = &mut self.shown {
                *at = Some(t);
            }
            self.expire_step_decisions(t);
        }
        let mut effects = self.handle(msg);
        effects.extend(self.release_held());
        // A review or quiz card for a checkout means the saves for it are
        // done with; other cards (a permission, a step) say nothing of them.
        if !self.unclaimed.is_empty() {
            let checkouts: Vec<PathBuf> = self
                .queue
                .iter()
                .filter(|c| matches!(c.kind, CardKind::Review(_) | CardKind::Quiz(_)))
                .map(|c| c.checkout.0.clone())
                .collect();
            for c in checkouts {
                self.unclaimed.forget(&c);
            }
        }
        let current = self.queue.current().map(|c| c.id);
        if self.shown.map(|s| s.id) != current {
            let preempted = !by_user && self.shown.is_some();
            self.shown = current.map(|id| Shown { id, at, preempted });
        }
        effects
    }

    fn handle(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key, _) => self.on_key(key),
            Msg::Tick(now) => {
                self.expire_status(now);
                Vec::new()
            }
            Msg::Agents(agents) => self.on_agents(agents),
            Msg::AgentsFailed(e) => {
                self.set_status(format!("Could not list agents: {e}"));
                Vec::new()
            }
            Msg::StateSaveFailed(e) => {
                self.set_status(format!("Could not save state: {e}"));
                self.saved = None;
                Vec::new()
            }
            Msg::EffectFailed(e) => {
                self.set_status(e);
                Vec::new()
            }
            Msg::Ipc {
                req_id,
                req:
                    HookRequest::Permission {
                        session_id,
                        cwd,
                        tool_name,
                        tool_input,
                        pane,
                    },
                checkout,
                at,
                ..
            } => self.on_permission(
                req_id,
                at,
                PermissionRequest {
                    session_id,
                    cwd,
                    tool_name,
                    tool_input,
                    pane,
                    checkout,
                },
            ),
            // No card for the other kinds; nothing waits.
            Msg::Ipc { req_id, .. } => vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }],
            Msg::PreTool { req_id, edit, .. } => self.on_pre_tool(req_id, edit),
            Msg::PostTool {
                session_id,
                checkout,
                tool_name,
                tool_input,
                edited,
            } => self.on_post_tool(
                &session_id,
                checkout,
                &tool_name,
                &tool_input,
                edited.as_deref(),
            ),
            Msg::Snapshot { req, result, at } => self.on_snapshot(req, result, at),
            Msg::Resize { width, height } => {
                // What was seen is counted in rows at the old width.
                if self.screen.is_some_and(|(w, _)| w != width) {
                    for c in self.queue.cards_mut() {
                        if let CardKind::Permission(p) = &mut c.kind {
                            p.scroll = 0;
                            p.seen = 0;
                        }
                    }
                }
                self.screen = Some((width, height));
                Vec::new()
            }
            Msg::HookGone { req_id } => {
                self.on_hook_gone(req_id);
                Vec::new()
            }
            Msg::GrepDone { ident, result } => {
                self.on_grep_done(ident, result);
                Vec::new()
            }
            Msg::Llm(ev) if matches!(ev.kind, crate::llm::JobKind::Ask { .. }) => {
                self.on_ask_result(ev)
            }
            Msg::Llm(ev) => self.on_llm(ev),
            Msg::QuizDiff {
                checkout,
                agent,
                from_tree,
                to_tree,
                files,
                at,
            } => self.on_quiz_diff(checkout, agent, from_tree, to_tree, files, at),
            Msg::CodeRead {
                card,
                question,
                result,
            } => self.on_code_read(card, question, result),
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
            Some(Overlay::Ask(_)) => return self.on_ask_key(key),
            Some(Overlay::Comment(_)) => {
                self.on_comment_key(key);
                return Vec::new();
            }
            Some(Overlay::StepReason(_)) => return self.on_reason_key(key),
            Some(Overlay::Deny(_)) => return self.on_deny_key(key),
            Some(Overlay::QuizAnswer(_)) => return self.on_quiz_answer_key(key),
            Some(Overlay::QuizCode { .. }) => {
                self.on_quiz_code_key(key);
                return Vec::new();
            }
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
                self.open_ask();
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
                Some(CardKind::ShellChange(_)) => self.on_shell_key(key),
                Some(CardKind::Permission(_)) => self.on_permission_key(key),
                Some(CardKind::Quiz(_)) => self.on_quiz_key(key),
                _ => self.on_review_key(key),
            },
            _ => Vec::new(),
        }
    }

    fn next_theme(&mut self) -> Vec<Effect> {
        self.theme_idx = (self.theme_idx + 1) % self.themes.len();
        self.config.theme = self.theme().name.clone();
        self.set_status(format!("Theme: {}", self.config.theme));
        vec![Effect::SaveConfig]
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
        // A save from the old mode would never be claimed.
        self.unclaimed.forget(&checkout.0);
        self.set_status(format!("{}: {}", project_name(&checkout), mode_label(mode)));
        let mut effects = vec![Effect::SaveConfig];
        match mode {
            // A fresh baseline: what changed before step mode is not a command's doing.
            Mode::Step => {
                self.forget_baseline(&checkout);
                effects.extend(self.request_snapshot(checkout, None, None));
            }
            _ => self.forget_baseline(&checkout),
        }
        effects
    }

    fn on_agents(&mut self, agents: Vec<Agent>) -> Vec<Effect> {
        let mut started: Vec<Agent> = became_waiting(&self.agents, &agents)
            .into_iter()
            .cloned()
            .collect();
        // After a restart an agent already waiting gets its card again when
        // a saved draft or quiz may belong to it.
        started.extend(
            agents
                .iter()
                .filter(|a| a.waiting && !self.agents.iter().any(|p| p.id == a.id))
                .filter(|a| a.checkout.as_ref().is_some_and(|c| self.unclaimed.has(c)))
                .cloned(),
        );
        self.agents = agents;
        let mut effects: Vec<Effect> = started
            .iter()
            .flat_map(|a| self.on_agent_waiting(a))
            .collect();
        // Checkouts already in step mode at startup need a baseline too.
        let checkouts: BTreeSet<Checkout> = self
            .agents
            .iter()
            .filter_map(|a| a.checkout.clone())
            .collect();
        for checkout in checkouts {
            effects.extend(self.ensure_baseline(&checkout));
        }
        effects
    }

    /// An agent just went from working to waiting. Review cards and quiz
    /// cards are created here, by the mode of its checkout.
    fn on_agent_waiting(&mut self, agent: &Agent) -> Vec<Effect> {
        let Some(checkout) = &agent.checkout else {
            return Vec::new();
        };
        let from_tree = self.rounds.get(&checkout.0).map(|r| r.tree.clone());
        match self.config.mode_for(checkout) {
            Mode::Review => vec![Effect::ComputeReview(ReviewRequest {
                checkout: checkout.clone(),
                agent: agent.id.clone(),
                from_tree,
                full_branch: false,
                card: None,
            })],
            Mode::Quiz => vec![Effect::ComputeQuiz {
                checkout: checkout.clone(),
                agent: agent.id.clone(),
                from_tree,
            }],
            Mode::Step => self.on_step_turn_end(checkout, agent),
        }
    }

    fn on_review_computed(
        &mut self,
        req: ReviewRequest,
        from_tree: String,
        to_tree: String,
        files: Vec<FileDiff>,
        at: SystemTime,
    ) {
        // A finished round claims its checkout's saved draft. Whatever
        // happens next (a card there already, nothing to review), older
        // saves for the checkout are done with.
        let draft = match req.card {
            Some(_) => None,
            None => self.unclaimed.take_draft(&req.checkout.0, &from_tree),
        };
        let existing = match req.card {
            Some(id) => self.queue.find_mut(|c| c.id == id),
            None => self
                .queue
                .find_mut(|c| c.checkout == req.checkout && matches!(c.kind, CardKind::Review(_))),
        };
        if let Some(card) = existing {
            // A card whose message is going out keeps the diff it shows:
            // the round it closes must be the tree the user reviewed.
            // Anything newer shows in the next round.
            if self.sending.contains_key(&card.id) {
                return;
            }
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
        let mut comments = last.map(|l| l.comments.clone()).unwrap_or_default();
        // A saved comment goes back on its file, at the nearest line, as
        // older rounds' comments do; one whose file left the diff cannot.
        let (placed, lost): (Vec<Comment>, Vec<Comment>) = draft
            .unwrap_or_default()
            .into_iter()
            .partition(|c| files.iter().any(|f| review::file_path(f) == c.path));
        if !lost.is_empty() {
            let n = lost.len();
            let (s, its) = if n == 1 {
                ("", "its file is")
            } else {
                ("s", "their files are")
            };
            self.set_status(format!(
                "{n} saved comment{s} dropped: {its} no longer in the diff"
            ));
        }
        comments.extend(placed.into_iter().map(|c| Comment { round, ..c }));
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
        if matches!(key.code, KeyCode::Char('S' | 'A')) && !self.settled(id) {
            self.too_quick("review");
            return Vec::new();
        }
        // What is sent is what the card holds when the send lands.
        if matches!(key.code, KeyCode::Char('c' | 'x' | 'R')) && self.sending.contains_key(&id) {
            self.set_status(if self.held.contains_key(&id) {
                QUEUED_STATUS
            } else {
                "Sending…"
            });
            return Vec::new();
        }
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
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
        self.send_when_ready(id, agent, text, done)
    }

    /// Sends `text` for card `id` once `agent` is at its prompt: waiting,
    /// idle, and with no permission menu open. Until then the card holds it.
    /// `done` is the status once it lands.
    pub(crate) fn send_when_ready(
        &mut self,
        id: u64,
        agent: AgentId,
        text: String,
        done: String,
    ) -> Vec<Effect> {
        self.sending.insert(id, done);
        if self.at_prompt(&agent) {
            self.set_status("Sending");
            return vec![Effect::Send {
                agent,
                text,
                card: Some(id),
            }];
        }
        self.held.insert(id, (agent, text));
        self.set_status(QUEUED_STATUS);
        Vec::new()
    }

    /// Whether text pasted into the agent's pane reaches its prompt.
    fn at_prompt(&self, id: &AgentId) -> bool {
        let menu_open = self
            .queue
            .iter()
            .any(|c| matches!(c.kind, CardKind::Permission(_)) && &c.agent == id);
        !menu_open
            && self
                .agent(id)
                .is_some_and(|a| a.waiting && a.status == AgentStatus::Idle)
    }

    /// Sends the held messages whose agent is at its prompt now. A message
    /// whose card or agent has gone is dropped, its card kept.
    fn release_held(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let ids: Vec<u64> = self.held.keys().copied().collect();
        for id in ids {
            let agent = &self.held[&id].0;
            let card_gone = !self.queue.iter().any(|c| c.id == id);
            let agent_gone = self.agent(agent).is_none();
            if card_gone || agent_gone {
                self.held.remove(&id);
                self.sending.remove(&id);
                if !card_gone {
                    self.set_status("Could not send: agent gone");
                }
            } else if self.at_prompt(agent) {
                let (agent, text) = self.held.remove(&id).expect("held");
                self.set_status("Sending");
                effects.push(Effect::Send {
                    agent,
                    text,
                    card: Some(id),
                });
            }
        }
        effects
    }

    fn on_send_result(&mut self, card: u64, error: Option<String>) -> Vec<Effect> {
        let Some(done) = self.sending.remove(&card) else {
            return Vec::new();
        };
        let kind = self.queue.iter().find(|c| c.id == card).map(|c| &c.kind);
        let (shell, quiz) = (
            matches!(kind, Some(CardKind::ShellChange(_))),
            matches!(kind, Some(CardKind::Quiz(_))),
        );
        match error {
            None if shell => {
                self.queue.remove(card);
                self.set_status(done);
                Vec::new()
            }
            None if quiz => self.close_quiz(card, done),
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
        Vec::new()
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

    pub(crate) fn app() -> App {
        App::new(Config::default(), builtin_themes())
    }

    pub(crate) fn key(c: char) -> Msg {
        Msg::Key(KeyEvent::from(KeyCode::Char(c)), Instant::now())
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
            a.update(Msg::Ipc {
                checkout: None,
                req_id: 7,
                req,
                at: SystemTime::UNIX_EPOCH,
                now: Instant::now(),
            }),
            [Effect::Reply {
                req_id: 7,
                decision: HookReply::NoDecision,
            }]
        );
    }

    #[test]
    fn theme_key_cycles_and_saves() {
        let mut a = app();
        assert_eq!(a.update(key('T')), [Effect::SaveConfig]);
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
        // Entering step mode also takes the baseline tree for shell changes.
        assert_eq!(
            a.update(key('M')),
            [
                Effect::SaveConfig,
                Effect::Snapshot(SnapshotRequest {
                    checkout: ck.clone(),
                    from: None,
                    from_guard: None,
                    command: None,
                    approved: None,
                })
            ]
        );
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
        assert!(matches!(a.overlay, Some(Overlay::Ask(_))));
        a.update(Msg::Key(KeyEvent::from(KeyCode::Esc), Instant::now()));
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

    pub(crate) fn ctrl(c: char) -> Msg {
        Msg::Key(
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
            Instant::now(),
        )
    }

    pub(crate) fn typed(a: &mut App, s: &str) {
        for c in s.chars() {
            a.update(key(c));
        }
    }

    /// The effect the agent finishing in `/a` asks for.
    pub(crate) fn finish_round(a: &mut App) -> ReviewRequest {
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        req.clone()
    }

    pub(crate) fn computed(req: ReviewRequest, from: &str, to: &str, files: Vec<FileDiff>) -> Msg {
        Msg::ReviewComputed {
            req,
            from_tree: from.into(),
            to_tree: to.into(),
            files,
            at: SystemTime::UNIX_EPOCH,
        }
    }

    pub(crate) fn fixture_files() -> Vec<FileDiff> {
        crate::git::parse_unified(crate::review::tests::FIXTURE.as_bytes())
    }

    pub(crate) fn review(a: &App) -> &ReviewCard {
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
    fn quiz_checkout_computes_no_review() {
        let mut a = app();
        a.config.set_mode(&Checkout("/a".into()), Mode::Quiz);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ComputeReview(_))),
            "{effects:?}"
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
        assert!(a.update(Msg::SendDone { card: id }).is_empty());
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
        assert!(a.update(key('A')).is_empty());
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
        assert!(a.update(Msg::SendDone { card: id }).is_empty());
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
        assert!(a.update(key('A')).is_empty());
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
    fn review_card_back_on_screen_by_itself_waits_before_approve() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        let t = Instant::now();
        let ask = HookRequest::Permission {
            session_id: "s1".into(),
            cwd: "/a".into(),
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            pane: Some("%1".into()),
        };
        a.update(Msg::Ipc {
            checkout: None,
            req_id: 9,
            req: ask,
            at: SystemTime::UNIX_EPOCH,
            now: t,
        });
        // Answered in the window: the review card comes back by itself.
        a.update(Msg::HookGone { req_id: 9 });
        // The runtime's next tick stamps when it came back.
        a.update(Msg::Tick(t + Duration::from_millis(100)));
        assert!(matches!(
            a.queue.current().unwrap().kind,
            CardKind::Review(_)
        ));
        assert!(a.preempted());
        let screen = crate::ui::tests::render(&a, 90, 10);
        assert!(
            screen.lines().next().unwrap().contains("new card"),
            "{screen}"
        );
        let at = |ms| {
            Msg::Key(
                KeyEvent::from(KeyCode::Char('A')),
                t + Duration::from_millis(ms),
            )
        };
        assert!(a.update(at(500)).is_empty());
        assert_eq!(a.queue.len(), 1);
        assert_eq!(
            a.status_text(),
            Some("New card: look at it before deciding")
        );
        assert!(matches!(
            a.update(at(1600)).as_slice(),
            [Effect::Send { .. }]
        ));
    }

    /// A round-1 review card for `/a` with one comment.
    fn commented(a: &mut App) -> u64 {
        let req = finish_round(a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('c'));
        typed(a, "why?");
        a.update(ctrl('s'));
        a.queue.current().unwrap().id
    }

    #[test]
    fn send_to_a_working_agent_waits_until_it_is_idle() {
        let mut a = app();
        let id = commented(&mut a);
        // The user typed into the window: working again.
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(a.update(key('S')).is_empty(), "held, not sent");
        assert_eq!(a.status_text(), Some(QUEUED_STATUS));
        assert_eq!(a.queue.len(), 1);
        let screen = crate::ui::tests::render(&a, 100, 20);
        assert!(screen.contains(QUEUED_STATUS), "{screen}");
        // Waiting, but `claude agents` still says busy: not yet.
        let mut busy = agent("%1", 1, "api", "/a", true);
        busy.status = crate::agents::AgentStatus::Busy;
        let effects = a.update(Msg::Agents(vec![busy]));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Send { .. })),
            "{effects:?}"
        );
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let sends: Vec<&Effect> = effects
            .iter()
            .filter(|e| matches!(e, Effect::Send { card: Some(c), .. } if *c == id))
            .collect();
        assert_eq!(sends.len(), 1, "{effects:?}");
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Sent 1 comment to api"));
    }

    #[test]
    fn changes_made_while_a_send_is_held_stay_unreviewed() {
        let mut a = app();
        let id = commented(&mut a);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(a.update(key('S')).is_empty(), "held, not sent");
        // The agent finishes again: the held message goes, and a fresh
        // diff of the newer tree is asked for.
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let req = effects
            .iter()
            .find_map(|e| match e {
                Effect::ComputeReview(r) => Some(r.clone()),
                _ => None,
            })
            .expect("a recompute");
        a.update(computed(req, "base", "t2", fixture_files()));
        assert_eq!(
            review(&a).to_tree,
            "t1",
            "the card being sent keeps its diff"
        );
        a.update(Msg::SendDone { card: id });
        assert_eq!(
            a.rounds[std::path::Path::new("/a")].tree,
            "t1",
            "t2 was never shown"
        );
    }

    #[test]
    fn a_card_being_sent_takes_no_more_edits() {
        let mut a = app();
        let id = commented(&mut a);
        assert!(matches!(
            a.update(key('S')).as_slice(),
            [Effect::Send { .. }]
        ));
        for k in ['c', 'x', 'R'] {
            a.status_msg = None;
            assert!(a.update(key(k)).is_empty(), "{k}");
            assert_eq!(a.overlay, None, "{k}");
            assert_eq!(a.status_text(), Some("Sending…"), "{k}");
        }
        assert_eq!(review(&a).current_comments().count(), 1);
        // Held for a busy agent: the same, saying why.
        let mut b = app();
        commented(&mut b);
        b.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        b.update(key('S'));
        b.update(key('c'));
        assert_eq!(b.overlay, None);
        assert_eq!(b.status_text(), Some(QUEUED_STATUS));
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
    }

    #[test]
    fn an_open_permission_prompt_holds_the_send() {
        let mut a = app();
        let id = commented(&mut a);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(a.update(key('S')).is_empty());
        let ask = HookRequest::Permission {
            session_id: "s1".into(),
            cwd: "/a".into(),
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            pane: Some("%1".into()),
        };
        a.update(Msg::Ipc {
            checkout: None,
            req_id: 9,
            req: ask,
            at: SystemTime::UNIX_EPOCH,
            now: Instant::now(),
        });
        // The agent shows as waiting, but on the permission menu.
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Send { .. })),
            "{effects:?}"
        );
        let effects = a.update(Msg::HookGone { req_id: 9 });
        assert!(
            matches!(effects.as_slice(), [Effect::Send { card: Some(c), .. }] if *c == id),
            "{effects:?}"
        );
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
