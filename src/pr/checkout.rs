//! Preparing the local folder a PR is reviewed in.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::PrInfo;
use crate::flag::facts::{LONG_TIMEOUT, RunOpts, Runner};

/// The folder a PR is reviewed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheckout {
    pub dir: PathBuf,
    /// The main clone the folder belongs to.
    pub main: PathBuf,
    /// True if the folder is a worktree that aq made.
    pub made: bool,
    /// `origin/<baseRefName>`
    pub base_ref: String,
}

fn long() -> RunOpts<'static> {
    RunOpts {
        timeout: LONG_TIMEOUT,
        stdin: None,
    }
}

/// `<state>/pr/<owner>-<repo>-<number>`
pub fn worktree_dir(state_dir: &Path, repo: &str, number: u64) -> PathBuf {
    state_dir
        .join("pr")
        .join(format!("{}-{number}", repo.replace('/', "-")))
}

/// The main (non-bare) clone that `any_checkout` belongs to.
pub fn main_clone(r: &dyn Runner, any_checkout: &Path) -> Result<PathBuf> {
    let out = r.run(
        any_checkout,
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(out.trim());
    if common.as_os_str().is_empty() {
        bail!("git did not name the common directory");
    }
    if common.file_name().is_some_and(|n| n == ".git") {
        let parent = common
            .parent()
            .context("the .git directory has no parent")?;
        return Ok(parent.to_path_buf());
    }
    // A bare repo has no working tree to make worktrees from.
    if r.run(&common, "git", &["rev-parse", "--is-bare-repository"])
        .is_ok_and(|b| b.trim() == "true")
    {
        bail!("aq needs a non-bare clone");
    }
    Ok(common)
}

/// Readies the folder for `info`: a known checkout of the user's own PR
/// branch, or a worktree under `state_dir`. `known` holds each known
/// checkout of the repo with its current branch.
pub fn prepare(
    r: &dyn Runner,
    info: &PrInfo,
    main: &Path,
    known: &[(PathBuf, Option<String>)],
    state_dir: &Path,
) -> Result<PrCheckout> {
    let base_ref = format!("origin/{}", info.base_ref);
    let number = info.number.to_string();
    let mut made = false;
    let mut created = false;
    let dir = match known
        .iter()
        .find(|(_, b)| info.mine && b.as_deref() == Some(info.head_ref.as_str()))
    {
        Some((dir, _)) => dir.clone(),
        None => {
            let dir = worktree_dir(state_dir, &info.repo, info.number);
            made = true;
            if !dir.exists() {
                std::fs::create_dir_all(state_dir.join("pr")).context("making the PR folder")?;
                let d = dir.to_string_lossy();
                r.run_with(main, "git", &["worktree", "add", "--detach", &d], long())?;
                created = true;
            }
            if !created {
                let status = r.run(&dir, "git", &["status", "--porcelain"])?;
                if !status.trim().is_empty() {
                    bail!(
                        "PR #{}'s worktree has uncommitted changes: {}",
                        info.number,
                        dir.display()
                    );
                }
            }
            let mut args = vec!["pr", "checkout", &number, "-R", &info.repo];
            if !info.mine {
                args.push("--detach");
            }
            if let Err(e) = r.run_with(&dir, "gh", &args, long()) {
                if created {
                    let d = dir.to_string_lossy();
                    let _ = r.run(main, "git", &["worktree", "remove", "--force", &d]);
                }
                return Err(e.context(format!("could not check out PR #{}", info.number)));
            }
            dir
        }
    };
    r.run_with(
        &dir,
        "git",
        &["fetch", "origin", "--", &info.base_ref],
        long(),
    )?;
    Ok(PrCheckout {
        dir,
        main: main.to_path_buf(),
        made,
        base_ref,
    })
}

/// Removes a worktree aq made, unless it holds uncommitted changes.
pub fn remove(r: &dyn Runner, main: &Path, dir: &Path) -> Result<()> {
    let status = r.run(dir, "git", &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        bail!("{} has uncommitted changes", dir.display());
    }
    r.run(main, "git", &["worktree", "remove", &dir.to_string_lossy()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::sync::Mutex;

    /// Canned outputs by command line; records `(cwd, command line)`.
    struct Fake {
        answers: Vec<(String, Result<String, String>)>,
        log: Mutex<Vec<(PathBuf, String)>>,
    }

    impl Fake {
        fn new(answers: &[(&str, Result<&str, &str>)]) -> Self {
            Fake {
                answers: answers
                    .iter()
                    .map(|(k, v)| {
                        let v = v.map(String::from).map_err(String::from);
                        (k.to_string(), v)
                    })
                    .collect(),
                log: Mutex::new(Vec::new()),
            }
        }

        fn commands(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.1.clone())
                .collect()
        }

        fn ran(&self, prefix: &str) -> bool {
            self.commands().iter().any(|c| c.starts_with(prefix))
        }

        fn ran_in(&self, dir: &Path) -> bool {
            self.log.lock().unwrap().iter().any(|c| c.0 == dir)
        }
    }

    impl Runner for Fake {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
            let line = format!("{program} {}", args.join(" "));
            self.log
                .lock()
                .unwrap()
                .push((cwd.to_path_buf(), line.clone()));
            match self.answers.iter().find(|(k, _)| *k == line) {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(anyhow!("{e}")),
                None => Err(anyhow!("unexpected command: {line}")),
            }
        }

        fn run_with(&self, cwd: &Path, program: &str, args: &[&str], _: RunOpts) -> Result<String> {
            self.run(cwd, program, args)
        }
    }

    fn info(mine: bool) -> PrInfo {
        PrInfo {
            repo: "o/r".into(),
            number: 5,
            title: "Fix".into(),
            body: String::new(),
            author: "x".into(),
            head_ref: "fix".into(),
            base_ref: "main".into(),
            head_sha: "abc".into(),
            url: String::new(),
            mine,
        }
    }

    fn state() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn worktree_dir_is_under_the_state_dir() {
        assert_eq!(
            worktree_dir(Path::new("/s"), "o/r", 5),
            PathBuf::from("/s/pr/o-r-5")
        );
    }

    #[test]
    fn my_pr_on_a_known_branch_uses_that_folder() {
        let s = state();
        let f = Fake::new(&[("git fetch origin -- main", Ok(""))]);
        let known = vec![(PathBuf::from("/w/a"), Some("fix".to_string()))];
        let c = prepare(&f, &info(true), Path::new("/m"), &known, s.path()).unwrap();
        assert_eq!(c.dir, PathBuf::from("/w/a"));
        assert!(!c.made);
        assert_eq!(c.base_ref, "origin/main");
        assert!(!f.ran("git worktree add") && !f.ran("gh pr checkout"));
        assert_eq!(f.commands(), vec!["git fetch origin -- main"]);
    }

    #[test]
    fn someone_elses_pr_gets_a_detached_worktree() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        let add = format!("git worktree add --detach {}", dir.display());
        let f = Fake::new(&[
            (&add, Ok("")),
            ("gh pr checkout 5 -R o/r --detach", Ok("")),
            ("git fetch origin -- main", Ok("")),
        ]);
        // Even with a known checkout on the branch, a teammate's PR gets its own.
        let known = vec![(PathBuf::from("/w/a"), Some("fix".to_string()))];
        let c = prepare(&f, &info(false), Path::new("/m"), &known, s.path()).unwrap();
        assert_eq!(c.dir, dir);
        assert!(c.made);
        assert!(!f.ran_in(Path::new("/w/a")));
    }

    #[test]
    fn my_pr_without_a_checkout_gets_its_branch() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        let add = format!("git worktree add --detach {}", dir.display());
        let f = Fake::new(&[
            (&add, Ok("")),
            ("gh pr checkout 5 -R o/r", Ok("")),
            ("git fetch origin -- main", Ok("")),
        ]);
        let known = vec![(PathBuf::from("/w/a"), Some("other".to_string()))];
        let c = prepare(&f, &info(true), Path::new("/m"), &known, s.path()).unwrap();
        assert!(c.made);
        assert!(!f.ran("gh pr checkout 5 -R o/r --detach"));
        assert!(f.ran("gh pr checkout 5 -R o/r"));
    }

    #[test]
    fn a_failed_checkout_leaves_nothing_behind() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        let add = format!("git worktree add --detach {}", dir.display());
        let remove = format!("git worktree remove --force {}", dir.display());
        let f = Fake::new(&[
            (&add, Ok("")),
            ("gh pr checkout 5 -R o/r", Err("a branch named fix exists")),
            (&remove, Ok("")),
        ]);
        let err = prepare(&f, &info(true), Path::new("/m"), &[], s.path()).unwrap_err();
        assert!(format!("{err:#}").contains("a branch named fix exists"));
        assert!(f.ran("git worktree remove --force"));
        assert!(!f.ran("git fetch"));
    }

    #[test]
    fn an_existing_worktree_is_reused() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fake::new(&[
            ("git status --porcelain", Ok("")),
            ("gh pr checkout 5 -R o/r --detach", Ok("")),
            ("git fetch origin -- main", Ok("")),
        ]);
        let c = prepare(&f, &info(false), Path::new("/m"), &[], s.path()).unwrap();
        assert_eq!(c.dir, dir);
        assert!(!f.ran("git worktree add"));
    }

    #[test]
    fn a_failed_checkout_keeps_a_reused_worktree() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fake::new(&[
            ("git status --porcelain", Ok("")),
            ("gh pr checkout 5 -R o/r --detach", Err("boom")),
        ]);
        assert!(prepare(&f, &info(false), Path::new("/m"), &[], s.path()).is_err());
        assert!(!f.ran("git worktree remove"));
    }

    #[test]
    fn a_dirty_reused_worktree_is_left_alone() {
        let s = state();
        let dir = worktree_dir(s.path(), "o/r", 5);
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fake::new(&[("git status --porcelain", Ok(" M a.rs\n"))]);
        let err = prepare(&f, &info(false), Path::new("/m"), &[], s.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("PR #5's worktree has uncommitted changes")
        );
        assert_eq!(f.commands(), vec!["git status --porcelain"]);
    }

    #[test]
    fn remove_refuses_uncommitted_changes() {
        let f = Fake::new(&[("git status --porcelain", Ok(" M a.rs\n"))]);
        assert!(remove(&f, Path::new("/m"), Path::new("/w")).is_err());
        assert!(!f.ran("git worktree remove"));
    }

    #[test]
    fn remove_takes_a_clean_worktree_away() {
        let f = Fake::new(&[
            ("git status --porcelain", Ok("")),
            ("git worktree remove /w", Ok("")),
        ]);
        remove(&f, Path::new("/m"), Path::new("/w")).unwrap();
        assert!(f.ran("git worktree remove /w"));
    }

    #[test]
    fn main_clone_is_the_parent_of_the_git_dir() {
        let f = Fake::new(&[(
            "git rev-parse --path-format=absolute --git-common-dir",
            Ok("/home/u/repo/.git\n"),
        )]);
        assert_eq!(
            main_clone(&f, Path::new("/home/u/wt")).unwrap(),
            PathBuf::from("/home/u/repo")
        );
    }

    #[test]
    fn main_clone_refuses_a_bare_repo() {
        let f = Fake::new(&[
            (
                "git rev-parse --path-format=absolute --git-common-dir",
                Ok("/srv/repo.git\n"),
            ),
            ("git rev-parse --is-bare-repository", Ok("true\n")),
        ]);
        let err = main_clone(&f, Path::new("/w")).unwrap_err();
        assert!(err.to_string().contains("non-bare"));
    }
}
