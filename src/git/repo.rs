use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use super::diff::{FileDiff, parse_unified};
use crate::model::Checkout;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Settings that make git run programs named in the repository's own config
/// or hooks, which a shell command in the checkout could have planted.
pub const GIT_SAFETY_ARGS: [&str; 6] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.attributesFile=/dev/null",
];

/// Config keys naming programs git runs on file content: filters, and diff
/// drivers' textconv and commands.
const PROGRAM_KEYS: &str = r"^(filter|diff)\..*\.(clean|smudge|process|textconv|command)$";

fn base_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(GIT_SAFETY_ARGS)
        .args(["-c", "core.quotePath=false"]);
    // A parent environment (e.g. a Claude session) must not redirect git elsewhere.
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

/// The configured keys that name programs, read afresh each time so a
/// setting planted since the last call is still blanked.
fn program_keys(dir: &Path) -> Vec<String> {
    let mut cmd = base_cmd(dir);
    cmd.args(["config", "--get-regexp", PROGRAM_KEYS]);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// A git command in `dir` that runs no repository-configured programs (no
/// fsmonitor, hooks, filters or textconv) and is not redirected by the
/// parent environment.
pub fn git_in(dir: &Path) -> Command {
    let mut cmd = base_cmd(dir);
    for key in program_keys(dir) {
        cmd.arg("-c").arg(format!("{key}="));
    }
    cmd
}

fn git_cmd(c: &Checkout) -> Command {
    git_in(&c.0)
}

/// Longest file read for the guard.
const GUARD_READ: u64 = 1 << 20;

/// What a guarded path is, as a hash: a link's target text, a regular file's
/// size and first MiB, or just the kind of anything else (never opened).
fn guard_hash(path: &Path) -> Option<u64> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::io::Read;
    use std::os::unix::fs::FileTypeExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let ty = meta.file_type();
    let mut h = DefaultHasher::new();
    if ty.is_symlink() {
        ("link", std::fs::read_link(path).ok()?).hash(&mut h);
    } else if ty.is_file() {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(GUARD_READ)
            .read_to_end(&mut bytes)
            .ok()?;
        ("file", meta.len(), bytes).hash(&mut h);
    } else {
        let kind = if ty.is_fifo() {
            "fifo"
        } else if ty.is_socket() {
            "socket"
        } else if ty.is_block_device() || ty.is_char_device() {
            "device"
        } else {
            "other"
        };
        kind.hash(&mut h);
    }
    Some(h.finish())
}

/// Every file under `dir`, links not followed.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        if is_dir {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Hashes of the files that can make git (or a later `git commit`) run
/// programs: every config file git reads, and everything under hooks and
/// info in the git directories. Keys are paths relative to their git
/// directory, or absolute for files elsewhere (a global config).
pub fn git_guard(c: &Checkout) -> Result<std::collections::BTreeMap<String, u64>> {
    let mut dirs = Vec::new();
    for flag in ["--absolute-git-dir", "--git-common-dir"] {
        let mut cmd = git_cmd(c);
        cmd.args(["rev-parse", flag]);
        let dir = PathBuf::from(run_ok(cmd)?);
        let dir = if dir.is_absolute() {
            dir
        } else {
            c.0.join(dir)
        };
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let mut files = Vec::new();
    for dir in &dirs {
        files.push(dir.join("config"));
        files.push(dir.join("config.worktree"));
        for sub in ["hooks", "info"] {
            walk(&dir.join(sub), &mut files);
        }
    }
    // Entries are `origin NUL key NL value NUL`; only file origins count.
    let mut list = git_cmd(c);
    list.args(["config", "--list", "--show-origin", "--includes", "-z"]);
    let listed = run(list)?;
    for origin in listed.stdout.split(|&b| b == 0).step_by(2) {
        if let Some(path) = origin.strip_prefix(b"file:") {
            let path = PathBuf::from(String::from_utf8_lossy(path).into_owned());
            files.push(if path.is_absolute() {
                path
            } else {
                c.0.join(path)
            });
        }
    }
    let mut out = std::collections::BTreeMap::new();
    for file in files {
        let Some(hash) = guard_hash(&file) else {
            continue;
        };
        let name = dirs
            .iter()
            .find_map(|d| file.strip_prefix(d).ok())
            .unwrap_or(&file);
        out.insert(name.display().to_string(), hash);
    }
    Ok(out)
}

fn run(mut cmd: Command) -> Result<Output> {
    let out = cmd.output().context("running git")?;
    Ok(out)
}

fn run_ok(cmd: Command) -> Result<String> {
    let out = run(cmd)?;
    if !out.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn ref_exists(c: &Checkout, r: &str) -> bool {
    let mut cmd = git_cmd(c);
    cmd.args(["rev-parse", "--verify", "--quiet", r]);
    run(cmd).map(|o| o.status.success()).unwrap_or(false)
}

/// `origin/HEAD` target if set, else `main`, else `master`.
pub fn default_branch(c: &Checkout) -> Result<String> {
    let mut cmd = git_cmd(c);
    cmd.args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]);
    if let Ok(out) = run(cmd)
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return Ok(name);
        }
    }
    for name in ["main", "master"] {
        if ref_exists(c, &format!("refs/heads/{name}")) {
            return Ok(name.to_string());
        }
    }
    bail!("no default branch found")
}

/// Merge-base of HEAD and the default branch. Falls back to the empty tree when
/// there is no default branch or no commits yet, so a brand-new repo still diffs.
pub fn base_tree(c: &Checkout) -> Result<String> {
    if !ref_exists(c, "HEAD") {
        return Ok(EMPTY_TREE.to_string());
    }
    let Ok(branch) = default_branch(c) else {
        return Ok(EMPTY_TREE.to_string());
    };
    let mut cmd = git_cmd(c);
    cmd.args(["merge-base", "HEAD", &branch]);
    Ok(run_ok(cmd).unwrap_or_else(|_| EMPTY_TREE.to_string()))
}

/// Writes a tree of the working directory (tracked, modified and untracked files)
/// without touching the user's index.
pub fn snapshot_tree(c: &Checkout) -> Result<String> {
    let mut cmd = git_cmd(c);
    cmd.args(["rev-parse", "--git-path", "index"]);
    let rel = PathBuf::from(run_ok(cmd)?);
    let index = if rel.is_absolute() {
        rel
    } else {
        c.0.join(rel)
    };

    let dir = tempfile::tempdir().context("creating temp dir")?;
    let tmp_index = dir.path().join("index");
    if index.exists() {
        std::fs::copy(&index, &tmp_index).context("copying index")?;
    }

    let mut add = git_cmd(c);
    add.args(["add", "-A"]).env("GIT_INDEX_FILE", &tmp_index);
    run_ok(add)?;
    let mut write = git_cmd(c);
    write.arg("write-tree").env("GIT_INDEX_FILE", &tmp_index);
    run_ok(write)
}

pub fn diff_trees(c: &Checkout, from: &str, to: &str) -> Result<Vec<FileDiff>> {
    let mut cmd = git_cmd(c);
    cmd.args([
        "diff",
        "--no-textconv",
        "--no-color",
        "--no-ext-diff",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "-M",
        from,
        to,
    ]);
    let out = run(cmd)?;
    if !out.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_unified(&out.stdout))
}

pub fn branch_diff(c: &Checkout) -> Result<Vec<FileDiff>> {
    let base = base_tree(c)?;
    let snap = snapshot_tree(c)?;
    diff_trees(c, &base, &snap)
}

/// File contents at `tree`, or `None` if the path does not exist there.
/// Any other git failure (bad tree id, corrupt object) is an error.
pub fn file_at(c: &Checkout, tree: &str, path: &Path) -> Result<Option<String>> {
    let mut check = git_cmd(c);
    check.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{tree}^{{tree}}"),
    ]);
    run_ok(check).with_context(|| format!("not a tree: {tree}"))?;

    let mut ls = git_cmd(c);
    ls.args(["ls-tree", tree, "--"]).arg(path);
    let listing = run_ok(ls)?;
    // "<mode> <type> <sha>\t<name>"
    let Some((meta, _)) = listing.split_once('\t') else {
        return Ok(None);
    };
    let mut parts = meta.split(' ');
    let (Some(_), Some("blob"), Some(sha)) = (parts.next(), parts.next(), parts.next()) else {
        return Ok(None);
    };
    let mut cat = git_cmd(c);
    cat.args(["cat-file", "blob", sha]);
    let out = run(cat)?;
    if !out.status.success() {
        bail!(
            "git cat-file failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// One `git grep` match. `path` is relative to the checkout root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
}

/// At most this many matches come back from `grep_word`.
const GREP_MAX: usize = 200;

/// Tracked files' lines containing `word` as a whole word.
pub fn grep_word(c: &Checkout, word: &str) -> Result<Vec<GrepHit>> {
    let mut cmd = git_cmd(c);
    cmd.args(["grep", "-n", "-w", "-z", "-I", "--no-color", "-e", word]);
    let out = run(cmd)?;
    // Exit 1 means no match.
    if !out.status.success() && out.status.code() != Some(1) {
        bail!(
            "git grep failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_grep(&String::from_utf8_lossy(&out.stdout)))
}

/// `git grep -n -z` output: `path\0line\0text` per line.
fn parse_grep(out: &str) -> Vec<GrepHit> {
    out.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\0');
            Some(GrepHit {
                path: PathBuf::from(parts.next()?),
                line: parts.next()?.parse().ok()?,
                text: parts.next()?.to_string(),
            })
        })
        .take(GREP_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_commands_disable_fsmonitor_and_hooks() {
        let cmd = git_in(Path::new("/x"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
        for pair in [
            ["-c", "core.fsmonitor=false"],
            ["-c", "core.hooksPath=/dev/null"],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{args:?}");
        }
    }

    fn init(dir: &Path) {
        let out = git_in(dir).args(["init", "-q"]).output().unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn planted_filters_are_not_run_and_content_is_unfiltered() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        init(dir.path());
        let clean = dir.path().join("clean.sh");
        std::fs::write(
            &clean,
            format!("#!/bin/sh\ntouch {}\ntr a-z A-Z\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&clean, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut cfg = std::fs::read_to_string(dir.path().join(".git/config")).unwrap();
        cfg.push_str(&format!(
            "[filter \"x\"]\n\tclean = {}\n[diff \"y\"]\n\ttextconv = {}\n",
            clean.display(),
            clean.display()
        ));
        std::fs::write(dir.path().join(".git/config"), cfg).unwrap();
        std::fs::write(dir.path().join(".gitattributes"), "* filter=x diff=y\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let c = Checkout::new(dir.path());
        let tree = snapshot_tree(&c).unwrap();
        assert!(!marker.exists(), "filter ran");
        let text = file_at(&c, &tree, Path::new("a.txt")).unwrap();
        assert_eq!(text.as_deref(), Some("hello\n"));
        diff_trees(&c, EMPTY_TREE, &tree).unwrap();
        assert!(!marker.exists(), "textconv ran");
    }

    #[test]
    fn guard_sees_nested_hooks_and_does_not_open_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        let c = Checkout::new(dir.path());
        let before = git_guard(&c).unwrap();
        let hooks = dir.path().join(".git/hooks");
        std::fs::create_dir_all(hooks.join("sub")).unwrap();
        std::fs::write(hooks.join("sub/post-commit"), "x").unwrap();
        let fifo = hooks.join("pre-push");
        let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        // Reading a FIFO with no writer would block for ever.
        let after = git_guard(&c).unwrap();
        assert!(after.contains_key("hooks/sub/post-commit"), "{after:?}");
        assert!(after.contains_key("hooks/pre-push"), "{after:?}");
        assert!(before.contains_key("config"));
        assert!(!before.contains_key("hooks/pre-push"));
    }

    #[test]
    fn guard_includes_every_file_git_reads_for_config() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        std::fs::write(dir.path().join("extra.cfg"), "[core]\n\tpager = x\n").unwrap();
        let out = git_in(dir.path())
            .args(["config", "include.path", "../extra.cfg"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let c = Checkout::new(dir.path());
        let guard = git_guard(&c).unwrap();
        assert!(guard.keys().any(|k| k.ends_with("extra.cfg")), "{guard:?}");
    }

    #[test]
    fn planted_fsmonitor_is_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let sh = |args: &[&str]| {
            let out = git_in(dir.path()).args(args).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        sh(&["init", "-q"]);
        let hook = dir.path().join("mon.sh");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        // Written straight to the file, as a shell command could.
        let mut cfg = std::fs::read_to_string(dir.path().join(".git/config")).unwrap();
        cfg.push_str(&format!("[core]\n\tfsmonitor = {}\n", hook.display()));
        std::fs::write(dir.path().join(".git/config"), cfg).unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        snapshot_tree(&Checkout::new(dir.path())).unwrap();
        assert!(!marker.exists());
    }

    #[test]
    fn grep_output_keeps_colons_in_paths_and_text() {
        let hits = parse_grep("a:b.rs\x0012\x00let x: u32 = 1;\nsrc/c.rs\x003\x00x\n");
        assert_eq!(
            hits,
            [
                GrepHit {
                    path: "a:b.rs".into(),
                    line: 12,
                    text: "let x: u32 = 1;".into()
                },
                GrepHit {
                    path: "src/c.rs".into(),
                    line: 3,
                    text: "x".into()
                },
            ]
        );
    }
}
