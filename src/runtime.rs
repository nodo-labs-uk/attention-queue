//! The impure shell around `App::update`: terminal, agent polling, key input,
//! rendering and effects.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::net::UnixListener;
use tokio::sync::mpsc::{UnboundedSender, channel, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::agents::{self, AgentConn};
use crate::app::{App, Effect, Msg, ReviewRequest};
use crate::config::{Config, Paths};
use crate::edit::apply_edit;
use crate::git;
use crate::highlight::Highlighter;
use crate::ipc::{self, HookReply, HookRequest};
use crate::model::{AgentId, Checkout, Mode};
use crate::review::LastRound;
use crate::step::PendingEdit;
use crate::theme::load_themes;
use crate::transcript::description_before;

const AGENT_POLL: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_secs(1);
/// At most 30 frames a second.
const FRAME: Duration = Duration::from_millis(33);

pub async fn run(paths: Paths, conn: Arc<dyn AgentConn>) -> Result<()> {
    // Loaded before raw mode: bad theme files warn on stderr.
    let config = Config::load(&paths.config_file)?;
    let themes = load_themes(&paths.themes_dir);
    let mut app = App::new(config, themes);
    // Before raw mode, so "aq is already running" shows on the terminal.
    let listener = ipc::listen(&paths.socket)?;
    match load_rounds(&rounds_file(&paths)) {
        Ok(rounds) => app.rounds = rounds,
        Err(e) => app.set_status(format!("Could not read saved review rounds: {e:#}")),
    }

    // Installs a panic hook that leaves raw mode and the alternate screen.
    let mut terminal = ratatui::try_init().context("starting the terminal")?;
    let result = event_loop(&mut terminal, &mut app, &paths, conn, listener).await;
    ratatui::restore();
    let _ = std::fs::remove_file(&paths.socket);
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    conn: Arc<dyn AgentConn>,
    listener: UnixListener,
) -> Result<()> {
    let (tx, mut rx) = unbounded_channel();
    let (ipc_tx, mut ipc_rx) = channel(64);
    tokio::spawn(ipc::accept_loop(listener, ipc_tx));
    // Hooks waiting for an answer, by request id.
    let mut waiting: HashMap<u64, oneshot::Sender<HookReply>> = HashMap::new();
    // PreToolUse requests the effect worker is still reading files for.
    let mut loading: HashSet<u64> = HashSet::new();
    let (jobs, worker) = spawn_effect_worker(tx.clone(), conn.clone(), paths.clone());
    spawn_agent_poll(tx.clone(), conn);
    spawn_key_reader(tx.clone());
    spawn_ticker(tx);

    let mut hl = Highlighter::new(app.theme());
    let mut hl_theme = app.theme().name.clone();
    let mut dirty = true;
    let mut next_frame = Instant::now();
    while !app.quit {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                on_msg(app, msg, &jobs, &mut waiting, &mut loading);
                dirty = true;
            }
            Some(ev) = ipc_rx.recv() => {
                sweep(app, &jobs, &mut waiting, &loading);
                waiting.insert(ev.req_id, ev.reply);
                match ev.req {
                    HookRequest::PreTool {
                        session_id,
                        transcript_path,
                        cwd,
                        tool_name,
                        tool_use_id,
                        tool_input,
                    } => {
                        loading.insert(ev.req_id);
                        let _ = jobs.send(Job::LoadEdit {
                            req_id: ev.req_id,
                            req: PreToolReq {
                                session_id,
                                transcript_path,
                                cwd,
                                tool_name,
                                tool_use_id,
                                tool_input,
                            },
                            config: app.config.clone(),
                        });
                    }
                    req => apply(app, Msg::Ipc { req_id: ev.req_id, req }, &jobs, &mut waiting),
                }
                dirty = true;
            }
            _ = tokio::time::sleep_until(next_frame), if dirty => {
                if hl_theme != app.theme().name {
                    hl.set_theme(app.theme());
                    hl_theme = app.theme().name.clone();
                }
                terminal.draw(|f| crate::ui::draw(f, app, &mut hl))?;
                dirty = false;
                next_frame = Instant::now() + FRAME;
            }
        }
    }
    // Let queued saves and sends finish before exit.
    drop(jobs);
    let _ = worker.join();
    Ok(())
}

/// Runs a message from the runtime's own channel through the app.
fn on_msg(
    app: &mut App,
    msg: Msg,
    jobs: &std::sync::mpsc::Sender<Job>,
    waiting: &mut HashMap<u64, oneshot::Sender<HookReply>>,
    loading: &mut HashSet<u64>,
) {
    let loaded = match &msg {
        Msg::PreTool { req_id, .. } => Some(*req_id),
        _ => None,
    };
    // Before: a decision sees which hooks still wait.
    sweep(app, jobs, waiting, loading);
    apply(app, msg, jobs, waiting);
    // After: a hook that left while its file was read is reported now that
    // its card exists, so the card keeps its decision for the retry.
    if let Some(req_id) = loaded {
        loading.remove(&req_id);
    }
    sweep(app, jobs, waiting, loading);
}

/// Tells the app about hooks that went away without an answer.
fn sweep(
    app: &mut App,
    jobs: &std::sync::mpsc::Sender<Job>,
    waiting: &mut HashMap<u64, oneshot::Sender<HookReply>>,
    loading: &HashSet<u64>,
) {
    let gone: Vec<u64> = waiting
        .iter()
        .filter(|(id, reply)| reply.is_closed() && !loading.contains(id))
        .map(|(id, _)| *id)
        .collect();
    for req_id in gone {
        waiting.remove(&req_id);
        apply(app, Msg::HookGone { req_id }, jobs, waiting);
    }
}

/// Runs `msg` through `update` and carries out its effects.
fn apply(
    app: &mut App,
    msg: Msg,
    jobs: &std::sync::mpsc::Sender<Job>,
    waiting: &mut HashMap<u64, oneshot::Sender<HookReply>>,
) {
    for effect in app.update(msg) {
        let job = match effect {
            Effect::Send { agent, text, card } => Job::Send { agent, text, card },
            Effect::SaveState => Job::Save(app.config.clone()),
            Effect::ComputeReview(req) => Job::Review(req),
            Effect::SaveRounds => Job::SaveRounds(app.rounds.clone()),
            Effect::Grep { checkout, ident } => Job::Grep { checkout, ident },
            Effect::Reply { req_id, decision } => {
                // The hook may have gone; then nobody is listening.
                if let Some(reply) = waiting.remove(&req_id) {
                    let _ = reply.send(decision);
                }
                continue;
            }
            // LLM work arrives with the LLM worker.
            Effect::Llm(_) => continue,
        };
        // The worker outlives the loop, so this cannot fail while we run.
        let _ = jobs.send(job);
    }
}

/// Blocking effect work, done in order on the effect worker.
enum Job {
    Send {
        agent: AgentId,
        text: String,
        card: Option<u64>,
    },
    Save(Config),
    Review(ReviewRequest),
    SaveRounds(BTreeMap<PathBuf, LastRound>),
    /// Read what a step card needs. `config` is a snapshot, used only to
    /// skip reading the transcript outside step mode.
    LoadEdit {
        req_id: u64,
        req: PreToolReq,
        config: Config,
    },
    Grep {
        checkout: Checkout,
        ident: String,
    },
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

/// Runs effects off the event loop, one at a time so they land in order.
/// Failures come back as `Msg::EffectFailed`.
fn spawn_effect_worker(
    tx: UnboundedSender<Msg>,
    conn: Arc<dyn AgentConn>,
    paths: Paths,
) -> (std::sync::mpsc::Sender<Job>, JoinHandle<()>) {
    let (jobs, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for job in rx {
            let msg = match job {
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
                Job::Save(config) => config
                    .save(&paths.config_file)
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not save settings: {e:#}"))),
                Job::Review(req) => Some(compute_review(req)),
                Job::SaveRounds(rounds) => save_rounds(&rounds_file(&paths), &rounds)
                    .err()
                    .map(|e| Msg::EffectFailed(format!("Could not save review rounds: {e:#}"))),
                Job::LoadEdit {
                    req_id,
                    req,
                    config,
                } => Some(Msg::PreTool {
                    req_id,
                    edit: load_edit(req, &config),
                    at: std::time::Instant::now(),
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
                return;
            }
        }
    });
    (jobs, worker)
}

/// Diff from the request's tree (or the branch base) to a fresh snapshot.
fn compute_review(req: ReviewRequest) -> Msg {
    let c = &req.checkout;
    let result = (|| {
        let from = match &req.from_tree {
            Some(t) => t.clone(),
            None => git::base_tree(c)?,
        };
        let to = git::snapshot_tree(c)?;
        let files = git::diff_trees(c, &from, &to)?;
        anyhow::Ok((from, to, files))
    })();
    match result {
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

/// Finds the checkouts of the request's cwd and of the file it edits, and
/// reads the file and, in step mode, Claude's description of the edit.
fn load_edit(req: PreToolReq, config: &Config) -> PendingEdit {
    let cwd = Path::new(&req.cwd);
    let checkout = agents::checkout_of(cwd);
    let field = |k| req.tool_input.get(k).and_then(serde_json::Value::as_str);
    let path = cwd.join(field("file_path").or(field("notebook_path")).unwrap_or(""));
    let file_checkout = file_dir(&path).and_then(|d| agents::checkout_of(&d));
    let current = std::fs::read_to_string(&path).ok();
    let step = |c: &Option<Checkout>| c.as_ref().is_some_and(|c| config.mode_for(c) == Mode::Step);
    let step = step(&checkout) || step(&file_checkout);
    let description = step
        .then(|| description_before(Path::new(&req.transcript_path), &req.tool_use_id))
        .flatten();
    PendingEdit {
        preview: apply_edit(&req.tool_name, &req.tool_input, current.as_deref()),
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

fn rounds_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("snapshots.json")
}

/// A missing file means no rounds yet.
fn load_rounds(path: &Path) -> Result<BTreeMap<PathBuf, LastRound>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_rounds(path: &Path, rounds: &BTreeMap<PathBuf, LastRound>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(rounds).context("serialising review rounds")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
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
    use crate::review::{Comment, Side};

    #[test]
    fn rounds_round_trip_and_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/snapshots.json");
        assert!(load_rounds(&file).unwrap().is_empty());
        let mut rounds = BTreeMap::new();
        rounds.insert(
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
        save_rounds(&file, &rounds).unwrap();
        assert_eq!(load_rounds(&file).unwrap(), rounds);
    }

    #[test]
    fn hook_gone_while_loading_keeps_the_decision_for_the_retry() {
        use crate::step::tests::{input, pending};
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = App::new(Config::default(), crate::theme::builtin_themes());
        app.config.set_mode(&Checkout("/a".into()), Mode::Step);
        let (jobs, _job_rx) = std::sync::mpsc::channel();
        let mut waiting = HashMap::new();
        let mut loading = HashSet::from([1]);
        let (reply, hook) = oneshot::channel();
        waiting.insert(1, reply);
        // The hook leaves while the worker reads its file.
        drop(hook);
        let t = std::time::Instant::now();
        on_msg(&mut app, Msg::Tick(t), &jobs, &mut waiting, &mut loading);
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
        on_msg(&mut app, msg, &jobs, &mut waiting, &mut loading);
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
