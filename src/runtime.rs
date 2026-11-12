//! The impure shell around `App::update`: terminal, agent polling, key input,
//! rendering and effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{UnboundedSender, channel, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::agents::{self, AgentConn};
use crate::app::{App, Effect, MouseKind, Msg, ReviewRequest};
use crate::config::{Config, Paths};
use crate::edit::apply_edit;
use crate::git;
use crate::highlight::Highlighter;
use crate::ipc::{self, HookReply, HookRequest};
use crate::llm::{Children, ClaudeCli, Llm, LlmJob, run_job};
use crate::model::{AgentId, Checkout, Mode};
use crate::new_session::DirListing;
use crate::persist::{self, SavedState};
use crate::quiz::{CodeRequest, snippet};
use crate::shell::{SnapshotRequest, Taken};
use crate::step::PendingEdit;
use crate::theme::load_themes;
use crate::transcript::description_before;

const AGENT_POLL: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_secs(1);
/// At most 30 frames a second.
const FRAME: Duration = Duration::from_millis(33);
/// Saved state is written at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// How long quitting waits for saves and sends still running.
const QUIT_WAIT: Duration = Duration::from_secs(3);
/// Files larger than this are not read for a step card.
const READ_MAX: u64 = 10 * 1024 * 1024;

pub async fn run(paths: Paths, conn: Arc<dyn AgentConn>) -> Result<()> {
    // Loaded before raw mode: bad theme files warn on stderr.
    let config = Config::load(&paths.config_file)?;
    let themes = load_themes(&paths.themes_dir);
    let mut app = App::new(config, themes);
    app.start_tour_if_new();
    // Before raw mode, so "aq is already running" shows on the terminal.
    let listener = ipc::listen(&paths.socket)?;
    match persist::load(&persist::state_file(&paths)) {
        Ok(state) => app.restore(state),
        Err(e) => app.set_status(format!("Could not read saved state: {e:#}")),
    }

    // Installs a panic hook that leaves raw mode and the alternate screen.
    let mut terminal = ratatui::try_init().context("starting the terminal")?;
    // For the wheel. Selecting text then takes Shift, as in most TUIs.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let (result, background) = event_loop(&mut terminal, &mut app, &paths, conn, listener).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    let _ = std::fs::remove_file(&paths.socket);
    if let Some(e) = background.finish(QUIT_WAIT) {
        eprintln!("aq: {e}");
    }
    result
}

/// Threads that outlive the event loop, to be wound down after the
/// terminal is back.
struct Background {
    worker: JoinHandle<Option<String>>,
    children: Arc<Children>,
}

impl Background {
    /// Lets queued saves and sends finish, waiting at most `wait`, then
    /// kills the `claude` calls still running. Returns what went wrong.
    fn finish(self, wait: Duration) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = self.worker;
        std::thread::spawn(move || {
            let _ = tx.send(worker.join());
        });
        let problem = match rx.recv_timeout(wait) {
            Ok(Ok(save_failed)) => {
                save_failed.map(|e| format!("could not save state on exit: {e}"))
            }
            Ok(Err(_)) => Some("the effect worker had stopped".to_string()),
            Err(_) => Some(format!(
                "saves and sends still running after {} s; quitting anyway",
                wait.as_secs()
            )),
        };
        // `claude` calls still running are of no use once aq is gone.
        self.children.kill_all();
        problem
    }
}

/// The effect worker or the loader is gone, so nothing aq decides can be
/// carried out any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkerGone;

impl std::fmt::Display for WorkerGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a background worker stopped, so aq cannot carry on")
    }
}

impl std::error::Error for WorkerGone {}

/// Where blocking work goes.
pub(crate) struct Workers {
    /// Effects, done in order.
    jobs: std::sync::mpsc::Sender<Job>,
    /// Reads for hooks, apart from `jobs` so a slow diff, snapshot or send
    /// never holds up an edit.
    loads: std::sync::mpsc::Sender<Load>,
    /// The last frame drawn while a selection is shown, to copy it from.
    screen: Arc<std::sync::Mutex<Option<ratatui::buffer::Buffer>>>,
}

impl Workers {
    fn job(&self, job: Job) -> Result<(), WorkerGone> {
        self.jobs.send(job).map_err(|_| WorkerGone)
    }

    fn load(&self, load: Load) -> Result<(), WorkerGone> {
        self.loads.send(load).map_err(|_| WorkerGone)
    }
}

/// Hooks waiting for an answer, by request id.
type Waiting = HashMap<u64, oneshot::Sender<HookReply>>;

/// Requests the loader is still reading files for, and whether each is an
/// edit (rather than a permission request).
type Loading = HashMap<u64, bool>;

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    conn: Arc<dyn AgentConn>,
    listener: UnixListener,
) -> (Result<()>, Background) {
    let (tx, mut rx) = unbounded_channel();
    let (ipc_tx, mut ipc_rx) = channel(64);
    tokio::spawn(ipc::accept_loop(listener, ipc_tx));
    let mut waiting = Waiting::new();
    // Requests the loader is still reading files for.
    let mut loading = Loading::new();
    let children = Arc::new(Children::default());
    let llm: Arc<dyn Llm> = Arc::new(ClaudeCli {
        children: children.clone(),
    });
    let (jobs, worker) = spawn_effect_worker(tx.clone(), conn.clone(), paths.clone(), llm);
    let workers = Workers {
        jobs,
        loads: spawn_loader(tx.clone()),
        screen: Default::default(),
    };
    spawn_agent_poll(tx.clone(), conn);
    spawn_key_reader(tx.clone());
    spawn_ticker(tx);

    let result = serve(
        terminal,
        app,
        &workers,
        &mut waiting,
        &mut loading,
        &mut rx,
        &mut ipc_rx,
    )
    .await;
    if result.is_err() {
        abandon_hooks(app, &mut waiting, &loading);
    }
    (result, Background { worker, children })
}

async fn serve(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    workers: &Workers,
    waiting: &mut Waiting,
    loading: &mut Loading,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    ipc_rx: &mut tokio::sync::mpsc::Receiver<ipc::IpcEvent>,
) -> Result<()> {
    let mut hl = Highlighter::new(app.theme());
    let mut hl_theme = app.theme().name.clone();
    let mut dirty = true;
    let mut next_frame = Instant::now();
    // Closing aq's window sends SIGHUP: quit as if Q was pressed, so state
    // is saved and the tmux flag aq set is cleared.
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    while !app.quit {
        tokio::select! {
            _ = hangup.recv() => break,
            _ = terminate.recv() => break,
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                sync_size(terminal, app, workers, waiting)?;
                on_msg(app, msg, workers, waiting, loading)?;
                dirty = true;
            }
            Some(ev) = ipc_rx.recv() => {
                sync_size(terminal, app, workers, waiting)?;
                sweep(app, workers, waiting, loading)?;
                on_ipc(app, ev, workers, waiting, loading)?;
                dirty = true;
            }
            _ = tokio::time::sleep_until(next_frame), if dirty => {
                if hl_theme != app.theme().name {
                    hl.set_theme(app.theme());
                    hl_theme = app.theme().name.clone();
                }
                sync_size(terminal, app, workers, waiting)?;
                let frame = terminal.draw(|f| crate::ui::draw(f, app, &mut hl))?;
                if app.selection.is_some()
                    && let Ok(mut screen) = workers.screen.lock()
                {
                    *screen = Some(frame.buffer.clone());
                }
                dirty = false;
                next_frame = Instant::now() + FRAME;
            }
        }
    }
    Ok(())
}

/// aq cannot go on: every hook gets "no decision" (a permission prompt
/// stays in the window), except step-mode edits, whose hooks are let go
/// unanswered so the edit is refused rather than let through.
fn abandon_hooks(app: &App, waiting: &mut Waiting, loading: &Loading) {
    for (req_id, reply) in waiting.drain() {
        let edit = loading.get(&req_id).copied().unwrap_or_else(|| {
            app.queue.iter().any(|c| {
                matches!(&c.kind, crate::model::CardKind::Step(s) if s.req_ids.contains(&req_id))
            })
        });
        if !edit {
            let _ = reply.send(HookReply::NoDecision);
        }
    }
}

/// A hook asks something. A tool that already ran, or an edit that cannot
/// touch a step-mode checkout, is answered at once; other requests wait for
/// a card.
fn on_ipc(
    app: &mut App,
    ev: ipc::IpcEvent,
    workers: &Workers,
    waiting: &mut Waiting,
    loading: &mut Loading,
) -> Result<(), WorkerGone> {
    let (req_id, reply) = (ev.req_id, ev.reply);
    match ev.req {
        HookRequest::PostTool {
            session_id,
            cwd,
            tool_name,
            tool_input,
        } => {
            // A tool that already ran never waits for an answer. Every one
            // is read: the checkouts a session works in get their cards
            // when its turn ends, whatever its own checkout.
            let _ = reply.send(HookReply::NoDecision);
            workers.load(Load::Post(PostToolReq {
                session_id,
                cwd,
                tool_name,
                tool_input,
            }))?;
            Ok(())
        }
        // An option menu waits for its card in any mode.
        req @ HookRequest::PreTool { .. } if matches!(&req, HookRequest::PreTool { tool_name, .. } if tool_name == crate::choice::TOOL) =>
        {
            waiting.insert(req_id, reply);
            loading.insert(req_id, false);
            workers.load(Load::Permission {
                req_id,
                req,
                at: std::time::SystemTime::now(),
            })
        }
        HookRequest::PreTool {
            session_id,
            transcript_path,
            cwd,
            tool_name,
            tool_use_id,
            tool_input,
        } => {
            if !may_be_step(&app.config, &cwd, &tool_input) {
                let _ = reply.send(HookReply::NoDecision);
                return Ok(());
            }
            waiting.insert(req_id, reply);
            loading.insert(req_id, true);
            workers.load(Load::Edit {
                req_id,
                req: PreToolReq {
                    session_id,
                    transcript_path,
                    cwd,
                    tool_name,
                    tool_use_id,
                    tool_input,
                },
                config: app.config.clone(),
            })
        }
        req @ (HookRequest::Permission { .. }
        | HookRequest::Tell { .. }
        | HookRequest::Tellable { .. }) => {
            waiting.insert(req_id, reply);
            loading.insert(req_id, false);
            workers.load(Load::Permission {
                req_id,
                req,
                at: std::time::SystemTime::now(),
            })
        }
    }
}

/// Whether a tool could touch a step-mode checkout: its cwd or its file
/// lies inside one. Path checks against the config only, no git, so the
/// answer is quick. A false answer is certain; a true one is checked
/// properly once the file is read.
fn may_be_step(config: &Config, cwd: &str, tool_input: &serde_json::Value) -> bool {
    let roots: Vec<&PathBuf> = config
        .modes
        .iter()
        .filter(|(_, m)| **m == Mode::Step)
        .map(|(p, _)| p)
        .collect();
    if roots.is_empty() {
        return false;
    }
    let cwd = Path::new(cwd);
    let mut places = vec![cwd.to_path_buf()];
    places.extend(file_dir(cwd));
    if let Some(f) = edited_file(tool_input) {
        let file = cwd.join(f);
        places.extend(file_dir(&file));
        places.push(file);
    }
    places
        .iter()
        .any(|p| roots.iter().any(|r| p.starts_with(r)))
}

/// The file an edit tool's input names.
fn edited_file(tool_input: &serde_json::Value) -> Option<&str> {
    ["file_path", "notebook_path"]
        .iter()
        .find_map(|k| tool_input.get(k).and_then(serde_json::Value::as_str))
}

/// Tells the app the terminal size when it changed, so it can judge how
/// much of a permission card is on screen.
fn sync_size(
    terminal: &DefaultTerminal,
    app: &mut App,
    workers: &Workers,
    waiting: &mut Waiting,
) -> Result<(), WorkerGone> {
    if let Ok(size) = terminal.size()
        && app.screen != Some((size.width, size.height))
    {
        let (width, height) = (size.width, size.height);
        apply(app, Msg::Resize { width, height }, workers, waiting)?;
    }
    Ok(())
}

/// Runs a message from the runtime's own channel through the app.
fn on_msg(
    app: &mut App,
    msg: Msg,
    workers: &Workers,
    waiting: &mut Waiting,
    loading: &mut Loading,
) -> Result<(), WorkerGone> {
    let loaded = match &msg {
        Msg::PreTool { req_id, .. } | Msg::Ipc { req_id, .. } => Some(*req_id),
        _ => None,
    };
    // Before: a decision sees which hooks still wait.
    sweep(app, workers, waiting, loading)?;
    apply(app, msg, workers, waiting)?;
    // After: a hook that left while its file was read is reported now that
    // its card exists, so the card keeps its decision for the retry.
    if let Some(req_id) = loaded {
        loading.remove(&req_id);
    }
    sweep(app, workers, waiting, loading)
}

/// Tells the app about hooks that went away without an answer.
fn sweep(
    app: &mut App,
    workers: &Workers,
    waiting: &mut Waiting,
    loading: &Loading,
) -> Result<(), WorkerGone> {
    let gone: Vec<u64> = waiting
        .iter()
        .filter(|(id, reply)| reply.is_closed() && !loading.contains_key(id))
        .map(|(id, _)| *id)
        .collect();
    for req_id in gone {
        waiting.remove(&req_id);
        apply(app, Msg::HookGone { req_id }, workers, waiting)?;
    }
    Ok(())
}

/// Runs `msg` through `update` and carries out its effects.
fn apply(
    app: &mut App,
    msg: Msg,
    workers: &Workers,
    waiting: &mut Waiting,
) -> Result<(), WorkerGone> {
    for effect in app.update(msg) {
        let job = match effect {
            Effect::Send { agent, text, card } => Job::Send { agent, text, card },
            Effect::SaveConfig => Job::SaveConfig(app.config.clone()),
            Effect::ComputeReview(req) => Job::Review(req),
            Effect::Grep { checkout, ident } => Job::Grep { checkout, ident },
            Effect::Snapshot(req) => Job::Snapshot(req),
            Effect::GoToWindow { pane } => Job::GoTo(pane),
            Effect::CapturePane { pane } => Job::Capture(pane),
            Effect::ReadConversation { session_id } => Job::Conversation(session_id),
            Effect::ReadLastMessage {
                agent,
                session_id,
                checkout,
            } => Job::LastMessage(agent, session_id, checkout),
            Effect::ClosePane { pane } => Job::Close(pane),
            Effect::TrustFolder { pane } => Job::Trust(pane),
            Effect::Interrupt { pane } => Job::Interrupt(pane),
            Effect::SwitchModel { agent, model } => Job::SwitchModel(agent, model),
            Effect::SetPermissionMode { agent, mode } => Job::SetPermissionMode(agent, mode),
            Effect::NewSession {
                dir,
                prompt,
                worktree,
            } => Job::NewSession(dir, prompt, worktree),
            Effect::ListDirs { typed, base } => Job::ListDirs(typed, base),
            Effect::FindRepos { root } => Job::FindRepos(root),
            Effect::OpenWorkspace {
                session,
                dirs,
                system,
            } => Job::OpenWorkspace(session, dirs, system),
            Effect::Copy(sel) => {
                let text = workers
                    .screen
                    .lock()
                    .ok()
                    .and_then(|s| s.as_ref().map(|b| crate::ui::selected_text(b, sel)))
                    .unwrap_or_default();
                Job::Copy(text)
            }
            Effect::Shell {
                dir,
                title,
                session_id,
            } => Job::Shell(dir, title, session_id),
            Effect::Edit { dir, path, line } => Job::Edit(dir, path, line),
            Effect::ComputeQuiz {
                checkout,
                agent,
                from_tree,
            } => Job::Quiz {
                checkout,
                agent,
                from_tree,
            },
            Effect::ReadCode(req) => Job::ReadCode(req),
            Effect::Llm(job) => Job::Llm(job),
            Effect::Reply { req_id, decision } => {
                // The hook may have gone; then nobody is listening.
                if let Some(reply) = waiting.remove(&req_id) {
                    let _ = reply.send(decision);
                }
                continue;
            }
        };
        workers.job(job)?;
    }
    if let Some(state) = app.state_to_save() {
        workers.job(Job::SaveState(state))?;
    }
    Ok(())
}

/// How long the loader trusts a folder's checkout before asking git again.
const CHECKOUT_TTL: Duration = Duration::from_secs(30);

/// Folders' checkouts as git last gave them, so a hook does not cost a git
/// call each time.
#[derive(Default)]
struct Checkouts(HashMap<PathBuf, (Option<Checkout>, std::time::Instant)>);

impl Checkouts {
    fn of(&mut self, dir: &Path) -> Option<Checkout> {
        self.of_at(dir, std::time::Instant::now())
    }

    fn of_at(&mut self, dir: &Path, now: std::time::Instant) -> Option<Checkout> {
        if let Some((c, at)) = self.0.get(dir)
            && now.saturating_duration_since(*at) < CHECKOUT_TTL
        {
            return c.clone();
        }
        let c = agents::checkout_of(dir);
        self.0.insert(dir.to_path_buf(), (c.clone(), now));
        c
    }
}

/// Reads for hooks, done in order on the loader thread.
enum Load {
    /// Read what a step card needs. `config` is a snapshot, used only to
    /// skip reading the transcript outside step mode.
    Edit {
        req_id: u64,
        req: PreToolReq,
        config: Config,
    },
    /// Find the checkout a finished tool worked in.
    Post(PostToolReq),
    /// Find the checkout a permission request comes from, for when its
    /// agent is not known.
    Permission {
        req_id: u64,
        req: HookRequest,
        at: std::time::SystemTime,
    },
}

/// Reads files for hooks on its own thread, so an edit never waits behind
/// a diff, a snapshot or a send. Stops when the event loop goes.
fn spawn_loader(tx: UnboundedSender<Msg>) -> std::sync::mpsc::Sender<Load> {
    let (loads, rx) = std::sync::mpsc::channel::<Load>();
    std::thread::spawn(move || {
        let mut checkouts = Checkouts::default();
        while let Ok(load) = rx.recv() {
            let msg = match load {
                Load::Edit {
                    req_id,
                    req,
                    config,
                } => Msg::PreTool {
                    req_id,
                    edit: load_edit(req, &config),
                    at: std::time::Instant::now(),
                },
                Load::Post(req) => load_post(req),
                Load::Permission { req_id, req, at } => {
                    let checkout = match &req {
                        HookRequest::Permission { cwd, .. } | HookRequest::PreTool { cwd, .. } => {
                            checkouts.of(Path::new(cwd))
                        }
                        _ => None,
                    };
                    Msg::Ipc {
                        req_id,
                        req,
                        checkout,
                        at,
                        now: std::time::Instant::now(),
                    }
                }
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
    loads
}

/// Blocking effect work, done in order on the effect worker.
enum Job {
    Send {
        agent: AgentId,
        text: String,
        card: Option<u64>,
    },
    SaveConfig(Config),
    /// Written at most once a second; the latest state wins.
    SaveState(SavedState),
    Review(ReviewRequest),
    Grep {
        checkout: Checkout,
        ident: String,
    },
    Snapshot(SnapshotRequest),
    GoTo(String),
    Capture(String),
    /// A session's conversation, by session id.
    Conversation(String),
    Close(String),
    Trust(String),
    Interrupt(String),
    SwitchModel(AgentId, &'static str),
    SetPermissionMode(AgentId, &'static str),
    Shell(PathBuf, String, Option<String>),
    Edit(PathBuf, PathBuf, u32),
    Copy(String),
    NewSession(PathBuf, Option<String>, bool),
    ListDirs(String, PathBuf),
    FindRepos(PathBuf),
    /// A tmux session's name, the folders to start Claude in there, and
    /// what to add to each one's system prompt.
    OpenWorkspace(String, Vec<PathBuf>, String),
    LastMessage(AgentId, String, Checkout),
    Quiz {
        checkout: Checkout,
        agent: AgentId,
        from_tree: Option<String>,
    },
    ReadCode(CodeRequest),
    /// Started on its own thread: it can take minutes.
    Llm(LlmJob),
}

/// The fields of `HookRequest::PostTool`.
struct PostToolReq {
    session_id: String,
    cwd: String,
    tool_name: String,
    tool_input: serde_json::Value,
}

/// The fields of `HookRequest::PreTool`.
struct PreToolReq {
    session_id: String,
    transcript_path: String,
    cwd: String,
    tool_name: String,
    tool_use_id: String,
    tool_input: serde_json::Value,
}

/// Runs effects off the event loop, one at a time so they land in order,
/// except LLM calls, which each get a thread so nothing waits behind them.
/// Saved state is written at most once every `SAVE_EVERY`, and once more on
/// the way out if a write is due. Failures come back as `Msg::EffectFailed`.
fn spawn_effect_worker(
    tx: UnboundedSender<Msg>,
    conn: Arc<dyn AgentConn>,
    paths: Paths,
    llm: Arc<dyn Llm>,
) -> (std::sync::mpsc::Sender<Job>, JoinHandle<Option<String>>) {
    let (jobs, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let state_file = persist::state_file(&paths);
        let save = |state: &SavedState| {
            persist::save(&state_file, state)
                .err()
                .map(|e| format!("{e:#}"))
        };
        // The latest state not written yet, and when it is due.
        let mut due: Option<(SavedState, std::time::Instant)> = None;
        loop {
            let next = match &due {
                Some((_, at)) => {
                    rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now()))
                }
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let job = match next {
                Ok(job) => job,
                Err(e) => {
                    let failed = due.take().and_then(|(state, _)| save(&state));
                    match e {
                        RecvTimeoutError::Timeout => {
                            if let Some(e) = failed {
                                let _ = tx.send(Msg::StateSaveFailed(e));
                            }
                            continue;
                        }
                        // aq is closing: the caller reports this once the
                        // terminal is back.
                        RecvTimeoutError::Disconnected => return failed,
                    }
                }
            };
            let msg = match job {
                Job::SaveState(state) => {
                    let at = due.map_or_else(|| std::time::Instant::now() + SAVE_EVERY, |d| d.1);
                    due = Some((state, at));
                    None
                }
                Job::Send { agent, text, card } => {
                    let sent = if conn.is_alive(&agent) {
                        conn.send_text(&agent, &text)
                            .map_err(|e| format!("Could not send to {}: {e:#}", agent.pane))
                    } else {
                        Err("agent gone".to_string())
                    };
                    match (card, sent) {
                        (Some(card), Ok(())) => Some(Msg::SendDone { card }),
                        (Some(card), Err(error)) => Some(Msg::SendFailed { card, error }),
                        (None, Ok(())) => None,
                        (None, Err(e)) => Some(Msg::EffectFailed(e)),
                    }
                }
                Job::SaveConfig(config) => config
                    .save(&paths.config_file)
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not save settings: {e:#}"))),
                Job::Review(req) => Some(compute_review(req)),
                Job::Quiz {
                    checkout,
                    agent,
                    from_tree,
                } => Some(compute_quiz(checkout, agent, from_tree)),
                Job::ReadCode(req) => Some(read_code(req)),
                Job::NewSession(dir, prompt, worktree) => {
                    let dir = expand_home(&dir);
                    match std::fs::create_dir_all(&dir)
                        .with_context(|| format!("making {}", dir.display()))
                        .and_then(|()| {
                            crate::agents::tmux::new_claude(&dir, prompt.as_deref(), worktree)
                        }) {
                        Ok(pane) => {
                            watch_trust(pane, dir, tx.clone());
                            None
                        }
                        Err(e) => Some(Msg::EffectFailed(format!("Could not start Claude: {e:#}"))),
                    }
                }
                Job::FindRepos(asked) => {
                    let root = expand_home(&asked);
                    let repos = find_repos(&root);
                    Some(Msg::Repos { asked, root, repos })
                }
                Job::OpenWorkspace(session, dirs, system) => {
                    let mut failed = Vec::new();
                    for dir in dirs {
                        match crate::agents::tmux::new_claude_in(&session, &dir, &system) {
                            Ok(pane) => watch_trust(pane, dir, tx.clone()),
                            Err(e) => failed.push(format!("{}: {e:#}", dir.display())),
                        }
                    }
                    (!failed.is_empty()).then(|| {
                        Msg::EffectFailed(format!(
                            "Could not start Claude in {}",
                            failed.join("; ")
                        ))
                    })
                }
                Job::ListDirs(typed, base) => Some(Msg::Dirs(list_dirs(typed, &base))),
                // Only when the transcript ends on words of Claude's: after a
                // tool call or a message of yours, the turn is not over.
                Job::LastMessage(agent, session_id, checkout) => {
                    crate::transcript::find(&session_id)
                        .and_then(|p| crate::transcript::conversation(&p).ok())
                        .and_then(|said| match said.into_iter().next_back() {
                            Some(crate::transcript::Said::Claude(text)) => Some(text),
                            _ => None,
                        })
                        .map(|text| Msg::LastMessage {
                            agent,
                            checkout,
                            text,
                            at: std::time::SystemTime::now(),
                        })
                }
                Job::Copy(text) => crate::agents::tmux::copy(&text)
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not copy: {e:#}"))),
                // The popup lasts as long as the shell, so it is waited on
                // apart from the other jobs.
                Job::Shell(dir, title, session_id) => {
                    let tx = tx.clone();
                    let zsh_dir = paths.socket.with_file_name("aq-shell");
                    std::thread::spawn(move || {
                        let typed = session_id
                            .and_then(|s| crate::transcript::find(&s))
                            .and_then(|p| crate::transcript::conversation(&p).ok())
                            .and_then(|said| crate::transcript::last_bang_command(&said));
                        let r = crate::agents::tmux::popup_shell(
                            &dir,
                            &title,
                            typed.as_deref(),
                            &zsh_dir,
                        );
                        if let Err(e) = r {
                            let _ = tx
                                .send(Msg::EffectFailed(format!("Could not open a shell: {e:#}")));
                        }
                    });
                    None
                }
                Job::Edit(dir, path, line) => {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = crate::agents::tmux::popup_editor(&dir, &path, line) {
                            let _ = tx.send(Msg::EffectFailed(format!(
                                "Could not open an editor: {e:#}"
                            )));
                        }
                    });
                    None
                }
                Job::Llm(job) => {
                    let (llm, tx) = (llm.clone(), tx.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Llm(run_job(&*llm, job)));
                    });
                    None
                }
                Job::Snapshot(req) => Some(take_snapshot(req)),
                Job::GoTo(pane) => crate::permission::go_to_window(&pane, &[])
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not go to {pane}: {e:#}"))),
                Job::Trust(pane) => crate::agents::tmux::trust_folder(&pane)
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not answer {pane}: {e:#}"))),
                Job::Close(pane) => crate::agents::tmux::kill_pane(&pane, &[])
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not close {pane}: {e:#}"))),
                Job::SwitchModel(agent, model) => {
                    use crate::agents::tmux::{capture_pane, confirm_model_switch};
                    capture_pane(&agent.pane, &[])
                        .and_then(|before| {
                            conn.send_text(&agent, &format!("/model {model}"))?;
                            confirm_model_switch(&agent.pane, &before, &[])
                        })
                        .err()
                        .map(|e| {
                            Msg::EffectFailed(format!(
                                "Could not switch {} to {model}: {e:#}",
                                agent.pane
                            ))
                        })
                }
                Job::SetPermissionMode(agent, mode) => {
                    crate::agents::tmux::set_permission_mode(&agent.pane, mode, &[])
                        .err()
                        .map(|e| {
                            Msg::EffectFailed(format!(
                                "Could not switch {} to {mode} mode: {e:#}",
                                agent.pane
                            ))
                        })
                }
                Job::Interrupt(pane) => crate::agents::tmux::press_escape(&pane, &[])
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not stop {pane}: {e:#}"))),
                Job::Conversation(session_id) => Some(Msg::Conversation {
                    said: crate::transcript::find(&session_id)
                        .ok_or_else(|| "No transcript found for this session".to_string())
                        .and_then(|p| {
                            crate::transcript::conversation(&p)
                                .map_err(|e| format!("Could not read {}: {e}", p.display()))
                        }),
                    session_id,
                }),
                Job::Capture(pane) => Some(Msg::PaneCaptured {
                    text: crate::agents::tmux::capture_pane(&pane, &[])
                        .map_err(|e| format!("Could not read {pane}: {e:#}")),
                    pane,
                }),
                Job::Grep { checkout, ident } => Some(Msg::GrepDone {
                    result: git::grep_word(&checkout, &ident)
                        .map_err(|e| format!("Could not search for {ident}: {e:#}")),
                    ident,
                }),
            };
            if let Some(msg) = msg
                && tx.send(msg).is_err()
            {
                return due.and_then(|(state, _)| save(&state));
            }
        }
    });
    (jobs, worker)
}

/// Diff from `from` (or the branch base) to a fresh snapshot: the from
/// tree, the snapshot, and the files.
fn diff_since(c: &Checkout, from: Option<&str>) -> Result<(String, String, Vec<git::FileDiff>)> {
    let from = match from {
        Some(t) => t.to_string(),
        None => git::base_tree(c)?,
    };
    let to = git::snapshot_tree(c)?;
    let files = git::diff_trees(c, &from, &to)?;
    Ok((from, to, files))
}

/// Diff from the request's tree (or the branch base) to a fresh snapshot.
fn compute_review(req: ReviewRequest) -> Msg {
    let c = &req.checkout;
    match diff_since(c, req.from_tree.as_deref()) {
        Ok((from_tree, to_tree, files)) => Msg::ReviewComputed {
            req,
            from_tree,
            to_tree,
            files,
            at: std::time::SystemTime::now(),
        },
        Err(e) => Msg::EffectFailed(format!(
            "Could not diff {}: {e:#}",
            crate::app::project_name(c)
        )),
    }
}

/// The diff a quiz is about.
fn compute_quiz(checkout: Checkout, agent: AgentId, from_tree: Option<String>) -> Msg {
    match diff_since(&checkout, from_tree.as_deref()) {
        Ok((from_tree, to_tree, files)) => Msg::QuizDiff {
            checkout,
            agent,
            from_tree,
            to_tree,
            files,
            at: std::time::SystemTime::now(),
        },
        Err(e) => Msg::EffectFailed(format!(
            "Could not diff {}: {e:#}",
            crate::app::project_name(&checkout)
        )),
    }
}

/// The lines around a quiz question's `code_ref`, from the quiz's tree.
fn read_code(req: CodeRequest) -> Msg {
    // The model may give a path from the checkout root, or an absolute one in it.
    let path = req
        .path
        .strip_prefix(&req.checkout.0)
        .unwrap_or(&req.path)
        .to_path_buf();
    let result = match git::file_at(&req.checkout, &req.tree, &path) {
        Ok(Some(text)) => Ok(snippet(&path, &text, req.line, req.end)),
        Ok(None) => Err(format!("{} is not in the change", path.display())),
        Err(e) => Err(format!("Could not read {}: {e:#}", path.display())),
    };
    Msg::CodeRead {
        card: req.card,
        question: req.question,
        result,
    }
}

/// Finds the checkouts of the request's cwd and of the file it edits, and
/// reads the file and, in step mode, Claude's description of the edit.
fn load_edit(req: PreToolReq, config: &Config) -> PendingEdit {
    let cwd = Path::new(&req.cwd);
    let checkout = agents::checkout_of(cwd);
    let path = cwd.join(edited_file(&req.tool_input).unwrap_or(""));
    let file_checkout = file_dir(&path).and_then(|d| agents::checkout_of(&d));
    let current = read_file(&path);
    let step = |c: &Option<Checkout>| c.as_ref().is_some_and(|c| config.mode_for(c) == Mode::Step);
    let step = step(&checkout) || step(&file_checkout);
    let description = step
        .then(|| description_before(Path::new(&req.transcript_path), &req.tool_use_id))
        .flatten();
    let preview = match &current {
        Ok(text) => apply_edit(&req.tool_name, &req.tool_input, text.as_deref()),
        Err(why) => unreadable(apply_edit(&req.tool_name, &req.tool_input, None), why),
    };
    PendingEdit {
        preview,
        session_id: req.session_id,
        checkout,
        file_checkout,
        tool_name: req.tool_name,
        tool_input: req.tool_input,
        path,
        description,
        at: std::time::SystemTime::now(),
    }
}

/// The text of the file an edit is about: `None` when it does not exist
/// yet, an error saying why when it is not a regular UTF-8 file of at most
/// `READ_MAX` bytes. Opened without blocking, so a FIFO cannot hold the
/// loader, and checked on the open file, so it cannot be swapped between.
fn read_file(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read: {e}")),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("could not read: {e}"))?;
    if !meta.is_file() {
        return Err("could not read: not a regular file".into());
    }
    if meta.len() > READ_MAX {
        return Err("could not read: too large".into());
    }
    let mut bytes = Vec::new();
    file.take(READ_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read: {e}"))?;
    if bytes.len() as u64 > READ_MAX {
        return Err("could not read: too large".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "could not read: not UTF-8".into())
}

/// `preview`, worked out as if the file were new, shown unplaced with `why`.
fn unreadable(preview: crate::edit::EditPreview, why: &str) -> crate::edit::EditPreview {
    use crate::edit::EditPreview;
    let (old, new) = match preview {
        EditPreview::Placed { after, .. } => (String::new(), after),
        EditPreview::Unplaced { old, new, .. } => (old, new),
    };
    EditPreview::Unplaced {
        old,
        new,
        why: why.to_string(),
    }
}

/// Finds the checkout a finished tool worked in: its file's for an edit,
/// for Bash the directory a leading `cd` goes to, else the cwd's.
fn load_post(req: PostToolReq) -> Msg {
    let cwd = Path::new(&req.cwd);
    let moved_to = req
        .tool_input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .and_then(|c| leading_cd(cwd, c));
    let file = edited_file(&req.tool_input).map(|f| cwd.join(f));
    // The file with every link resolved; a file not there yet is its
    // resolved parent plus its name.
    let edited = file.as_ref().and_then(|f| {
        f.canonicalize()
            .ok()
            .or_else(|| Some(f.parent()?.canonicalize().ok()?.join(f.file_name()?)))
    });
    let checkout = file
        .and_then(|f| file_dir(&f))
        .and_then(|d| agents::checkout_of(&d))
        .or_else(|| moved_to.and_then(|d| agents::checkout_of(&d)))
        .or_else(|| agents::checkout_of(cwd));
    Msg::PostTool {
        session_id: req.session_id,
        checkout,
        tool_name: req.tool_name,
        tool_input: req.tool_input,
        edited,
    }
}

/// `path` with a leading `~` as the home directory.
fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// The folders beside the last part of `typed` whose names start with it,
/// and whether `typed` is itself a folder. Hidden ones only when asked for
/// with a dot.
/// Watches a new session's pane on a thread of its own, since it can take
/// seconds, and says if Claude Code asks whether to trust `dir`.
fn watch_trust(pane: String, dir: PathBuf, tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        let msg = match crate::agents::tmux::waits_for_trust(&pane) {
            Ok(true) => Msg::TrustAsked { pane, dir },
            Ok(false) => return,
            Err(e) => Msg::EffectFailed(format!("Could not watch {pane} start: {e:#}")),
        };
        let _ = tx.send(msg);
    });
}

/// The git repos in `root`, relative to it: `root` itself if it is one
/// (as `""`), and the folders up to two levels down holding a `.git`,
/// without looking inside a repo. Hidden folders and the usual build and
/// dependency folders are passed over.
pub(crate) fn find_repos(root: &Path) -> Vec<PathBuf> {
    const SKIP: [&str; 3] = ["node_modules", "target", "vendor"];
    fn is_repo(dir: &Path) -> bool {
        dir.join(".git").exists()
    }
    fn walk(root: &Path, rel: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join(rel))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && !SKIP.contains(&n.as_str()))
            .map(|n| rel.join(n))
            .collect();
        dirs.sort();
        for dir in dirs {
            if is_repo(&root.join(&dir)) {
                out.push(dir);
            } else if depth > 1 {
                walk(root, &dir, depth - 1, out);
            }
        }
    }
    let mut repos = Vec::new();
    if is_repo(root) {
        repos.push(PathBuf::new());
    }
    walk(root, Path::new(""), 2, &mut repos);
    repos
}

fn list_dirs(typed: String, base: &Path) -> DirListing {
    use crate::new_session::resolve;
    const MOST: usize = 200;
    let cut = typed.rfind('/').map_or(0, |i| i + 1);
    let (parent, start) = typed.split_at(cut);
    let mut names: Vec<String> = std::fs::read_dir(expand_home(&resolve(parent, base)))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(start) && (start.starts_with('.') || !n.starts_with('.')))
        .collect();
    names.sort();
    names.truncate(MOST);
    let exists = !typed.trim().is_empty() && expand_home(&resolve(typed.trim(), base)).is_dir();
    DirListing {
        typed,
        names,
        exists,
    }
}

/// The directory `command` starts by changing to (`cd dir && ...`), from
/// `cwd`; `~` is the home directory.
fn leading_cd(cwd: &Path, command: &str) -> Option<PathBuf> {
    let rest = command.trim_start().strip_prefix("cd ")?.trim_start();
    let dir = match rest.chars().next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next()?,
        _ => rest.split([' ', ';', '&', '|', '\n']).next()?,
    };
    let dir = match dir.strip_prefix('~') {
        Some(tail) => PathBuf::from(std::env::var_os("HOME")?).join(tail.trim_start_matches('/')),
        None => PathBuf::from(dir),
    };
    (!dir.as_os_str().is_empty()).then(|| cwd.join(dir))
}

/// A fresh snapshot of the checkout, and its diff from `req.from`.
fn take_snapshot(req: SnapshotRequest) -> Msg {
    let c = &req.checkout;
    let result = (|| {
        let tree = git::snapshot_tree(c)?;
        let guard = git::git_guard(c)?;
        let mut files = Vec::new();
        let mut git_changes = Vec::new();
        if req.command.is_some() {
            if let Some(from) = &req.from {
                files = git::diff_trees(c, from, &tree)?;
                // The edited file is explained only if it holds what was approved.
                if let Some((path, approved)) = &req.approved {
                    let text = git::file_at(c, &tree, path)?;
                    if text.as_deref() == Some(approved.as_str()) {
                        files = crate::shell::without_path(files, path);
                    }
                }
            }
            if let Some(before) = &req.from_guard {
                git_changes = crate::shell::guard_changes(before, &guard);
            }
        }
        anyhow::Ok(Taken {
            tree,
            files,
            guard,
            git_changes,
        })
    })()
    .map_err(|e| format!("Could not snapshot {}: {e:#}", crate::app::project_name(c)));
    Msg::Snapshot {
        req,
        result,
        at: std::time::SystemTime::now(),
    }
}

/// The real directory holding `path`: links resolved, and for a file not
/// there yet, its nearest existing ancestor.
fn file_dir(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = path.canonicalize() {
        return if real.is_dir() {
            Some(real)
        } else {
            real.parent().map(Path::to_path_buf)
        };
    }
    path.ancestors()
        .skip(1)
        .find_map(|a| a.canonicalize().ok().filter(|r| r.is_dir()))
}

fn spawn_agent_poll(tx: UnboundedSender<Msg>, conn: Arc<dyn AgentConn>) {
    tokio::spawn(async move {
        loop {
            let conn = conn.clone();
            // A failed poll keeps the last known list; the next poll retries.
            let msg = match tokio::task::spawn_blocking(move || conn.list()).await {
                Ok(Ok(agents)) => Msg::Agents(agents),
                Ok(Err(e)) => Msg::AgentsFailed(format!("{e:#}")),
                Err(e) => Msg::AgentsFailed(e.to_string()),
            };
            if tx.send(msg).is_err() {
                return;
            }
            tokio::time::sleep(AGENT_POLL).await;
        }
    });
}

/// crossterm's `read` blocks, so it gets its own thread.
fn spawn_key_reader(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let msg = match ev {
                Event::Key(key) => Msg::Key(key, std::time::Instant::now()),
                Event::Mouse(m) => match m.kind {
                    event::MouseEventKind::ScrollUp => Msg::Wheel { up: true },
                    event::MouseEventKind::ScrollDown => Msg::Wheel { up: false },
                    event::MouseEventKind::Down(event::MouseButton::Left) => Msg::Mouse {
                        kind: MouseKind::Down,
                        col: m.column,
                        row: m.row,
                        at: std::time::Instant::now(),
                    },
                    event::MouseEventKind::Drag(event::MouseButton::Left) => Msg::Mouse {
                        kind: MouseKind::Drag,
                        col: m.column,
                        row: m.row,
                        at: std::time::Instant::now(),
                    },
                    event::MouseEventKind::Up(event::MouseButton::Left) => Msg::Mouse {
                        kind: MouseKind::Up,
                        col: m.column,
                        row: m.row,
                        at: std::time::Instant::now(),
                    },
                    _ => continue,
                },
                // A tick redraws, which picks up the new size.
                Event::Resize(..) => Msg::Tick(std::time::Instant::now()),
                _ => continue,
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
}

fn spawn_ticker(tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(TICK);
        loop {
            every.tick().await;
            if tx.send(Msg::Tick(std::time::Instant::now())).is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_are_found_two_levels_down_without_looking_inside_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for repo in [
            "api",
            "tools/cli",
            "api/inner",
            ".hidden/x",
            "node_modules/y",
        ] {
            std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
        }
        // A worktree or submodule has a .git file.
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("web/.git"), "gitdir: elsewhere").unwrap();
        std::fs::create_dir_all(root.join("a/b/c/.git")).unwrap();
        assert_eq!(
            super::find_repos(root),
            [PathBuf::from("api"), "tools/cli".into(), "web".into()]
        );
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(
            super::find_repos(root)[0],
            PathBuf::new(),
            "the folder itself"
        );
    }

    #[test]
    fn folders_are_listed_by_the_start_of_their_name() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("api");
        for d in ["api", "web", "worker", ".cache", "web/src"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        std::fs::write(tmp.path().join("wfile"), "").unwrap();
        let root = format!("{}/", tmp.path().display());
        let l = list_dirs(root.clone(), &base);
        assert_eq!(
            l.names,
            ["api", "web", "worker"],
            "no files, nothing hidden"
        );
        assert!(l.exists);
        let l = list_dirs(format!("{root}w"), &base);
        assert_eq!(
            (l.names.as_slice(), l.exists),
            (&["web".to_string(), "worker".into()][..], false)
        );
        assert_eq!(list_dirs(format!("{root}."), &base).names, [".cache"]);
        // A bare name is looked for inside the project; `..` leaves it.
        assert_eq!(list_dirs("../web/".into(), &base).names, ["src"]);
        let l = list_dirs("new".into(), &base);
        assert!(l.names.is_empty() && !l.exists);
        assert_eq!(expand_home(Path::new("/x/~")), Path::new("/x/~"));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_home(Path::new("~/a")), home.join("a"));
        assert_eq!(expand_home(Path::new("~")), home.join(""));
    }

    /// Workers whose threads never take anything: a wedged effect worker
    /// and loader. The receivers are returned so the channels stay open.
    fn wedged() -> (
        Workers,
        std::sync::mpsc::Receiver<Job>,
        std::sync::mpsc::Receiver<Load>,
    ) {
        let (jobs, job_rx) = std::sync::mpsc::channel();
        let (loads, load_rx) = std::sync::mpsc::channel();
        (
            Workers {
                jobs,
                loads,
                screen: Default::default(),
            },
            job_rx,
            load_rx,
        )
    }

    /// A hook request as the socket hands it over, and the hook's end.
    fn ipc_event(req_id: u64, req: HookRequest) -> (ipc::IpcEvent, oneshot::Receiver<HookReply>) {
        let (reply, hook) = oneshot::channel();
        (ipc::IpcEvent { req_id, req, reply }, hook)
    }

    fn pre_tool_req(cwd: &str, file: &str) -> HookRequest {
        HookRequest::PreTool {
            session_id: "s1".into(),
            transcript_path: String::new(),
            cwd: cwd.into(),
            tool_name: "Write".into(),
            tool_use_id: "t1".into(),
            tool_input: serde_json::json!({"file_path": file, "content": "x"}),
        }
    }

    #[test]
    fn hook_gone_while_loading_keeps_the_decision_for_the_retry() {
        use crate::step::tests::{input, pending};
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&Checkout("/a".into()), Mode::Step);
        let (workers, _jobs, _loads) = wedged();
        let mut waiting = HashMap::new();
        let mut loading = Loading::from([(1, true)]);
        let (reply, hook) = oneshot::channel();
        waiting.insert(1, reply);
        // The hook leaves while the worker reads its file.
        drop(hook);
        let t = std::time::Instant::now();
        on_msg(&mut app, Msg::Tick(t), &workers, &mut waiting, &mut loading).unwrap();
        assert!(
            waiting.contains_key(&1),
            "not reported before its card exists"
        );
        let edit = pending("s1", "/a", input());
        let msg = Msg::PreTool {
            req_id: 1,
            edit: edit.clone(),
            at: t,
        };
        on_msg(&mut app, msg, &workers, &mut waiting, &mut loading).unwrap();
        assert!(waiting.is_empty() && loading.is_empty());
        let y = KeyEvent::from(KeyCode::Char('y'));
        let later = t + Duration::from_secs(1);
        assert!(
            app.update(Msg::Key(y, later)).is_empty(),
            "nobody to answer"
        );
        assert_eq!(
            app.update(Msg::PreTool {
                req_id: 2,
                edit,
                at: later
            }),
            [Effect::Reply {
                req_id: 2,
                decision: HookReply::Allow
            }]
        );
    }

    #[test]
    fn edit_outside_step_mode_is_answered_while_the_workers_are_wedged() {
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&Checkout("/w/step".into()), Mode::Step);
        let (workers, _jobs, _loads) = wedged();
        let (mut waiting, mut loading) = (Waiting::new(), Loading::new());
        let (ev, mut hook) = ipc_event(1, pre_tool_req("/w/other", "/w/other/a.rs"));
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        assert_eq!(hook.try_recv(), Ok(HookReply::NoDecision));
        assert!(waiting.is_empty() && loading.is_empty());
        // No step-mode checkout at all: the same, and nothing is read.
        app.config = Config::default();
        let (ev, mut hook) = ipc_event(2, pre_tool_req("/w/step", "/w/step/a.rs"));
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        assert_eq!(hook.try_recv(), Ok(HookReply::NoDecision));
    }

    #[test]
    fn edit_into_a_step_checkout_waits_for_its_card() {
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&Checkout("/w/step".into()), Mode::Step);
        let (workers, _jobs, loads) = wedged();
        let (mut waiting, mut loading) = (Waiting::new(), Loading::new());
        // From another checkout, into the step one.
        let (ev, mut hook) = ipc_event(1, pre_tool_req("/w/other", "/w/step/src/a.rs"));
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        assert!(hook.try_recv().is_err(), "no answer yet");
        assert!(waiting.contains_key(&1) && loading.contains_key(&1));
        assert!(matches!(loads.try_recv(), Ok(Load::Edit { req_id: 1, .. })));
    }

    #[test]
    fn step_edit_loads_while_the_effect_worker_is_wedged() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let checkout = Checkout::new(dir.path());
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&checkout, Mode::Step);
        let (tx, mut rx) = unbounded_channel();
        let (jobs, _wedged) = std::sync::mpsc::channel();
        let workers = Workers {
            jobs,
            loads: spawn_loader(tx),
            screen: Default::default(),
        };
        let (mut waiting, mut loading) = (Waiting::new(), Loading::new());
        let cwd = checkout.0.display().to_string();
        let file = checkout.0.join("a.rs").display().to_string();
        let (ev, _hook) = ipc_event(1, pre_tool_req(&cwd, &file));
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        let start = std::time::Instant::now();
        let msg = loop {
            match rx.try_recv() {
                Ok(msg) => break msg,
                Err(_) if start.elapsed() > Duration::from_secs(5) => panic!("no card loaded"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        assert!(matches!(msg, Msg::PreTool { req_id: 1, .. }), "{msg:?}");
    }

    #[test]
    fn a_dead_worker_is_fatal_and_permission_hooks_get_no_decision() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&Checkout("/w/step".into()), Mode::Step);
        let (workers, jobs, _loads) = wedged();
        drop(jobs);
        let (mut waiting, mut loading) = (Waiting::new(), Loading::new());
        let ask = HookRequest::Permission {
            session_id: "s1".into(),
            cwd: "/w/api".into(),
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            pane: None,
        };
        let (ev, mut asked) = ipc_event(1, ask);
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        let (ev, mut edit) = ipc_event(2, pre_tool_req("/w/step", "/w/step/a.rs"));
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        // `t` saves the config, which needs the worker.
        let t = KeyEvent::from(KeyCode::Char('t'));
        let msg = Msg::Key(t, std::time::Instant::now());
        assert_eq!(
            on_msg(&mut app, msg, &workers, &mut waiting, &mut loading),
            Err(WorkerGone)
        );
        abandon_hooks(&app, &mut waiting, &loading);
        assert_eq!(asked.try_recv(), Ok(HookReply::NoDecision));
        // The step-mode edit's hook is let go unanswered, so it refuses.
        assert_eq!(edit.try_recv(), Err(oneshot::error::TryRecvError::Closed));
        assert!(waiting.is_empty());
    }

    #[test]
    fn permission_from_a_subfolder_gets_its_checkout() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        let (tx, mut rx) = unbounded_channel();
        let (jobs, _wedged) = std::sync::mpsc::channel();
        let workers = Workers {
            jobs,
            loads: spawn_loader(tx),
            screen: Default::default(),
        };
        let (mut waiting, mut loading) = (Waiting::new(), Loading::new());
        let ask = HookRequest::Permission {
            session_id: "nobody".into(),
            cwd: dir.path().join("sub").display().to_string(),
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            pane: None,
        };
        let (ev, _hook) = ipc_event(1, ask);
        on_ipc(&mut app, ev, &workers, &mut waiting, &mut loading).unwrap();
        assert!(loading.contains_key(&1));
        let start = std::time::Instant::now();
        let msg = loop {
            match rx.try_recv() {
                Ok(msg) => break msg,
                Err(_) if start.elapsed() > Duration::from_secs(5) => panic!("no card"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        on_msg(&mut app, msg, &workers, &mut waiting, &mut loading).unwrap();
        assert!(loading.is_empty());
        let card = app.queue.current().expect("a permission card");
        assert_eq!(card.checkout, Checkout::new(dir.path()));
    }

    #[test]
    fn checkout_lookups_are_kept_for_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let mut seen = Checkouts::default();
        let t = std::time::Instant::now();
        assert_eq!(seen.of_at(dir.path(), t), None);
        git_init(dir.path());
        assert_eq!(
            seen.of_at(dir.path(), t + Duration::from_secs(1)),
            None,
            "cached"
        );
        let later = t + CHECKOUT_TTL;
        assert_eq!(
            seen.of_at(dir.path(), later),
            Some(Checkout::new(dir.path()))
        );
    }

    #[test]
    fn leading_cd_finds_where_a_command_works() {
        let cwd = Path::new("/w/here");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let cases = [
            ("cd /w/aq && cargo test", Some(PathBuf::from("/w/aq"))),
            ("  cd sub; ls", Some(PathBuf::from("/w/here/sub"))),
            ("cd '/w/a b' && ls", Some(PathBuf::from("/w/a b"))),
            ("cd ~/nodo/aq && ls", Some(home.join("nodo/aq"))),
            ("ls && cd /w/aq", None),
            ("cargo test", None),
        ];
        for (command, want) in cases {
            assert_eq!(leading_cd(cwd, command), want, "{command}");
        }
    }

    #[test]
    fn may_be_step_sees_links_and_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let mut config = Config::default();
        config.set_mode(&Checkout(real.clone()), Mode::Step);
        let input = |f: &Path| serde_json::json!({"file_path": f});
        let other = tempfile::tempdir().unwrap();
        let link = other.path().join("l");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let elsewhere = other.path().display().to_string();
        assert!(may_be_step(
            &config,
            &elsewhere,
            &input(&link.join("new/a.rs"))
        ));
        assert!(may_be_step(
            &config,
            &link.display().to_string(),
            &input(Path::new("b.rs"))
        ));
        assert!(!may_be_step(
            &config,
            &elsewhere,
            &input(&other.path().join("c.rs"))
        ));
        config.set_mode(&Checkout(real), Mode::Review);
        assert!(!may_be_step(
            &config,
            &elsewhere,
            &input(&link.join("a.rs"))
        ));
    }

    fn git_init(dir: &Path) {
        let out = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn file_dir_resolves_new_files_to_an_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::write(real.join("a.rs"), "").unwrap();
        assert_eq!(file_dir(&real.join("a.rs")), Some(real.clone()));
        assert_eq!(file_dir(&real.join("new/deeper/b.rs")), Some(real.clone()));
        let other = tempfile::tempdir().unwrap();
        let link = other.path().join("l.rs");
        std::os::unix::fs::symlink(real.join("a.rs"), &link).unwrap();
        assert_eq!(file_dir(&link), Some(real));
    }

    #[test]
    fn load_edit_reads_file_and_description_in_step_mode() {
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "one\ntwo\n").unwrap();
        let transcript = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/transcript.jsonl"
        );
        let req = || PreToolReq {
            session_id: "s1".into(),
            transcript_path: transcript.into(),
            cwd: dir.path().join("src").display().to_string(),
            tool_name: "Edit".into(),
            tool_use_id: "toolu_edit".into(),
            tool_input: serde_json::json!({"file_path": "a.rs", "old_string": "two", "new_string": "2"}),
        };
        let checkout = Checkout::new(dir.path());
        let mut config = Config::default();
        let e = load_edit(req(), &config);
        assert_eq!(e.checkout.as_ref(), Some(&checkout));
        assert_eq!(e.path, dir.path().join("src/a.rs"));
        assert!(matches!(e.preview, crate::edit::EditPreview::Placed { .. }));
        assert_eq!(e.description, None, "not read outside step mode");
        config.set_mode(&checkout, Mode::Step);
        let e = load_edit(req(), &config);
        assert!(e.description.unwrap().starts_with("Return 429"));
    }

    /// `load_edit` of a Write to `path`, on a thread so a read that blocks
    /// fails the test instead of hanging it.
    fn load_write(path: &Path) -> crate::edit::EditPreview {
        let req = PreToolReq {
            session_id: "s1".into(),
            transcript_path: String::new(),
            cwd: "/".into(),
            tool_name: "Write".into(),
            tool_use_id: "t1".into(),
            tool_input: serde_json::json!({"file_path": path, "content": "new\n"}),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_edit(req, &Config::default()).preview);
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("reading the file blocked")
    }

    fn why(p: crate::edit::EditPreview) -> String {
        match p {
            crate::edit::EditPreview::Unplaced { why, new, .. } => {
                assert_eq!(new, "new\n", "the edit is still shown");
                why
            }
            p => panic!("expected an unplaced edit, got {p:?}"),
        }
    }

    #[test]
    fn load_edit_reads_only_small_regular_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid C string and mode.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(why(load_write(&fifo)), "could not read: not a regular file");
        assert_eq!(
            why(load_write(dir.path())),
            "could not read: not a regular file"
        );
        let big = dir.path().join("big");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(READ_MAX + 1)
            .unwrap();
        assert_eq!(why(load_write(&big)), "could not read: too large");
        let bin = dir.path().join("bin");
        std::fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(why(load_write(&bin)), "could not read: not UTF-8");
        // A link to a small text file is read through.
        let text = dir.path().join("t.txt");
        std::fs::write(&text, "old\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&text, &link).unwrap();
        match load_write(&link) {
            crate::edit::EditPreview::Placed { before, .. } => assert_eq!(before, "old\n"),
            p => panic!("{p:?}"),
        }
        // A file not there yet is a new file.
        match load_write(&dir.path().join("none")) {
            crate::edit::EditPreview::Placed { before, .. } => assert_eq!(before, ""),
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn snapshot_after_a_command_diffs_from_the_last_tree() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let checkout = Checkout::new(dir.path());
        let agent = AgentId {
            pane: "%1".into(),
            claude_pid: 1,
        };
        let snap = |from: Option<&Taken>, command: bool, approved: Option<(&str, &str)>| {
            let Msg::Snapshot { result, .. } = take_snapshot(SnapshotRequest {
                checkout: checkout.clone(),
                from: from.map(|t| t.tree.clone()),
                from_guard: from.map(|t| t.guard.clone()),
                command: command.then(|| ("echo".to_string(), agent.clone())),
                approved: approved.map(|(p, t)| (p.into(), t.to_string())),
            }) else {
                panic!("expected a snapshot");
            };
            result.unwrap()
        };
        let base = snap(None, false, None);
        assert!(base.files.is_empty());
        assert!(base.guard.contains_key("config"), "{:?}", base.guard);
        let t = snap(Some(&base), true, None);
        assert!(
            t.files.is_empty() && t.git_changes.is_empty(),
            "nothing changed"
        );
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        assert_eq!(snap(Some(&base), true, None).files.len(), 2);
        // After an approved edit of a.txt only the unrelated b.txt is left.
        let t = snap(Some(&base), true, Some(("a.txt", "two\n")));
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files[0].new_path.as_deref(), Some(Path::new("b.txt")));
        // Not what was approved: the file stays on the card.
        let other = "something else\n";
        assert_eq!(
            snap(Some(&base), true, Some(("a.txt", other))).files.len(),
            2
        );
        // A planted hook is no file change, but is seen.
        std::fs::write(dir.path().join(".git/hooks/pre-commit"), "#!/bin/sh\n").unwrap();
        let t = snap(Some(&base), true, None);
        assert_eq!(t.git_changes, ["hooks/pre-commit"]);
    }

    #[test]
    fn post_tool_finds_the_edited_files_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success());
        let Msg::PostTool { checkout, .. } = load_post(PostToolReq {
            session_id: "s1".into(),
            cwd: dir.path().display().to_string(),
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
        }) else {
            panic!("expected PostTool");
        };
        assert_eq!(checkout, Some(Checkout::new(dir.path())));
    }

    #[test]
    fn compute_review_diffs_base_to_working_tree() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let req = ReviewRequest {
            checkout: crate::model::Checkout::new(dir.path()),
            agent: AgentId {
                pane: "%1".into(),
                claude_pid: 1,
            },
            from_tree: None,
            full_branch: false,
            card: None,
        };
        let Msg::ReviewComputed { files, to_tree, .. } = compute_review(req) else {
            panic!("expected a diff");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
        assert_eq!(to_tree.len(), 40);
    }
}
