//! The impure shell around `App::update`: terminal, agent polling, key input,
//! rendering and effects.

use std::collections::{BTreeMap, HashMap};
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

use crate::agents::AgentConn;
use crate::app::{App, Effect, Msg, ReviewRequest};
use crate::config::{Config, Paths};
use crate::git;
use crate::highlight::Highlighter;
use crate::ipc::{self, HookReply};
use crate::model::AgentId;
use crate::review::LastRound;
use crate::theme::load_themes;

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
                apply(app, msg, &jobs, &mut waiting);
                dirty = true;
            }
            Some(ev) = ipc_rx.recv() => {
                // Forget hooks that went away without an answer.
                waiting.retain(|_, reply| !reply.is_closed());
                waiting.insert(ev.req_id, ev.reply);
                apply(app, Msg::Ipc { req_id: ev.req_id, req: ev.req }, &jobs, &mut waiting);
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
                Event::Key(key) => Msg::Key(key),
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
