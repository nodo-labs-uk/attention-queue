use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::agents::Agent;
use crate::config::Config;
use crate::ipc::{Decision, IpcEvent};
use crate::llm::{LlmEvent, LlmJob};
use crate::model::{AgentId, Card, Checkout, Mode};
use crate::queue::Queue;
use crate::theme::Theme;

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
    Ipc(IpcEvent),
    Llm(LlmEvent),
}

/// Work for the runtime. `update` never does I/O itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Send {
        agent: AgentId,
        text: String,
    },
    Reply {
        req_id: u64,
        decision: Decision,
    },
    Llm(LlmJob),
    /// Write the config (and, later, drafts and quiz progress) to disk.
    SaveState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// `?`: a side question about the current card.
    Ask,
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
        }
    }

    pub fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status_msg.as_ref().map(|s| s.text.as_str())
    }

    fn set_status(&mut self, text: impl Into<String>) {
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
            Msg::Ipc(IpcEvent::Noop) => Vec::new(),
            Msg::Llm(LlmEvent::Noop) => Vec::new(),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
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

    /// An agent just went from working to waiting. Review cards (Task 6) and
    /// quiz cards (Task 11) are created here, by the mode of its checkout.
    fn on_agent_waiting(&mut self, _agent: &Agent) -> Vec<Effect> {
        Vec::new()
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
        a.queue.push(card(1, "/a", CardKind::Review(ReviewCard), 0));
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
        a.queue.push(card(1, "/a", CardKind::Review(ReviewCard), 0));
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
            .push(card(1, "/a", CardKind::Review(ReviewCard), 10));
        a.queue.push(card(2, "/b", CardKind::Review(ReviewCard), 5));
        a.update(key('s'));
        assert_eq!(a.queue.current().unwrap().id, 2);
    }

    #[test]
    fn shared_checkout_names_both_windows() {
        let c = card(1, "/a", CardKind::Review(ReviewCard), 0);
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
