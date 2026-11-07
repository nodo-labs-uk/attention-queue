use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
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
    // Nor plant config: `GIT_CONFIG_PARAMETERS` (from `git -c`) is read after
    // `GIT_CONFIG_COUNT`, so it would override the blanking `git_in` sets.
    cmd.env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT");
    for (key, _) in std::env::vars_os() {
        let k = key.as_encoded_bytes();
        if k.starts_with(b"GIT_CONFIG_KEY_") || k.starts_with(b"GIT_CONFIG_VALUE_") {
            cmd.env_remove(&key);
        }
    }
    // Attributes come from no tree and no system file, which switches off
    // filters set by a committed .gitattributes. (.git/info/attributes is
    // still read, so the blanking below stays.)
    cmd.env("GIT_ATTR_SOURCE", EMPTY_TREE)
        .env("GIT_ATTR_NOSYSTEM", "1");
    cmd
}

/// The configured keys that name programs, read afresh each time so a
/// setting planted since the last call is still blanked.
fn program_keys(dir: &Path) -> Result<Vec<Vec<u8>>> {
    let mut cmd = base_cmd(dir);
    cmd.args(["config", "-z", "--get-regexp", PROGRAM_KEYS]);
    let out = cmd.output().context("running git config")?;
    // Exit 1 with nothing printed means no key matched.
    let none = out.status.code() == Some(1) && out.stdout.is_empty();
    if !out.status.success() && !none {
        bail!(
            "git config failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // Records are `key NL value NUL`. A key may hold spaces, `=` or bytes
    // that are not UTF-8, so it stays raw bytes.
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter_map(|rec| rec.split(|&b| b == b'\n').next())
        .filter(|k| !k.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// `GIT_CONFIG_*` settings that blank each program key, and turn off
/// `required` for each filter named by one.
fn blanking(keys: Vec<Vec<u8>>) -> Vec<(Vec<u8>, &'static str)> {
    let mut settings: Vec<(Vec<u8>, &'static str)> = Vec::new();
    for key in keys {
        let name = key
            .strip_prefix(b"filter.")
            .and_then(|k| k.iter().rposition(|&b| b == b'.').map(|i| &k[..i]));
        if let Some(name) = name {
            let mut required = b"filter.".to_vec();
            required.extend_from_slice(name);
            required.extend_from_slice(b".required");
            if !settings.iter().any(|(k, _)| *k == required) {
                settings.push((required, "false"));
            }
        }
        settings.push((key, ""));
    }
    settings
}

/// A git command in `dir` that runs no repository-configured programs (no
/// fsmonitor, hooks, filters or textconv) and is not redirected by the
/// parent environment. The blanking goes through the environment, which
/// does not split keys at `=` the way `-c` does.
///
/// Fails if the config cannot be read, so nothing runs without the blanking.
pub fn git_in(dir: &Path) -> Result<Command> {
    let mut cmd = base_cmd(dir);
    let settings = blanking(program_keys(dir)?);
    cmd.env("GIT_CONFIG_COUNT", settings.len().to_string());
    for (i, (key, value)) in settings.into_iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), OsString::from_vec(key));
        cmd.env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
    Ok(cmd)
}

fn git_cmd(c: &Checkout) -> Result<Command> {
    git_in(&c.0)
}

/// What a guarded path is, as a hash: a link's target text, a regular file's
/// size and whole content, or just the kind of anything else (never opened).
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
        let mut file = std::fs::File::open(path).ok()?;
        ("file", meta.len()).hash(&mut h);
        // Streamed, so memory stays bounded however large the file.
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            h.write(&chunk[..n]);
        }
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
        let mut cmd = git_cmd(c)?;
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
    let mut list = git_cmd(c)?;
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
    let Ok(mut cmd) = git_cmd(c) else {
        return false;
    };
    cmd.args(["rev-parse", "--verify", "--quiet", r]);
    run(cmd).map(|o| o.status.success()).unwrap_or(false)
}

/// `origin/HEAD` target if set, else `main`, else `master`.
pub fn default_branch(c: &Checkout) -> Result<String> {
    let mut cmd = git_cmd(c)?;
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
    let mut cmd = git_cmd(c)?;
    cmd.args(["merge-base", "HEAD", &branch]);
    Ok(run_ok(cmd).unwrap_or_else(|_| EMPTY_TREE.to_string()))
}

/// Writes a tree of the working directory (tracked, modified and untracked files)
/// without touching the user's index.
pub fn snapshot_tree(c: &Checkout) -> Result<String> {
    let mut cmd = git_cmd(c)?;
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

    let mut add = git_cmd(c)?;
    add.args(["add", "-A"]).env("GIT_INDEX_FILE", &tmp_index);
    run_ok(add)?;
    let mut write = git_cmd(c)?;
    write.arg("write-tree").env("GIT_INDEX_FILE", &tmp_index);
    run_ok(write)
}

pub fn diff_trees(c: &Checkout, from: &str, to: &str) -> Result<Vec<FileDiff>> {
    let mut cmd = git_cmd(c)?;
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

/// File contents at `tree`, or `None` if the path does not exist there.
/// Any other git failure (bad tree id, corrupt object) is an error.
pub fn file_at(c: &Checkout, tree: &str, path: &Path) -> Result<Option<String>> {
    let mut check = git_cmd(c)?;
    check.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{tree}^{{tree}}"),
    ]);
    run_ok(check).with_context(|| format!("not a tree: {tree}"))?;

    let mut ls = git_cmd(c)?;
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
    let mut cat = git_cmd(c)?;
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
    let mut cmd = git_cmd(c)?;
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
        let dir = tempfile::tempdir().unwrap();
        let cmd = git_in(dir.path()).unwrap();
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
        for pair in [
            ["-c", "core.fsmonitor=false"],
            ["-c", "core.hooksPath=/dev/null"],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{args:?}");
        }
    }

    fn init(dir: &Path) {
        let out = git_in(dir).unwrap().args(["init", "-q"]).output().unwrap();
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

    /// A filter whose name holds `=` or a space still must not run.
    #[test]
    fn filters_with_odd_names_are_not_run() {
        for name in ["a=b", "a b"] {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("ran");
            init(dir.path());
            let clean = dir.path().join("clean.sh");
            std::fs::write(
                &clean,
                format!("#!/bin/sh\ntouch {}\ncat\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&clean, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let mut cfg = std::fs::read_to_string(dir.path().join(".git/config")).unwrap();
            cfg.push_str(&format!(
                "[filter \"{name}\"]\n\tclean = {}\n\trequired = true\n",
                clean.display()
            ));
            std::fs::write(dir.path().join(".git/config"), cfg).unwrap();
            // Attribute values cannot hold spaces; the name is what git looks up.
            let attr = name.replace(' ', "");
            std::fs::write(
                dir.path().join(".gitattributes"),
                format!("* filter={attr}\n"),
            )
            .unwrap();
            std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
            let c = Checkout::new(dir.path());
            let tree = snapshot_tree(&c).unwrap();
            assert!(!marker.exists(), "filter {name:?} ran");
            let text = file_at(&c, &tree, Path::new("a.txt")).unwrap();
            assert_eq!(text.as_deref(), Some("hello\n"));
        }
    }

    /// A filter planted through the parent's `GIT_CONFIG_PARAMETERS` (what
    /// `git -c` passes down) or `GIT_CONFIG_COUNT` must not run either. The
    /// variables have to be in this process's own environment, so the check
    /// runs in a copy of this test binary started with them set.
    #[test]
    fn filter_from_the_parent_environment_is_not_run() {
        const CHILD: &str = "AQ_TEST_PARENT_GIT_ENV";
        if let Some(dir) = std::env::var_os(CHILD) {
            let c = Checkout::new(PathBuf::from(dir));
            let tree = snapshot_tree(&c).unwrap();
            let text = file_at(&c, &tree, Path::new("a.txt")).unwrap();
            assert_eq!(text.as_deref(), Some("hello\n"));
            return;
        }
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
        // aq reads attributes from no tree, but still from .git/info.
        std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
        std::fs::write(dir.path().join(".git/info/attributes"), "* filter=x\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let planted = [
            (
                "GIT_CONFIG_PARAMETERS",
                format!("'filter.x.clean'='{}'", clean.display()),
            ),
            ("GIT_CONFIG_COUNT", "1".to_string()),
            ("GIT_CONFIG_KEY_0", "filter.x.required".to_string()),
            ("GIT_CONFIG_VALUE_0", "true".to_string()),
        ];
        // Plain git does run it, so the check below means something.
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["hash-object", "a.txt"])
            .envs(planted.iter().map(|(k, v)| (k, v)))
            .output()
            .unwrap();
        assert!(out.status.success() && marker.exists(), "{out:?}");
        std::fs::remove_file(&marker).unwrap();

        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::repo::tests::filter_from_the_parent_environment_is_not_run",
            ])
            .env(CHILD, dir.path())
            .envs(planted.iter().map(|(k, v)| (k, v)))
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{log}");
        assert!(log.contains("1 passed"), "{log}");
        assert!(!marker.exists(), "filter ran");
    }

    #[test]
    fn unreadable_config_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        std::fs::write(
            dir.path().join(".git/config"),
            "[core\nthis is not config\n",
        )
        .unwrap();
        assert!(git_in(dir.path()).is_err());
        let c = Checkout::new(dir.path());
        assert!(snapshot_tree(&c).is_err(), "nothing may run unblanked");
        assert!(crate::agents::checkout_of(dir.path()).is_none());
    }

    #[test]
    fn blanked_keys_keep_their_raw_bytes() {
        let key = b"filter.a\xffb=c d.clean".to_vec();
        let settings = blanking(vec![key.clone()]);
        assert_eq!(
            settings,
            [(b"filter.a\xffb=c d.required".to_vec(), "false"), (key, "")]
        );
    }

    #[test]
    fn filter_named_with_a_non_utf8_byte_is_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        init(dir.path());
        let clean = dir.path().join("clean.sh");
        std::fs::write(
            &clean,
            format!("#!/bin/sh\ntouch {}\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&clean, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut cfg = std::fs::read(dir.path().join(".git/config")).unwrap();
        cfg.extend_from_slice(b"[filter \"a\xffb\"]\n\tclean = ");
        cfg.extend_from_slice(clean.to_string_lossy().as_bytes());
        cfg.push(b'\n');
        std::fs::write(dir.path().join(".git/config"), cfg).unwrap();
        let mut attrs = b"* filter=a\xffb\n".to_vec();
        attrs.push(b'\n');
        std::fs::write(dir.path().join(".gitattributes"), attrs).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        snapshot_tree(&Checkout::new(dir.path())).unwrap();
        assert!(!marker.exists(), "filter ran");
    }

    /// What `GIT_ATTR_SOURCE` switches off by itself, and what the blanking
    /// must still cover: `.git/info/attributes` is read whatever the source.
    #[test]
    fn attribute_source_alone_leaves_info_attributes_so_blanking_stays() {
        for (where_, committed) in [(".gitattributes", true), (".git/info/attributes", false)] {
            let dir = tempfile::tempdir().unwrap();
            let marker = dir.path().join("ran");
            init(dir.path());
            let cfg = format!("touch {}; cat", marker.display());
            let set = base_cmd(dir.path())
                .args(["config", "filter.x.clean", &cfg])
                .output()
                .unwrap();
            assert!(set.status.success());
            std::fs::write(dir.path().join(where_), "* filter=x\n").unwrap();
            std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
            // Only the attribute source, no blanking.
            let out = base_cmd(dir.path()).args(["add", "-A"]).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(marker.exists(), !committed, "{where_}");
            std::fs::remove_file(&marker).ok();
            // With the blanking, neither runs.
            snapshot_tree(&Checkout::new(dir.path())).unwrap();
            assert!(!marker.exists(), "{where_} ran under blanking");
        }
    }

    #[test]
    fn guard_hashes_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        let c = Checkout::new(dir.path());
        let hook = dir.path().join(".git/hooks/big");
        let mut data = vec![b'a'; 2 << 20];
        std::fs::write(&hook, &data).unwrap();
        let before = git_guard(&c).unwrap();
        // Same size, same first MiB; only the tail differs.
        *data.last_mut().unwrap() = b'b';
        std::fs::write(&hook, &data).unwrap();
        let after = git_guard(&c).unwrap();
        assert_ne!(before["hooks/big"], after["hooks/big"]);
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
            .unwrap()
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
            let out = git_in(dir.path()).unwrap().args(args).output().unwrap();
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
