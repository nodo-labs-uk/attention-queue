//! Reading pull requests from GitHub through `gh`.

pub mod app;
pub mod checkout;
pub mod place;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::flag::candidate::valid_repo;
use crate::flag::facts::{Me, RunOpts, Runner, SLOW_TIMEOUT};

/// One open PR in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSummary {
    /// `owner/name`
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head: String,
    pub draft: bool,
    pub mine: bool,
    pub updated: String,
}

/// One PR read in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_sha: String,
    pub url: String,
    pub mine: bool,
}

fn slow() -> RunOpts<'static> {
    RunOpts {
        timeout: SLOW_TIMEOUT,
        stdin: None,
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn is_me(author: &str, me: &Me) -> bool {
    author.eq_ignore_ascii_case(&me.login)
}

/// The `owner/name` of the repository in `dir`.
pub fn repo_of(r: &dyn Runner, dir: &Path) -> Result<String> {
    let out = r.run(dir, "gh", &["repo", "view", "--json", "nameWithOwner"])?;
    let v: Value = serde_json::from_str(&out).context("reading gh repo view")?;
    let repo = text(&v["nameWithOwner"]);
    if repo.is_empty() {
        anyhow::bail!("gh did not name the repository");
    }
    Ok(repo)
}

/// The open PRs of the repository in `dir`, newest update first.
pub fn list_prs(r: &dyn Runner, dir: &Path, me: &Me) -> Result<Vec<PrSummary>> {
    let repo = repo_of(r, dir)?;
    prs_of(r, dir, &repo, me)
}

/// The open PRs of `repo`, whose checkout `dir` is, newest update first.
fn prs_of(r: &dyn Runner, dir: &Path, repo: &str, me: &Me) -> Result<Vec<PrSummary>> {
    let args = [
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,title,author,headRefName,isDraft,updatedAt",
    ];
    let out = r.run_with(dir, "gh", &args, slow())?;
    let v: Value = serde_json::from_str(&out).context("reading gh pr list")?;
    let mut prs: Vec<PrSummary> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let author = text(&p["author"]["login"]);
            Some(PrSummary {
                repo: repo.to_string(),
                number: p["number"].as_u64()?,
                title: text(&p["title"]),
                mine: is_me(&author, me),
                author,
                head: text(&p["headRefName"]),
                draft: p["isDraft"].as_bool().unwrap_or(false),
                updated: text(&p["updatedAt"]),
            })
        })
        .collect();
    prs.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(prs)
}

/// One PR of `repo`, read in full.
pub fn view_pr(r: &dyn Runner, dir: &Path, repo: &str, number: u64, me: &Me) -> Result<PrInfo> {
    let n = number.to_string();
    let args = [
        "pr",
        "view",
        &n,
        "-R",
        repo,
        "--json",
        "number,title,body,author,headRefName,baseRefName,headRefOid,url",
    ];
    let out = r.run_with(dir, "gh", &args, slow())?;
    let v: Value = serde_json::from_str(&out).context("reading gh pr view")?;
    let author = text(&v["author"]["login"]);
    Ok(PrInfo {
        repo: repo.to_string(),
        number: v["number"].as_u64().context("no PR number")?,
        title: text(&v["title"]),
        body: text(&v["body"]),
        mine: is_me(&author, me),
        author,
        head_ref: text(&v["headRefName"]),
        base_ref: text(&v["baseRefName"]),
        head_sha: text(&v["headRefOid"]),
        url: text(&v["url"]),
    })
}

/// Reads `5`, `#5` or a `github.com/<owner>/<repo>/pull/<n>` link. Only a link names a repo.
pub fn parse_pr_ref(text: &str) -> Option<(Option<String>, u64)> {
    let t = text.trim();
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    if let Some(n) = number(t.strip_prefix('#').unwrap_or(t)) {
        return Some((None, n));
    }
    let rest = t
        .strip_prefix("https://github.com/")
        .or_else(|| t.strip_prefix("http://github.com/"))?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next()? != "pull" {
        return None;
    }
    let n = number(parts.next()?)?;
    let repo = format!("{owner}/{name}");
    valid_repo(&repo).then_some((Some(repo), n))
}

/// What `list_all` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrListing {
    /// Newest update first.
    pub prs: Vec<PrSummary>,
    /// Every repo that could be named, listed or not.
    pub repos: Vec<String>,
    /// One line for each repo that could not be listed.
    pub failed: Vec<String>,
}

/// The open PRs of every repo that `dirs` are checkouts of. Checkouts of
/// one clone are listed once. A repo that fails is named in `failed`; only
/// when every repo fails is the first error returned.
pub fn list_all(r: &dyn Runner, dirs: &[PathBuf], me: &Me) -> Result<PrListing> {
    let mut clones = Vec::new();
    let mut found = PrListing::default();
    let mut errors = Vec::new();
    for dir in dirs {
        let clone = checkout::main_clone(r, dir).unwrap_or_else(|_| dir.clone());
        if clones.contains(&clone) {
            continue;
        }
        clones.push(clone);
        let listed = match repo_of(r, dir) {
            Err(e) => Err(e.context(format!("Could not list {}", dir.display()))),
            Ok(repo) => {
                let prs =
                    prs_of(r, dir, &repo, me).with_context(|| format!("Could not list {repo}"));
                if !found.repos.contains(&repo) {
                    found.repos.push(repo);
                }
                prs
            }
        };
        match listed {
            Ok(list) => found.prs.extend(list),
            Err(e) => {
                found.failed.push(format!("{e:#}"));
                errors.push(e);
            }
        }
    }
    if errors.len() == clones.len()
        && let Some(e) = errors.into_iter().next()
    {
        return Err(e);
    }
    found.prs.sort_by(|a, b| b.updated.cmp(&a.updated));
    let mut seen = std::collections::BTreeSet::new();
    found
        .prs
        .retain(|p| seen.insert((p.repo.clone(), p.number)));
    Ok(found)
}

/// GitHub's diff of a PR, or why `gh pr diff` could not read it.
pub type GhDiffRead = std::result::Result<place::GhDiff, String>;

/// Readies PR `number` for review: finds the known checkout of its repo
/// (`repo`, else the repo of `dir`), reads the PR, prepares its folder and
/// reads GitHub's diff of it. `known` holds every known session's checkout;
/// those of the same clone, with their branches, decide whether the PR is
/// reviewed in one of them.
pub fn open(
    r: &dyn Runner,
    dir: &Path,
    repo: Option<&str>,
    number: u64,
    known: &[PathBuf],
    me: &Me,
    state_dir: &Path,
) -> Result<(PrInfo, checkout::PrCheckout, GhDiffRead)> {
    let (dir, repo) = match repo {
        None => (dir.to_path_buf(), repo_of(r, dir)?),
        Some(repo) => {
            let mut tried = Vec::new();
            let found = std::iter::once(dir)
                .chain(known.iter().map(PathBuf::as_path))
                .filter(|d| {
                    let new = !tried.contains(d);
                    tried.push(*d);
                    new
                })
                .find(|d| repo_of(r, d).is_ok_and(|of| of.eq_ignore_ascii_case(repo)));
            match found {
                Some(d) => (d.to_path_buf(), repo.to_string()),
                None => bail!("No session is open in a clone of {repo}: start one there first"),
            }
        }
    };
    let info = view_pr(r, &dir, &repo, number, me)?;
    let main = checkout::main_clone(r, &dir)?;
    let branches: Vec<(PathBuf, Option<String>)> = known
        .iter()
        .filter(|k| checkout::main_clone(r, k).is_ok_and(|m| m == main))
        .map(|k| {
            let branch = r
                .run(k, "git", &["rev-parse", "--abbrev-ref", "HEAD"])
                .ok()
                .map(|b| b.trim().to_string())
                .filter(|b| !b.is_empty() && b != "HEAD");
            (k.clone(), branch)
        })
        .collect();
    let mut info = info;
    let prepared = checkout::prepare(r, &info, &main, &branches, state_dir)?;
    info.head_sha = checked_out(r, &prepared.dir)?;
    let diff = gh_diff(r, &prepared.dir, &repo, number);
    Ok((info, prepared, diff))
}

/// The commit checked out in `dir`: the one the user reviews, and so the
/// one a review of it is posted against.
fn checked_out(r: &dyn Runner, dir: &Path) -> Result<String> {
    let sha = r
        .run(dir, "git", &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if sha.is_empty() {
        bail!(
            "git did not name the commit checked out in {}",
            dir.display()
        );
    }
    Ok(sha)
}

/// GitHub's diff of PR `number`, or why `gh pr diff` could not read it.
fn gh_diff(r: &dyn Runner, dir: &Path, repo: &str, number: u64) -> GhDiffRead {
    let n = number.to_string();
    r.run_with(dir, "gh", &["pr", "diff", &n, "-R", repo], slow())
        .map(|text| place::parse_gh_diff(&text))
        .map_err(|e| format!("{e:#}"))
}

/// Someone else's PR read again, checked out again at its head on GitHub
/// (`prepare` refuses a worktree with uncommitted changes, and fetches the
/// PR's base as it is now), with GitHub's diff of it read again. The
/// head is the commit checked out.
pub fn refresh(
    r: &dyn Runner,
    info: &PrInfo,
    main: &Path,
    me: &Me,
    state_dir: &Path,
) -> Result<(PrInfo, checkout::PrCheckout, GhDiffRead)> {
    let dir = checkout::worktree_dir(state_dir, &info.repo, info.number);
    let at = if dir.exists() { dir.as_path() } else { main };
    let mut fresh = view_pr(r, at, &info.repo, info.number, me)?;
    let prepared = checkout::prepare(r, &fresh, main, &[], state_dir)?;
    fresh.head_sha = checked_out(r, &prepared.dir)?;
    let diff = gh_diff(r, &prepared.dir, &info.repo, info.number);
    Ok((fresh, prepared, diff))
}

/// Why a review was not posted, or may not have been.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostError {
    /// Nothing reached GitHub.
    NotPosted(String),
    /// The POST itself failed: GitHub may have made the review all the
    /// same. `started` is when the attempt began, in seconds since 1970.
    MayHavePosted { error: String, started: u64 },
    /// An earlier attempt that may have failed did make the review.
    Reached,
}

/// How much earlier than the failed attempt a review may say it was
/// submitted and still count as that attempt's: GitHub's clock and this
/// machine's can differ.
const CLOCK_SLACK: u64 = 60;

/// An earlier attempt to post that may have reached GitHub: the user's
/// login, when the first such attempt started (seconds since 1970), and
/// every head commit it was tried on.
#[derive(Debug, Clone, Copy)]
pub struct Earlier<'a> {
    pub login: &'a str,
    pub since: u64,
    pub heads: &'a [String],
}

/// Posts `body` as one review of PR `number`, only if the PR's head on
/// GitHub is still `head_sha`. With `earlier`, a review by its login on
/// any of its heads, with this body and event, submitted since it
/// started, means an earlier attempt posted it. `started` is when this
/// attempt began, in seconds since 1970.
#[allow(clippy::too_many_arguments)]
pub fn post_review(
    r: &dyn Runner,
    dir: &Path,
    repo: &str,
    number: u64,
    head_sha: &str,
    body: &Value,
    earlier: Option<Earlier>,
    started: u64,
) -> std::result::Result<(), PostError> {
    let not_posted = |e: anyhow::Error| PostError::NotPosted(format!("{e:#}"));
    let n = number.to_string();
    let out = r
        .run(
            dir,
            "gh",
            &["pr", "view", &n, "-R", repo, "--json", "headRefOid"],
        )
        .map_err(not_posted)?;
    let v: Value = serde_json::from_str(&out)
        .context("reading gh pr view")
        .map_err(not_posted)?;
    if text(&v["headRefOid"]) != head_sha {
        return Err(PostError::NotPosted(format!(
            "PR #{number} changed on GitHub since you opened it: press r to refresh"
        )));
    }
    let path = format!("repos/{repo}/pulls/{number}/reviews");
    if let Some(Earlier {
        login,
        since,
        heads,
    }) = earlier
    {
        let unread =
            |e: String| PostError::NotPosted(format!("could not read the PR's reviews: {e}"));
        let out = r
            .run_with(dir, "gh", &["api", &path, "--paginate"], slow())
            .map_err(|e| unread(format!("{e:#}")))?;
        let mut reviews = Vec::new();
        for page in serde_json::Deserializer::from_str(&out).into_iter::<Value>() {
            match page.map_err(|e| unread(e.to_string()))? {
                Value::Array(list) => reviews.extend(list),
                _ => return Err(unread("a page is not a list".into())),
            }
        }
        // Only a review with this attempt's event and text is this attempt's:
        // an earlier review of the user's on the same commit is not.
        let state = match text(&body["event"]).as_str() {
            "APPROVE" => "APPROVED",
            "REQUEST_CHANGES" => "CHANGES_REQUESTED",
            _ => "COMMENTED",
        };
        let reached = reviews.iter().any(|review| {
            text(&review["user"]["login"]).eq_ignore_ascii_case(login)
                && heads.contains(&text(&review["commit_id"]))
                && text(&review["state"]) == state
                && text(&review["body"]) == text(&body["body"])
                && epoch_secs(&text(&review["submitted_at"]))
                    .is_some_and(|at| at + CLOCK_SLACK >= since)
        });
        if reached {
            return Err(PostError::Reached);
        }
    }
    let input = body.to_string();
    let opts = RunOpts {
        timeout: SLOW_TIMEOUT,
        stdin: Some(&input),
    };
    r.run_with(
        dir,
        "gh",
        &["api", &path, "--method", "POST", "--input", "-"],
        opts,
    )
    .map_err(|e| {
        let error = format!("{e:#}");
        if refused(&error) {
            PostError::NotPosted(error)
        } else {
            PostError::MayHavePosted { error, started }
        }
    })?;
    Ok(())
}

/// True when gh's error says GitHub answered with a 4xx status: it refused
/// the request, so nothing was made. A 408 is GitHub timing out, which
/// may come after the review was made.
fn refused(error: &str) -> bool {
    error.match_indices("HTTP 4").any(|(i, _)| {
        let code = &error.as_bytes()[i + 5..];
        code.len() >= 3
            && code[1..3].iter().all(u8::is_ascii_digit)
            && code.get(3).is_none_or(|b| !b.is_ascii_digit())
            && &code[..3] != b"408"
    })
}

/// Seconds since 1970 of a UTC time as GitHub writes it
/// (`2026-11-19T10:00:00Z`).
fn epoch_secs(t: &str) -> Option<u64> {
    let t = t.strip_suffix('Z')?;
    let (date, time) = t.split_once('T')?;
    let num = |s: &str| s.parse::<i64>().ok();
    let mut d = date.split('-').map(num);
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut c = time
        .split(':')
        .map(|s| num(s.split('.').next().unwrap_or(s)));
    let (h, min, sec) = (c.next()??, c.next()??, c.next()??);
    // Days from 1970-01-01 to the date (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + sec).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Answers only the commands it was given; anything else fails.
    struct FakeRunner(HashMap<String, String>);

    impl FakeRunner {
        fn new(answers: &[(&str, &str)]) -> Self {
            Self(
                answers
                    .iter()
                    .map(|(a, out)| (format!("gh {a}"), out.to_string()))
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

    const REPO_ARGS: &str = "repo view --json nameWithOwner";
    const REPO_OUT: &str = r#"{"nameWithOwner":"o/r"}"#;
    const LIST_ARGS: &str =
        "pr list --state open --limit 100 --json number,title,author,headRefName,isDraft,updatedAt";
    const VIEW_ARGS: &str =
        "pr view 7 -R o/r --json number,title,body,author,headRefName,baseRefName,headRefOid,url";

    fn me() -> Me {
        Me {
            login: "Smnai".into(),
            orgs: vec![],
        }
    }

    const LIST_JSON: &str = r#"[
      {"number":1,"title":"Old","author":{"login":"bob","is_bot":false},"headRefName":"old","isDraft":false,"updatedAt":"2026-11-01T10:00:00Z"},
      {"number":2,"title":"Fresh","author":{"login":"smnai","is_bot":false},"headRefName":"fresh","isDraft":true,"updatedAt":"2026-11-18T09:00:00Z"}
    ]"#;

    fn view_json(login: &str) -> String {
        format!(
            r#"{{"number":7,"title":"T","body":"B","author":{{"login":"{login}","is_bot":false}},
            "headRefName":"feat","baseRefName":"main","headRefOid":"abc123","url":"https://github.com/o/r/pull/7"}}"#
        )
    }

    #[test]
    fn lists_open_prs_newest_first_and_marks_mine() {
        let r = FakeRunner::new(&[(REPO_ARGS, REPO_OUT), (LIST_ARGS, LIST_JSON)]);
        let prs = list_prs(&r, Path::new("/w"), &me()).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(
            prs[0],
            PrSummary {
                repo: "o/r".into(),
                number: 2,
                title: "Fresh".into(),
                author: "smnai".into(),
                head: "fresh".into(),
                draft: true,
                mine: true,
                updated: "2026-11-18T09:00:00Z".into(),
            }
        );
        assert_eq!(prs[1].number, 1);
        assert!(!prs[1].mine);
    }

    #[test]
    fn views_a_pr() {
        let json = view_json("smnai");
        let r = FakeRunner::new(&[(VIEW_ARGS, &json)]);
        let info = view_pr(&r, Path::new("/w"), "o/r", 7, &me()).unwrap();
        assert_eq!(
            info,
            PrInfo {
                repo: "o/r".into(),
                number: 7,
                title: "T".into(),
                body: "B".into(),
                author: "smnai".into(),
                head_ref: "feat".into(),
                base_ref: "main".into(),
                head_sha: "abc123".into(),
                url: "https://github.com/o/r/pull/7".into(),
                mine: true,
            }
        );
    }

    #[test]
    fn a_teammates_pr_is_not_mine() {
        let json = view_json("alice");
        let r = FakeRunner::new(&[(VIEW_ARGS, &json)]);
        let info = view_pr(&r, Path::new("/w"), "o/r", 7, &me()).unwrap();
        assert!(!info.mine);
    }

    #[test]
    fn parses_numbers_hashes_and_links() {
        let some = |repo: Option<&str>, n| Some((repo.map(String::from), n));
        assert_eq!(parse_pr_ref("5"), some(None, 5));
        assert_eq!(parse_pr_ref(" #5 "), some(None, 5));
        let link = Some("o/r");
        assert_eq!(parse_pr_ref("https://github.com/o/r/pull/5"), some(link, 5));
        assert_eq!(
            parse_pr_ref("https://github.com/o/r/pull/5/files"),
            some(link, 5)
        );
        assert_eq!(
            parse_pr_ref("https://github.com/o/r/pull/5?diff=split"),
            some(link, 5)
        );
        assert_eq!(parse_pr_ref("abc"), None);
        assert_eq!(parse_pr_ref("https://example.com/o/r/pull/5"), None);
        assert_eq!(parse_pr_ref("--repo=x/5"), None);
        assert_eq!(parse_pr_ref("https://github.com/o/r/issues/5"), None);
    }

    /// Answers by folder and command line (`"<dir>|<program> <args>"`);
    /// an answer of `Err` fails, anything unknown fails too.
    struct DirRunner(Vec<(String, Result<String, String>)>);

    impl DirRunner {
        fn new(answers: &[(&str, Result<&str, &str>)]) -> Self {
            Self(
                answers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.map(String::from).map_err(String::from)))
                    .collect(),
            )
        }
    }

    impl Runner for DirRunner {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
            let key = format!("{}|{program} {}", cwd.display(), args.join(" "));
            match self.0.iter().find(|(k, _)| *k == key) {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(anyhow::anyhow!("{e}")),
                None => Err(anyhow::anyhow!("unexpected command: {key}")),
            }
        }

        fn run_with(&self, cwd: &Path, program: &str, args: &[&str], _: RunOpts) -> Result<String> {
            self.run(cwd, program, args)
        }
    }

    const COMMON: &str = "git rev-parse --path-format=absolute --git-common-dir";

    fn list_json(number: u64, updated: &str) -> String {
        format!(
            r#"[{{"number":{number},"title":"T{number}","author":{{"login":"bob"}},"headRefName":"h","isDraft":false,"updatedAt":"{updated}"}}]"#
        )
    }

    #[test]
    fn list_all_lists_each_clone_once_and_merges_newest_first() {
        let (a, b) = (list_json(1, "2026-11-01"), list_json(2, "2026-11-02"));
        let list = |d: &str| format!("{d}|gh {LIST_ARGS}");
        let repo = |d: &str| format!("{d}|gh {REPO_ARGS}");
        let common = |d: &str| format!("{d}|{COMMON}");
        let keys = [list("/a"), list("/b"), repo("/a"), repo("/b")];
        let keys2 = [common("/a"), common("/a2"), common("/b")];
        let r = DirRunner::new(&[
            (&keys2[0], Ok("/a/.git")),
            (&keys2[1], Ok("/a/.git")),
            (&keys2[2], Ok("/b/.git")),
            (&keys[2], Ok(r#"{"nameWithOwner":"o/a"}"#)),
            (&keys[3], Ok(r#"{"nameWithOwner":"o/b"}"#)),
            (&keys[0], Ok(&a)),
            (&keys[1], Ok(&b)),
        ]);
        let dirs = ["/a", "/a2", "/b"].map(PathBuf::from);
        let listing = list_all(&r, &dirs, &me()).unwrap();
        assert_eq!(listing.repos, ["o/a", "o/b"]);
        let got: Vec<(&str, u64)> = listing
            .prs
            .iter()
            .map(|p| (p.repo.as_str(), p.number))
            .collect();
        assert_eq!(got, [("o/b", 2), ("o/a", 1)]);
    }

    #[test]
    fn list_all_fails_only_when_every_repo_fails() {
        let a = list_json(1, "2026-11-01");
        let ok_repo = format!("/a|gh {REPO_ARGS}");
        let ok_list = format!("/a|gh {LIST_ARGS}");
        let bad_repo = format!("/b|gh {REPO_ARGS}");
        let r = DirRunner::new(&[
            (&ok_repo, Ok(r#"{"nameWithOwner":"o/a"}"#)),
            (&ok_list, Ok(&a)),
            (&bad_repo, Err("not logged in")),
        ]);
        let both = ["/a", "/b"].map(PathBuf::from);
        let listing = list_all(&r, &both, &me()).unwrap();
        assert_eq!(listing.prs.len(), 1);
        assert_eq!(listing.repos, ["o/a"]);
        assert_eq!(listing.failed, ["Could not list /b: not logged in"]);
        let e = list_all(&r, &[PathBuf::from("/b")], &me()).unwrap_err();
        assert_eq!(format!("{e:#}"), "Could not list /b: not logged in");
    }

    #[test]
    fn open_finds_the_linked_repos_checkout_and_reviews_my_pr_on_its_branch() {
        let view = format!("/b|gh {VIEW_ARGS}");
        let json = view_json("smnai");
        let keys = [
            format!("/a|gh {REPO_ARGS}"),
            format!("/b|gh {REPO_ARGS}"),
            format!("/a|{COMMON}"),
            format!("/b|{COMMON}"),
            format!("/b2|{COMMON}"),
            "/b|git rev-parse --abbrev-ref HEAD".to_string(),
            "/b2|git rev-parse --abbrev-ref HEAD".to_string(),
            "/b2|git fetch origin -- main".to_string(),
            "/b2|gh pr diff 7 -R o/r".to_string(),
            "/b2|git rev-parse HEAD".to_string(),
        ];
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let r = DirRunner::new(&[
            (&keys[0], Ok(r#"{"nameWithOwner":"o/other"}"#)),
            (&keys[1], Ok(REPO_OUT)),
            (&keys[2], Ok("/a/.git")),
            (&keys[3], Ok("/b/.git")),
            (&keys[4], Ok("/b/.git")),
            (&keys[5], Ok("main\n")),
            (&keys[6], Ok("feat\n")),
            (&keys[7], Ok("")),
            (&keys[8], Ok(diff)),
            (&keys[9], Ok("local1\n")),
            (&view, Ok(&json)),
        ]);
        let known = ["/a", "/b", "/b2"].map(PathBuf::from);
        let (info, prepared, gh) = open(
            &r,
            Path::new("/a"),
            Some("o/r"),
            7,
            &known,
            &me(),
            Path::new("/state"),
        )
        .unwrap();
        assert_eq!(info.number, 7);
        assert_eq!(info.head_sha, "local1", "the commit checked out");
        assert_eq!(prepared.dir, PathBuf::from("/b2"));
        assert!(!prepared.made);
        assert_eq!(prepared.base_ref, "origin/main");
        assert!(gh.is_ok());
    }

    #[test]
    fn open_without_a_clone_of_the_repo_says_so() {
        let key = format!("/a|gh {REPO_ARGS}");
        let r = DirRunner::new(&[(&key, Ok(r#"{"nameWithOwner":"o/other"}"#))]);
        let e = open(
            &r,
            Path::new("/a"),
            Some("o/r"),
            7,
            &[PathBuf::from("/a")],
            &me(),
            Path::new("/state"),
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "No session is open in a clone of o/r: start one there first"
        );
    }

    /// Answers by command line, and records each call with its stdin.
    struct Recorder {
        answers: Vec<(String, Result<String, String>)>,
        calls: std::sync::Mutex<Vec<(String, Option<String>)>>,
    }

    impl Recorder {
        fn new(answers: &[(&str, Result<&str, &str>)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.map(String::from).map_err(String::from)))
                    .collect(),
                calls: Default::default(),
            }
        }

        fn calls(&self) -> Vec<(String, Option<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Recorder {
        fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
            self.run_with(
                cwd,
                program,
                args,
                RunOpts {
                    timeout: SLOW_TIMEOUT,
                    stdin: None,
                },
            )
        }

        fn run_with(
            &self,
            _: &Path,
            program: &str,
            args: &[&str],
            opts: RunOpts,
        ) -> Result<String> {
            let key = format!("{program} {}", args.join(" "));
            self.calls
                .lock()
                .unwrap()
                .push((key.clone(), opts.stdin.map(String::from)));
            match self.answers.iter().find(|(k, _)| *k == key) {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(anyhow::anyhow!("{e}")),
                None => Err(anyhow::anyhow!("unexpected command: {key}")),
            }
        }
    }

    const HEAD: &str = "gh pr view 7 -R o/r --json headRefOid";
    const POST: &str = "gh api repos/o/r/pulls/7/reviews --method POST --input -";
    const LIST_REVIEWS: &str = "gh api repos/o/r/pulls/7/reviews --paginate";
    const HEAD_OUT: &str = r#"{"headRefOid":"abc123"}"#;

    fn body() -> Value {
        serde_json::json!({
            "commit_id": "abc123",
            "event": "COMMENT",
            "body": "sum",
            "comments": [
                {"path": "a.rs", "line": 1, "side": "RIGHT", "body": "x"},
            ],
        })
    }

    fn post(r: &Recorder, earlier: Option<(&str, u64)>) -> std::result::Result<(), PostError> {
        let heads = ["abc123".to_string()];
        let earlier = earlier.map(|(login, since)| Earlier {
            login,
            since,
            heads: &heads,
        });
        post_review(
            r,
            Path::new("/w"),
            "o/r",
            7,
            "abc123",
            &body(),
            earlier,
            500,
        )
    }

    fn not_posted(e: &str) -> std::result::Result<(), PostError> {
        Err(PostError::NotPosted(e.into()))
    }

    #[test]
    fn posts_the_body_on_stdin_once_the_head_matches() {
        let r = Recorder::new(&[(HEAD, Ok(HEAD_OUT)), (POST, Ok("{}"))]);
        assert_eq!(post(&r, None), Ok(()));
        let calls = r.calls();
        assert_eq!(calls[0], (HEAD.to_string(), None));
        let (cmd, stdin) = &calls[1];
        assert_eq!(cmd, POST);
        let posted: Value = serde_json::from_str(stdin.as_deref().unwrap()).unwrap();
        assert_eq!(posted, body());
    }

    #[test]
    fn a_moved_head_is_refused() {
        let r = Recorder::new(&[(HEAD, Ok(r#"{"headRefOid":"def456"}"#)), (POST, Ok("{}"))]);
        assert_eq!(
            post(&r, None),
            not_posted("PR #7 changed on GitHub since you opened it: press r to refresh")
        );
        assert_eq!(r.calls().len(), 1, "nothing was posted");
        // A head check that fails posts nothing either.
        let r = Recorder::new(&[(HEAD, Err("HTTP 502")), (POST, Ok("{}"))]);
        assert_eq!(post(&r, None), not_posted("HTTP 502"));
        assert_eq!(r.calls().len(), 1);
    }

    #[test]
    fn a_failed_post_may_have_posted() {
        let r = Recorder::new(&[(HEAD, Ok(HEAD_OUT)), (POST, Err("timed out after 60s"))]);
        assert_eq!(
            post(&r, None),
            Err(PostError::MayHavePosted {
                error: "timed out after 60s".into(),
                started: 500,
            })
        );
    }

    #[test]
    fn a_post_github_refused_did_not_post() {
        for e in [
            "gh: Validation Failed (HTTP 422)",
            "gh: Not Found (HTTP 404)",
            "HTTP 403: Resource not accessible",
        ] {
            let r = Recorder::new(&[(HEAD, Ok(HEAD_OUT)), (POST, Err(e))]);
            assert_eq!(post(&r, None), not_posted(e), "{e}");
        }
        for e in [
            "gh took longer than 60 s",
            "gh: Server Error (HTTP 502)",
            "error connecting to api.github.com",
            "gh: Request Timeout (HTTP 408)",
            "HTTP 4000",
        ] {
            let r = Recorder::new(&[(HEAD, Ok(HEAD_OUT)), (POST, Err(e))]);
            assert_eq!(
                post(&r, None),
                Err(PostError::MayHavePosted {
                    error: e.into(),
                    started: 500,
                }),
                "{e}"
            );
        }
    }

    #[test]
    fn a_review_that_reached_github_on_an_earlier_head_is_not_posted_again() {
        // Posted on abc123 with an uncertain result; the PR was then
        // refreshed to def456, and the retry runs there.
        let mine = review_json("smnai", "abc123", "1970-01-01T00:08:30Z");
        let r = Recorder::new(&[
            (HEAD, Ok(r#"{"headRefOid":"def456"}"#)),
            (LIST_REVIEWS, Ok(&format!("[{mine}]"))),
            (POST, Ok("{}")),
        ]);
        let heads = ["abc123".to_string(), "def456".to_string()];
        let posted = post_review(
            &r,
            Path::new("/w"),
            "o/r",
            7,
            "def456",
            &body(),
            Some(Earlier {
                login: "smnai",
                since: 500,
                heads: &heads,
            }),
            700,
        );
        assert_eq!(posted, Err(PostError::Reached));
        assert!(r.calls().iter().all(|c| c.0 != POST));
    }

    /// A Comment review with the text "sum", as `body()` posts.
    fn review_json(login: &str, commit: &str, at: &str) -> String {
        review_as(login, commit, at, "COMMENTED", "sum")
    }

    fn review_as(login: &str, commit: &str, at: &str, state: &str, text: &str) -> String {
        format!(
            r#"{{"user":{{"login":"{login}"}},"commit_id":"{commit}","submitted_at":"{at}","state":"{state}","body":"{text}"}}"#
        )
    }

    #[test]
    fn after_an_uncertain_post_one_that_reached_github_is_not_posted_again() {
        // 1970-01-01T00:08:20Z is 500 s.
        let mine = review_json("smnai", "abc123", "1970-01-01T00:08:20Z");
        let pages = format!(
            "[{}][{mine}]",
            review_json("bob", "abc123", "1970-01-01T00:09:00Z")
        );
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Ok(&pages)),
            (POST, Ok("{}")),
        ]);
        assert_eq!(post(&r, Some(("Smnai", 500))), Err(PostError::Reached));
        assert!(r.calls().iter().all(|c| c.0 != POST));
    }

    #[test]
    fn after_an_uncertain_post_that_did_not_reach_github_it_posts() {
        // Mine, but older, on another commit, or still pending; and bob's.
        let pages = format!(
            "[{},{},{},{}]",
            review_json("smnai", "abc123", "1970-01-01T00:01:00Z"),
            review_json("smnai", "old999", "1970-01-01T00:09:00Z"),
            r#"{"user":{"login":"smnai"},"commit_id":"abc123","submitted_at":null}"#,
            review_json("bob", "abc123", "1970-01-01T00:09:00Z"),
        );
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Ok(&pages)),
            (POST, Ok("{}")),
        ]);
        assert_eq!(post(&r, Some(("smnai", 500))), Ok(()));
        let cmds: Vec<String> = r.calls().into_iter().map(|c| c.0).collect();
        assert_eq!(cmds, [HEAD, LIST_REVIEWS, POST]);
        // A list that cannot be read posts nothing.
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Err("HTTP 502")),
            (POST, Ok("{}")),
        ]);
        assert_eq!(
            post(&r, Some(("smnai", 500))),
            not_posted("could not read the PR's reviews: HTTP 502")
        );
        assert!(r.calls().iter().all(|c| c.0 != POST));
    }

    #[test]
    fn a_review_list_that_does_not_parse_posts_nothing() {
        for out in ["[{\"user\":", "{\"message\":\"oops\"}"] {
            let r = Recorder::new(&[
                (HEAD, Ok(HEAD_OUT)),
                (LIST_REVIEWS, Ok(out)),
                (POST, Ok("{}")),
            ]);
            let Err(PostError::NotPosted(e)) = post(&r, Some(("smnai", 500))) else {
                panic!("{out} was read as a list");
            };
            assert!(e.starts_with("could not read the PR's reviews: "), "{e}");
            assert!(r.calls().iter().all(|c| c.0 != POST));
        }
    }

    #[test]
    fn an_earlier_review_of_another_kind_does_not_block_a_retry() {
        // A Comment review the user posted just before, on the same commit;
        // then an Approve with no text timed out.
        let earlier = review_as("smnai", "abc123", "1970-01-01T00:08:00Z", "COMMENTED", "");
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Ok(&format!("[{earlier}]"))),
            (POST, Ok("{}")),
        ]);
        let approve = serde_json::json!({
            "commit_id": "abc123", "event": "APPROVE", "body": "", "comments": [],
        });
        let posted = post_review(
            &r,
            Path::new("/w"),
            "o/r",
            7,
            "abc123",
            &approve,
            Some(Earlier {
                login: "smnai",
                since: 500,
                heads: &["abc123".to_string()],
            }),
            600,
        );
        assert_eq!(posted, Ok(()));
        assert!(r.calls().iter().any(|c| c.0 == POST));
        // Another text with the same event does not count either.
        let earlier = review_as(
            "smnai",
            "abc123",
            "1970-01-01T00:08:30Z",
            "COMMENTED",
            "other",
        );
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Ok(&format!("[{earlier}]"))),
            (POST, Ok("{}")),
        ]);
        assert_eq!(post(&r, Some(("smnai", 500))), Ok(()));
        // The approval itself does.
        let it = review_as("smnai", "abc123", "1970-01-01T00:08:30Z", "APPROVED", "");
        let r = Recorder::new(&[
            (HEAD, Ok(HEAD_OUT)),
            (LIST_REVIEWS, Ok(&format!("[{it}]"))),
            (POST, Ok("{}")),
        ]);
        let posted = post_review(
            &r,
            Path::new("/w"),
            "o/r",
            7,
            "abc123",
            &approve,
            Some(Earlier {
                login: "smnai",
                since: 500,
                heads: &["abc123".to_string()],
            }),
            600,
        );
        assert_eq!(posted, Err(PostError::Reached));
    }

    #[test]
    fn reads_githubs_times() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2026-11-19T10:00:00Z"), Some(1_795_082_400));
        assert_eq!(epoch_secs("2000-03-01T00:00:01Z"), Some(951_868_801));
        assert_eq!(epoch_secs(""), None);
        assert_eq!(epoch_secs("2026-11-19"), None);
    }

    #[test]
    fn refresh_posts_against_the_commit_checked_out() {
        let state = std::env::temp_dir().join(format!("aq-refresh-{}", std::process::id()));
        let dir = checkout::worktree_dir(&state, "o/r", 7);
        std::fs::create_dir_all(&dir).unwrap();
        // GitHub says def456, but the author pushed again before the
        // checkout: the worktree is on fed789.
        let json = view_json("alice")
            .replace("abc123", "def456")
            .replace("\"main\"", "\"dev\"");
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let r = Recorder::new(&[
            (&format!("gh {VIEW_ARGS}"), Ok(&json)),
            ("git status --porcelain", Ok("")),
            ("gh pr checkout 7 -R o/r --detach", Ok("")),
            ("git fetch origin -- dev", Ok("")),
            ("git rev-parse HEAD", Ok("fed789\n")),
            ("gh pr diff 7 -R o/r", Ok(diff)),
            (
                "gh pr view 7 -R o/r --json headRefOid",
                Ok(r#"{"headRefOid":"abc999"}"#),
            ),
        ]);
        let old = view_pr(
            &FakeRunner::new(&[(VIEW_ARGS, &view_json("alice"))]),
            Path::new("/w"),
            "o/r",
            7,
            &me(),
        )
        .unwrap();
        let (info, prepared, gh) = refresh(&r, &old, Path::new("/m"), &me(), &state).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
        assert_eq!(info.head_sha, "fed789");
        assert_eq!(prepared.dir, dir);
        assert_eq!(prepared.base_ref, "origin/dev", "the base as it is now");
        assert!(prepared.made);
        assert!(gh.is_ok());
        let cmds: Vec<String> = r.calls().into_iter().map(|c| c.0).collect();
        assert_eq!(
            cmds,
            [
                &format!("gh {VIEW_ARGS}"),
                "git status --porcelain",
                "gh pr checkout 7 -R o/r --detach",
                "git fetch origin -- dev",
                "git rev-parse HEAD",
                "gh pr diff 7 -R o/r",
            ]
        );
        // GitHub's head is not the commit reviewed: nothing is posted.
        let e = post_review(&r, &dir, "o/r", 7, &info.head_sha, &body(), None, 0);
        assert_eq!(
            e,
            not_posted("PR #7 changed on GitHub since you opened it: press r to refresh")
        );
        assert!(r.calls().iter().all(|c| !c.0.contains("--method POST")));
    }
}
