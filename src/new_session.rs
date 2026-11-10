//! `N`: a new Claude session, in the project in front of you or, after
//! Ctrl-O, in a folder you pick or name. Pure: no I/O.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Effect, Overlay};
use crate::ui::input::{InputEvent, TextInput};

/// What a new Claude session should start on, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionDraft {
    /// Where it starts. May begin with `~`, which the runtime expands.
    pub dir: PathBuf,
    pub name: String,
    /// `dir` was typed and is not there yet: it is made on start.
    pub new: bool,
    /// Tab: start it in a git worktree of its own.
    pub worktree: bool,
    pub input: TextInput,
    /// Ctrl-O: choosing another folder.
    pub folder: Option<FolderPick>,
}

impl NewSessionDraft {
    pub fn new(dir: PathBuf, name: String) -> Self {
        NewSessionDraft {
            dir,
            name,
            new: false,
            worktree: false,
            input: TextInput::single_line(),
            folder: None,
        }
    }
}

/// The folder being typed, with the folders that match it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPick {
    pub input: TextInput,
    /// The last listing to arrive; it may be for text since changed.
    pub listing: Option<DirListing>,
    /// The match chosen with the arrows; none until one is pressed.
    pub cursor: Option<usize>,
}

impl FolderPick {
    /// The listing, when it is for the text as it stands.
    pub fn listing(&self) -> Option<&DirListing> {
        self.listing
            .as_ref()
            .filter(|l| l.typed == self.input.text())
    }
}

/// What the runtime found for a typed folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    pub typed: String,
    /// Folders beside the last part typed whose names start with it.
    pub names: Vec<String>,
    /// The text as typed is a folder already.
    pub exists: bool,
}

/// `typed` as a path: as written when absolute or under `~`, else inside
/// `base`.
pub fn resolve(typed: &str, base: &Path) -> PathBuf {
    if typed.starts_with('/') || typed == "~" || typed.starts_with("~/") {
        PathBuf::from(typed)
    } else {
        base.join(typed)
    }
}

/// `typed` with its last part replaced by `name`, ready for the next part.
fn completed(typed: &str, name: &str) -> String {
    let keep = typed.rfind('/').map_or(0, |i| i + 1);
    format!("{}{name}/", &typed[..keep])
}

impl App {
    /// `N`: the box for a new session's first prompt.
    pub(crate) fn open_new_session(&mut self, dir: PathBuf, name: String) {
        self.overlay = Some(Overlay::NewSession(NewSessionDraft::new(dir, name)));
    }

    /// Enter starts the session, with the text as its first prompt if any;
    /// Tab puts it in a worktree; Ctrl-O picks its folder; Esc keeps things
    /// as they were.
    pub(crate) fn on_new_session_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::NewSession(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        if draft.folder.is_some() {
            return on_folder_key(draft, key);
        }
        if key.code == KeyCode::Tab {
            draft.worktree = !draft.worktree;
            return Vec::new();
        }
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // The folder that holds this project: its neighbours are the
            // likeliest pick.
            let typed = match draft.dir.parent() {
                Some(p) if p != Path::new("/") => format!("{}/", p.display()),
                _ => "/".to_string(),
            };
            draft.folder = Some(FolderPick {
                input: TextInput::single_line_with(&typed),
                listing: None,
                cursor: None,
            });
            return vec![Effect::ListDirs {
                typed,
                base: draft.dir.clone(),
            }];
        }
        let text = match draft.input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => None,
            InputEvent::Submit(text) => Some(text),
        };
        let Some(Overlay::NewSession(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        let Some(text) = text else {
            return Vec::new();
        };
        let place = match (draft.worktree, draft.new) {
            (true, _) => "a new worktree of ",
            (false, true) => "the new folder ",
            (false, false) => "",
        };
        self.set_status(format!("Started a Claude session in {place}{}", draft.name));
        let prompt = Some(text.trim().to_string()).filter(|t| !t.is_empty());
        vec![Effect::NewSession {
            dir: draft.dir,
            prompt,
            worktree: draft.worktree,
        }]
    }

    /// The folders matching what is typed, from the runtime.
    pub(crate) fn on_dirs(&mut self, listing: DirListing) {
        if let Some(Overlay::NewSession(NewSessionDraft {
            folder: Some(pick), ..
        })) = &mut self.overlay
        {
            pick.listing = Some(listing);
        }
    }
}

/// Keys while picking the folder: Tab completes the chosen match (the
/// first, until the arrows choose another); Enter takes the chosen match,
/// or the text as typed; Esc goes back to the prompt with the folder as it
/// was.
fn on_folder_key(draft: &mut NewSessionDraft, key: KeyEvent) -> Vec<Effect> {
    let Some(pick) = draft.folder.as_mut() else {
        return Vec::new();
    };
    let before = pick.input.text();
    let names = pick.listing().map_or_else(Vec::new, |l| l.names.clone());
    match key.code {
        KeyCode::Tab => {
            if let Some(name) = names.get(pick.cursor.unwrap_or(0)) {
                pick.input = TextInput::single_line_with(&completed(&before, name));
            }
        }
        KeyCode::Up => {
            pick.cursor = Some(pick.cursor.unwrap_or(0).saturating_sub(1));
            return Vec::new();
        }
        KeyCode::Down => {
            let next = pick.cursor.map_or(0, |c| c + 1);
            pick.cursor = Some(next.min(names.len().saturating_sub(1)));
            return Vec::new();
        }
        _ => match pick.input.handle_key(key) {
            InputEvent::Editing => {}
            InputEvent::Cancel => {
                draft.folder = None;
                return Vec::new();
            }
            InputEvent::Submit(text) => {
                let chosen = pick.cursor.and_then(|c| names.get(c));
                let (typed, new) = match chosen {
                    Some(name) => (completed(&text, name), false),
                    None => (text, pick.listing().is_some_and(|l| !l.exists)),
                };
                draft.folder = None;
                let typed = typed.trim();
                let typed = match typed.trim_end_matches('/') {
                    "" if typed.starts_with('/') => "/",
                    t => t,
                };
                if !typed.is_empty() {
                    draft.dir = resolve(typed, &draft.dir);
                    draft.name = draft
                        .dir
                        .file_name()
                        .map_or_else(|| typed.to_string(), |n| n.to_string_lossy().into_owned());
                    draft.new = new;
                }
                return Vec::new();
            }
        },
    }
    let typed = pick.input.text();
    if typed == before {
        return Vec::new();
    }
    pick.cursor = None;
    vec![Effect::ListDirs {
        typed,
        base: draft.dir.clone(),
    }]
}

#[cfg(test)]
mod tests {
    use super::{DirListing, resolve};
    use crate::app::tests::{agent, ctrl, key};
    use crate::app::{App, Effect, Msg, Overlay};
    use crate::config::Config;
    use crate::theme::builtin_themes;
    use crossterm::event::{KeyCode, KeyEvent};
    use std::path::Path;
    use std::time::Instant;

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Msg::Key(KeyEvent::from(code), Instant::now()))
    }

    fn typed(a: &App) -> String {
        match &a.overlay {
            Some(Overlay::NewSession(d)) => d.folder.as_ref().unwrap().input.text(),
            other => panic!("no folder box: {other:?}"),
        }
    }

    fn listing(typed: &str, names: &[&str], exists: bool) -> Msg {
        Msg::Dirs(DirListing {
            typed: typed.into(),
            names: names.iter().map(|n| n.to_string()).collect(),
            exists,
        })
    }

    fn app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        a.update(key('N'));
        a
    }

    #[test]
    fn ctrl_o_picks_a_neighbouring_folder_with_tab_and_the_arrows() {
        let mut a = app();
        let list = |typed: &str| {
            vec![Effect::ListDirs {
                typed: typed.into(),
                base: "/w/api".into(),
            }]
        };
        // It opens on the folder holding the project.
        assert_eq!(a.update(ctrl('o')), list("/w/"));
        a.update(listing("/w/", &["api", "web", "worker"], true));
        // Typing narrows; a listing for older text is not used.
        assert_eq!(a.update(key('w')), list("/w/w"));
        assert!(press(&mut a, KeyCode::Tab).is_empty(), "stale listing");
        a.update(listing("/w/w", &["web", "worker"], false));
        // Tab completes the first match, the arrows choose another.
        assert_eq!(press(&mut a, KeyCode::Tab), list("/w/web/"));
        assert_eq!(typed(&a), "/w/web/");
        a.update(listing("/w/web/", &["src", "tests"], true));
        assert!(press(&mut a, KeyCode::Down).is_empty());
        assert!(press(&mut a, KeyCode::Down).is_empty());
        assert!(press(&mut a, KeyCode::Down).is_empty(), "stops at the last");
        // Enter takes the chosen match and goes back to the prompt.
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        let Some(Overlay::NewSession(d)) = &a.overlay else {
            panic!("the box closed");
        };
        assert_eq!(d.folder, None);
        assert_eq!(
            (d.dir.as_path(), d.name.as_str()),
            (Path::new("/w/web/tests"), "tests")
        );
        assert!(!d.new);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            [Effect::NewSession {
                dir: "/w/web/tests".into(),
                prompt: None,
                worktree: false,
            }]
        );
    }

    #[test]
    fn a_folder_that_is_not_there_is_named_and_made_on_start() {
        let mut a = app();
        a.update(ctrl('o'));
        for c in "scratch/".chars() {
            a.update(key(c));
        }
        a.update(listing("/w/scratch/", &[], false));
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        let Some(Overlay::NewSession(d)) = &a.overlay else {
            panic!("the box closed");
        };
        assert_eq!(d.dir, Path::new("/w/scratch"));
        assert!(d.new);
        for c in "try it".chars() {
            a.update(key(c));
        }
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            [Effect::NewSession {
                dir: "/w/scratch".into(),
                prompt: Some("try it".into()),
                worktree: false,
            }]
        );
        assert!(a.status_text().unwrap().contains("the new folder scratch"));
    }

    #[test]
    fn each_box_names_its_keys_along_its_bottom() {
        let mut a = app();
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(
            screen.contains("Enter start  Ctrl-O other folder  Tab own worktree  Esc cancel"),
            "{screen}"
        );
        a.update(ctrl('o'));
        let screen = crate::ui::tests::render(&a, 120, 30);
        assert!(
            screen.contains("Enter use it  Tab complete  ↑/↓ choose  Esc cancel"),
            "{screen}"
        );
    }

    #[test]
    fn esc_in_the_folder_box_keeps_the_folder_and_the_prompt() {
        let mut a = app();
        a.update(key('x'));
        a.update(ctrl('o'));
        a.update(key('q'));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        let Some(Overlay::NewSession(d)) = &a.overlay else {
            panic!("the box closed");
        };
        assert_eq!(
            (d.dir.as_path(), d.input.text().as_str()),
            (Path::new("/w/api"), "x")
        );
    }

    #[test]
    fn typed_folders_are_absolute_under_home_or_inside_the_project() {
        let base = Path::new("/w/api");
        assert_eq!(resolve("/srv/x", base), Path::new("/srv/x"));
        assert_eq!(resolve("~/code", base), Path::new("~/code"));
        assert_eq!(resolve("~", base), Path::new("~"));
        assert_eq!(resolve("sub/dir", base), Path::new("/w/api/sub/dir"));
        assert_eq!(resolve("~x", base), Path::new("/w/api/~x"));
    }
}
