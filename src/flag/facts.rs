//! Facts about the repo, PR or issue that an action touches. The rules read
//! them; they are gathered elsewhere.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::candidate::{Action, expands};
use crate::git::repo::git_in;

/// The user: their GitHub login and the orgs they belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub login: String,
    pub orgs: Vec<String>,
}

impl Me {
    /// True if `owner` (of a repo, or a login) is the user, one of their
    /// orgs, or a bot. Case-insensitive.
    pub fn inside(&self, owner: &str) -> bool {
        is_bot_login(owner)
            || owner.eq_ignore_ascii_case(&self.login)
            || self.orgs.iter().any(|o| o.eq_ignore_ascii_case(owner))
    }

    /// True if the person `login` who opened a PR or issue on `repo`
    /// (`owner/name`) is inside: the user, a bot, or someone with owner,
    /// member or collaborator access (`association`, GitHub's
    /// `author_association`) to a repo the user or one of their orgs owns.
    pub fn author_inside(&self, login: &str, is_bot: bool, association: &str, repo: &str) -> bool {
        let repo_owner = repo.split_once('/').map_or(repo, |(owner, _)| owner);
        is_bot
            || is_bot_login(login)
            || login.eq_ignore_ascii_case(&self.login)
            || (matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR")
                && self.inside(repo_owner))
    }
}

/// True for a bot's login: `app/name` as `gh` shows it, or `name[bot]`.
fn is_bot_login(login: &str) -> bool {
    login.starts_with("app/") || login.to_lowercase().ends_with("[bot]")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushFacts {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub default_branch: String,
    /// True if any pushed ref is a tag.
    pub pushes_tag: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFacts {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub author_is_bot: bool,
    /// GitHub's `author_association`, such as `MEMBER`. Empty when it was
    /// not needed: the author is the user or a bot, or the repo is outside.
    pub author_association: String,
    pub base: String,
    pub default_branch: String,
    pub cross_repo: bool,
    pub head_owner: String,
    pub draft: bool,
    pub checks_failing: bool,
    pub my_last_review: Option<Review>,
    pub commits_since_my_review: Option<usize>,
}

/// A review as `gh` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    /// For example `CHANGES_REQUESTED`.
    pub state: String,
    /// A date such as `2026-11-17`.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueFacts {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub author_is_bot: bool,
    /// As for `PrFacts`.
    pub author_association: String,
}

impl PrFacts {
    /// True if the author is inside (see `Me::author_inside`).
    pub fn author_inside(&self, me: &Me) -> bool {
        me.author_inside(
            &self.author,
            self.author_is_bot,
            &self.author_association,
            &self.repo,
        )
    }

    /// True if the author and the repo the PR comes from are inside. A
    /// fork owned by an inside author is inside.
    pub fn inside(&self, me: &Me) -> bool {
        let author = self.author_inside(me);
        let head = me.inside(&self.head_owner)
            || (author && self.head_owner.eq_ignore_ascii_case(&self.author));
        author && head
    }
}

impl IssueFacts {
    /// True if the author is inside (see `Me::author_inside`).
    pub fn inside(&self, me: &Me) -> bool {
        me.author_inside(
            &self.author,
            self.author_is_bot,
            &self.author_association,
            &self.repo,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facts {
    Push(PushFacts),
    Pr(PrFacts),
    Issue(IssueFacts),
    /// A lookup failed or the target is not on GitHub.
    Unknown,
}

/// How long one `git` or `gh` call may run.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `git fetch`, `git worktree add` and `gh pr checkout` may run.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(120);

/// How long `gh pr diff`, `gh pr list` and posting may run.
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(60);

/// How one call runs: how long it may take and what it reads on stdin.
pub struct RunOpts<'a> {
    pub timeout: Duration,
    pub stdin: Option<&'a str>,
}

/// Runs `git` and `gh`. Tests replace it with canned answers.
pub trait Runner: Send + Sync {
    /// stdout of `program args` run in `cwd`, failing past 10 s or on a non-zero exit.
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Result<String>;

    /// Like `run`, with its own timeout and optional stdin.
    fn run_with(&self, cwd: &Path, program: &str, args: &[&str], opts: RunOpts) -> Result<String>;
}

/// The real runner.
pub struct Cli;

impl Runner for Cli {
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
        let opts = RunOpts {
            timeout: LOOKUP_TIMEOUT,
            stdin: None,
        };
        self.run_with(cwd, program, args, opts)
    }

    fn run_with(&self, cwd: &Path, program: &str, args: &[&str], opts: RunOpts) -> Result<String> {
        let mut cmd = if program == "git" {
            git_in(cwd)?
        } else {
            let mut c = Command::new(program);
            c.current_dir(cwd);
            c
        };
        cmd.envs(no_prompts(program));
        // In a session of its own the child has no controlling terminal, so
        // ssh cannot open /dev/tty to ask for a passphrase: it fails instead
        // of drawing over aq. The user's ssh settings are left alone.
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.args(args)
            .stdin(if opts.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("starting {program}"))?;
        let stdout = read_in_background(child.stdout.take().context("no stdout")?);
        let stderr = read_in_background(child.stderr.take().context("no stderr")?);
        // A separate thread, so a child that does not read cannot block the
        // timeout. Dropping stdin at the end of the thread closes the pipe.
        if let (Some(text), Some(mut stdin)) = (opts.stdin, child.stdin.take()) {
            let text = text.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(text.as_bytes());
            });
        }
        let deadline = Instant::now() + opts.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                // Killing the child closes its pipes, so the readers end.
                let _ = child.kill();
                let _ = child.wait();
                bail!("{program} took longer than {} s", opts.timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // A process the child started (an SSH master, say) can hold the pipes
        // open after the child exits. The child itself has exited, so all it
        // wrote is already in the pipes: the readers get a short grace, not
        // the rest of the deadline.
        let grace = deadline.min(Instant::now() + PIPE_GRACE);
        let out = stdout.wait_until(grace);
        let err = stderr.wait_until(grace);
        if !status.success() {
            let err = String::from_utf8_lossy(&err);
            let err = err.trim();
            if err.is_empty() {
                bail!("{program} exited with {status}");
            }
            bail!("{}", err.chars().take(500).collect::<String>());
        }
        String::from_utf8(out).map_err(|_| anyhow!("{program} printed text that is not UTF-8"))
    }
}

/// How long the pipes are read after the child exits, for output a process
/// it started still holds open.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// The environment that stops `program` asking anything: git fails instead
/// of prompting for a password, ssh opens no graphical askpass, and gh asks
/// nothing. The terminal itself is out of reach (`run_with` gives the child
/// a session of its own), so the user's ssh settings need no change.
fn no_prompts(program: &str) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("GIT_TERMINAL_PROMPT", "0".to_string()),
        ("SSH_ASKPASS_REQUIRE", "never".to_string()),
    ];
    if program == "gh" {
        env.push(("GH_PROMPT_DISABLED", "1".into()));
    }
    env
}

/// A pipe being read on its own thread.
struct Reading {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::mpsc::Receiver<()>,
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> Reading {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx, done) = std::sync::mpsc::channel();
    let into = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut b) = into.lock() {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(());
    });
    Reading { buf, done }
}

impl Reading {
    /// What was read by the end of the pipe, or by `deadline` if that comes first.
    fn wait_until(self, deadline: Instant) -> Vec<u8> {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        self.buf.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

/// The user's login and orgs, from `gh`.
pub fn load_me(r: &dyn Runner, cwd: &Path) -> Result<Me> {
    let login = r.run(cwd, "gh", &["api", "user", "--jq", ".login"])?;
    let login = login.trim().to_string();
    if login.is_empty() {
        bail!("gh returned no login");
    }
    let orgs = r.run(cwd, "gh", &["api", "user/orgs", "--jq", ".[].login"])?;
    let orgs = orgs
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    Ok(Me { login, orgs })
}

/// The facts for `action`, or `Facts::Unknown` if any lookup fails or the
/// action needs none. The result never holds an empty owner, repo, branch,
/// base or default branch.
pub fn gather(action: &Action, cwd: &Path, me: &Me, r: &dyn Runner) -> Facts {
    // A name the shell will expand is not the name the command will use.
    let unread = match action {
        Action::Push {
            dir,
            remote,
            refspecs,
            ..
        } => {
            remote.iter().chain(refspecs).any(|w| expands(w))
                || dir.as_ref().is_some_and(|d| expands(&d.to_string_lossy()))
        }
        Action::Pr { repo, selector, .. } => repo.iter().chain(selector).any(|w| expands(w)),
        Action::Issue { repo, .. } => repo.as_deref().is_some_and(expands),
        _ => false,
    };
    if unread {
        return Facts::Unknown;
    }
    let found = match action {
        Action::Push {
            dir,
            remote,
            refspecs,
            ..
        } => {
            let dir = dir
                .as_ref()
                .map_or_else(|| cwd.to_path_buf(), |d| cwd.join(d));
            push_facts(r, &dir, remote.as_deref(), refspecs).map(Facts::Push)
        }
        Action::Pr {
            number,
            repo,
            selector,
            ..
        } => pr_facts(r, cwd, me, *number, selector.as_deref(), repo.as_deref()).map(Facts::Pr),
        Action::Issue { number, repo, .. } => {
            issue_facts(r, cwd, me, *number, repo.as_deref()).map(Facts::Issue)
        }
        _ => return Facts::Unknown,
    };
    found.unwrap_or(Facts::Unknown)
}

fn non_empty(s: &str, what: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        bail!("no {what}");
    }
    Ok(s.to_string())
}

fn push_facts(
    r: &dyn Runner,
    dir: &Path,
    remote: Option<&str>,
    refspecs: &[String],
) -> Result<PushFacts> {
    let config = |key: &str| {
        r.run(dir, "git", &["config", "--get", key])
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    // The current branch, looked up only when needed; `None` when detached.
    let current = std::cell::OnceCell::new();
    let current = || {
        current
            .get_or_init(|| current_branch(r, dir).ok().filter(|b| b != "HEAD"))
            .clone()
    };
    let of_branch = |key: &str| current().and_then(|b| config(&format!("branch.{b}.{key}")));
    // As git chooses where a push goes when the command names no remote.
    let remote = match remote {
        Some(remote) => remote.to_string(),
        None => of_branch("pushRemote")
            .or_else(|| config("remote.pushDefault"))
            .or_else(|| of_branch("remote"))
            .unwrap_or_else(|| "origin".to_string()),
    };
    if remote.starts_with('-') {
        bail!("remote looks like a flag");
    }
    // A URL, on the command line or in the config, is not a configured remote.
    let url = if remote.contains(':') || remote.contains('/') {
        remote
    } else {
        r.run(dir, "git", &["remote", "get-url", "--push", &remote])?
    };
    let (owner, repo) = parse_github_url(url.trim()).context("not a GitHub remote")?;

    // Each refspec is a tag or a branch. One branch is judged; several are not.
    let mut branches = Vec::new();
    let mut tags = Vec::new();
    for spec in refspecs {
        if spec.contains('*') {
            bail!("glob refspec");
        }
        let target = spec.rsplit(':').next().unwrap_or(spec);
        let target = target.strip_prefix('+').unwrap_or(target);
        if let Some(tag) = target.strip_prefix("refs/tags/") {
            tags.push(tag.to_string());
            continue;
        }
        if let Some(branch) = target.strip_prefix("refs/heads/") {
            branches.push(branch.to_string());
        } else if target.is_empty() || target == "HEAD" {
            branches.push(current_branch(r, dir)?);
        } else if is_tag_name(r, dir, target) {
            tags.push(target.to_string());
        } else {
            branches.push(target.to_string());
        }
    }
    if branches.len() > 1 {
        bail!("several branches");
    }
    let pushes_tag = !tags.is_empty();
    let branch = match (branches.pop(), tags.first(), refspecs.is_empty()) {
        (Some(branch), _, _) => branch,
        (None, Some(tag), _) => tag.clone(),
        // No refspec: the branch's upstream branch, else its own name.
        (None, None, _) => match of_branch("merge") {
            Some(merge) => merge
                .strip_prefix("refs/heads/")
                .unwrap_or(&merge)
                .to_string(),
            None => current().context("no current branch")?,
        },
    };
    let branch = non_empty(&branch, "branch")?;
    if branch == "HEAD" {
        bail!("detached HEAD");
    }

    let view = r.run(
        dir,
        "gh",
        &[
            "repo",
            "view",
            &format!("{owner}/{repo}"),
            "--json",
            "defaultBranchRef",
        ],
    )?;
    let default_branch = default_branch_of(&serde_json::from_str(&view)?)?;
    Ok(PushFacts {
        owner: non_empty(&owner, "owner")?,
        repo: non_empty(&repo, "repo")?,
        branch,
        default_branch,
        pushes_tag,
    })
}

/// True if `name` is a tag and not also a branch.
fn is_tag_name(r: &dyn Runner, dir: &Path, name: &str) -> bool {
    let verify = |full: String| {
        r.run(dir, "git", &["rev-parse", "-q", "--verify", &full])
            .is_ok()
    };
    verify(format!("refs/tags/{name}")) && !verify(format!("refs/heads/{name}"))
}

fn current_branch(r: &dyn Runner, dir: &Path) -> Result<String> {
    non_empty(
        &r.run(dir, "git", &["rev-parse", "--abbrev-ref", "HEAD"])?,
        "branch",
    )
}

/// `owner` and `repo` of a `github.com` remote URL, in the scp form
/// (`git@github.com:o/r.git`) or with a scheme (`https://github.com/o/r`).
/// `github.com` must be the whole host.
fn parse_github_url(url: &str) -> Option<(String, String)> {
    let (host_and_path, sep) = match url.split_once("://") {
        Some((_, rest)) => (rest, '/'),
        None => (url, ':'),
    };
    let (host, path) = host_and_path.split_once(sep)?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    // A scheme URL may carry a port.
    let host = host.split(':').next().unwrap_or(host);
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn default_branch_of(v: &Value) -> Result<String> {
    non_empty(
        v["defaultBranchRef"]["name"].as_str().unwrap_or(""),
        "default branch",
    )
}

/// The `-R repo` arguments for `gh`, if the command named a repo.
fn repo_flag(repo: Option<&str>) -> Vec<&str> {
    repo.map_or_else(Vec::new, |repo| vec!["-R", repo])
}

fn pr_facts(
    r: &dyn Runner,
    dir: &Path,
    me: &Me,
    number: Option<u64>,
    selector: Option<&str>,
    repo: Option<&str>,
) -> Result<PrFacts> {
    // `gh pr view` with no number reads the current branch, which `-R` cannot qualify.
    if number.is_none() && selector.is_none() && repo.is_some() {
        bail!("a repo without a PR number");
    }
    // A link to a host other than github.com is not judged.
    if selector.is_some_and(|s| s.starts_with('-') || s.contains("://")) {
        bail!("a PR aq cannot name");
    }
    let number = number.map(|n| n.to_string());
    let mut args = vec!["pr", "view"];
    args.extend(number.as_deref().or(selector));
    args.extend(repo_flag(repo));
    args.extend([
        "--json",
        "number,author,baseRefName,isCrossRepository,headRepositoryOwner,isDraft,reviews,commits,statusCheckRollup",
    ]);
    let pr: Value = serde_json::from_str(&r.run(dir, "gh", &args)?)?;

    let mut args = vec!["repo", "view"];
    args.extend(repo);
    args.extend(["--json", "defaultBranchRef,nameWithOwner"]);
    let repo_view: Value = serde_json::from_str(&r.run(dir, "gh", &args)?)?;

    let my_last_review = pr["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|rv| {
            rv["author"]["login"]
                .as_str()
                .is_some_and(|l| l.eq_ignore_ascii_case(&me.login))
        })
        .filter_map(|rv| Some((rv["state"].as_str()?, rv["submittedAt"].as_str()?)))
        .max_by_key(|(_, at)| *at);
    let commits_since_my_review = my_last_review.map(|(_, at)| {
        pr["commits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["committedDate"].as_str().is_some_and(|d| d > at))
            .count()
    });
    let my_last_review = my_last_review.map(|(state, at)| Review {
        state: state.to_string(),
        at: at.chars().take(10).collect(),
    });

    let text = |v: &Value, what: &str| non_empty(v.as_str().unwrap_or(""), what);
    let repo = text(&repo_view["nameWithOwner"], "repo")?;
    let number = pr["number"].as_u64().context("no PR number")?;
    let author = text(&pr["author"]["login"], "author")?;
    let author_is_bot = pr["author"]["is_bot"].as_bool().unwrap_or(false);
    let author_association = association(
        r,
        dir,
        me,
        &author,
        author_is_bot,
        &repo,
        &format!("repos/{repo}/pulls/{number}"),
    )?;
    Ok(PrFacts {
        repo,
        number,
        author,
        author_is_bot,
        author_association,
        base: text(&pr["baseRefName"], "base")?,
        default_branch: default_branch_of(&repo_view)?,
        cross_repo: pr["isCrossRepository"].as_bool().unwrap_or(false),
        head_owner: pr["headRepositoryOwner"]["login"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        draft: pr["isDraft"].as_bool().unwrap_or(false),
        checks_failing: pr["statusCheckRollup"]
            .as_array()
            .into_iter()
            .flatten()
            .any(check_failed),
        my_last_review,
        commits_since_my_review,
    })
}

fn check_failed(check: &Value) -> bool {
    let is = |key: &str, bad: &[&str]| check[key].as_str().is_some_and(|v| bad.contains(&v));
    is(
        "conclusion",
        &["FAILURE", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED"],
    ) || is("state", &["FAILURE", "ERROR"])
}

/// The author's `author_association` on `repo`, from the REST API. Looked
/// up only when it can decide: not for the user or a bot, and not on a repo
/// outside, where nobody else is inside.
fn association(
    r: &dyn Runner,
    dir: &Path,
    me: &Me,
    author: &str,
    is_bot: bool,
    repo: &str,
    path: &str,
) -> Result<String> {
    let repo_owner = repo.split_once('/').map_or(repo, |(owner, _)| owner);
    if me.author_inside(author, is_bot, "", repo) || !me.inside(repo_owner) {
        return Ok(String::new());
    }
    let out = r.run(dir, "gh", &["api", path, "--jq", ".author_association"])?;
    non_empty(&out, "author association")
}

fn issue_facts(
    r: &dyn Runner,
    dir: &Path,
    me: &Me,
    number: Option<u64>,
    repo: Option<&str>,
) -> Result<IssueFacts> {
    let number = number.context("no issue number")?.to_string();
    let mut args = vec!["issue", "view", number.as_str()];
    args.extend(repo_flag(repo));
    args.extend(["--json", "number,author"]);
    let issue: Value = serde_json::from_str(&r.run(dir, "gh", &args)?)?;

    let mut args = vec!["repo", "view"];
    args.extend(repo);
    args.extend(["--json", "nameWithOwner"]);
    let repo_view: Value = serde_json::from_str(&r.run(dir, "gh", &args)?)?;

    let repo = non_empty(repo_view["nameWithOwner"].as_str().unwrap_or(""), "repo")?;
    let number = issue["number"].as_u64().context("no issue number")?;
    let author = non_empty(issue["author"]["login"].as_str().unwrap_or(""), "author")?;
    let author_is_bot = issue["author"]["is_bot"].as_bool().unwrap_or(false);
    let author_association = association(
        r,
        dir,
        me,
        &author,
        author_is_bot,
        &repo,
        &format!("repos/{repo}/issues/{number}"),
    )?;
    Ok(IssueFacts {
        repo,
        number,
        author,
        author_is_bot,
        author_association,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::flag::candidate::PrVerb;

    /// Answers only the commands it was given; anything else fails.
    struct FakeRunner(HashMap<String, String>);

    impl FakeRunner {
        fn new(answers: &[(&str, &str, &str)]) -> Self {
            Self(
                answers
                    .iter()
                    .map(|(p, a, out)| (format!("{p} {a}"), out.to_string()))
                    .collect(),
            )
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, _cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
            let key = format!("{program} {}", args.join(" "));
            self.0
                .get(&key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unexpected command: {key}"))
        }

        fn run_with(
            &self,
            cwd: &Path,
            program: &str,
            args: &[&str],
            _opts: RunOpts,
        ) -> Result<String> {
            self.run(cwd, program, args)
        }
    }

    #[test]
    fn run_with_passes_stdin() {
        let opts = RunOpts {
            timeout: LOOKUP_TIMEOUT,
            stdin: Some("hello there"),
        };
        let out = Cli.run_with(Path::new("."), "cat", &[], opts).unwrap();
        assert_eq!(out, "hello there");
    }

    #[test]
    fn a_failure_carries_stderr() {
        let err = Cli
            .run(Path::new("."), "sh", &["-c", "echo nope >&2; exit 1"])
            .unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn run_with_kills_past_its_timeout() {
        let opts = RunOpts {
            timeout: Duration::from_millis(200),
            stdin: None,
        };
        let start = Instant::now();
        let r = Cli.run_with(Path::new("."), "sleep", &["5"], opts);
        assert!(r.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_process_left_holding_the_pipe_does_not_hold_up_the_call() {
        let opts = RunOpts {
            timeout: Duration::from_secs(1),
            stdin: None,
        };
        let start = Instant::now();
        let out = Cli
            .run_with(Path::new("."), "sh", &["-c", "sleep 30 & echo hi"], opts)
            .unwrap();
        assert_eq!(out, "hi\n");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn network_calls_never_prompt() {
        let git = no_prompts("git");
        assert_eq!(
            git,
            [
                ("GIT_TERMINAL_PROMPT", "0".to_string()),
                ("SSH_ASKPASS_REQUIRE", "never".to_string()),
            ]
        );
        assert_eq!(no_prompts("gh")[2], ("GH_PROMPT_DISABLED", "1".to_string()));
        // The user's ssh command is left as it is.
        assert!(!git.iter().any(|(k, _)| *k == "GIT_SSH_COMMAND"));
        let out = Cli
            .run(Path::new("."), "sh", &["-c", "echo $GIT_TERMINAL_PROMPT"])
            .unwrap();
        assert_eq!(out, "0\n");
        // No controlling terminal: /dev/tty cannot be opened.
        let out = Cli
            .run(
                Path::new("."),
                "sh",
                &[
                    "-c",
                    "(exec 3</dev/tty) 2>/dev/null && echo tty || echo none",
                ],
            )
            .unwrap();
        assert_eq!(out, "none\n");
    }

    fn me() -> Me {
        Me {
            login: "smnai".into(),
            orgs: vec!["nodo-labs-uk".into()],
        }
    }

    fn push(remote: Option<&str>, refspecs: &[&str]) -> Action {
        Action::Push {
            dir: None,
            remote: remote.map(String::from),
            refspecs: refspecs.iter().map(|s| s.to_string()).collect(),
            force: false,
            delete: false,
            tags: false,
        }
    }

    fn pr(number: Option<u64>, repo: Option<&str>) -> Action {
        Action::Pr {
            verb: PrVerb::Merge,
            number,
            repo: repo.map(String::from),
            selector: None,
            speaks: false,
        }
    }

    const REPO_VIEW: &str = r#"{"defaultBranchRef":{"name":"main"}}"#;

    const PR_JSON: &str = r#"{
      "number": 57,
      "author": {"id": "MDQ6", "is_bot": false, "login": "alice", "name": "Alice"},
      "baseRefName": "main",
      "isCrossRepository": true,
      "headRepositoryOwner": {"id": "MDQ6", "login": "alice"},
      "isDraft": false,
      "reviews": [
        {"author": {"login": "smnai"}, "state": "COMMENTED", "submittedAt": "2026-11-15T08:00:00Z"},
        {"author": {"login": "smnai"}, "state": "CHANGES_REQUESTED", "submittedAt": "2026-11-17T09:30:00Z"},
        {"author": {"login": "bob"}, "state": "APPROVED", "submittedAt": "2026-11-18T09:30:00Z"}
      ],
      "commits": [
        {"committedDate": "2026-11-16T10:00:00Z"},
        {"committedDate": "2026-11-18T10:00:00Z"},
        {"committedDate": "2026-11-18T11:00:00Z"}
      ],
      "statusCheckRollup": [
        {"__typename": "CheckRun", "conclusion": "SUCCESS", "status": "COMPLETED"},
        {"__typename": "CheckRun", "conclusion": "FAILURE", "status": "COMPLETED"}
      ]
    }"#;

    const PR_ARGS: &str = "pr view 57 --json number,author,baseRefName,isCrossRepository,headRepositoryOwner,isDraft,reviews,commits,statusCheckRollup";
    const REPO_ARGS: &str = "repo view --json defaultBranchRef,nameWithOwner";
    const REPO_OUT: &str = r#"{"defaultBranchRef":{"name":"main"},"nameWithOwner":"o/r"}"#;

    #[test]
    fn pr_facts_from_gh_json() {
        let r = FakeRunner::new(&[("gh", PR_ARGS, PR_JSON), ("gh", REPO_ARGS, REPO_OUT)]);
        let facts = gather(&pr(Some(57), None), Path::new("/w"), &me(), &r);
        assert_eq!(
            facts,
            Facts::Pr(PrFacts {
                repo: "o/r".into(),
                number: 57,
                author: "alice".into(),
                author_is_bot: false,
                author_association: String::new(),
                base: "main".into(),
                default_branch: "main".into(),
                cross_repo: true,
                head_owner: "alice".into(),
                draft: false,
                checks_failing: true,
                my_last_review: Some(Review {
                    state: "CHANGES_REQUESTED".into(),
                    at: "2026-11-17".into(),
                }),
                commits_since_my_review: Some(2),
            })
        );
    }

    #[test]
    fn pr_without_number_uses_the_current_branch() {
        let json = r#"{"number":3,"author":{"login":"smnai"},"baseRefName":"main",
          "isCrossRepository":false,"headRepositoryOwner":{"login":"smnai"},
          "isDraft":true,"reviews":[],"commits":[],"statusCheckRollup":[]}"#;
        let r = FakeRunner::new(&[
            (
                "gh",
                "pr view --json number,author,baseRefName,isCrossRepository,headRepositoryOwner,isDraft,reviews,commits,statusCheckRollup",
                json,
            ),
            ("gh", REPO_ARGS, REPO_OUT),
        ]);
        let Facts::Pr(f) = gather(&pr(None, None), Path::new("/w"), &me(), &r) else {
            panic!("expected PR facts");
        };
        assert_eq!(f.number, 3);
        assert!(f.draft && !f.checks_failing);
        assert_eq!(f.my_last_review, None);
        assert_eq!(f.commits_since_my_review, None);
    }

    #[test]
    fn push_facts_from_the_remote_url() {
        for url in ["git@github.com:o/r.git", "https://github.com/o/r"] {
            let r = FakeRunner::new(&[
                ("git", "remote get-url --push origin", url),
                ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
            ]);
            let facts = gather(&push(Some("origin"), &["fix"]), Path::new("/w"), &me(), &r);
            assert_eq!(
                facts,
                Facts::Push(PushFacts {
                    owner: "o".into(),
                    repo: "r".into(),
                    branch: "fix".into(),
                    default_branch: "main".into(),
                    pushes_tag: false,
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn push_with_no_arguments_uses_the_upstream() {
        let r = FakeRunner::new(&[
            ("git", "rev-parse --abbrev-ref HEAD", "local\n"),
            ("git", "config --get branch.local.remote", "origin\n"),
            (
                "git",
                "config --get branch.local.merge",
                "refs/heads/feat/x\n",
            ),
            (
                "git",
                "remote get-url --push origin",
                "git@github.com:o/r.git\n",
            ),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ]);
        let Facts::Push(f) = gather(&push(None, &[]), Path::new("/w"), &me(), &r) else {
            panic!("expected push facts");
        };
        assert_eq!(f.branch, "feat/x");
    }

    #[test]
    fn push_with_no_upstream_goes_to_origin_under_the_same_name() {
        let r = FakeRunner::new(&[
            ("git", "rev-parse --abbrev-ref HEAD", "fix\n"),
            (
                "git",
                "remote get-url --push origin",
                "git@github.com:o/r.git\n",
            ),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ]);
        let Facts::Push(f) = gather(&push(None, &[]), Path::new("/w"), &me(), &r) else {
            panic!("expected push facts");
        };
        assert_eq!((f.owner.as_str(), f.branch.as_str()), ("o", "fix"));
    }

    #[test]
    fn a_branch_checked_out_from_a_fork_pushes_to_the_fork() {
        // As `gh pr checkout` leaves it: the branch's remote is the fork's URL.
        let r = FakeRunner::new(&[
            ("git", "rev-parse --abbrev-ref HEAD", "Timonsc/fix-5\n"),
            (
                "git",
                "config --get branch.Timonsc/fix-5.remote",
                "https://github.com/Timonsc/socratui.git\n",
            ),
            (
                "git",
                "config --get branch.Timonsc/fix-5.merge",
                "refs/heads/fix-5\n",
            ),
            (
                "gh",
                "repo view Timonsc/socratui --json defaultBranchRef",
                REPO_VIEW,
            ),
        ]);
        let Facts::Push(f) = gather(&push(None, &[]), Path::new("/w"), &me(), &r) else {
            panic!("expected push facts");
        };
        assert_eq!(
            (f.owner.as_str(), f.repo.as_str(), f.branch.as_str()),
            ("Timonsc", "socratui", "fix-5")
        );
    }

    #[test]
    fn push_remote_and_push_default_come_before_the_branch_remote() {
        let answers = |push_remote: bool| {
            let mut a = vec![
                ("git", "rev-parse --abbrev-ref HEAD", "fix\n"),
                ("git", "config --get remote.pushDefault", "mine\n"),
                ("git", "config --get branch.fix.remote", "origin\n"),
                (
                    "git",
                    "remote get-url --push origin",
                    "git@github.com:o/r.git",
                ),
                (
                    "git",
                    "remote get-url --push mine",
                    "git@github.com:smnai/r.git",
                ),
                (
                    "git",
                    "remote get-url --push fork",
                    "git@github.com:Timonsc/r.git",
                ),
                ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
                ("gh", "repo view smnai/r --json defaultBranchRef", REPO_VIEW),
                (
                    "gh",
                    "repo view Timonsc/r --json defaultBranchRef",
                    REPO_VIEW,
                ),
            ];
            if push_remote {
                a.push(("git", "config --get branch.fix.pushRemote", "fork\n"));
            }
            FakeRunner::new(&a)
        };
        let owner = |r: &FakeRunner| match gather(&push(None, &[]), Path::new("/w"), &me(), r) {
            Facts::Push(f) => f.owner,
            f => panic!("{f:?}"),
        };
        assert_eq!(owner(&answers(true)), "Timonsc");
        assert_eq!(owner(&answers(false)), "smnai");
    }

    #[test]
    fn a_name_the_shell_expands_is_unknown() {
        let r = FakeRunner::new(&[
            (
                "git",
                "remote get-url --push origin",
                "git@github.com:o/r.git",
            ),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ]);
        for a in [
            push(Some("$REMOTE"), &["fix"]),
            push(Some("origin"), &["`git branch --show-current`"]),
            push(Some("origin"), &["HEAD:$B"]),
            Action::Pr {
                verb: PrVerb::Merge,
                number: None,
                repo: None,
                selector: Some("$n".into()),
                speaks: false,
            },
            pr(Some(5), Some("$REPO")),
        ] {
            assert_eq!(
                gather(&a, Path::new("/w"), &me(), &r),
                Facts::Unknown,
                "{a:?}"
            );
        }
    }

    #[test]
    fn a_pr_named_by_branch_is_looked_up_by_that_name() {
        let json = r#"{"number":8,"author":{"login":"smnai"},"baseRefName":"main",
          "isCrossRepository":false,"headRepositoryOwner":{"login":"o"},
          "isDraft":false,"reviews":[],"commits":[],"statusCheckRollup":[]}"#;
        let r = FakeRunner::new(&[
            (
                "gh",
                "pr view feat/x -R o/r --json number,author,baseRefName,isCrossRepository,headRepositoryOwner,isDraft,reviews,commits,statusCheckRollup",
                json,
            ),
            (
                "gh",
                "repo view o/r --json defaultBranchRef,nameWithOwner",
                REPO_OUT,
            ),
        ]);
        let a = Action::Pr {
            verb: PrVerb::Merge,
            number: None,
            repo: Some("o/r".into()),
            selector: Some("feat/x".into()),
            speaks: false,
        };
        let Facts::Pr(f) = gather(&a, Path::new("/w"), &me(), &r) else {
            panic!("expected PR facts");
        };
        assert_eq!(f.number, 8);
        // A link to another host is not looked up.
        let a = Action::Pr {
            verb: PrVerb::Merge,
            number: None,
            repo: None,
            selector: Some("https://ghe.example/o/r/pull/8".into()),
            speaks: false,
        };
        assert_eq!(gather(&a, Path::new("/w"), &me(), &r), Facts::Unknown);
    }

    #[test]
    fn a_teammates_association_is_looked_up_only_on_an_inside_repo() {
        let pr_json = |author: &str, bot: bool| {
            format!(
                r#"{{"number":5,"author":{{"login":"{author}","is_bot":{bot}}},"baseRefName":"feat",
                "isCrossRepository":false,"headRepositoryOwner":{{"login":"nodo-labs-uk"}},
                "isDraft":false,"reviews":[],"commits":[],"statusCheckRollup":[]}}"#
            )
        };
        const ARGS: &str = "pr view 5 -R nodo-labs-uk/aq --json number,author,baseRefName,isCrossRepository,headRepositoryOwner,isDraft,reviews,commits,statusCheckRollup";
        const VIEW: &str = "repo view nodo-labs-uk/aq --json defaultBranchRef,nameWithOwner";
        const VIEW_OUT: &str =
            r#"{"defaultBranchRef":{"name":"main"},"nameWithOwner":"nodo-labs-uk/aq"}"#;
        let teammate = pr_json("bob", false);
        let r = FakeRunner::new(&[
            ("gh", ARGS, &teammate),
            ("gh", VIEW, VIEW_OUT),
            (
                "gh",
                "api repos/nodo-labs-uk/aq/pulls/5 --jq .author_association",
                "MEMBER\n",
            ),
        ]);
        let a = pr(Some(5), Some("nodo-labs-uk/aq"));
        let Facts::Pr(f) = gather(&a, Path::new("/w"), &me(), &r) else {
            panic!("expected PR facts");
        };
        assert_eq!(f.author_association, "MEMBER");
        assert!(f.inside(&me()));

        // A bot needs no lookup: this runner would fail it.
        let bot = pr_json("app/dependabot", true);
        let r = FakeRunner::new(&[("gh", ARGS, &bot), ("gh", VIEW, VIEW_OUT)]);
        let Facts::Pr(f) = gather(&a, Path::new("/w"), &me(), &r) else {
            panic!("expected PR facts");
        };
        assert!(f.author_is_bot && f.inside(&me()));

        // A failed lookup leaves the facts unknown.
        let r = FakeRunner::new(&[("gh", ARGS, &teammate), ("gh", VIEW, VIEW_OUT)]);
        assert_eq!(gather(&a, Path::new("/w"), &me(), &r), Facts::Unknown);
    }

    #[test]
    fn a_refspec_names_its_target_branch() {
        let r = FakeRunner::new(&[
            (
                "git",
                "remote get-url --push origin",
                "https://github.com/o/r.git",
            ),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ]);
        let a = push(Some("origin"), &["HEAD:refs/heads/release"]);
        let Facts::Push(f) = gather(&a, Path::new("/w"), &me(), &r) else {
            panic!("expected push facts");
        };
        assert_eq!(f.branch, "release");
    }

    #[test]
    fn a_relative_dir_joins_the_cwd() {
        struct Spy(std::sync::Mutex<Vec<PathBuf>>);
        impl Runner for Spy {
            fn run(&self, cwd: &Path, _: &str, _: &[&str]) -> Result<String> {
                self.0.lock().unwrap().push(cwd.to_path_buf());
                anyhow::bail!("no answer")
            }

            fn run_with(
                &self,
                cwd: &Path,
                program: &str,
                args: &[&str],
                _opts: RunOpts,
            ) -> Result<String> {
                self.run(cwd, program, args)
            }
        }
        let a = Action::Push {
            dir: Some(PathBuf::from("../x")),
            remote: Some("origin".into()),
            refspecs: vec![],
            force: false,
            delete: false,
            tags: false,
        };
        let spy = Spy(Default::default());
        assert_eq!(gather(&a, Path::new("/w"), &me(), &spy), Facts::Unknown);
        let dirs = spy.0.lock().unwrap();
        assert!(!dirs.is_empty());
        assert!(dirs.iter().all(|d| d == Path::new("/w/../x")), "{dirs:?}");
    }

    #[test]
    fn a_pr_with_a_repo_and_no_number_is_unknown() {
        let r = FakeRunner::new(&[]);
        assert_eq!(
            gather(&pr(None, Some("o/r")), Path::new("/w"), &me(), &r),
            Facts::Unknown
        );
    }

    fn tag_runner(heads: bool) -> FakeRunner {
        let mut answers = vec![
            (
                "git",
                "remote get-url --push origin",
                "git@github.com:o/r.git",
            ),
            ("git", "rev-parse -q --verify refs/tags/v1.2", "abc\n"),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ];
        if heads {
            answers.push(("git", "rev-parse -q --verify refs/heads/v1.2", "abc\n"));
        }
        FakeRunner::new(&answers)
    }

    #[test]
    fn a_tag_refspec_sets_pushes_tag() {
        for spec in ["v1.2", "refs/tags/v1.2"] {
            let a = push(Some("origin"), &[spec]);
            let Facts::Push(f) = gather(&a, Path::new("/w"), &me(), &tag_runner(false)) else {
                panic!("expected push facts for {spec}");
            };
            assert!(f.pushes_tag, "{spec}");
            assert_eq!(f.branch, "v1.2");
        }
    }

    #[test]
    fn a_name_that_is_also_a_branch_is_a_branch() {
        let a = push(Some("origin"), &["v1.2"]);
        let Facts::Push(f) = gather(&a, Path::new("/w"), &me(), &tag_runner(true)) else {
            panic!("expected push facts");
        };
        assert!(!f.pushes_tag);
    }

    #[test]
    fn several_branches_or_a_glob_are_unknown() {
        let r = FakeRunner::new(&[
            (
                "git",
                "remote get-url --push origin",
                "git@github.com:o/r.git",
            ),
            ("gh", "repo view o/r --json defaultBranchRef", REPO_VIEW),
        ]);
        // Branch names are checked as tags first, which this runner cannot answer: a failed check means a branch.
        for specs in [
            &["fix", "main"][..],
            &["refs/heads/*:refs/heads/*"],
            &["+*"],
        ] {
            let a = push(Some("origin"), specs);
            assert_eq!(
                gather(&a, Path::new("/w"), &me(), &r),
                Facts::Unknown,
                "{specs:?}"
            );
        }
    }

    #[test]
    fn a_remote_whose_host_is_not_github_com_is_unknown() {
        for url in [
            "https://notgithub.com/o/r",
            "https://evil.com/github.com/o/r",
            "git@github.com.evil.com:o/r.git",
        ] {
            let r = FakeRunner::new(&[("git", "remote get-url --push origin", url)]);
            let a = push(Some("origin"), &["fix"]);
            assert_eq!(
                gather(&a, Path::new("/w"), &me(), &r),
                Facts::Unknown,
                "{url}"
            );
        }
        assert_eq!(
            parse_github_url("ssh://git@github.com:22/o/r.git"),
            Some(("o".into(), "r".into()))
        );
    }

    #[test]
    fn a_non_github_remote_is_unknown() {
        let r = FakeRunner::new(&[(
            "git",
            "remote get-url --push origin",
            "git@gitlab.com:o/r.git",
        )]);
        let facts = gather(&push(Some("origin"), &["fix"]), Path::new("/w"), &me(), &r);
        assert_eq!(facts, Facts::Unknown);
    }

    #[test]
    fn a_failing_gh_is_unknown() {
        let r = FakeRunner::new(&[]);
        assert_eq!(
            gather(&pr(Some(57), None), Path::new("/w"), &me(), &r),
            Facts::Unknown
        );
    }

    #[test]
    fn an_unknown_default_branch_is_unknown() {
        let r = FakeRunner::new(&[
            ("gh", PR_ARGS, PR_JSON),
            (
                "gh",
                REPO_ARGS,
                r#"{"defaultBranchRef":null,"nameWithOwner":"o/r"}"#,
            ),
        ]);
        assert_eq!(
            gather(&pr(Some(57), None), Path::new("/w"), &me(), &r),
            Facts::Unknown
        );
    }

    #[test]
    fn issue_facts_from_gh_json() {
        let r = FakeRunner::new(&[
            (
                "gh",
                "issue view 9 -R o/r --json number,author",
                r#"{"number":9,"author":{"login":"alice"}}"#,
            ),
            (
                "gh",
                "repo view o/r --json nameWithOwner",
                r#"{"nameWithOwner":"o/r"}"#,
            ),
        ]);
        let a = Action::Issue {
            verb: crate::flag::candidate::IssueVerb::Comment,
            number: Some(9),
            repo: Some("o/r".into()),
            speaks: true,
        };
        assert_eq!(
            gather(&a, Path::new("/w"), &me(), &r),
            Facts::Issue(IssueFacts {
                repo: "o/r".into(),
                number: 9,
                author: "alice".into(),
                author_is_bot: false,
                author_association: String::new(),
            })
        );
    }

    #[test]
    fn actions_without_facts_run_nothing() {
        let r = FakeRunner::new(&[]);
        let a = Action::Release { repo: None };
        assert_eq!(gather(&a, Path::new("/w"), &me(), &r), Facts::Unknown);
    }

    #[test]
    fn load_me_reads_login_and_orgs() {
        let r = FakeRunner::new(&[
            ("gh", "api user --jq .login", "smnai\n"),
            (
                "gh",
                "api user/orgs --jq .[].login",
                "nodo-labs-uk\nother\n",
            ),
        ]);
        assert_eq!(
            load_me(&r, Path::new("/w")).unwrap(),
            Me {
                login: "smnai".into(),
                orgs: vec!["nodo-labs-uk".into(), "other".into()],
            }
        );
    }

    #[test]
    fn load_me_fails_on_an_empty_login() {
        let r = FakeRunner::new(&[
            ("gh", "api user --jq .login", "\n"),
            ("gh", "api user/orgs --jq .[].login", ""),
        ]);
        assert!(load_me(&r, Path::new("/w")).is_err());
    }

    #[test]
    fn cli_runs_a_command_and_fails_on_a_bad_exit() {
        let out = Cli.run(Path::new("/"), "echo", &["hi"]).unwrap();
        assert_eq!(out, "hi\n");
        assert!(Cli.run(Path::new("/"), "false", &[]).is_err());
    }
}
