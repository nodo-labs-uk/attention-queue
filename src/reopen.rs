//! The Claude sessions that were open, kept so a crash or reboot loses none:
//! when aq starts and some are no longer running, it offers to reopen them.
//! Pure: no I/O.

use crossterm::event::{KeyCode, KeyEvent};

use crate::agents::Agent;
use crate::app::{App, Effect, Overlay};
use crate::persist::OpenSession;

/// Kept apart from `Agent` so a session can outlive its pane.
fn of(agent: &Agent) -> Option<OpenSession> {
    (!agent.session_id.is_empty()).then(|| OpenSession {
        dir: agent.cwd.clone(),
        session_id: agent.session_id.clone(),
        name: agent.window_name.clone(),
    })
}

fn has(list: &[OpenSession], id: &str) -> bool {
    list.iter().any(|s| s.session_id == id)
}

impl App {
    /// What to save: the sessions open now, then the lost ones. No session
    /// is in both.
    pub(crate) fn open_sessions(&self) -> Vec<OpenSession> {
        self.sessions.iter().chain(&self.lost).cloned().collect()
    }

    /// Takes in the sessions running now. On the first list after aq
    /// starts, the saved sessions not among them are lost, and aq offers to
    /// reopen them. Later, a session that ends on its own is taken as
    /// closed, unless it is the last one or others ended in the same look:
    /// that is a crash or a shutdown, and they are kept to offer next time.
    pub(crate) fn track_sessions(&mut self, agents: &[Agent]) {
        let live: Vec<OpenSession> = agents.iter().filter_map(of).collect();
        let gone: Vec<OpenSession> = self
            .sessions
            .iter()
            .filter(|s| !has(&live, &s.session_id))
            .cloned()
            .collect();
        let first = !self.sessions_listed;
        self.sessions_listed = true;
        if first || gone.len() >= 2 || (live.is_empty() && !gone.is_empty()) {
            for s in gone {
                if !has(&self.lost, &s.session_id) {
                    self.lost.push(s);
                }
            }
        }
        self.lost.retain(|l| !has(&live, &l.session_id));
        self.sessions = live;
        if first && !self.lost.is_empty() && self.overlay.is_none() {
            self.overlay = Some(Overlay::Reopen);
        }
    }

    /// `x` closed this session on purpose: it is not offered again.
    pub(crate) fn forget_session(&mut self, session_id: &str) {
        self.sessions.retain(|s| s.session_id != session_id);
        self.lost.retain(|s| s.session_id != session_id);
    }

    /// y or Enter reopens every lost session; n, q or Esc forgets them.
    pub(crate) fn on_reopen_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let reopen = match key.code {
            KeyCode::Char('y') | KeyCode::Enter => true,
            KeyCode::Char('n' | 'q') | KeyCode::Esc => false,
            _ => return Vec::new(),
        };
        self.overlay = None;
        let lost = std::mem::take(&mut self.lost);
        let n = lost.len();
        let sessions = if n == 1 { "session" } else { "sessions" };
        if !reopen {
            self.set_status(format!("Forgot {n} {sessions}"));
            return Vec::new();
        }
        self.set_status(format!("Reopening {n} {sessions}"));
        lost.into_iter().map(Effect::ResumeSession).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{agent, app, key};
    use crate::persist::SavedState;

    fn with_id(id: &str, pane: &str, dir: &str) -> Agent {
        let mut a = agent(pane, 1, id, dir, false);
        a.session_id = id.into();
        a
    }

    fn saved(ids: &[&str]) -> SavedState {
        SavedState {
            sessions: ids
                .iter()
                .map(|id| OpenSession {
                    dir: format!("/w/{id}").into(),
                    session_id: id.to_string(),
                    name: id.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn ids(list: &[OpenSession]) -> Vec<&str> {
        list.iter().map(|s| s.session_id.as_str()).collect()
    }

    #[test]
    fn sessions_missing_at_start_are_offered_and_reopened() {
        let mut a = app();
        a.restore(saved(&["api", "web", "docs"]));
        a.update(Msg::Agents(vec![with_id("web", "%1", "/w/web")]));
        assert_eq!(a.overlay, Some(Overlay::Reopen));
        assert_eq!(ids(&a.lost), ["api", "docs"]);
        let effects = a.update(key('y'));
        let resumed: Vec<&str> = effects
            .iter()
            .map(|e| match e {
                Effect::ResumeSession(s) => s.session_id.as_str(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(resumed, ["api", "docs"]);
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("Reopening 2 sessions"));
        assert_eq!(ids(&a.saved_state().sessions), ["web"]);
    }

    #[test]
    fn saying_no_forgets_them() {
        let mut a = app();
        a.restore(saved(&["api"]));
        a.update(Msg::Agents(Vec::new()));
        assert!(a.update(key('n')).is_empty());
        assert_eq!(a.status_text(), Some("Forgot 1 session"));
        assert!(a.saved_state().sessions.is_empty());
    }

    #[test]
    fn nothing_is_offered_when_all_are_running() {
        let mut a = app();
        a.restore(saved(&["api"]));
        a.update(Msg::Agents(vec![with_id("api", "%1", "/w/api")]));
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn unanswered_offer_is_kept_for_next_start() {
        let mut a = app();
        a.restore(saved(&["api", "web"]));
        a.update(Msg::Agents(Vec::new()));
        let state = a.saved_state();
        let mut b = app();
        b.restore(state);
        b.update(Msg::Agents(Vec::new()));
        assert_eq!(b.overlay, Some(Overlay::Reopen));
        assert_eq!(ids(&b.lost), ["api", "web"]);
    }

    #[test]
    fn one_session_ending_while_others_run_is_closed() {
        let mut a = app();
        a.update(Msg::Agents(vec![
            with_id("api", "%1", "/w/api"),
            with_id("web", "%2", "/w/web"),
        ]));
        a.update(Msg::Agents(vec![with_id("web", "%2", "/w/web")]));
        assert_eq!(ids(&a.saved_state().sessions), ["web"]);
    }

    #[test]
    fn sessions_ending_together_are_kept() {
        let mut a = app();
        a.update(Msg::Agents(vec![
            with_id("api", "%1", "/w/api"),
            with_id("web", "%2", "/w/web"),
            with_id("docs", "%3", "/w/docs"),
        ]));
        a.update(Msg::Agents(vec![with_id("docs", "%3", "/w/docs")]));
        assert_eq!(ids(&a.saved_state().sessions), ["docs", "api", "web"]);
        // The last one going is kept too.
        a.update(Msg::Agents(Vec::new()));
        assert_eq!(ids(&a.saved_state().sessions), ["api", "web", "docs"]);
        assert_eq!(a.overlay, None, "offered only when aq starts");
    }

    #[test]
    fn a_kept_session_that_comes_back_is_no_longer_lost() {
        let mut a = app();
        a.restore(saved(&["api"]));
        a.update(Msg::Agents(Vec::new()));
        a.overlay = None;
        a.update(Msg::Agents(vec![with_id("api", "%4", "/w/api")]));
        assert!(a.lost.is_empty());
        assert_eq!(ids(&a.saved_state().sessions), ["api"]);
    }

    #[test]
    fn closing_with_x_forgets_even_the_last_session() {
        let mut a = app();
        a.update(Msg::Agents(vec![with_id("api", "%1", "/w/api")]));
        a.update(key('x'));
        a.update(key('y'));
        a.update(Msg::Agents(Vec::new()));
        assert!(a.saved_state().sessions.is_empty());
    }
}
