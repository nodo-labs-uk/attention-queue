use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use super::diff::{FileDiff, parse_unified};
use crate::model::Checkout;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn git_cmd(c: &Checkout) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&c.0).args(["-c", "core.quotePath=false"]);
    // A parent environment (e.g. a Claude session) must not redirect git elsewhere.
    cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE");
    cmd
}

fn run(mut cmd: Command) -> Result<Output> {
    let out = cmd.output().context("running git")?;
    Ok(out)
}

fn run_ok(cmd: Command) -> Result<String> {
    let out = run(cmd)?;
    if !out.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&out.stderr).trim());
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
    let index = if rel.is_absolute() { rel } else { c.0.join(rel) };

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
        bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr).trim());
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
    check.args(["rev-parse", "--verify", "--quiet", &format!("{tree}^{{tree}}")]);
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
        bail!("git cat-file failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}
