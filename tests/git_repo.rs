use std::path::Path;
use std::process::Command;

use aq::git::*;
use aq::model::Checkout;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "T"]);
    git(dir, &["config", "user.email", "t@example.com"]);
}

fn commit_all(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn tree_files(dir: &Path, tree: &str) -> Vec<String> {
    git(dir, &["ls-tree", "-r", "--name-only", tree]).lines().map(String::from).collect()
}

fn check_snapshot(dir: &Path) {
    std::fs::write(dir.join("a.txt"), "a2\n").unwrap();
    git(dir, &["add", "a.txt"]);
    std::fs::write(dir.join("b.txt"), "b2\n").unwrap();
    std::fs::write(dir.join("c.txt"), "new\n").unwrap();
    let c = Checkout::new(dir);
    let tree = snapshot_tree(&c).unwrap();
    assert_eq!(git(dir, &["diff", "--cached", "--name-only"]).trim(), "a.txt");
    assert_eq!(tree_files(dir, &tree), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(file_at(&c, &tree, Path::new("b.txt")).unwrap().as_deref(), Some("b2\n"));
    assert_eq!(file_at(&c, &tree, Path::new("nope.txt")).unwrap(), None);
}

fn seeded_repo() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    init_repo(t.path());
    std::fs::write(t.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(t.path().join("b.txt"), "b\n").unwrap();
    commit_all(t.path(), "init");
    t
}

#[test]
fn snapshot_does_not_touch_user_index() {
    let t = seeded_repo();
    check_snapshot(t.path());
}

#[test]
fn snapshot_works_in_a_worktree() {
    let t = seeded_repo();
    let wt = tempfile::tempdir().unwrap();
    let wt_path = wt.path().join("wt");
    git(t.path(), &["worktree", "add", "-q", "-b", "feat", wt_path.to_str().unwrap()]);
    check_snapshot(&wt_path);
}

#[test]
fn round_two_diff_shows_only_new_change() {
    let t = seeded_repo();
    let dir = t.path();
    let c = Checkout::new(dir);
    std::fs::write(dir.join("a.txt"), "a\nline2\n").unwrap();
    let snap1 = snapshot_tree(&c).unwrap();
    std::fs::write(dir.join("a.txt"), "a\nline2\nline3\n").unwrap();
    let snap2 = snapshot_tree(&c).unwrap();
    let files = diff_trees(&c, &snap1, &snap2).unwrap();
    assert_eq!(files.len(), 1);
    let adds: Vec<_> = files[0]
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l, DiffLine::Add { .. }))
        .collect();
    assert_eq!(adds, [&DiffLine::Add { new: 3, text: "line3".into() }]);
}

#[test]
fn fresh_repo_without_commits_still_diffs() {
    let t = tempfile::tempdir().unwrap();
    init_repo(t.path());
    std::fs::write(t.path().join("x y.txt"), "hi\n").unwrap();
    let c = Checkout::new(t.path());
    let files = branch_diff(&c).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, FileStatus::Added);
    assert_eq!(files[0].new_path.as_deref(), Some(Path::new("x y.txt")));
}

#[test]
fn branch_diff_against_merge_base() {
    let t = seeded_repo();
    let dir = t.path();
    git(dir, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(dir.join("a.txt"), "a\nmore\n").unwrap();
    std::fs::write(dir.join("new.txt"), "n\n").unwrap();
    let c = Checkout::new(dir);
    assert_eq!(default_branch(&c).unwrap(), "main");
    let files = branch_diff(&c).unwrap();
    let names: Vec<_> = files.iter().filter_map(|f| f.new_path.clone()).collect();
    assert_eq!(names, [Path::new("a.txt"), Path::new("new.txt")]);
}
