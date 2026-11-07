//! The impure shell around `App::update`: terminal, agent polling, key input,
//! rendering and effects.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use crate::agents::AgentConn;
use crate::app::{App, Effect, Msg};
use crate::config::{Config, Paths};
use crate::model::AgentId;
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

    // Installs a panic hook that leaves raw mode and the alternate screen.
    let mut terminal = ratatui::try_init().context("starting the terminal")?;
    let result = event_loop(&mut terminal, &mut app, &paths, conn).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    paths: &Paths,
    conn: Arc<dyn AgentConn>,
) -> Result<()> {
    let (tx, mut rx) = unbounded_channel();
    let (jobs, worker) = spawn_effect_worker(tx.clone(), conn.clone(), paths.config_file.clone());
    spawn_agent_poll(tx.clone(), conn);
    spawn_key_reader(tx.clone());
    spawn_ticker(tx);

    let mut dirty = true;
    let mut next_frame = Instant::now();
    while !app.quit {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                for effect in app.update(msg) {
                    let job = match effect {
                        Effect::Send { agent, text } => Job::Send { agent, text },
                        Effect::SaveState => Job::Save(app.config.clone()),
                        // Hook replies and LLM work arrive with the socket server and the LLM worker.
                        Effect::Reply { .. } | Effect::Llm(_) => continue,
                    };
                    // The worker outlives the loop, so this cannot fail while we run.
                    let _ = jobs.send(job);
                }
                dirty = true;
            }
            _ = tokio::time::sleep_until(next_frame), if dirty => {
                terminal.draw(|f| crate::ui::draw(f, app))?;
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

/// Blocking effect work, done in order on the effect worker.
enum Job {
    Send { agent: AgentId, text: String },
    Save(Config),
}

/// Runs effects off the event loop, one at a time so they land in order.
/// Failures come back as `Msg::EffectFailed`.
fn spawn_effect_worker(
    tx: UnboundedSender<Msg>,
    conn: Arc<dyn AgentConn>,
    config_file: PathBuf,
) -> (std::sync::mpsc::Sender<Job>, JoinHandle<()>) {
    let (jobs, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for job in rx {
            let failed = match job {
                Job::Send { agent, text } => conn
                    .send_text(&agent, &text)
                    .err()
                    .map(|e| format!("Could not send to {}: {e:#}", agent.pane)),
                Job::Save(config) => config
                    .save(&config_file)
                    .err()
                    .map(|e| format!("Could not save settings: {e:#}")),
            };
            if let Some(e) = failed
                && tx.send(Msg::EffectFailed(e)).is_err()
            {
                return;
            }
        }
    });
    (jobs, worker)
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
