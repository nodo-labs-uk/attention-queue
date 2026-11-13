use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Checkout, Mode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub modes: BTreeMap<PathBuf, Mode>,
    /// The tour has been finished or skipped once, so it no longer opens
    /// by itself.
    pub tutorial_done: bool,
    /// `f` hid the file list on review and shell change cards.
    pub hide_files: bool,
    /// F3 showed the row of keys along the bottom.
    pub show_keys: bool,
    /// `r`: words a minute when reading fast.
    pub read_wpm: u32,
    /// `O`: sets of repos opened together, by name. Saved by aq when one is
    /// opened, not written by hand.
    pub workspaces: BTreeMap<String, Workspace>,
}

/// A folder and the git repos in it that open together, each a session in
/// a tmux session named after the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub root: PathBuf,
    /// Relative to `root`; `""` is `root` itself.
    pub repos: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "tokyo-night".to_string(),
            modes: BTreeMap::new(),
            tutorial_done: false,
            hide_files: false,
            show_keys: false,
            read_wpm: 350,
            workspaces: BTreeMap::new(),
        }
    }
}

impl Config {
    /// A missing file gives the defaults; any other failure is an error.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serialising config")?;
        write_atomic(path, text.as_bytes())
    }

    pub fn mode_for(&self, checkout: &Checkout) -> Mode {
        self.modes.get(&checkout.0).copied().unwrap_or_default()
    }

    pub fn set_mode(&mut self, checkout: &Checkout, mode: Mode) {
        self.modes.insert(checkout.0.clone(), mode);
    }
}

/// Writes `bytes` to `path` through a temp file in the same directory and a
/// rename, so a crash never leaves a half-written file. Makes the directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating a temp file in {}", dir.display()))?;
    tmp.write_all(bytes)
        .and_then(|()| tmp.as_file().sync_all())
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    tmp.persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub themes_dir: PathBuf,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
}

pub fn paths() -> Paths {
    paths_with(|k| std::env::var(k).ok())
}

/// Same as `paths`, with the environment lookup supplied by the caller.
pub fn paths_with(env: impl Fn(&str) -> Option<String>) -> Paths {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let home = get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let config_dir = get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("aq");
    let state_dir = get("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("aq");
    let socket = match (get("AQ_SOCKET"), get("XDG_RUNTIME_DIR")) {
        (Some(s), _) => PathBuf::from(s),
        (None, Some(r)) => PathBuf::from(r).join("aq.sock"),
        (None, None) => fallback_socket_dir().join("aq.sock"),
    };
    Paths {
        config_file: config_dir.join("config.toml"),
        themes_dir: config_dir.join("themes"),
        state_dir,
        socket,
    }
}

/// `/tmp/aq-$UID`, used when `$XDG_RUNTIME_DIR` is not set.
pub fn fallback_socket_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/aq-{}", current_uid()))
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.theme, "tokyo-night");
        assert_eq!(cfg.mode_for(&Checkout::new("/anything")), Mode::Review);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/config.toml");
        let x = Checkout::new("/tmp/x");
        let mut cfg = Config::default();
        cfg.set_mode(&x, Mode::Step);
        cfg.workspaces.insert(
            "billing".into(),
            Workspace {
                root: "/w/billing".into(),
                repos: vec!["api".into(), "".into()],
            },
        );
        cfg.save(&file).unwrap();
        let loaded = Config::load(&file).unwrap();
        assert_eq!(loaded.mode_for(&x), Mode::Step);
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn save_replaces_the_file_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "theme = \"old\"\n").unwrap();
        Config::default().save(&file).unwrap();
        assert_eq!(Config::load(&file).unwrap(), Config::default());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[test]
    fn paths_follow_xdg() {
        let p = paths_with(env_of(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_RUNTIME_DIR", "/r"),
        ]));
        assert_eq!(p.socket, PathBuf::from("/r/aq.sock"));
        assert_eq!(p.config_file, PathBuf::from("/c/aq/config.toml"));
        assert_eq!(p.themes_dir, PathBuf::from("/c/aq/themes"));
        assert_eq!(p.state_dir, PathBuf::from("/s/aq"));
    }

    #[test]
    fn paths_default_under_home() {
        let p = paths_with(env_of(&[("HOME", "/h"), ("XDG_RUNTIME_DIR", "/r")]));
        assert_eq!(p.config_file, PathBuf::from("/h/.config/aq/config.toml"));
        assert_eq!(p.state_dir, PathBuf::from("/h/.local/state/aq"));
    }

    #[test]
    fn socket_falls_back_to_tmp() {
        let p = paths_with(env_of(&[("HOME", "/h")]));
        assert!(p.socket.to_string_lossy().starts_with("/tmp/aq-"));
        assert!(p.socket.ends_with("aq.sock"));
    }

    #[test]
    fn aq_socket_overrides() {
        let p = paths_with(env_of(&[
            ("XDG_RUNTIME_DIR", "/r"),
            ("AQ_SOCKET", "/x/test.sock"),
        ]));
        assert_eq!(p.socket, PathBuf::from("/x/test.sock"));
    }
}
