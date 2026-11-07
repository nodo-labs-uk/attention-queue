//! Background `claude -p` work: quiz questions and judging answers.

use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

pub const QUIZ_PROMPT: &str = include_str!("../prompts/quiz.md");
pub const JUDGE_PROMPT: &str = include_str!("../prompts/judge.md");
pub const QUIZ_SCHEMA: &str = include_str!("../schemas/quiz.json");
pub const JUDGE_SCHEMA: &str = include_str!("../schemas/judge.json");

/// How long one `claude -p` call may take before it is killed.
pub const TIMEOUT: Duration = Duration::from_secs(180);

/// What a job is for, so its result finds its way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// Write the questions for the quiz card.
    Quiz,
    /// Judge the free-text answer to question `question`.
    Judge { question: usize },
    /// A side question, number `index` of the ask overlay. With a session it
    /// is a fork of that live session; without, a plain call.
    Ask {
        index: usize,
        session_id: Option<String>,
    },
}

/// One `claude -p` call: the prompt goes on stdin, run in `cwd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmJob {
    pub card: u64,
    pub kind: JobKind,
    pub cwd: PathBuf,
    pub prompt: String,
}

impl LlmJob {
    /// The JSON schema the answer must follow; side questions answer in text.
    pub fn schema(&self) -> Option<&'static str> {
        match self.kind {
            JobKind::Quiz => Some(QUIZ_SCHEMA),
            JobKind::Judge { .. } => Some(JUDGE_SCHEMA),
            JobKind::Ask { .. } => None,
        }
    }
}

/// A finished job: the JSON object the model gave, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmEvent {
    pub card: u64,
    pub kind: JobKind,
    pub result: Result<String, String>,
}

pub trait Llm: Send + Sync {
    /// The structured output, as JSON text.
    fn run(&self, job: &LlmJob) -> Result<String>;
}

/// Runs `job` on `llm` and wraps the outcome for the app.
pub fn run_job(llm: &dyn Llm, job: LlmJob) -> LlmEvent {
    LlmEvent {
        result: llm.run(&job).map_err(|e| format!("{e:#}")),
        card: job.card,
        kind: job.kind,
    }
}

/// Process groups of the `claude` children still running, so they can be
/// killed when aq quits.
#[derive(Debug, Default)]
pub struct Children(Mutex<BTreeSet<i32>>);

impl Children {
    pub fn is_empty(&self) -> bool {
        self.0.lock().map_or(true, |s| s.is_empty())
    }

    /// SIGKILLs every registered process group.
    pub fn kill_all(&self) {
        if let Ok(groups) = self.0.lock() {
            for &g in groups.iter() {
                kill_group(g);
            }
        }
    }

    fn add(&self, g: i32) {
        if let Ok(mut s) = self.0.lock() {
            s.insert(g);
        }
    }

    fn remove(&self, g: i32) {
        if let Ok(mut s) = self.0.lock() {
            s.remove(&g);
        }
    }
}

fn kill_group(g: i32) {
    // SAFETY: killpg only sends a signal; `g` is a group this process made.
    unsafe {
        libc::killpg(g, libc::SIGKILL);
    }
}

/// The real model, through the user's own `claude` login.
pub struct ClaudeCli {
    pub children: Arc<Children>,
}

impl Llm for ClaudeCli {
    fn run(&self, job: &LlmJob) -> Result<String> {
        if matches!(job.kind, JobKind::Ask { .. }) {
            return ask_with_fallback(job, |j| self.once(j, ask_output));
        }
        self.once(job, structured_output)
    }
}

impl ClaudeCli {
    fn once(&self, job: &LlmJob, parse: fn(&str) -> Result<String>) -> Result<String> {
        let (status, out, err) =
            run_child(claude_command(job), &job.prompt, TIMEOUT, &self.children)?;
        if !status.success() && out.trim().is_empty() {
            bail!("claude failed ({status}): {}", err.trim());
        }
        parse(&out)
    }
}

/// Runs a side question as a fork of its session; if that fails for any
/// reason, once more as a plain call in the checkout.
pub fn ask_with_fallback(
    job: &LlmJob,
    mut attempt: impl FnMut(&LlmJob) -> Result<String>,
) -> Result<String> {
    let JobKind::Ask { index, session_id } = &job.kind else {
        return attempt(job);
    };
    if session_id.is_none() {
        return attempt(job);
    }
    attempt(job).or_else(|fork_err| {
        let plain = LlmJob {
            kind: JobKind::Ask {
                index: *index,
                session_id: None,
            },
            ..job.clone()
        };
        attempt(&plain)
            .map_err(|e| anyhow!("{e:#} (the fork of the live session failed first: {fork_err:#})"))
    })
}

/// `claude -p` for `job`: read-only tools, no MCP servers, slash commands,
/// skills or hooks, so text in the diff cannot make it change anything.
pub fn claude_command(job: &LlmJob) -> Command {
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--output-format", "json"]);
    if let Some(schema) = job.schema() {
        cmd.arg("--json-schema").arg(schema);
    }
    // A fork of the live session. That session may allow writes, but the
    // flags below still apply to the fork.
    if let JobKind::Ask {
        session_id: Some(id),
        ..
    } = &job.kind
    {
        cmd.args(["--resume", id, "--fork-session"]);
    }
    cmd.args([
        "--no-session-persistence",
        "--permission-mode",
        "plan",
        "--tools",
        "Read,Grep,Glob",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--settings",
        r#"{"disableAllHooks":true}"#,
    ])
    .current_dir(&job.cwd)
    // The user's Claude login, as in their own sessions; and no tmux,
    // so nothing takes this for the user's live window.
    .env_remove("ANTHROPIC_API_KEY")
    .env_remove("TMUX")
    .env_remove("TMUX_PANE");
    cmd
}

/// The `result` text of `claude -p --output-format json`.
pub fn ask_output(stdout: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).context("claude printed no JSON")?;
    let text = v.get("result").and_then(serde_json::Value::as_str);
    if v.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
        bail!(
            "claude reported an error: {}",
            text.unwrap_or("unknown error")
        );
    }
    match text {
        Some(t) if !t.trim().is_empty() => Ok(t.to_string()),
        _ => bail!("claude gave no answer"),
    }
}

/// Runs `cmd` in its own process group with `input` on stdin, and returns
/// its status, stdout and stderr. Past `timeout`, counting until both
/// outputs close, the whole group is killed and this is an error. The
/// group is in `children` while it runs; the child dies with aq.
pub fn run_child(
    mut cmd: Command,
    input: &str,
    timeout: Duration,
    children: &Children,
) -> Result<(ExitStatus, String, String)> {
    let deadline = Instant::now() + timeout;
    // SAFETY: getpid is async-signal-safe.
    let parent = unsafe { libc::getpid() };
    cmd.process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // aq died before the line above took effect.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting the command")?;
    let group = child.id() as i32;
    children.add(group);
    let result = wait_child(&mut child, group, input, deadline, timeout);
    children.remove(group);
    result
}

fn wait_child(
    child: &mut Child,
    group: i32,
    input: &str,
    deadline: Instant,
    timeout: Duration,
) -> Result<(ExitStatus, String, String)> {
    let late = || anyhow!("took longer than {} s", timeout.as_secs());
    // Every pipe on its own thread, so a full pipe never stalls the others.
    // The writer is left alone: it ends when the pipe closes.
    let mut stdin = child.stdin.take().context("child stdin")?;
    let input = input.to_string();
    std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let (tx, rx) = mpsc::channel();
    read_all(child.stdout.take().context("child stdout")?, 0, tx.clone());
    read_all(child.stderr.take().context("child stderr")?, 1, tx);
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for the child")? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_group(group);
            let _ = child.wait();
            return Err(late());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Something the child started may still hold its output open.
    let mut outs = [String::new(), String::new()];
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((i, s)) => outs[i] = s,
            Err(_) => {
                kill_group(group);
                return Err(late());
            }
        }
    }
    let [out, err] = outs;
    Ok((status, out, err))
}

fn read_all(mut pipe: impl Read + Send + 'static, i: usize, tx: mpsc::Sender<(usize, String)>) {
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe.read_to_string(&mut s);
        let _ = tx.send((i, s));
    });
}

/// The `structured_output` object of `claude -p --output-format json`
/// (`result` holds the same as a string).
pub fn structured_output(stdout: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).context("claude printed no JSON")?;
    if v.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
        let why = v
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        bail!("claude reported an error: {why}");
    }
    v.get("structured_output")
        .filter(|o| o.is_object())
        .map(serde_json::Value::to_string)
        .ok_or_else(|| anyhow!("claude gave no structured output"))
}

/// Answers jobs from a list, in order, for tests.
pub struct FakeLlm(Mutex<VecDeque<Result<String>>>);

impl FakeLlm {
    pub fn new(results: Vec<Result<String>>) -> Self {
        FakeLlm(Mutex::new(results.into()))
    }
}

impl Llm for FakeLlm {
    fn run(&self, _job: &LlmJob) -> Result<String> {
        self.0
            .lock()
            .map_err(|_| anyhow!("fake poisoned"))?
            .pop_front()
            .unwrap_or_else(|| Err(anyhow!("no more fake answers")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_output_is_preferred_and_errors_are_reported() {
        let ok = r#"{"type":"result","is_error":false,"result":"{\"a\":1}","structured_output":{"a":1}}"#;
        assert_eq!(structured_output(ok).unwrap(), r#"{"a":1}"#);
        let err = r#"{"type":"result","is_error":true,"result":"Not logged in"}"#;
        let msg = structured_output(err).unwrap_err().to_string();
        assert!(msg.contains("Not logged in"), "{msg}");
        assert!(structured_output("oops").is_err());
        assert!(structured_output(r#"{"result":"x"}"#).is_err());
    }

    #[test]
    fn schemas_are_json() {
        for s in [QUIZ_SCHEMA, JUDGE_SCHEMA] {
            serde_json::from_str::<serde_json::Value>(s).unwrap();
        }
    }

    #[test]
    fn fake_answers_in_order() {
        let job = LlmJob {
            card: 1,
            kind: JobKind::Quiz,
            cwd: "/".into(),
            prompt: String::new(),
        };
        let fake = FakeLlm::new(vec![Ok("a".into()), Err(anyhow!("b"))]);
        assert_eq!(run_job(&fake, job.clone()).result, Ok("a".into()));
        assert_eq!(run_job(&fake, job.clone()).result, Err("b".into()));
        assert!(run_job(&fake, job).result.is_err());
    }

    #[test]
    fn claude_gets_only_read_tools_and_no_hooks() {
        let job = LlmJob {
            card: 1,
            kind: JobKind::Judge { question: 0 },
            cwd: "/tmp".into(),
            prompt: String::new(),
        };
        let cmd = claude_command(&job);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
        for pair in [
            ["--tools", "Read,Grep,Glob"],
            ["--permission-mode", "plan"],
            ["--settings", r#"{"disableAllHooks":true}"#],
            ["--json-schema", JUDGE_SCHEMA],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{pair:?} in {args:?}");
        }
        for flag in [
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag} in {args:?}");
        }
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into())
            .collect();
        for k in ["ANTHROPIC_API_KEY", "TMUX", "TMUX_PANE"] {
            assert!(removed.iter().any(|r| r == k), "{k} removed: {removed:?}");
        }
    }

    fn ask_args(session_id: Option<&str>) -> Vec<String> {
        let job = LlmJob {
            card: 1,
            kind: JobKind::Ask {
                index: 0,
                session_id: session_id.map(Into::into),
            },
            cwd: "/tmp".into(),
            prompt: String::new(),
        };
        claude_command(&job)
            .get_args()
            .map(|a| a.to_string_lossy().into())
            .collect()
    }

    #[test]
    fn ask_fork_is_locked_down_like_every_other_call() {
        let args = ask_args(Some("abc"));
        for pair in [
            ["--resume", "abc"],
            ["--tools", "Read,Grep,Glob"],
            ["--permission-mode", "plan"],
            ["--settings", r#"{"disableAllHooks":true}"#],
            ["--output-format", "json"],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{pair:?} in {args:?}");
        }
        for flag in [
            "--fork-session",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag} in {args:?}");
        }
        assert!(!args.iter().any(|a| a == "--json-schema"));
        let plain = ask_args(None);
        assert!(
            !plain
                .iter()
                .any(|a| a == "--resume" || a == "--fork-session")
        );
        assert!(plain.iter().any(|a| a == "--tools"));
    }

    #[test]
    fn ask_output_reads_the_result_text() {
        let ok = r#"{"is_error":false,"result":"kiwi"}"#;
        assert_eq!(ask_output(ok).unwrap(), "kiwi");
        assert!(ask_output(r#"{"is_error":true,"result":"Not logged in"}"#).is_err());
        assert!(ask_output(r#"{"is_error":false,"result":" "}"#).is_err());
        assert!(ask_output("oops").is_err());
    }

    /// Needs a real session: `AQ_ASK_SESSION=<id> AQ_ASK_DIR=<its cwd>`.
    #[test]
    #[ignore = "calls the real claude"]
    fn real_fork_answers_from_the_live_session() {
        let (Ok(id), Ok(dir)) = (std::env::var("AQ_ASK_SESSION"), std::env::var("AQ_ASK_DIR"))
        else {
            return;
        };
        let cli = ClaudeCli {
            children: Arc::new(Children::default()),
        };
        let job = LlmJob {
            card: 1,
            kind: JobKind::Ask {
                index: 0,
                session_id: Some(id),
            },
            cwd: dir.into(),
            prompt: "what word did I ask you to remember?".into(),
        };
        let answer = cli.run(&job).unwrap();
        println!("ANSWER: {answer}");
        assert!(answer.to_lowercase().contains("kiwi"), "{answer}");
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn child_output_and_registry_empty_after() {
        let children = Children::default();
        let out = run_child(sh("cat"), "hello", Duration::from_secs(10), &children).unwrap();
        assert_eq!(out.1, "hello");
        assert!(children.is_empty());
    }

    #[test]
    fn background_child_holding_stdout_times_out() {
        let children = Children::default();
        let start = Instant::now();
        // The shell exits at once; its `sleep` keeps stdout open.
        let r = run_child(
            sh("sleep 600 & echo started"),
            "",
            Duration::from_secs(1),
            &children,
        );
        let took = start.elapsed();
        let e = r.unwrap_err().to_string();
        assert!(e.contains("longer than"), "{e}");
        assert!(took < Duration::from_secs(4), "{took:?}");
        assert!(children.is_empty());
    }

    #[test]
    fn slow_child_is_killed_with_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let children = Children::default();
        let start = Instant::now();
        let r = run_child(
            sh(&format!(
                "sleep 600 & echo $! > {}; wait",
                pidfile.display()
            )),
            "",
            Duration::from_secs(1),
            &children,
        );
        assert!(r.is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(gone(pid), "the background sleep was killed too");
    }

    /// Dead or a zombie.
    fn gone(pid: i32) -> bool {
        for _ in 0..50 {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
            let alive = state.is_some_and(|s| {
                s.rsplit_once(") ")
                    .is_some_and(|(_, rest)| !rest.starts_with('Z'))
            });
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn kill_all_kills_a_running_child() {
        let children = std::sync::Arc::new(Children::default());
        let c = children.clone();
        let start = Instant::now();
        let job = std::thread::spawn(move || {
            run_child(sh("sleep 600"), "", Duration::from_secs(600), &c)
        });
        while children.is_empty() {
            assert!(start.elapsed() < Duration::from_secs(5), "never registered");
            std::thread::sleep(Duration::from_millis(10));
        }
        children.kill_all();
        let r = job.join().unwrap();
        assert!(r.is_ok_and(|(status, ..)| !status.success()));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(children.is_empty());
    }
}
