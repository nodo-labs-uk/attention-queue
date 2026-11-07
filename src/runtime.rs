//! The impure shell around `App::update`: terminal, agent polling, key input,
//! rendering and effects.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::Instant;

use crate::agents::AgentConn;
use crate::app::{App, Effect, Msg};
use crate::config::{Config, Paths};
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
    spawn_agent_poll(tx.clone(), conn.clone());
    spawn_key_reader(tx.clone());
    spawn_ticker(tx);

    let mut dirty = true;
    let mut next_frame = Instant::now();
    while !app.quit {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                for effect in app.update(msg) {
                    run_effect(app, paths, conn.as_ref(), effect);
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
    Ok(())
}

fn run_effect(app: &mut App, paths: &Paths, conn: &dyn AgentConn, effect: Effect) {
    let failed = match effect {
        Effect::Send { agent, text } => conn
            .send_text(&agent, &text)
            .err()
            .map(|e| format!("Could not send to {}: {e:#}", agent.pane)),
        Effect::SaveState => app
            .config
            .save(&paths.config_file)
            .err()
            .map(|e| format!("Could not save settings: {e:#}")),
        // Hook replies and LLM work arrive with the socket server and the LLM worker.
        Effect::Reply { .. } | Effect::Llm(_) => None,
    };
    if failed.is_some() {
        app.status_msg = failed;
    }
}

fn spawn_agent_poll(tx: UnboundedSender<Msg>, conn: Arc<dyn AgentConn>) {
    tokio::spawn(async move {
        loop {
            let conn = conn.clone();
            // A failed poll keeps the last known list; the next poll retries.
            if let Ok(Ok(agents)) = tokio::task::spawn_blocking(move || conn.list()).await
                && tx.send(Msg::Agents(agents)).is_err()
            {
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
                Event::Resize(..) => Msg::Tick,
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
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        }
    });
}
