use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::agents::{Agent, AgentStatus, InputBox};
use crate::config::Config;
use crate::git::{FileDiff, GrepHit};
use crate::ipc::{HookReply, HookRequest};
use crate::llm::{LlmEvent, LlmJob};
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::new_session::DirListing;
pub use crate::new_session::NewSessionDraft;
use crate::permission::{DenyDraft, PermissionRequest};
use crate::queue::Queue;
use crate::quiz::{AnswerDraft, CodeRequest, Snippet};
use crate::review::{self, APPROVE_MESSAGE, Comment, LastRound, ReviewCard, Target};
use crate::shell::{Shell, Snapshot, SnapshotRequest};
use crate::step::{PendingEdit, ReasonDraft, SearchList, Steps};
use crate::theme::Theme;
use crate::transcript::Said;
use crate::ui::input::{InputEvent, TextInput};

/// What the left mouse button did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Drag,
    Up,
}

/// A mouse selection, from the cell where it started to where it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub from: (u16, u16),
    pub to: (u16, u16),
    /// What each end grows to on screen, as in a terminal.
    pub unit: Unit,
}

/// What a selection's ends take in: set by clicking once, twice or three
/// times in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Cell,
    Word,
    Line,
}

/// Clicks on one cell closer together than this count as a double or
/// triple click.
const MULTI_CLICK: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum Msg {
    /// A key, stamped by the runtime when it was read.
    Key(KeyEvent, Instant),
    /// Time passes; `now` comes from the runtime so `update` stays pure.
    Tick(Instant),
    /// The left mouse button went down, moved or came up at a cell, stamped
    /// by the runtime when it was read.
    Mouse {
        kind: MouseKind,
        col: u16,
        row: u16,
        at: Instant,
    },
    /// The mouse wheel turned one notch.
    Wheel {
        up: bool,
    },
    /// The conversation a `ReadConversation` asked for.
    Conversation {
        session_id: String,
        said: Result<Vec<Said>, String>,
    },
    /// The folders an `Effect::ListDirs` asked for.
    Dirs(DirListing),
    /// The message a session ended its turn with, from its transcript.
    LastMessage {
        agent: AgentId,
        checkout: Checkout,
        text: String,
        at: SystemTime,
    },
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
    /// What a `CapturePane` effect read from the pane's screen.
    PaneCaptured {
        pane: String,
        text: Result<String, String>,
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
    /// The card to refresh (`r`). `None` means the agent finished a round:
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
    /// `g` on a permission card, or Enter on an idle agent: switch tmux to
    /// the pane's window.
    GoToWindow {
        pane: String,
    },
    /// Read the text on a pane's screen, for the empty screen's live view.
    CapturePane {
        pane: String,
    },
    /// Read a session's conversation from its transcript, for scrolling
    /// back in the live view.
    ReadConversation {
        session_id: String,
    },
    /// Read the message a session ended its turn with, in case it asks
    /// for an answer.
    ReadLastMessage {
        agent: AgentId,
        session_id: String,
        checkout: Checkout,
    },
    /// Close a pane, ending the agent in it.
    ClosePane {
        pane: String,
    },
    /// Send `/model` to a session, and say yes if Claude Code asks whether
    /// to switch.
    SwitchModel {
        agent: AgentId,
        model: &'static str,
    },
    /// Press Shift+Tab in a session until its footer shows `mode`.
    SetPermissionMode {
        agent: AgentId,
        mode: &'static str,
    },
    /// Press Escape in a pane, stopping the agent's turn.
    Interrupt {
        pane: String,
    },
    /// Start `claude` in a new tmux window in `dir`, made first if it is
    /// not there, with `prompt` as its first message if there is one.
    NewSession {
        dir: PathBuf,
        prompt: Option<String>,
        /// In a git worktree of its own, made by `claude --worktree`.
        worktree: bool,
    },
    /// List the folders matching `typed`, a path as `new_session::resolve`
    /// reads it against `base`, for the new session's folder box.
    ListDirs {
        typed: String,
        base: PathBuf,
    },
    /// Copy the screen's text under a selection (row by row, as a terminal
    /// selects) to the tmux buffer, and so the clipboard.
    Copy(Selection),
    /// Open `path` at `line` in $VISUAL or $EDITOR, in a tmux popup over aq.
    Edit {
        dir: PathBuf,
        path: PathBuf,
        line: u32,
    },
    /// Open a shell in a tmux popup over aq, gone when the shell exits.
    Shell {
        dir: PathBuf,
        title: String,
        /// Whose last `! command` to have typed in, if any.
        session_id: Option<String>,
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
    /// `o` on a choice card: typing an answer that is none of the options.
    OwnAnswer(crate::choice::OwnAnswerDraft),
    /// `n` on a permission card: typing the reason Claude reads.
    Deny(DenyDraft),
    /// `i` on a quiz card: typing a free-text answer.
    QuizAnswer(AnswerDraft),
    /// `m` on a quiz card: the code behind a question.
    QuizCode { card: u64, question: usize },
    /// F1: the help page, scrolled down by `scroll` lines.
    Help { scroll: u16 },
    /// The tour, on a first run or after F2: page `page`.
    Tour { page: usize },
    /// `i` on the empty screen: a message being typed to an agent.
    Prompt(PromptDraft),
    /// `x` on the empty screen: asking before closing an agent's pane.
    CloseAgent(AgentId),
    /// `N`: the first prompt for a new session.
    NewSession(NewSessionDraft),
    /// `m` on the empty screen: choosing a model for an agent.
    Model(AgentId),
    /// Picking a session's permission mode.
    PermissionMode(AgentId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDraft {
    pub agent: AgentId,
    pub input: TextInput,
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
    /// The card on screen, if its hook went away (answered in the window or
    /// timed out): it stays until a key moves on, and sends nothing.
    pub(crate) stale: Option<u64>,
    pub(crate) next_card_id: u64,
    /// When Esc was last pressed on the empty screen; a second one soon
    /// after stops the picked agent.
    esc_at: Option<Instant>,
    /// Saved drafts and quizzes waiting for their card to come back.
    pub(crate) unclaimed: crate::persist::Unclaimed,
    /// The state as last handed out to be written; `None` after a failed
    /// write, so the next look writes it again.
    pub(crate) saved: Option<crate::persist::SavedState>,
    /// The idle agent picked on the empty screen, to go to with Enter.
    pub(crate) picked: Option<AgentId>,
    /// The session shown before the picked one, for Tab to go back to.
    pub(crate) last_picked: Option<AgentId>,
    /// The last screen read from a pane, for the empty screen's live view.
    pub preview: Option<Preview>,
    /// A `CapturePane` is on its way, so another is not asked for.
    pub(crate) capturing: bool,
    /// Messages typed on the empty screen, waiting for their agent to be at
    /// its prompt.
    pub(crate) prompts: BTreeMap<AgentId, String>,
    /// Checkouts each session (by id) worked in this turn, its own or not,
    /// from its tool hooks. Each gets its card when the turn ends.
    pub(crate) touched: BTreeMap<String, BTreeSet<Checkout>>,
    /// 0: the live view shows the picked agent's screen. More: it shows
    /// its conversation, scrolled up this many rows less one from the end.
    pub live_scroll: usize,
    /// Text being selected with the mouse: from where the button went down
    /// to where it is now. Drawn reversed; copied when the button comes up.
    pub selection: Option<Selection>,
    /// The last click: where, when, and how many came in a row there.
    last_click: Option<((u16, u16), Instant, u8)>,
    /// The tour's practice: which step it is on.
    pub(crate) practice: Option<usize>,
    /// The tour's made-up sessions, listed in place of the real ones while
    /// its sessions practice lasts.
    pub(crate) practice_agents: Vec<Agent>,
    /// The conversation last read, and whose.
    pub conversation: Option<(String, Result<Vec<Said>, String>)>,
    /// A `ReadConversation` is on its way.
    pub(crate) reading_conversation: bool,
}

/// A pane's screen as last read, or why it could not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub pane: String,
    pub text: Result<String, String>,
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

/// What p on the empty screen sends: the skill that drafts, checks and opens a PR.
pub const PR_COMMAND: &str = "/create-pr";

/// The models `m` on the empty screen offers: the key that picks each, and
/// the alias `/model` takes.
pub const MODELS: [(char, &str); 4] = [
    ('f', "fable"),
    ('o', "opus"),
    ('s', "sonnet"),
    ('h', "haiku"),
];

/// The keys that pick a permission mode, with the mode's name as
/// `PERMISSION_MODES` has it.
pub const PERMISSION_KEYS: [(char, &str); 4] = [
    ('a', "auto"),
    ('m', "manual"),
    ('e', "accept edits"),
    ('p', "plan"),
];

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
            stale: None,
            next_card_id: 0,
            esc_at: None,
            unclaimed: Default::default(),
            saved: Some(Default::default()),
            picked: None,
            last_picked: None,
            preview: None,
            capturing: false,
            prompts: BTreeMap::new(),
            touched: BTreeMap::new(),
            live_scroll: 0,
            selection: None,
            last_click: None,
            practice: None,
            practice_agents: Vec::new(),
            conversation: None,
            reading_conversation: false,
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
        self.agents
            .iter()
            .chain(&self.practice_agents)
            .find(|a| &a.id == id)
    }

    /// The agents in window order, as the empty screen lists them.
    pub fn agents_in_order(&self) -> Vec<&Agent> {
        let listed = if self.practice_agents.is_empty() {
            &self.agents
        } else {
            &self.practice_agents
        };
        let mut agents: Vec<&Agent> = listed.iter().collect();
        agents.sort_by(|a, b| (&a.session, a.window_index).cmp(&(&b.session, b.window_index)));
        agents
    }

    /// The agent picked on the empty screen. Until one is picked, or once the
    /// pick is gone, the first waiting agent, else the first agent.
    pub fn picked_agent(&self) -> Option<&Agent> {
        let agents = self.agents_in_order();
        agents
            .iter()
            .find(|a| Some(&a.id) == self.picked.as_ref())
            .or(agents.iter().find(|a| a.waiting))
            .or(agents.first())
            .copied()
    }

    /// Shows `id` on the empty screen, remembering the session shown
    /// before it for Tab.
    fn pick(&mut self, id: AgentId) {
        let shown = self.picked_agent().map(|a| a.id.clone());
        if shown.as_ref() != Some(&id) {
            self.last_picked = shown;
        }
        self.picked = Some(id);
    }

    /// h/l or j/k pick an agent on the empty screen; Tab goes back to the
    /// one shown before; Enter or i write it a message;
    /// g goes to its window; p asks it to open a pull request; m picks its model.
    fn on_empty_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ids: Vec<AgentId> = self
            .agents_in_order()
            .iter()
            .map(|a| a.id.clone())
            .collect();
        let Some(current) = self.picked_agent().map(|a| a.id.clone()) else {
            return Vec::new();
        };
        let i = ids.iter().position(|id| *id == current).unwrap_or(0);
        let n = ids.len();
        match key.code {
            KeyCode::Char('j' | 'l') | KeyCode::Down | KeyCode::Right => {
                self.pick(ids[(i + 1) % n].clone());
                self.live_scroll = 0;
                return self.capture_picked();
            }
            KeyCode::Char('k' | 'h') | KeyCode::Up | KeyCode::Left => {
                self.pick(ids[(i + n - 1) % n].clone());
                self.live_scroll = 0;
                return self.capture_picked();
            }
            KeyCode::Tab => {
                let Some(last) = self.last_picked.clone().filter(|id| ids.contains(id)) else {
                    self.set_status("No other session shown yet: h/l picks one");
                    return Vec::new();
                };
                self.pick(last);
                self.live_scroll = 0;
                return self.capture_picked();
            }
            KeyCode::PageUp => return self.scroll_live(true, self.live_page()),
            KeyCode::PageDown => return self.scroll_live(false, self.live_page()),
            KeyCode::Home => return self.scroll_live(true, usize::MAX),
            KeyCode::Char('G') | KeyCode::End => self.live_scroll = 0,
            KeyCode::Char('g') => {
                return vec![Effect::GoToWindow { pane: current.pane }];
            }
            KeyCode::Char('p') => return self.ask_for_pr(current),
            KeyCode::Char('m') => self.open_model(current),
            KeyCode::Char('P') => self.overlay = Some(Overlay::PermissionMode(current)),
            KeyCode::Esc => return self.on_empty_esc(current),
            KeyCode::Char('x') => self.overlay = Some(Overlay::CloseAgent(current)),
            KeyCode::Enter | KeyCode::Char('i') => {
                self.overlay = Some(Overlay::Prompt(PromptDraft {
                    agent: current,
                    input: TextInput::single_line(),
                }));
            }
            _ => {}
        }
        Vec::new()
    }

    /// Esc twice within `DOUBLE_ESC` stops the agent: its turn is
    /// interrupted and a message queued for it is dropped. One Esc only says
    /// so, since stopping by accident costs the turn's work.
    fn on_empty_esc(&mut self, agent: AgentId) -> Vec<Effect> {
        const DOUBLE_ESC: Duration = Duration::from_millis(800);
        let now = self.now.unwrap_or_else(Instant::now);
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        let second = self
            .esc_at
            .take()
            .is_some_and(|t| now.duration_since(t) <= DOUBLE_ESC);
        if !second {
            self.esc_at = Some(now);
            self.set_status(format!("Esc again to stop {name}"));
            return Vec::new();
        }
        let dropped = self.prompts.remove(&agent).is_some();
        let working = self.agent(&agent).is_some_and(|a| !a.waiting);
        let what = match (working, dropped) {
            (true, true) => "Stopped {name} and dropped its queued message",
            (true, false) => "Stopped {name}",
            (false, true) => "Dropped the message queued for {name}",
            (false, false) => "{name} is not working on anything",
        };
        self.set_status(what.replace("{name}", &name));
        if working {
            vec![Effect::Interrupt { pane: agent.pane }]
        } else {
            Vec::new()
        }
    }

    /// y or Enter closes the agent's pane; n, q or Esc keeps it.
    fn on_close_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::CloseAgent(agent)) = &self.overlay else {
            return Vec::new();
        };
        let agent = agent.clone();
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.overlay = None;
                let name = self
                    .agent(&agent)
                    .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
                self.set_status(format!("Closed {name}"));
                vec![Effect::ClosePane { pane: agent.pane }]
            }
            KeyCode::Char('n' | 'q') | KeyCode::Esc => {
                self.overlay = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_prompt_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Prompt(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let text = match draft.input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => None,
            InputEvent::Submit(text) => Some(text),
        };
        let Some(Overlay::Prompt(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        match text.filter(|t| !t.trim().is_empty()) {
            Some(text) => self.send_prompt(draft.agent, text.trim().to_string()),
            // On the sessions view the Esc that closes the box is also the
            // first of the two that stop the session, as in Claude Code.
            None if key.code == KeyCode::Esc && self.queue.current().is_none() => {
                self.esc_at = None;
                self.on_empty_esc(draft.agent)
            }
            None => Vec::new(),
        }
    }

    /// Sends a message typed on the empty screen now if the agent is at its
    /// prompt, or once it is. A second message for the same agent joins the
    /// one waiting, so both arrive as one.
    pub(crate) fn send_prompt(&mut self, agent: AgentId, text: String) -> Vec<Effect> {
        // Any message to the session answers what it asked.
        self.drop_questions(false, |a| *a == agent);
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        if let Some(waiting) = self.prompts.get_mut(&agent) {
            waiting.push_str("\n\n");
            waiting.push_str(&text);
            self.set_status(format!("Added to the message waiting for {name}"));
            return Vec::new();
        }
        if self.at_prompt(&agent) {
            self.set_status(format!("Sent to {name}"));
            return vec![Effect::Send {
                agent,
                text,
                card: None,
            }];
        }
        // Say which of the two it waits on: only one of them is yours to fix.
        let status = if self.prompt_has_text(&agent) {
            format!("Queued: {name} has text typed in its box. Clear it there and this is sent")
        } else {
            format!("Queued: will send to {name} when it is idle")
        };
        self.prompts.insert(agent, text);
        self.set_status(status);
        Vec::new()
    }

    /// Whether a message typed on the empty screen waits for this agent.
    pub fn prompt_waiting(&self, agent: &AgentId) -> bool {
        self.prompts.contains_key(agent)
    }

    /// Ctrl-d/u half a page and Ctrl-f/b a whole page down or up, as that
    /// many j or k, on a card or the help page. `None` for any other key.
    fn on_page_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let KeyCode::Char(c @ ('d' | 'u' | 'f' | 'b')) = key.code else {
            return None;
        };
        if self.overlay.is_none()
            && self.queue.current().is_none()
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            let page = self.live_page();
            let n = if matches!(c, 'd' | 'u') {
                page / 2
            } else {
                page
            };
            return Some(self.scroll_live(matches!(c, 'u' | 'b'), n));
        }
        let on_page = match &self.overlay {
            None => self.queue.current().is_some(),
            Some(o) => matches!(o, Overlay::Help { .. }),
        };
        if !key.modifiers.contains(KeyModifiers::CONTROL) || !on_page {
            return None;
        }
        let rows = self
            .screen
            .map_or(20, |s| crate::permission::body_size(s).1)
            .max(2);
        let n = if matches!(c, 'd' | 'u') {
            rows / 2
        } else {
            rows
        };
        let step = KeyEvent::from(KeyCode::Char(if matches!(c, 'd' | 'f') {
            'j'
        } else {
            'k'
        }));
        Some((0..n).flat_map(|_| self.on_key(step)).collect())
    }

    /// Rows the live view moves for a page: most of its height.
    fn live_page(&self) -> usize {
        self.screen.map_or(20, |(_, h)| h as usize / 2).max(2)
    }

    /// Scrolls the live view `n` rows. Up from the screen goes into the
    /// conversation, read from the transcript; down past its end goes back
    /// to the screen.
    fn scroll_live(&mut self, up: bool, n: usize) -> Vec<Effect> {
        // The tour's sessions have a screen and no conversation behind it.
        if !self.practice_agents.is_empty() {
            return Vec::new();
        }
        if !up {
            self.live_scroll = self.live_scroll.saturating_sub(n);
            return Vec::new();
        }
        if self.live_scroll == 0 {
            // The first step up shows the conversation's latest rows.
            self.live_scroll = 1;
            if n < usize::MAX {
                return self.read_conversation();
            }
        }
        let width = self.screen.map_or(80, |(w, _)| w as usize);
        let rows = match self.picked_conversation() {
            Some(Ok(said)) => crate::ui::empty::conversation_rows(said, width).len(),
            _ => 0,
        };
        self.live_scroll = self.live_scroll.saturating_add(n).min(rows.max(1));
        self.read_conversation()
    }

    /// Asks for the picked agent's conversation, one read at a time.
    fn read_conversation(&mut self) -> Vec<Effect> {
        if self.reading_conversation {
            return Vec::new();
        }
        let Some(session_id) = self.picked_agent().map(|a| a.session_id.clone()) else {
            return Vec::new();
        };
        self.reading_conversation = true;
        vec![Effect::ReadConversation { session_id }]
    }

    /// The picked agent's conversation, once read.
    pub fn picked_conversation(&self) -> Option<&Result<Vec<Said>, String>> {
        let id = &self.picked_agent()?.session_id;
        self.conversation
            .as_ref()
            .filter(|(s, _)| s == id)
            .map(|(_, said)| said)
    }

    /// The wheel scrolls what is under it: the live view, the help page,
    /// or the card. Boxes being typed in ignore it.
    fn on_wheel(&mut self, up: bool) -> Vec<Effect> {
        const NOTCH: usize = 3;
        let step = KeyEvent::from(KeyCode::Char(if up { 'k' } else { 'j' }));
        match &self.overlay {
            None if self.queue.current().is_none() => self.scroll_live(up, NOTCH),
            None | Some(Overlay::Help { .. }) => {
                (0..NOTCH).flat_map(|_| self.on_key(step)).collect()
            }
            Some(_) => Vec::new(),
        }
    }

    /// Scrolls the help page, or closes it with q, Esc or F1.
    fn on_help_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Help { scroll }) = &mut self.overlay else {
            return;
        };
        let last = crate::ui::help::TEXT.lines().count().saturating_sub(1) as u16;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::F(1) => self.overlay = None,
            KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char('G') | KeyCode::End => *scroll = last,
            KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
            KeyCode::PageDown => *scroll = (*scroll + 10).min(last),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            _ => {}
        }
    }

    /// Asks for the picked agent's screen while the empty screen shows, one
    /// capture at a time.
    pub(crate) fn capture_picked(&mut self) -> Vec<Effect> {
        if self.capturing || self.queue.current().is_some() {
            return Vec::new();
        }
        let Some(pane) = self.picked_agent().map(|a| a.id.pane.clone()) else {
            return Vec::new();
        };
        if let Some(screen) = crate::tour::practice_screen(&pane) {
            self.preview = Some(Preview {
                pane,
                text: Ok(screen.to_string()),
            });
            return Vec::new();
        }
        self.capturing = true;
        vec![Effect::CapturePane { pane }]
    }

    /// The picked agent's screen, once read: `None` until then.
    pub fn picked_preview(&self) -> Option<&Result<String, String>> {
        let pane = &self.picked_agent()?.id.pane;
        self.preview
            .as_ref()
            .filter(|p| &p.pane == pane)
            .map(|p| &p.text)
    }

    /// Sends `/create-pr` to an agent at its prompt and goes to its window,
    /// where the skill shows the draft before pushing. With no card to hold
    /// it, a busy agent gets nothing.
    fn ask_for_pr(&mut self, agent: AgentId) -> Vec<Effect> {
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        if !self.at_prompt(&agent) {
            self.set_status(format!(
                "{name} is busy; try p again when it is at its prompt"
            ));
            return Vec::new();
        }
        self.set_status(format!("Asked {name} to open a PR"));
        vec![
            Effect::Send {
                agent: agent.clone(),
                text: PR_COMMAND.into(),
                card: None,
            },
            Effect::GoToWindow { pane: agent.pane },
        ]
    }

    /// Opens the model choice for an agent at its prompt. `/model` is a
    /// command typed at the prompt, so a busy agent cannot take it.
    fn open_model(&mut self, agent: AgentId) {
        if self.at_prompt(&agent) {
            self.overlay = Some(Overlay::Model(agent));
            return;
        }
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        self.set_status(format!(
            "{name} is busy; try m again when it is at its prompt"
        ));
    }

    /// A model's key sends `/model` with its alias; q or Esc changes nothing.
    fn on_model_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::Model(agent)) = &self.overlay else {
            return Vec::new();
        };
        let agent = agent.clone();
        let picked = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => None,
            KeyCode::Char(c) => match MODELS.iter().find(|(k, _)| *k == c) {
                Some((_, model)) => Some(*model),
                None => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        self.overlay = None;
        let Some(model) = picked else {
            return Vec::new();
        };
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        self.set_status(format!("Switched {name} to {model}"));
        vec![Effect::SwitchModel { agent, model }]
    }

    /// A mode's key presses Shift+Tab in the session until it is on; q or
    /// Esc changes nothing.
    fn on_permission_mode_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::PermissionMode(agent)) = &self.overlay else {
            return Vec::new();
        };
        let agent = agent.clone();
        let picked = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => None,
            KeyCode::Char(c) => match PERMISSION_KEYS.iter().find(|(k, _)| *k == c) {
                Some((_, mode)) => Some(*mode),
                None => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        self.overlay = None;
        let Some(mode) = picked else {
            return Vec::new();
        };
        let name = self
            .agent(&agent)
            .map_or_else(|| agent.pane.clone(), |a| a.window_name.clone());
        self.set_status(format!("Switching {name} to {mode} mode"));
        vec![Effect::SetPermissionMode { agent, mode }]
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
        self.queue.pin();
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
                let mut effects = self.capture_picked();
                if self.live_scroll > 0 {
                    // New messages keep coming while scrolled back.
                    effects.extend(self.read_conversation());
                }
                effects
            }
            Msg::Dirs(listing) => {
                self.on_dirs(listing);
                Vec::new()
            }
            Msg::LastMessage {
                agent,
                checkout,
                text,
                at,
            } => {
                self.on_last_message(agent, checkout, text, at);
                Vec::new()
            }
            Msg::Conversation { session_id, said } => {
                self.reading_conversation = false;
                self.conversation = Some((session_id, said));
                Vec::new()
            }
            Msg::Agents(agents) => self.on_agents(agents),
            Msg::Wheel { up } => self.on_wheel(up),
            Msg::Mouse { kind, col, row, at } => self.on_mouse(kind, (col, row), at),
            Msg::PaneCaptured { pane, text } => {
                self.capturing = false;
                self.preview = Some(Preview { pane, text });
                Vec::new()
            }
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
            Msg::Ipc {
                req_id,
                req:
                    HookRequest::PreTool {
                        session_id,
                        cwd,
                        tool_name,
                        tool_input,
                        ..
                    },
                checkout,
                at,
                ..
            } if tool_name == crate::choice::TOOL => {
                self.on_choice(req_id, at, &session_id, &cwd, checkout, tool_input)
            }
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
            } => {
                if let Some(c) = &checkout {
                    self.touched
                        .entry(session_id.clone())
                        .or_default()
                        .insert(c.clone());
                }
                self.on_post_tool(
                    &session_id,
                    checkout,
                    &tool_name,
                    &tool_input,
                    edited.as_deref(),
                )
            }
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

    /// Drag to select; letting go copies what was selected. A click alone
    /// clears the selection; a double click selects a word, a triple click
    /// the line, and a drag after either goes on by words or lines.
    fn on_mouse(&mut self, kind: MouseKind, at: (u16, u16), now: Instant) -> Vec<Effect> {
        match kind {
            MouseKind::Down => {
                let clicks = match self.last_click {
                    Some((cell, then, n))
                        if cell == at && now.saturating_duration_since(then) < MULTI_CLICK =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((at, now, clicks));
                let unit = match clicks {
                    1 => Unit::Cell,
                    2 => Unit::Word,
                    _ => Unit::Line,
                };
                self.selection = Some(Selection {
                    from: at,
                    to: at,
                    unit,
                });
            }
            MouseKind::Drag => {
                if let Some(s) = &mut self.selection {
                    s.to = at;
                }
            }
            MouseKind::Up => {
                let Some(s) = &mut self.selection else {
                    return Vec::new();
                };
                s.to = at;
                if s.from == s.to && s.unit == Unit::Cell {
                    self.selection = None;
                    return Vec::new();
                }
                let s = *s;
                self.set_status("Copied the selection".to_string());
                return vec![Effect::Copy(s)];
            }
        }
        Vec::new()
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.selection = None;
        match self.practice_step() {
            Some(step) => self.on_practice_key(key, step),
            None => self.handle_key(key),
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // Not over another overlay: it may hold something being typed.
        if key.code == KeyCode::F(1) && self.overlay.is_none() {
            self.overlay = Some(Overlay::Help { scroll: 0 });
            return Vec::new();
        }
        if key.code == KeyCode::F(2) && matches!(self.overlay, None | Some(Overlay::Help { .. })) {
            self.open_tour();
            return Vec::new();
        }
        if matches!(self.overlay, Some(Overlay::Tour { .. })) {
            return self.on_tour_key(key);
        }
        if let Some(effects) = self.on_page_key(key) {
            return effects;
        }
        match &self.overlay {
            Some(Overlay::Help { .. }) => {
                self.on_help_key(key);
                return Vec::new();
            }
            Some(Overlay::Prompt(_)) => return self.on_prompt_key(key),
            Some(Overlay::NewSession(_)) => return self.on_new_session_key(key),
            Some(Overlay::CloseAgent(_)) => return self.on_close_key(key),
            Some(Overlay::Model(_)) => return self.on_model_key(key),
            Some(Overlay::PermissionMode(_)) => return self.on_permission_mode_key(key),
            Some(Overlay::Ask(_)) => return self.on_ask_key(key),
            Some(Overlay::Comment(_)) => {
                self.on_comment_key(key);
                return Vec::new();
            }
            Some(Overlay::StepReason(_)) => return self.on_reason_key(key),
            Some(Overlay::Deny(_)) => return self.on_deny_key(key),
            Some(Overlay::OwnAnswer(_)) => return self.on_own_answer_key(key),
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
            KeyCode::Char('W') => self.ask_where_are_we(),
            KeyCode::Char('!') => self.open_shell(),
            KeyCode::Char('N') => {
                if let Some((dir, name, _)) = self.here() {
                    self.open_new_session(dir, name);
                }
                Vec::new()
            }
            KeyCode::Char('t') => self.next_theme(),
            KeyCode::Char('M') => self.cycle_mode(),
            KeyCode::Char('f')
                if self.overlay.is_none()
                    && matches!(
                        self.queue.current().map(|c| &c.kind),
                        Some(CardKind::Review(_) | CardKind::ShellChange(_))
                    ) =>
            {
                self.config.hide_files = !self.config.hide_files;
                vec![Effect::SaveConfig]
            }
            _ if self.overlay.is_none() && self.on_stale() => self.on_stale_key(key),
            // Space, since s sends on the review board. Where several
            // options can be ticked, Space ticks one.
            KeyCode::Char(' ')
                if !matches!(
                    self.queue.current().map(|c| &c.kind),
                    Some(CardKind::Choice(cc)) if cc.multi() && self.overlay.is_none()
                ) =>
            {
                self.queue.skip();
                Vec::new()
            }
            _ if self.overlay.is_none() => match self.queue.current().map(|c| &c.kind) {
                Some(CardKind::Step(_)) => self.on_step_key(key),
                Some(CardKind::ShellChange(_)) => self.on_shell_key(key),
                Some(CardKind::Permission(_)) => self.on_permission_key(key),
                Some(CardKind::Question(_)) => self.on_question_key(key),
                Some(CardKind::Choice(_)) => self.on_choice_key(key),
                Some(CardKind::Quiz(_)) => self.on_quiz_key(key),
                Some(CardKind::Review(_)) => self.on_review_key(key),
                None => self.on_empty_key(key),
            },
            _ => Vec::new(),
        }
    }

    /// Card `id` was answered outside aq, or its hook timed out. On screen,
    /// the card stays, marked, until a key moves on; waiting, it goes
    /// without a word.
    pub(crate) fn answered_elsewhere(&mut self, id: u64) {
        if self.stale == Some(id) {
            return;
        }
        if self.queue.current().map(|c| c.id) == Some(id) {
            self.stale = Some(id);
            self.set_status("Answered in its window or timed out: press a key to move on");
        } else {
            self.queue.remove(id);
        }
    }

    /// Whether the card on screen was answered elsewhere.
    pub fn on_stale(&self) -> bool {
        self.stale.is_some() && self.stale == self.queue.current().map(|c| c.id)
    }

    /// A key on a card answered elsewhere: moving keys still move, and any
    /// other dismisses the card without sending anything.
    fn on_stale_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('j' | 'k' | 'G')
            | KeyCode::Down
            | KeyCode::Up
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End => match self.queue.current().map(|c| &c.kind) {
                Some(CardKind::Permission(_)) => self.on_permission_key(key),
                Some(CardKind::Choice(_)) => self.on_choice_key(key),
                Some(CardKind::Question(_)) => self.on_question_key(key),
                _ => Vec::new(),
            },
            _ => {
                if let Some(id) = self.stale.take() {
                    self.queue.remove(id);
                }
                Vec::new()
            }
        }
    }

    /// The project in front of you: the current card's checkout, or with
    /// no card the picked session's; its name, and the session if known.
    fn here(&self) -> Option<(PathBuf, String, Option<&Agent>)> {
        match self.queue.current() {
            Some(card) => Some((
                card.checkout.0.clone(),
                project_name(&card.checkout),
                self.agent(&card.agent),
            )),
            None => self.picked_agent().map(|a| {
                let dir = a.checkout.as_ref().map_or(&a.cwd, |c| &c.0).clone();
                (dir, a.window_name.clone(), Some(a))
            }),
        }
    }

    /// `!`: a shell in a popup, in the current card's checkout or the picked
    /// session's, for what the agent may not run itself (sudo, a login, a
    /// command its permissions block). The session's last suggested
    /// `! command` comes typed in.
    fn open_shell(&mut self) -> Vec<Effect> {
        let Some((dir, name, agent)) = self.here() else {
            return Vec::new();
        };
        let session_id = agent
            .map(|a| a.session_id.clone())
            .filter(|s| crate::llm::valid_session_id(s));
        vec![Effect::Shell {
            dir,
            title: format!(" shell in {name} · exit or Ctrl-D closes it "),
            session_id,
        }]
    }

    fn next_theme(&mut self) -> Vec<Effect> {
        self.theme_idx = (self.theme_idx + 1) % self.themes.len();
        self.config.theme = self.theme().name.clone();
        self.set_status(format!("Theme: {}", self.config.theme));
        vec![Effect::SaveConfig]
    }

    /// The next mode for the current card's project, or with no card the
    /// picked session's, so a project in vibe mode (which makes no cards)
    /// can be switched back.
    fn cycle_mode(&mut self) -> Vec<Effect> {
        let checkout = match self.queue.current() {
            Some(c) => Some(c.checkout.clone()),
            None => self.picked_agent().and_then(|a| a.checkout.clone()),
        };
        let Some(checkout) = checkout else {
            self.set_status("No project to set a mode for");
            return Vec::new();
        };
        let mode = match self.config.mode_for(&checkout) {
            Mode::Review => Mode::Step,
            Mode::Step => Mode::Quiz,
            Mode::Quiz => Mode::Vibe,
            Mode::Vibe => Mode::Review,
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

    fn on_agents(&mut self, mut agents: Vec<Agent>) -> Vec<Effect> {
        // A menu or dialog can hide the footer for a moment: the mode last
        // seen still holds.
        for a in agents.iter_mut().filter(|a| a.permission_mode.is_none()) {
            a.permission_mode = self
                .agents
                .iter()
                .find(|p| p.id == a.id)
                .and_then(|p| p.permission_mode);
        }
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
        // A session found already waiting, as all are when aq starts, may
        // be waiting on a question it asked before aq was looking.
        let found_waiting: Vec<Effect> = agents
            .iter()
            .filter(|a| a.waiting && !self.agents.iter().any(|p| p.id == a.id))
            .filter(|a| !started.iter().any(|s| s.id == a.id))
            .filter_map(read_last_message)
            .collect();
        self.agents = agents;
        let waiting: BTreeSet<AgentId> = self
            .agents
            .iter()
            .filter(|a| a.waiting)
            .map(|a| a.id.clone())
            .collect();
        self.drop_questions(true, |a| !waiting.contains(a));
        let mut effects: Vec<Effect> = started
            .iter()
            .flat_map(|a| self.on_agent_waiting(a))
            .collect();
        effects.extend(found_waiting);
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
    /// cards are created here, for its checkout and every other one it
    /// worked in this turn, each by its own mode.
    fn on_agent_waiting(&mut self, agent: &Agent) -> Vec<Effect> {
        let mut checkouts: Vec<Checkout> = agent.checkout.iter().cloned().collect();
        let others = self.touched.remove(&agent.session_id).unwrap_or_default();
        checkouts.extend(
            others
                .into_iter()
                .filter(|c| Some(c) != agent.checkout.as_ref()),
        );
        let mut effects: Vec<Effect> = checkouts
            .iter()
            .flat_map(|c| self.on_turn_end_in(c, agent))
            .collect();
        // Whatever the mode: a question needs its answer.
        effects.extend(read_last_message(agent));
        effects
    }

    /// What `agent`'s finished turn asks for in `checkout`, by its mode.
    fn on_turn_end_in(&mut self, checkout: &Checkout, agent: &Agent) -> Vec<Effect> {
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
            Mode::Vibe => Vec::new(),
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
        if matches!(key.code, KeyCode::Char('s' | 'a')) && !self.settled(id) {
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
            // Once the card closes, the empty screen shows this agent.
            review::Action::Send => {
                let agent = card.agent.clone();
                self.pick(agent);
                self.finish_review(false)
            }
            review::Action::Approve => {
                let agent = card.agent.clone();
                self.pick(agent);
                self.finish_review(true)
            }
            review::Action::ToggleFullBranch => self.toggle_full_branch(),
            review::Action::Edit(target) => vec![Effect::Edit {
                dir: card.checkout.0.clone(),
                path: target.path,
                line: target.line,
            }],
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

    /// `s` (send this round's comments) or `a` (approve). The card closes,
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

    /// Whether text pasted into the agent's pane reaches its prompt as the
    /// whole message. An empty input box on screen is enough, even while
    /// Claude reports busy with a background job; text left typed in the box
    /// is never pasted onto. Unread, Claude's own idle is the test.
    fn at_prompt(&self, id: &AgentId) -> bool {
        let menu_open = self
            .queue
            .iter()
            .any(|c| matches!(c.kind, CardKind::Permission(_)) && &c.agent == id);
        !menu_open
            && self.agent(id).is_some_and(|a| {
                let idle = a.status == AgentStatus::Idle;
                match a.input {
                    // A session that has not had a turn yet is not
                    // "waiting", but idle with an empty box it is there.
                    InputBox::Empty => a.waiting || idle,
                    InputBox::Holding(_) => false,
                    InputBox::Unseen => a.waiting && idle,
                }
            })
    }

    /// Whether the agent's input box holds text typed and not sent.
    pub fn prompt_has_text(&self, id: &AgentId) -> bool {
        self.agent(id)
            .is_some_and(|a| matches!(a.input, InputBox::Holding(_)))
    }

    /// Sends the held messages whose agent is at its prompt now. A message
    /// whose card or agent has gone is dropped, its card kept. Messages typed
    /// on the empty screen go the same way.
    fn release_held(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let agents: Vec<AgentId> = self.prompts.keys().cloned().collect();
        for agent in agents {
            if self.agent(&agent).is_none() {
                self.prompts.remove(&agent);
                self.set_status("Could not send the message: agent gone");
            } else if self.at_prompt(&agent) {
                let text = self.prompts.remove(&agent).expect("waiting");
                self.set_status("Sending the queued message");
                effects.push(Effect::Send {
                    agent,
                    text,
                    card: None,
                });
            }
        }
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
        // What was sent answers the session, whatever it had asked.
        let agent = self
            .queue
            .iter()
            .find(|c| c.id == card)
            .map(|c| c.agent.clone());
        if let (Some(agent), None) = (agent, &error) {
            self.drop_questions(false, |a| *a == agent);
        }
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

/// Asks for the message `agent` ended its turn with, when its session can
/// be found.
fn read_last_message(agent: &Agent) -> Option<Effect> {
    crate::llm::valid_session_id(&agent.session_id).then(|| Effect::ReadLastMessage {
        agent: agent.id.clone(),
        session_id: agent.session_id.clone(),
        checkout: agent
            .checkout
            .clone()
            .unwrap_or_else(|| Checkout(agent.cwd.clone())),
    })
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
        Mode::Vibe => "vibe: nothing checked",
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
            permission_mode: None,
            input: InputBox::Unseen,
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
        a.update(key('t'));
        a.update(Msg::Tick(t0));
        a.update(Msg::Tick(t0 + Duration::from_millis(4900)));
        assert!(a.status_text().is_some());
        a.update(Msg::Tick(t0 + Duration::from_secs(5)));
        assert_eq!(a.status_text(), None);
        // A new message starts its own five seconds.
        a.update(key('t'));
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
        assert_eq!(a.update(key('t')), [Effect::SaveConfig]);
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
        assert_eq!(a.config.mode_for(&ck), Mode::Vibe);
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
        a.update(key(' '));
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

    /// A finished turn's effects without the read of its last message,
    /// which every turn asks for.
    pub(crate) fn turn_end(a: &mut App, agents: Vec<Agent>) -> Vec<Effect> {
        let mut effects = a.update(Msg::Agents(agents));
        effects.retain(|e| !matches!(e, Effect::ReadLastMessage { .. }));
        effects
    }

    /// The effect the agent finishing in `/a` asks for.
    pub(crate) fn finish_round(a: &mut App) -> ReviewRequest {
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = turn_end(a, vec![agent("%1", 1, "api", "/a", true)]);
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
        // Keys go to the comment box, so this `s` is typed, not a send.
        typed(&mut a, "s");
        a.update(ctrl('s'));
        assert_eq!(a.overlay, None);
        assert_eq!(review(&a).comments[0].body, "store down?s");
        let id = a.queue.current().unwrap().id;
        let effects = a.update(key('s'));
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Review comments (round 1):\n\nsrc/limit.rs:40\n\
                       > let b = store.get(key).unwrap_or_default();\nstore down?s"
                    .into(),
                card: Some(id),
            }]
        );
        assert_eq!(a.queue.len(), 1, "the card waits for the send");
        assert!(a.rounds.is_empty());
        assert!(
            a.update(key('s')).is_empty(),
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
        assert!(a.update(key('s')).is_empty());
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
        let effects = a.update(key('s'));
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
        assert!(a.update(key('a')).is_empty());
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
        a.update(key('s'));
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
            a.update(key('s')).as_slice(),
            [Effect::Send { .. }]
        ));
    }

    #[test]
    fn send_done_closes_card_and_records_round() {
        let mut a = app();
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        let id = a.queue.current().unwrap().id;
        a.update(key('a'));
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
            a.update(key('a')),
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Approved, carry on.".into(),
                card: Some(id),
            }]
        );
        assert_eq!(
            a.picked.as_ref(),
            Some(&a.agents[0].id),
            "its agent shows next"
        );
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        // The agent is working again by the time the user approves the next round.
        a.update(computed(req, "t1", "t2", fixture_files()));
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(a.update(key('a')).is_empty());
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
        a.update(key('s'));
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

    /// `a`, and the send (if any) succeeds.
    fn approve(a: &mut App) {
        let id = a.queue.current().unwrap().id;
        if !a.update(key('a')).is_empty() {
            a.update(Msg::SendDone { card: id });
        }
    }

    fn toggle(a: &mut App) -> ReviewRequest {
        match a.update(key('r')).as_slice() {
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
            a.update(key('r')).is_empty(),
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
    fn a_key_on_a_card_answered_elsewhere_only_closes_it() {
        let mut a = app();
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
        // The review card queues behind the permission on screen.
        let req = finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        assert!(matches!(
            a.queue.current().unwrap().kind,
            CardKind::Permission(_)
        ));
        // Answered in the window: the permission stays, marked, and the
        // `a` meant for it neither approves the review nor sends anything.
        a.update(Msg::HookGone { req_id: 9 });
        a.update(Msg::Tick(t + Duration::from_millis(100)));
        assert!(a.on_stale());
        let at = |ms| {
            Msg::Key(
                KeyEvent::from(KeyCode::Char('a')),
                t + Duration::from_millis(ms),
            )
        };
        assert!(a.update(at(500)).is_empty());
        assert_eq!(a.queue.len(), 1);
        // The review card came up by the user's key: no "new card" wait.
        assert!(matches!(
            a.queue.current().unwrap().kind,
            CardKind::Review(_)
        ));
        assert!(!a.preempted());
        a.update(Msg::Tick(t + Duration::from_millis(600)));
        assert!(matches!(
            a.update(at(1000)).as_slice(),
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
    fn a_turn_gets_a_card_for_each_checkout_it_worked_in() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.config.set_mode(&Checkout("/c".into()), Mode::Vibe);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        for c in ["/b", "/a", "/c", "/b"] {
            a.update(Msg::PostTool {
                session_id: "s1".into(),
                checkout: Some(Checkout(c.into())),
                tool_name: "Bash".into(),
                tool_input: serde_json::json!({"command": "true"}),
                edited: None,
            });
        }
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        let reviewed: Vec<&std::path::Path> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::ComputeReview(r) => Some(r.checkout.0.as_path()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reviewed,
            [std::path::Path::new("/a"), std::path::Path::new("/b")],
            "its own first, each once, none for vibe"
        );
        // The next turn starts with nothing touched.
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = turn_end(&mut a, vec![agent("%1", 1, "api", "/a", true)]);
        assert_eq!(effects.len(), 1, "{effects:?}");
    }

    #[test]
    fn vibe_mode_makes_no_card_and_m_without_a_card_leaves_it() {
        let mut a = App::new(Config::default(), builtin_themes());
        let ck = Checkout("/a".into());
        a.config.set_mode(&ck, Mode::Vibe);
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = turn_end(&mut a, vec![agent("%1", 1, "api", "/a", true)]);
        assert!(effects.is_empty(), "nothing checked: {effects:?}");
        assert!(a.queue.is_empty());
        // No card to press M on: it goes to the picked session's project.
        assert_eq!(a.update(key('M')), [Effect::SaveConfig]);
        assert_eq!(a.config.mode_for(&ck), Mode::Review);
        let effects = a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(effects.is_empty());
        let effects = turn_end(&mut a, vec![agent("%1", 1, "api", "/a", true)]);
        assert!(matches!(effects.as_slice(), [Effect::ComputeReview(_)]));
    }

    #[test]
    fn tab_goes_back_and_forth_between_the_last_two_sessions() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "docs", "/w/docs", false),
            agent("%3", 3, "web", "/w/web", true),
        ];
        let tab = || Msg::Key(KeyEvent::from(KeyCode::Tab), Instant::now());
        let picked = |a: &App| a.picked_agent().unwrap().id.pane.clone();
        a.update(tab());
        assert_eq!(picked(&a), "%1", "nothing shown before yet");
        assert!(a.status_text().unwrap().contains("h/l"));
        a.update(key('l'));
        a.update(key('l'));
        assert_eq!(picked(&a), "%3");
        a.update(tab());
        assert_eq!(picked(&a), "%2");
        a.update(tab());
        assert_eq!(picked(&a), "%3");
        // A session that has gone is not gone back to.
        a.agents.remove(1);
        a.update(tab());
        assert_eq!(picked(&a), "%3");
    }

    #[test]
    fn empty_screen_picks_any_agent_and_goes_to_it() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![
            agent("%3", 3, "web", "/w/web", true),
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "docs", "/w/docs", false),
        ];
        let enter = || Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        let picked = |a: &App| a.picked_agent().unwrap().id.pane.clone();
        // The first waiting agent until one is picked; then window order, wrapping.
        assert_eq!(picked(&a), "%1");
        a.update(key('j'));
        assert_eq!(picked(&a), "%2", "a working agent can be picked too");
        a.update(key('j'));
        assert_eq!(picked(&a), "%3");
        a.update(key('j'));
        assert_eq!(picked(&a), "%1");
        a.update(key('k'));
        assert_eq!(picked(&a), "%3");
        assert_eq!(
            a.update(key('g')),
            vec![Effect::GoToWindow { pane: "%3".into() }]
        );
        assert_eq!(
            a.update(key('p')),
            vec![
                Effect::Send {
                    agent: a.agents[0].id.clone(),
                    text: PR_COMMAND.into(),
                    card: None,
                },
                Effect::GoToWindow { pane: "%3".into() },
            ]
        );
        // Not at its prompt: P sends nothing.
        a.update(key('k'));
        assert_eq!(picked(&a), "%2");
        assert!(a.update(key('p')).is_empty());
        assert!(a.status_text().unwrap().contains("busy"));
        // A pick that has gone falls back to the first waiting agent.
        a.agents.remove(2);
        assert_eq!(picked(&a), "%1");
        // With nobody waiting, the first agent; with nobody at all, nothing.
        a.agents.iter_mut().for_each(|x| x.waiting = false);
        assert_eq!(picked(&a), "%1");
        a.agents.clear();
        assert!(a.picked_agent().is_none());
        assert!(a.update(key('g')).is_empty());
        assert_eq!(
            a.overlay, None,
            "Enter opens no message box without an agent"
        );
        a.update(enter());
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn i_messages_the_picked_agent_now_or_once_it_is_idle() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", false),
        ];
        let enter = || Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        let send = |pane: &str, n: u32, text: &str| Effect::Send {
            agent: AgentId {
                pane: pane.into(),
                claude_pid: n * 100,
            },
            text: text.into(),
            card: None,
        };
        a.update(enter());
        typed(&mut a, "hello");
        assert_eq!(a.update(enter()), [send("%1", 1, "hello")]);
        assert_eq!(a.overlay, None);
        // Esc drops the message.
        a.update(key('i'));
        typed(&mut a, "never");
        assert!(
            a.update(Msg::Key(KeyEvent::from(KeyCode::Esc), Instant::now()))
                .is_empty()
        );
        assert_eq!(a.overlay, None);
        // A working agent gets it when it is next at its prompt, both messages as one.
        a.update(key('j'));
        a.update(key('i'));
        typed(&mut a, "next");
        assert!(a.update(enter()).is_empty());
        a.update(key('i'));
        typed(&mut a, "more");
        assert!(a.update(enter()).is_empty());
        assert!(a.prompt_waiting(&a.agents[1].id));
        let effects = a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", true),
        ]));
        assert!(
            effects.contains(&send("%2", 2, "next\n\nmore")),
            "{effects:?}"
        );
        assert!(!a.prompt_waiting(&a.agents[1].id));
    }

    #[test]
    fn f1_opens_help_that_pages_like_vim_but_never_over_typing() {
        let mut a = App::new(Config::default(), builtin_themes());
        let press =
            |code: KeyCode, mods: KeyModifiers| Msg::Key(KeyEvent::new(code, mods), Instant::now());
        let help = |a: &App| match a.overlay {
            Some(Overlay::Help { scroll }) => scroll,
            _ => panic!("help is not open: {:?}", a.overlay),
        };
        a.screen = Some((80, 30));
        let half = (crate::permission::body_size((80, 30)).1 / 2) as u16;
        a.update(press(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(help(&a), 0);
        a.update(press(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(help(&a), half, "Ctrl-d is half a page");
        a.update(key('k'));
        a.update(press(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(help(&a), 0);
        a.update(key('G'));
        assert!(help(&a) > half);
        a.update(key('g'));
        assert_eq!(help(&a), 0);
        a.update(key('q'));
        assert_eq!(a.overlay, None);
        // Over a reason being typed, F1 does nothing.
        a.overlay = Some(Overlay::Deny(DenyDraft {
            card: 1,
            input: TextInput::single_line(),
        }));
        a.update(press(KeyCode::F(1), KeyModifiers::NONE));
        assert!(matches!(a.overlay, Some(Overlay::Deny(_))));
    }

    #[test]
    fn ctrl_d_on_the_empty_screen_does_not_cycle_the_pick() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", true),
        ];
        a.update(Msg::Key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Instant::now(),
        ));
        assert_eq!(a.picked_agent().unwrap().id.pane, "%1");
    }

    #[test]
    fn a_message_goes_to_an_empty_box_even_when_busy_but_never_onto_typed_text() {
        let mut a = App::new(Config::default(), builtin_themes());
        let mut busy = agent("%1", 1, "api", "/w/api", true);
        // Turn over, a background job still running: Claude says busy.
        busy.status = AgentStatus::Busy;
        busy.input = InputBox::Holding("half typed".into());
        a.agents = vec![busy.clone()];
        a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now()));
        typed(&mut a, "next");
        let enter = Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        assert!(a.update(enter).is_empty(), "never pasted onto typed text");
        assert!(
            a.status_text()
                .unwrap()
                .contains("has text typed in its box")
        );
        assert!(a.prompt_waiting(&busy.id) && a.prompt_has_text(&busy.id));
        // Unread screen and busy: still waits.
        busy.input = InputBox::Unseen;
        assert!(a.update(Msg::Agents(vec![busy.clone()])).is_empty());
        // The box is cleared: sent, though Claude still says busy.
        busy.input = InputBox::Empty;
        assert_eq!(
            a.update(Msg::Agents(vec![busy.clone()])),
            [Effect::Send {
                agent: busy.id.clone(),
                text: "next".into(),
                card: None,
            }]
        );
    }

    #[test]
    fn a_session_that_has_not_had_a_turn_yet_takes_a_message() {
        let mut a = App::new(Config::default(), builtin_themes());
        // Just started: no turn has ended, so nothing marked it waiting.
        let mut fresh = agent("%1", 1, "new", "/w/new", false);
        fresh.status = AgentStatus::Idle;
        fresh.input = InputBox::Empty;
        a.agents = vec![fresh.clone()];
        a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now()));
        typed(&mut a, "hello");
        let enter = Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        assert_eq!(
            a.update(enter),
            [Effect::Send {
                agent: fresh.id.clone(),
                text: "hello".into(),
                card: None,
            }]
        );
        // Working, or with its screen unread: the message waits.
        fresh.status = AgentStatus::Busy;
        a.agents = vec![fresh.clone()];
        a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now()));
        typed(&mut a, "more");
        let enter = Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        assert!(a.update(enter).is_empty());
        assert!(a.prompt_waiting(&fresh.id));
    }

    #[test]
    fn double_esc_stops_the_picked_agent() {
        let mut a = App::new(Config::default(), builtin_themes());
        let mut busy = agent("%1", 1, "api", "/w/api", false);
        busy.waiting = false;
        a.agents = vec![busy];
        let t0 = Instant::now();
        let esc = |t| Msg::Key(KeyEvent::from(KeyCode::Esc), t);
        // One Esc only hints; a second too late hints again.
        assert!(a.update(esc(t0)).is_empty());
        assert_eq!(a.status_text(), Some("Esc again to stop api"));
        assert!(a.update(esc(t0 + Duration::from_secs(2))).is_empty());
        // Two close together interrupt it and drop a queued message.
        a.prompts.insert(a.agents[0].id.clone(), "later".into());
        let t1 = t0 + Duration::from_secs(5);
        a.update(esc(t1));
        assert_eq!(
            a.update(esc(t1 + Duration::from_millis(300))),
            [Effect::Interrupt { pane: "%1".into() }]
        );
        assert!(a.prompts.is_empty());
        assert_eq!(
            a.status_text(),
            Some("Stopped api and dropped its queued message")
        );
        // An idle agent gets no Escape: Claude Code would open its rewind menu.
        a.agents[0].waiting = true;
        let t2 = t1 + Duration::from_secs(5);
        a.update(esc(t2));
        assert!(a.update(esc(t2)).is_empty());
        assert_eq!(a.status_text(), Some("api is not working on anything"));
        // The Esc that closes the message box is the first of the two.
        a.agents[0].waiting = false;
        let t3 = t2 + Duration::from_secs(5);
        a.update(Msg::Key(KeyEvent::from(KeyCode::Enter), t3));
        assert!(matches!(a.overlay, Some(Overlay::Prompt(_))));
        assert!(a.update(esc(t3)).is_empty());
        assert!(a.overlay.is_none());
        assert_eq!(a.status_text(), Some("Esc again to stop api"));
        assert_eq!(
            a.update(esc(t3 + Duration::from_millis(300))),
            [Effect::Interrupt { pane: "%1".into() }]
        );
    }

    #[test]
    fn bang_opens_a_shell_where_the_card_or_session_is() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        let e = a.update(key('!'));
        assert!(
            matches!(&e[..], [Effect::Shell { dir, title, session_id: Some(s) }]
                if dir == std::path::Path::new("/w/api") && title.contains("api") && s == "s1"),
            "{e:?}"
        );
        a.queue.push(crate::queue::tests::card(
            1,
            "/w/web",
            CardKind::Review(crate::model::ReviewCard::new(
                1,
                "a".into(),
                "b".into(),
                Vec::new(),
                Vec::new(),
                false,
            )),
            0,
        ));
        let e = a.update(key('!'));
        assert!(
            matches!(&e[..], [Effect::Shell { dir, .. }] if dir == std::path::Path::new("/w/web")),
            "{e:?}"
        );
    }

    #[test]
    fn dragging_selects_and_letting_go_copies() {
        let mut a = App::new(Config::default(), builtin_themes());
        let t = Instant::now();
        let mouse = |kind, col, row| Msg::Mouse {
            kind,
            col,
            row,
            at: t,
        };
        assert!(a.update(mouse(MouseKind::Down, 2, 3)).is_empty());
        a.update(mouse(MouseKind::Drag, 9, 4));
        assert_eq!(
            a.selection,
            Some(Selection {
                from: (2, 3),
                to: (9, 4),
                unit: Unit::Cell
            })
        );
        assert_eq!(
            a.update(mouse(MouseKind::Up, 10, 4)),
            [Effect::Copy(Selection {
                from: (2, 3),
                to: (10, 4),
                unit: Unit::Cell
            })]
        );
        // It stays shown until a key; a click alone copies nothing.
        assert!(a.selection.is_some());
        a.update(key('j'));
        assert_eq!(a.selection, None);
        a.update(mouse(MouseKind::Down, 5, 5));
        assert!(a.update(mouse(MouseKind::Up, 5, 5)).is_empty());
        assert_eq!(a.selection, None);
    }

    #[test]
    fn clicks_in_a_row_select_a_word_then_the_line_and_copy_it() {
        let mut a = App::new(Config::default(), builtin_themes());
        let t = Instant::now();
        let click = |a: &mut App, col, ms| {
            let at = t + Duration::from_millis(ms);
            a.update(Msg::Mouse {
                kind: MouseKind::Down,
                col,
                row: 2,
                at,
            });
            a.update(Msg::Mouse {
                kind: MouseKind::Up,
                col,
                row: 2,
                at,
            })
        };
        let copied = |unit| {
            vec![Effect::Copy(Selection {
                from: (4, 2),
                to: (4, 2),
                unit,
            })]
        };
        assert!(click(&mut a, 4, 0).is_empty());
        assert_eq!(click(&mut a, 4, 200), copied(Unit::Word));
        assert_eq!(click(&mut a, 4, 400), copied(Unit::Line));
        // A fourth starts over as a plain click.
        assert!(click(&mut a, 4, 600).is_empty());
        assert_eq!(click(&mut a, 4, 800), copied(Unit::Word));
        // Too slow, or on another cell: a plain click again.
        assert!(click(&mut a, 4, 1400).is_empty());
        assert!(click(&mut a, 5, 1500).is_empty());
    }

    #[test]
    fn n_starts_a_session_where_you_are_with_or_without_a_prompt() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        a.update(key('N'));
        assert!(matches!(&a.overlay, Some(Overlay::NewSession(d)) if d.name == "api"));
        for c in "fix the flaky test".chars() {
            a.update(key(c));
        }
        let enter = || Msg::Key(KeyEvent::from(KeyCode::Enter), Instant::now());
        assert_eq!(
            a.update(enter()),
            [Effect::NewSession {
                dir: "/w/api".into(),
                prompt: Some("fix the flaky test".into()),
                worktree: false,
            }]
        );
        assert_eq!(a.overlay, None);
        // Empty: claude with no prompt. Tab: in a worktree of its own.
        a.update(key('N'));
        a.update(Msg::Key(KeyEvent::from(KeyCode::Tab), Instant::now()));
        assert_eq!(
            a.update(enter()),
            [Effect::NewSession {
                dir: "/w/api".into(),
                prompt: None,
                worktree: true,
            }]
        );
        assert!(a.status_text().unwrap().contains("a new worktree of api"));
        // Esc: nothing.
        a.update(key('N'));
        let esc = Msg::Key(KeyEvent::from(KeyCode::Esc), Instant::now());
        assert!(a.update(esc).is_empty());
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn shift_p_switches_the_picked_sessions_permission_mode() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", false)];
        a.update(key('P'));
        assert_eq!(
            a.overlay,
            Some(Overlay::PermissionMode(a.agents[0].id.clone()))
        );
        let screen = crate::ui::tests::render(&a, 90, 12);
        assert!(screen.contains("Permission mode for api"), "{screen}");
        assert!(screen.contains("e accept edits"), "{screen}");
        assert!(a.update(key('j')).is_empty(), "other keys do nothing");
        // Even mid-turn: Shift+Tab needs no prompt to type at.
        assert_eq!(
            a.update(key('p')),
            [Effect::SetPermissionMode {
                agent: a.agents[0].id.clone(),
                mode: "plan",
            }]
        );
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("Switching api to plan mode"));
    }

    #[test]
    fn each_tab_shows_its_sessions_permission_mode_and_keeps_it_while_hidden() {
        let mut a = App::new(Config::default(), builtin_themes());
        let with = |mode| {
            let mut x = agent("%1", 1, "api", "/w/api", true);
            x.permission_mode = mode;
            let mut y = agent("%2", 2, "web", "/w/web", false);
            y.permission_mode = Some("auto");
            vec![x, y]
        };
        a.update(Msg::Agents(with(Some("plan"))));
        let screen = crate::ui::tests::render(&a, 100, 12);
        assert!(screen.contains("1 api ⏸ plan"), "{screen}");
        assert!(screen.contains("2 web ⏵⏵ auto"), "{screen}");
        // A menu hides the footer: the mode last seen stays.
        a.update(Msg::Agents(with(None)));
        assert_eq!(a.agents[0].permission_mode, Some("plan"));
    }

    #[test]
    fn every_permission_key_names_a_mode_aq_can_read() {
        for (_, mode) in PERMISSION_KEYS {
            assert!(
                crate::agents::tmux::PERMISSION_MODES
                    .iter()
                    .any(|(m, _)| *m == mode)
            );
        }
    }

    #[test]
    fn m_switches_the_picked_agents_model() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", false),
        ];
        let esc = || Msg::Key(KeyEvent::from(KeyCode::Esc), Instant::now());
        a.update(key('m'));
        assert_eq!(a.overlay, Some(Overlay::Model(a.agents[0].id.clone())));
        let screen = crate::ui::tests::render(&a, 90, 12);
        assert!(screen.contains("Model for api"), "{screen}");
        assert!(screen.contains("o opus"), "{screen}");
        assert!(a.update(key('j')).is_empty(), "other keys do nothing");
        assert!(a.overlay.is_some());
        assert!(a.update(esc()).is_empty());
        assert_eq!(a.overlay, None);
        a.update(key('m'));
        assert_eq!(
            a.update(key('o')),
            [Effect::SwitchModel {
                agent: a.agents[0].id.clone(),
                model: "opus",
            }]
        );
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("Switched api to opus"));
        // Not at its prompt: there is nowhere to type the command.
        a.update(key('j'));
        a.update(key('m'));
        assert_eq!(a.overlay, None);
        assert!(a.status_text().unwrap().contains("busy"));
    }

    #[test]
    fn x_closes_the_picked_agent_only_after_yes() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        let esc = || Msg::Key(KeyEvent::from(KeyCode::Esc), Instant::now());
        a.update(key('x'));
        assert_eq!(a.overlay, Some(Overlay::CloseAgent(a.agents[0].id.clone())));
        assert!(
            a.update(key('j')).is_empty(),
            "other keys do nothing while asking"
        );
        assert!(a.update(esc()).is_empty());
        assert_eq!(a.overlay, None);
        a.update(key('x'));
        assert!(a.update(key('n')).is_empty());
        a.update(key('x'));
        assert_eq!(
            a.update(key('y')),
            [Effect::ClosePane { pane: "%1".into() }]
        );
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("Closed api"));
    }

    #[test]
    fn empty_screen_reads_the_picked_agents_screen() {
        let mut a = App::new(Config::default(), builtin_themes());
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/w/api", true),
            agent("%2", 2, "web", "/w/web", true),
        ]));
        let tick = || Msg::Tick(Instant::now());
        let capture = |pane: &str| vec![Effect::CapturePane { pane: pane.into() }];
        assert_eq!(a.update(tick()), capture("%1"));
        // One capture at a time: the next tick and a new pick ask for none.
        assert!(a.update(tick()).is_empty());
        assert!(a.update(key('j')).is_empty());
        // A screen for the old pick is kept but not shown for the new one.
        a.update(Msg::PaneCaptured {
            pane: "%1".into(),
            text: Ok("> ".into()),
        });
        assert_eq!(a.picked_preview(), None);
        assert_eq!(a.update(tick()), capture("%2"));
        a.update(Msg::PaneCaptured {
            pane: "%2".into(),
            text: Ok("done\n> ".into()),
        });
        assert_eq!(a.picked_preview(), Some(&Ok("done\n> ".to_string())));
    }

    #[test]
    fn send_to_a_working_agent_waits_until_it_is_idle() {
        let mut a = app();
        let id = commented(&mut a);
        // The user typed into the window: working again.
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        assert!(a.update(key('s')).is_empty(), "held, not sent");
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
        assert!(a.update(key('s')).is_empty(), "held, not sent");
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
            a.update(key('s')).as_slice(),
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
        b.update(key('s'));
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
        assert!(a.update(key('s')).is_empty());
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
