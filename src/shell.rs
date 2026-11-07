//! Shell change cards: after a Bash call in a step-mode checkout, whatever the
//! command changed becomes a card to approve or ask to undo. Pure: the
//! snapshots are taken by the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use crate::app::{App, Effect};
use crate::git::FileDiff;
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::review::{self, ReviewCard};
use crate::step::EditKey;

/// `u`: the message starts with this, then the command.
pub const UNDO_PREFIX: &str = "Please undo what this command changed: ";

/// `u` on an `OUTSIDE_TITLE` card: the message starts with this, then the files.
pub const UNDO_OUTSIDE_PREFIX: &str =
    "Please undo these changes, which no edit of yours explains: ";

/// Tools whose edits step mode already approved; they change the tree too.
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Title of the card for changes no reviewed edit or command explains.
pub const OUTSIDE_TITLE: &str = "Changed outside a reviewed edit";
/// Shown on a card when git's own settings changed.
pub const GIT_WARNING: &str = "Git settings or hooks changed (.git/config, hooks or info)";

/// Hashes of git's config, hooks and info files, by name.
pub type Guard = BTreeMap<String, u64>;

/// What a shell command changed in a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellCard {
    /// The Bash command, or `OUTSIDE_TITLE` when `outside`.
    pub command: String,
    /// Changes found after an edit that its own file does not explain.
    pub outside: bool,
    /// The git files that changed, when any did.
    pub git_changes: Vec<String>,
    /// The diff, with the review board's cursor and view.
    pub diff: ReviewCard,
}

impl ShellCard {
    pub fn new(command: String, from_tree: String, to_tree: String, files: Vec<FileDiff>) -> Self {
        ShellCard {
            command,
            diff: ReviewCard::new(1, from_tree, to_tree, files, Vec::new(), false),
            ..Default::default()
        }
    }

    /// The warning line for changed git settings.
    pub fn warning(&self) -> Option<String> {
        (!self.git_changes.is_empty())
            .then(|| format!("{GIT_WARNING}: {}", self.git_changes.join(", ")))
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
    /// The git files' hashes at `from`.
    pub from_guard: Option<Guard>,
    /// The Bash command to make a card for (or `OUTSIDE_TITLE` after an
    /// edit), and the agent that ran it.
    pub command: Option<(String, AgentId)>,
    /// A file an approved edit touched, relative to the checkout, and the
    /// its text as that edit leaves it. Left out of the diff only if the new
    /// snapshot holds exactly that text.
    pub approved: Option<(PathBuf, String)>,
}

/// A snapshot taken: the new tree, its diff from the request's tree (without
/// the edited file), the git files' hashes now and which of them changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub tree: String,
    pub files: Vec<FileDiff>,
    pub guard: Guard,
    pub git_changes: Vec<String>,
}

pub type Snapshot = Result<Taken, String>;

/// Names in `now` that differ from `before` (added, changed or removed).
pub fn guard_changes(before: &Guard, now: &Guard) -> Vec<String> {
    let mut out: Vec<String> = now
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .chain(before.keys().filter(|k| !now.contains_key(*k)).cloned())
        .collect();
    out.sort();
    out
}

/// `files` without the one at `path`.
pub fn without_path(files: Vec<FileDiff>, path: &Path) -> Vec<FileDiff> {
    files
        .into_iter()
        .filter(|f| f.new_path.as_deref() != Some(path) && f.old_path.as_deref() != Some(path))
        .collect()
}

/// A snapshot waiting its turn: the command (`None`: a baseline) and edited file.
type Queued = (Option<(String, AgentId)>, Option<(PathBuf, String)>);

#[derive(Debug, Default)]
pub struct Shell {
    /// Working tree of each step-mode checkout as of the last command or edit.
    pub last_tree: BTreeMap<Checkout, String>,
    /// The git files' hashes taken with `last_tree`.
    pub last_guard: BTreeMap<Checkout, Guard>,
    /// Checkouts with a snapshot being taken.
    busy: BTreeSet<Checkout>,
    /// Commands (`None`: just a baseline) waiting for the busy snapshot, in order.
    queued: BTreeMap<Checkout, Vec<Queued>>,
}

impl App {
    /// A tool finished in `checkout` (the file's for an edit, else the cwd's).
    pub(crate) fn on_post_tool(
        &mut self,
        session_id: &str,
        checkout: Option<Checkout>,
        tool_name: &str,
        tool_input: &Value,
        edited: Option<&Path>,
    ) -> Vec<Effect> {
        let Some(checkout) = checkout.filter(|c| self.config.mode_for(c) == Mode::Step) else {
            return Vec::new();
        };
        let is_edit = EDIT_TOOLS.contains(&tool_name);
        if !is_edit && tool_name != "Bash" {
            return Vec::new();
        }
        let command = if is_edit {
            OUTSIDE_TITLE.to_string()
        } else {
            tool_input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let approved = if is_edit {
            self.approved_edit(session_id, tool_name, tool_input, edited, &checkout)
        } else {
            None
        };
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
        self.request_snapshot(checkout, Some((command, agent)), approved)
    }

    /// The edited file and its approved text, when the step history holds an
    /// allowed edit with this identity (which it then forgets).
    fn approved_edit(
        &mut self,
        session_id: &str,
        tool_name: &str,
        tool_input: &Value,
        edited: Option<&Path>,
        checkout: &Checkout,
    ) -> Option<(PathBuf, String)> {
        let key = EditKey::new(session_id, tool_name, tool_input);
        let at = self
            .steps
            .history
            .iter()
            .rposition(|d| d.allowed && d.key == key && d.after.is_some())?;
        let after = self.steps.history.remove(at).after?;
        let rel = edited?.strip_prefix(&checkout.0).ok()?;
        Some((rel.to_path_buf(), after))
    }

    /// Takes a baseline when `checkout` has none and none is on its way.
    pub(crate) fn ensure_baseline(&mut self, checkout: &Checkout) -> Vec<Effect> {
        let known = self.shell.last_tree.contains_key(checkout)
            || self.shell.busy.contains(checkout)
            || self.config.mode_for(checkout) != Mode::Step;
        if known {
            Vec::new()
        } else {
            self.request_snapshot(checkout.clone(), None, None)
        }
    }

    /// Snapshots happen one at a time per checkout, in the order asked, so
    /// each diffs from the tree the one before left.
    pub(crate) fn request_snapshot(
        &mut self,
        checkout: Checkout,
        command: Option<(String, AgentId)>,
        approved: Option<(PathBuf, String)>,
    ) -> Vec<Effect> {
        if self.shell.busy.contains(&checkout) {
            let queue = self.shell.queued.entry(checkout).or_default();
            // A baseline right behind another baseline adds nothing.
            if command.is_some() || queue.last().is_none_or(|(c, _)| c.is_some()) {
                queue.push((command, approved));
            }
            return Vec::new();
        }
        self.shell.busy.insert(checkout.clone());
        let from = self.shell.last_tree.get(&checkout).cloned();
        let from_guard = self.shell.last_guard.get(&checkout).cloned();
        vec![Effect::Snapshot(SnapshotRequest {
            checkout,
            from,
            from_guard,
            command,
            approved,
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
            Ok(taken) => {
                let still_step = self.config.mode_for(&checkout) == Mode::Step;
                if still_step {
                    self.shell
                        .last_tree
                        .insert(checkout.clone(), taken.tree.clone());
                    self.shell.last_guard.insert(checkout.clone(), taken.guard);
                }
                let changed = !taken.files.is_empty() || !taken.git_changes.is_empty();
                if let (true, true, Some((command, agent)), Some(from)) =
                    (still_step, changed, req.command, req.from)
                {
                    self.next_card_id += 1;
                    let mut card = ShellCard::new(command, from, taken.tree, taken.files);
                    card.outside = card.command == OUTSIDE_TITLE;
                    card.git_changes = taken.git_changes;
                    self.queue.push(Card {
                        id: self.next_card_id,
                        checkout: checkout.clone(),
                        agent,
                        created: at,
                        kind: CardKind::ShellChange(card),
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
            Some((command, approved)) => self.request_snapshot(checkout, command, approved),
            None => {
                self.shell.queued.remove(&checkout);
                Vec::new()
            }
        }
    }

    /// The checkout left step mode: its baseline is stale.
    pub(crate) fn forget_baseline(&mut self, checkout: &Checkout) {
        self.shell.last_tree.remove(checkout);
        self.shell.last_guard.remove(checkout);
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
                let text = if sc.outside {
                    let files: Vec<String> = sc
                        .files()
                        .iter()
                        .map(|f| crate::review::file_path(f).display().to_string())
                        .chain(sc.git_changes.iter().map(|g| format!(".git/{g}")))
                        .collect();
                    format!("{UNDO_OUTSIDE_PREFIX}{}", files.join(", "))
                } else {
                    format!("{UNDO_PREFIX}{}", sc.command)
                };
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
    use std::path::PathBuf;
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
            edited: None,
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
            result: Ok(Taken {
                tree: tree.into(),
                files,
                guard: Guard::new(),
                git_changes: Vec::new(),
            }),
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
            from_guard: None,
            command: None,
            approved: None,
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

    fn post_edit(a: &mut App, file: &str) -> Vec<Effect> {
        a.update(Msg::PostTool {
            session_id: "s1".into(),
            checkout: Some(Checkout("/a".into())),
            tool_name: "Edit".into(),
            tool_input: edit_input(file),
            edited: Some(PathBuf::from("/a").join(file)),
        })
    }

    fn edit_input(file: &str) -> Value {
        json!({"file_path": file, "old_string": "one", "new_string": "two"})
    }

    /// Step history as if the edit of `file` had been allowed, leaving "two\n".
    fn allowed(a: &mut App, file: &str) {
        a.steps.history.push(crate::step::Done {
            session_id: "s1".into(),
            step_no: 1,
            path: PathBuf::from("/a").join(file),
            allowed: true,
            key: EditKey::new("s1", "Edit", &edit_input(file)),
            after: Some("two\n".to_string()),
        });
    }

    #[test]
    fn edit_tool_post_updates_last_tree_without_card() {
        let mut a = baselined();
        allowed(&mut a, "a.txt");
        let req = only_request(post_edit(&mut a, "a.txt"));
        assert_eq!(
            (req.from.as_deref(), req.approved.clone()),
            (Some("t0"), Some(("a.txt".into(), "two\n".to_string())))
        );
        // The edited file is left out of the diff, so nothing else changed.
        snapshot(&mut a, req, "t1", Vec::new());
        assert!(a.queue.is_empty());
        assert_eq!(a.shell.last_tree[&Checkout("/a".into())], "t1");
        // A later command that changes nothing sees the edit as the baseline.
        let req = only_request(post(&mut a, "Bash", bash("ls")));
        assert_eq!(req.from.as_deref(), Some("t1"));
        snapshot(&mut a, req, "t1", Vec::new());
        assert!(a.queue.is_empty());
    }

    #[test]
    fn edit_without_an_approved_step_leaves_its_file_on_the_card() {
        let mut a = baselined();
        // Never allowed in step mode (or already reported once).
        let req = only_request(post_edit(&mut a, "a.txt"));
        assert_eq!(req.approved, None);
        snapshot(&mut a, req, "t0", Vec::new());
        allowed(&mut a, "a.txt");
        let req = only_request(post_edit(&mut a, "a.txt"));
        snapshot(&mut a, req, "t1", Vec::new());
        // The approval is used up by the first report.
        let req = only_request(post_edit(&mut a, "a.txt"));
        assert_eq!(req.approved, None);
    }

    #[test]
    fn edit_with_unrelated_changes_makes_an_outside_card() {
        let mut a = baselined();
        let req = only_request(post_edit(&mut a, "a.txt"));
        snapshot(&mut a, req, "t1", vec![file()]);
        let card = shell(&a);
        assert!(card.outside);
        assert_eq!(card.command, OUTSIDE_TITLE);
        assert_eq!(card.files().len(), 1);
        assert_eq!(a.shell.last_tree[&Checkout("/a".into())], "t1");
    }

    #[test]
    fn git_settings_change_adds_a_warning_even_without_file_changes() {
        let mut a = baselined();
        let req = only_request(post(&mut a, "Bash", bash("echo x > .git/hooks/pre-commit")));
        a.update(Msg::Snapshot {
            req,
            result: Ok(Taken {
                tree: "t0".into(),
                files: Vec::new(),
                guard: Guard::new(),
                git_changes: vec!["hooks/pre-commit".into()],
            }),
            at: SystemTime::UNIX_EPOCH,
        });
        let card = shell(&a);
        assert!(card.files().is_empty());
        assert_eq!(
            card.warning().unwrap(),
            format!("{GIT_WARNING}: hooks/pre-commit")
        );
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
        let next = only_request(snapshot(&mut a, first, "t1", Vec::new()));
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
            edited: None,
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
