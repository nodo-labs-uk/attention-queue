//! `O`: opens a workspace, the git repos in one folder, each in a Claude
//! session of its own, all in a tmux session named after the workspace.
//! aq finds the repos and saves the set; nobody writes it by hand. Pure: no
//! I/O.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Overlay};
use crate::config::Workspace;
use crate::new_session::{FolderKey, FolderPick, parent_typed, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDraft {
    pub stage: Stage,
    /// What a typed folder is read against: the project in front of you.
    pub base: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// The saved workspaces, then "Other folder" last.
    Choose { cursor: usize },
    /// Typing the folder that holds the repos.
    Folder(FolderPick),
    /// The runtime is looking for repos in `asked`.
    Finding { asked: PathBuf },
    /// Ticking the repos to open.
    Repos(RepoPick),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPick {
    /// The workspace's name, and its tmux session's.
    pub name: String,
    pub root: PathBuf,
    /// Each repo, relative to `root`, and whether it is ticked.
    pub repos: Vec<(PathBuf, bool)>,
    pub cursor: usize,
}

impl App {
    /// `O`: the saved workspaces to choose from, or with none saved yet,
    /// straight to the folder.
    pub(crate) fn open_workspace_box(&mut self) -> Vec<Effect> {
        let base = self
            .here()
            .map_or_else(|| PathBuf::from("~"), |(dir, ..)| dir);
        let mut draft = WorkspaceDraft {
            stage: Stage::Choose { cursor: 0 },
            base,
        };
        let effects = if self.config.workspaces.is_empty() {
            to_folder(&mut draft)
        } else {
            Vec::new()
        };
        self.overlay = Some(Overlay::Workspace(draft));
        effects
    }

    pub(crate) fn on_workspace_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let saved: Vec<(String, Workspace)> = self
            .config
            .workspaces
            .iter()
            .map(|(n, w)| (n.clone(), w.clone()))
            .collect();
        let Some(Overlay::Workspace(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let ctrl_o =
            key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut draft.stage {
            Stage::Choose { cursor } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    *cursor = (*cursor + 1).min(saved.len());
                    Vec::new()
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    *cursor = cursor.saturating_sub(1);
                    Vec::new()
                }
                KeyCode::Enter => match saved.get(*cursor) {
                    Some((_, w)) => find(draft, w.root.clone()),
                    None => to_folder(draft),
                },
                _ if ctrl_o => to_folder(draft),
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.overlay = None;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Stage::Folder(pick) => match pick.on_key(key) {
                FolderKey::Stay => Vec::new(),
                FolderKey::List(typed) => vec![Effect::ListDirs {
                    typed,
                    base: draft.base.clone(),
                }],
                FolderKey::Cancel => {
                    self.back_or_close(saved.is_empty());
                    Vec::new()
                }
                FolderKey::Chosen { typed, .. } if typed.is_empty() => Vec::new(),
                FolderKey::Chosen { typed, .. } => {
                    let root = resolve(&typed, &draft.base);
                    find(draft, root)
                }
            },
            Stage::Finding { .. } => {
                if key.code == KeyCode::Esc {
                    self.back_or_close(saved.is_empty());
                }
                Vec::new()
            }
            Stage::Repos(pick) => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    pick.cursor = (pick.cursor + 1).min(pick.repos.len().saturating_sub(1));
                    Vec::new()
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    pick.cursor = pick.cursor.saturating_sub(1);
                    Vec::new()
                }
                KeyCode::Char(' ') => {
                    if let Some((_, on)) = pick.repos.get_mut(pick.cursor) {
                        *on = !*on;
                    }
                    Vec::new()
                }
                KeyCode::Char('a') => {
                    let all = pick.repos.iter().all(|(_, on)| *on);
                    pick.repos.iter_mut().for_each(|(_, on)| *on = !all);
                    Vec::new()
                }
                KeyCode::Enter => {
                    let pick = pick.clone();
                    self.open_workspace(pick)
                }
                KeyCode::Esc => {
                    self.back_or_close(saved.is_empty());
                    Vec::new()
                }
                _ => Vec::new(),
            },
        }
    }

    /// Esc: back to the saved workspaces, or closed when there are none.
    fn back_or_close(&mut self, none_saved: bool) {
        match &mut self.overlay {
            Some(Overlay::Workspace(d)) if !none_saved => d.stage = Stage::Choose { cursor: 0 },
            _ => self.overlay = None,
        }
    }

    /// The repos the runtime found in `root`, asked for as `asked`.
    pub(crate) fn on_repos(
        &mut self,
        asked: PathBuf,
        root: PathBuf,
        repos: Vec<PathBuf>,
    ) -> Vec<Effect> {
        let Some(Overlay::Workspace(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        if !matches!(&draft.stage, Stage::Finding { asked: a } if *a == asked) {
            return Vec::new();
        }
        if repos.is_empty() {
            let effects = to_folder_at(draft, &format!("{}/", asked.display()));
            self.set_status(format!("No git repos in {}", root.display()));
            return effects;
        }
        let saved = self
            .config
            .workspaces
            .iter()
            .find(|(_, w)| w.root == root)
            .map(|(n, w)| (n.clone(), w.repos.clone()));
        let (name, repos) = match saved {
            Some((name, ticked)) => {
                let repos = repos
                    .into_iter()
                    .map(|r| {
                        let on = ticked.contains(&r);
                        (r, on)
                    })
                    .collect();
                (name, repos)
            }
            None => (
                new_name(&root, &self.config.workspaces),
                repos.into_iter().map(|r| (r, true)).collect(),
            ),
        };
        draft.stage = Stage::Repos(RepoPick {
            name,
            root,
            repos,
            cursor: 0,
        });
        Vec::new()
    }

    /// Enter on the repos: saves the workspace and starts Claude in each
    /// ticked repo that has no session in the workspace yet.
    fn open_workspace(&mut self, pick: RepoPick) -> Vec<Effect> {
        let ticked: Vec<PathBuf> = pick
            .repos
            .iter()
            .filter(|(_, on)| *on)
            .map(|(r, _)| r.clone())
            .collect();
        if ticked.is_empty() {
            self.set_status("Tick at least one repo: Space ticks, a ticks all");
            return Vec::new();
        }
        let open: Vec<PathBuf> = self
            .agents
            .iter()
            .filter(|a| a.session == pick.name)
            .flat_map(|a| {
                [
                    Some(a.cwd.clone()),
                    a.checkout.as_ref().map(|c| c.0.clone()),
                ]
            })
            .flatten()
            .collect();
        let dirs: Vec<PathBuf> = ticked
            .iter()
            .map(|r| pick.root.join(r))
            .filter(|d| !open.contains(d))
            .collect();
        let already = ticked.len() - dirs.len();
        let names: Vec<String> = ticked
            .iter()
            .map(|r| crate::app::project_name(&crate::model::Checkout(pick.root.join(r))))
            .collect();
        let system = crate::tell::how_to_tell(&pick.name, &names);
        self.overlay = None;
        self.config.workspaces.insert(
            pick.name.clone(),
            Workspace {
                root: pick.root,
                repos: ticked,
            },
        );
        let mut effects = vec![Effect::SaveConfig];
        let name = &pick.name;
        if dirs.is_empty() {
            self.set_status(format!("All {already} are open already in {name}"));
            return effects;
        }
        self.set_status(match already {
            0 => format!("Starting {} sessions in {name}", dirs.len()),
            n => format!(
                "Starting {} sessions in {name}; {n} open already",
                dirs.len()
            ),
        });
        effects.push(Effect::OpenWorkspace {
            session: pick.name,
            dirs,
            system,
        });
        effects
    }
}

/// To the folder box, on the folder holding the project in front of you.
fn to_folder(draft: &mut WorkspaceDraft) -> Vec<Effect> {
    let typed = if draft.base == Path::new("~") {
        "~/".to_string()
    } else {
        parent_typed(&draft.base)
    };
    to_folder_at(draft, &typed)
}

fn to_folder_at(draft: &mut WorkspaceDraft, typed: &str) -> Vec<Effect> {
    draft.stage = Stage::Folder(FolderPick::new(typed));
    vec![Effect::ListDirs {
        typed: typed.to_string(),
        base: draft.base.clone(),
    }]
}

fn find(draft: &mut WorkspaceDraft, root: PathBuf) -> Vec<Effect> {
    draft.stage = Stage::Finding {
        asked: root.clone(),
    };
    vec![Effect::FindRepos { root }]
}

/// A name for a new workspace in `root`: the folder's name, made safe for
/// tmux (which takes `.` and `:` as separators), with `-2`, `-3`... when a
/// workspace elsewhere has it already.
fn new_name(root: &Path, saved: &std::collections::BTreeMap<String, Workspace>) -> String {
    let base: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "workspace".to_string()
    } else {
        base
    };
    (1..)
        .map(|i| match i {
            1 => base.clone(),
            i => format!("{base}-{i}"),
        })
        .find(|n| !saved.contains_key(n))
        .expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::{Stage, new_name};
    use crate::app::tests::{agent, ctrl, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::{Config, Workspace};
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    fn repos(asked: &str, root: &str, repos: &[&str]) -> Msg {
        Msg::Repos {
            asked: asked.into(),
            root: root.into(),
            repos: repos.iter().map(PathBuf::from).collect(),
        }
    }

    /// The tmux session and folders an `OpenWorkspace` starts.
    fn opened(effects: &[Effect]) -> (&str, &[PathBuf]) {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::OpenWorkspace { session, dirs, .. } => Some((session.as_str(), &dirs[..])),
                _ => None,
            })
            .expect("an OpenWorkspace")
    }

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        a
    }

    fn stage(a: &App) -> &Stage {
        match &a.overlay {
            Some(Overlay::Workspace(d)) => &d.stage,
            other => panic!("no workspace box: {other:?}"),
        }
    }

    #[test]
    fn with_none_saved_o_opens_on_the_projects_parent_folder() {
        let mut a = app();
        assert_eq!(
            a.update(key('O')),
            [Effect::ListDirs {
                typed: "/w/".into(),
                base: "/w/api".into(),
            }]
        );
        assert!(matches!(stage(&a), Stage::Folder(_)));
    }

    #[test]
    fn a_folder_is_searched_its_repos_ticked_and_opened_and_saved() {
        let mut a = app();
        a.update(key('O'));
        for c in "billing".chars() {
            a.update(key(c));
        }
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            [Effect::FindRepos {
                root: "/w/billing".into()
            }]
        );
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(screen.contains("Looking for git repos"), "{screen}");
        a.update(repos(
            "/w/billing",
            "/w/billing",
            &["api", "web", "tools/cli"],
        ));
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(screen.contains("[x] tools/cli"), "{screen}");
        // Untick web.
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Char(' '));
        let effects = press(&mut a, KeyCode::Enter);
        assert_eq!(effects[0], Effect::SaveConfig);
        assert_eq!(
            opened(&effects),
            (
                "billing",
                &["/w/billing/api".into(), "/w/billing/tools/cli".into()][..]
            )
        );
        let Effect::OpenWorkspace { system, .. } = &effects[1] else {
            unreachable!()
        };
        assert!(
            system.contains("workspace billing") && system.contains("api, cli"),
            "{system}"
        );
        assert_eq!(a.overlay, None);
        assert_eq!(
            a.config.workspaces["billing"],
            Workspace {
                root: "/w/billing".into(),
                repos: vec!["api".into(), "tools/cli".into()],
            }
        );
    }

    #[test]
    fn a_saved_workspace_opens_with_its_ticks_and_skips_sessions_already_open() {
        let mut a = app();
        a.config.workspaces.insert(
            "billing".into(),
            Workspace {
                root: "/w/billing".into(),
                repos: vec!["api".into(), "web".into()],
            },
        );
        let mut open = agent("%5", 1, "api", "/w/billing/api", true);
        open.session = "billing".into();
        a.agents.push(open);
        assert!(
            a.update(key('O')).is_empty(),
            "the saved list needs nothing"
        );
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(
            screen.contains("billing") && screen.contains("Other folder"),
            "{screen}"
        );
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            [Effect::FindRepos {
                root: "/w/billing".into()
            }]
        );
        // A repo added since is found but left unticked.
        a.update(repos("/w/billing", "/w/billing", &["api", "new", "web"]));
        let Stage::Repos(pick) = stage(&a) else {
            panic!("not on the repos");
        };
        let ticks: Vec<bool> = pick.repos.iter().map(|(_, on)| *on).collect();
        assert_eq!(ticks, [true, false, true]);
        assert_eq!(
            opened(&press(&mut a, KeyCode::Enter)),
            ("billing", &["/w/billing/web".into()][..])
        );
        assert!(a.status_text().unwrap().contains("1 open already"));
    }

    #[test]
    fn ctrl_o_and_esc_move_between_the_saved_list_and_the_folder() {
        let mut a = app();
        a.config.workspaces.insert(
            "billing".into(),
            Workspace {
                root: "/w/billing".into(),
                repos: vec![],
            },
        );
        a.update(key('O'));
        a.update(ctrl('o'));
        assert!(matches!(stage(&a), Stage::Folder(_)));
        press(&mut a, KeyCode::Esc);
        assert!(matches!(stage(&a), Stage::Choose { .. }));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn a_folder_without_repos_says_so_and_stays_on_the_folder() {
        let mut a = app();
        a.update(key('O'));
        press(&mut a, KeyCode::Enter);
        a.update(repos("/w", "/w", &[]));
        assert!(matches!(stage(&a), Stage::Folder(_)));
        assert!(a.status_text().unwrap().contains("No git repos in /w"));
    }

    #[test]
    fn a_name_is_safe_for_tmux_and_free() {
        let mut saved = std::collections::BTreeMap::new();
        assert_eq!(new_name(Path::new("/w/my.app"), &saved), "my-app");
        saved.insert(
            "api".to_string(),
            Workspace {
                root: "/x/api".into(),
                repos: vec![],
            },
        );
        assert_eq!(new_name(Path::new("/w/api"), &saved), "api-2");
    }
}
