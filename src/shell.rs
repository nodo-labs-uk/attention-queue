//! Shell change cards: after a Bash call in a step-mode checkout, whatever the
//! command changed becomes a card to approve or ask to undo. Pure: the
//! snapshots are taken by the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use crate::app::{App, Effect};
use crate::git::FileDiff;
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::review::{self, ReviewCard};

/// `u`: the message starts with this, then the command.
pub const UNDO_PREFIX: &str = "Please undo what this command changed: ";

/// Tools whose edits step mode already approved; they change the tree too.
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// What a shell command changed in a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellCard {
    pub command: String,
    /// The diff, with the review board's cursor and view.
    pub diff: ReviewCard,
}

impl ShellCard {
    pub fn new(command: String, from_tree: String, to_tree: String, files: Vec<FileDiff>) -> Self {
        ShellCard {
            command,
            diff: ReviewCard::new(1, from_tree, to_tree, files, Vec::new(), false),
        }
    }

    pub fn files(&self) -> &[FileDiff] {
        &self.diff.files
    }
}

/// A snapshot of a checkout for the runtime to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRequest {
    pub checkout: Checkout,
    /// The tree to diff from; `None` means only record the new one.
    pub from: Option<String>,
    /// The Bash command to make a card for, and the agent that ran it.
    pub command: Option<(String, AgentId)>,
}

/// The new tree and what changed since `from`.
pub type Snapshot = Result<(String, Vec<FileDiff>), String>;

#[derive(Debug, Default)]
pub struct Shell {
    /// Working tree of each step-mode checkout as of the last command or edit.
    pub last_tree: BTreeMap<Checkout, String>,
    /// Checkouts with a snapshot being taken.
    busy: BTreeSet<Checkout>,
    /// Commands (`None`: just a baseline) waiting for the busy snapshot, in order.
    queued: BTreeMap<Checkout, Vec<Option<(String, AgentId)>>>,
}

impl App {
    /// A tool finished in `checkout` (the file's for an edit, else the cwd's).
    pub(crate) fn on_post_tool(
        &mut self,
        session_id: &str,
        checkout: Option<Checkout>,
        tool_name: &str,
        tool_input: &Value,
    ) -> Vec<Effect> {
        let Some(checkout) = checkout.filter(|c| self.config.mode_for(c) == Mode::Step) else {
            return Vec::new();
        };
        if EDIT_TOOLS.contains(&tool_name) {
            return self.request_snapshot(checkout, None);
        }
        if tool_name != "Bash" {
            return Vec::new();
        }
        let command = tool_input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let agent = self
            .agents
            .iter()
            .find(|a| a.session_id == session_id)
            .map_or_else(
                || AgentId {
                    pane: "?".into(),
                    claude_pid: 0,
                },
                |a| a.id.clone(),
            );
        self.request_snapshot(checkout, Some((command, agent)))
    }

    /// Takes a baseline when `checkout` has none and none is on its way.
    pub(crate) fn ensure_baseline(&mut self, checkout: &Checkout) -> Vec<Effect> {
        let known = self.shell.last_tree.contains_key(checkout)
            || self.shell.busy.contains(checkout)
            || self.config.mode_for(checkout) != Mode::Step;
        if known {
            Vec::new()
        } else {
            self.request_snapshot(checkout.clone(), None)
        }
    }

    /// Snapshots happen one at a time per checkout, in the order asked, so
    /// each diffs from the tree the one before left.
    pub(crate) fn request_snapshot(
        &mut self,
        checkout: Checkout,
        command: Option<(String, AgentId)>,
    ) -> Vec<Effect> {
        if self.shell.busy.contains(&checkout) {
            let queue = self.shell.queued.entry(checkout).or_default();
            // A baseline right behind another baseline adds nothing.
            if command.is_some() || queue.last().is_none_or(Option::is_some) {
                queue.push(command);
            }
            return Vec::new();
        }
        self.shell.busy.insert(checkout.clone());
        let from = self.shell.last_tree.get(&checkout).cloned();
        vec![Effect::Snapshot(SnapshotRequest {
            checkout,
            from,
            command,
        })]
    }

    pub(crate) fn on_snapshot(
        &mut self,
        req: SnapshotRequest,
        result: Snapshot,
        at: SystemTime,
    ) -> Vec<Effect> {
        let checkout = req.checkout;
        self.shell.busy.remove(&checkout);
        match result {
            Err(e) => self.set_status(e),
            Ok((tree, files)) => {
                let still_step = self.config.mode_for(&checkout) == Mode::Step;
                if still_step {
                    self.shell.last_tree.insert(checkout.clone(), tree.clone());
                }
                if let (true, Some((command, agent)), Some(from)) =
                    (still_step, req.command, req.from)
                    && !files.is_empty()
                {
                    self.next_card_id += 1;
                    self.queue.push(Card {
                        id: self.next_card_id,
                        checkout: checkout.clone(),
                        agent,
                        created: at,
                        kind: CardKind::ShellChange(ShellCard::new(command, from, tree, files)),
                    });
                }
            }
        }
        let next = self
            .shell
            .queued
            .get_mut(&checkout)
            .and_then(|q| (!q.is_empty()).then(|| q.remove(0)));
        match next {
            Some(command) => self.request_snapshot(checkout, command),
            None => {
                self.shell.queued.remove(&checkout);
                Vec::new()
            }
        }
    }

    /// The checkout left step mode: its baseline is stale.
    pub(crate) fn forget_baseline(&mut self, checkout: &Checkout) {
        self.shell.last_tree.remove(checkout);
    }

    pub(crate) fn on_shell_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let id = card.id;
        let agent = card.agent.clone();
        let CardKind::ShellChange(sc) = &mut card.kind else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('y') => return self.finish_shell(id, None),
            KeyCode::Char('u') => {
                let text = format!("{UNDO_PREFIX}{}", sc.command);
                return self.finish_shell(id, Some((agent, text)));
            }
            _ => {}
        }
        if let review::Action::Status(s) = review::on_key(&mut sc.diff, key) {
            self.set_status(s);
        }
        Vec::new()
    }

    /// `y` closes the card; `u` sends `undo` and closes it once that lands.
    fn finish_shell(&mut self, id: u64, undo: Option<(AgentId, String)>) -> Vec<Effect> {
        if self.sending.contains_key(&id) {
            self.set_status("Still sending");
            return Vec::new();
        }
        let Some((agent, text)) = undo else {
            self.queue.remove(id);
            self.set_status("Approved");
            return Vec::new();
        };
        let Some(a) = self.agent(&agent) else {
            self.set_status("agent gone");
            return Vec::new();
        };
        let done = format!("Asked {} to undo it", a.window_name);
        self.sending.insert(id, done);
        self.set_status("Sending");
        vec![Effect::Send {
            agent,
            text,
            card: Some(id),
        }]
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::KeyEvent;
    use serde_json::json;

    use super::*;
    use crate::app::Msg;
    use crate::app::tests::agent;
    use crate::config::Config;
    use crate::git::{DiffLine, FileStatus, Hunk};
    use crate::theme::builtin_themes;

    fn shell_app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.config.set_mode(&Checkout("/a".into()), Mode::Step);
        a.agents = vec![agent("%1", 1, "api", "/a", true)];
        a
    }

    fn post(a: &mut App, tool: &str, input: Value) -> Vec<Effect> {
        a.update(Msg::PostTool {
            session_id: "s1".into(),
            checkout: Some(Checkout("/a".into())),
            tool_name: tool.into(),
            tool_input: input,
        })
    }

    fn bash(cmd: &str) -> Value {
        json!({"command": cmd})
    }

    fn file() -> FileDiff {
        FileDiff {
            old_path: Some("a.txt".into()),
            new_path: Some("a.txt".into()),
            status: FileStatus::Modified,
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![
                    DiffLine::Del {
                        old: 1,
                        text: "one".into(),
                    },
                    DiffLine::Add {
                        new: 1,
                        text: "two".into(),
                    },
                ],
            }],
        }
    }

    fn snapshot(
        a: &mut App,
        req: SnapshotRequest,
        tree: &str,
        files: Vec<FileDiff>,
    ) -> Vec<Effect> {
        a.update(Msg::Snapshot {
            req,
            result: Ok((tree.into(), files)),
            at: SystemTime::UNIX_EPOCH,
        })
    }

    fn only_request(effects: Vec<Effect>) -> SnapshotRequest {
        match <[Effect; 1]>::try_from(effects).expect("one effect") {
            [Effect::Snapshot(req)] => req,
            [e] => panic!("not a snapshot: {e:?}"),
        }
    }

    /// An app whose checkout already has the baseline tree "t0".
    fn baselined() -> App {
        let mut a = shell_app();
        let base = only_request(a.ensure_baseline(&Checkout("/a".into())));
        assert_eq!((base.from, base.command), (None, None));
        let base = SnapshotRequest {
            checkout: Checkout("/a".into()),
            from: None,
            command: None,
        };
        assert!(snapshot(&mut a, base, "t0", Vec::new()).is_empty());
        a
    }

    fn shell(a: &App) -> &ShellCard {
        match &a.queue.current().expect("a card").kind {
            CardKind::ShellChange(s) => s,
            k => panic!("not a shell card: {k:?}"),
        }
    }

    #[test]
    fn bash_that_changes_files_makes_card() {
        let mut a = baselined();
        let req = only_request(post(&mut a, "Bash", bash("sed -i s/one/two/ a.txt")));
        assert_eq!(req.from.as_deref(), Some("t0"));
        assert_eq!(req.command.as_ref().unwrap().0, "sed -i s/one/two/ a.txt");
        assert!(snapshot(&mut a, req, "t1", vec![file()]).is_empty());
        let card = shell(&a);
        assert_eq!(card.command, "sed -i s/one/two/ a.txt");
        assert_eq!(card.files().len(), 1);
        assert_eq!(a.shell.last_tree[&Checkout("/a".into())], "t1");
        assert_eq!(a.queue.current().unwrap().agent.pane, "%1");
    }

    #[test]
    fn bash_with_no_change_makes_no_card() {
        let mut a = baselined();
        let req = only_request(post(&mut a, "Bash", bash("ls")));
        snapshot(&mut a, req, "t0", Vec::new());
        assert!(a.queue.is_empty());
    }

    #[test]
    fn edit_tool_post_updates_last_tree_without_card() {
        let mut a = baselined();
        let req = only_request(post(&mut a, "Edit", json!({"file_path": "/a/a.txt"})));
        assert_eq!((req.from.as_deref(), &req.command), (Some("t0"), &None));
        // The edit changed files, yet no card: it was approved as a step.
        snapshot(&mut a, req, "t1", vec![file()]);
        assert!(a.queue.is_empty());
        assert_eq!(a.shell.last_tree[&Checkout("/a".into())], "t1");
        // A later command that changes nothing sees the edit as the baseline.
        let req = only_request(post(&mut a, "Bash", bash("ls")));
        assert_eq!(req.from.as_deref(), Some("t1"));
        snapshot(&mut a, req, "t1", Vec::new());
        assert!(a.queue.is_empty());
    }

    #[test]
    fn first_bash_without_baseline_only_records_one() {
        let mut a = shell_app();
        let req = only_request(post(&mut a, "Bash", bash("touch x")));
        assert_eq!(req.from, None);
        // Files cannot be diffed without a start, so no card.
        snapshot(&mut a, req, "t1", vec![file()]);
        assert!(a.queue.is_empty());
        assert_eq!(a.shell.last_tree[&Checkout("/a".into())], "t1");
    }

    #[test]
    fn snapshots_of_one_checkout_run_in_order() {
        let mut a = baselined();
        let first = only_request(post(&mut a, "Edit", json!({})));
        // These arrive while the edit's snapshot is still being taken.
        assert!(post(&mut a, "Bash", bash("touch x")).is_empty());
        assert!(post(&mut a, "Bash", bash("touch y")).is_empty());
        let next = only_request(snapshot(&mut a, first, "t1", vec![file()]));
        assert_eq!(next.from.as_deref(), Some("t1"));
        assert_eq!(next.command.as_ref().unwrap().0, "touch x");
        let next = only_request(snapshot(&mut a, next, "t2", vec![file()]));
        assert_eq!(next.from.as_deref(), Some("t2"));
        assert!(snapshot(&mut a, next, "t3", Vec::new()).is_empty());
        assert_eq!(a.queue.len(), 1);
    }

    #[test]
    fn outside_step_mode_or_other_tools_do_nothing() {
        let mut a = shell_app();
        a.config.set_mode(&Checkout("/a".into()), Mode::Review);
        assert!(post(&mut a, "Bash", bash("ls")).is_empty());
        a.config.set_mode(&Checkout("/a".into()), Mode::Step);
        assert!(post(&mut a, "Read", json!({})).is_empty());
        let none = a.update(Msg::PostTool {
            session_id: "s1".into(),
            checkout: None,
            tool_name: "Bash".into(),
            tool_input: bash("ls"),
        });
        assert!(none.is_empty());
    }

    #[test]
    fn entering_step_mode_takes_a_baseline() {
        let mut a = shell_app();
        a.config.set_mode(&Checkout("/a".into()), Mode::Review);
        a.queue.push(crate::queue::tests::card(
            1,
            "/a",
            CardKind::Review(ReviewCard::default()),
            0,
        ));
        let effects = a.update(Msg::Key(KeyEvent::from(KeyCode::Char('M')), Instant::now()));
        assert!(effects.contains(&Effect::SaveState));
        let req = effects
            .into_iter()
            .find_map(|e| match e {
                Effect::Snapshot(r) => Some(r),
                _ => None,
            })
            .expect("a baseline snapshot");
        assert_eq!((req.from, req.command), (None, None));
    }

    #[test]
    fn y_approves_and_u_sends_the_undo_message() {
        let mut a = baselined();
        let req = only_request(post(&mut a, "Bash", bash("rm a.txt")));
        snapshot(&mut a, req, "t1", vec![file()]);
        let press =
            |a: &mut App, c| a.update(Msg::Key(KeyEvent::from(KeyCode::Char(c)), Instant::now()));
        let effects = press(&mut a, 'u');
        let [Effect::Send { agent, text, card }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(agent.pane, "%1");
        assert_eq!(text, "Please undo what this command changed: rm a.txt");
        let card = card.expect("tracked send");
        // A failed send keeps the card; a landed one closes it.
        a.update(Msg::SendFailed {
            card,
            error: "agent gone".into(),
        });
        assert_eq!(a.queue.len(), 1);
        assert!(!press(&mut a, 'u').is_empty());
        a.update(Msg::SendDone { card });
        assert!(a.queue.is_empty());

        let req = only_request(post(&mut a, "Bash", bash("rm b.txt")));
        snapshot(&mut a, req, "t2", vec![file()]);
        assert!(press(&mut a, 'y').is_empty());
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Approved"));
    }
}
