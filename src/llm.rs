//! Background `claude -p` work: quiz questions and judging answers.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
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
    pub fn schema(&self) -> &'static str {
        match self.kind {
            JobKind::Quiz => QUIZ_SCHEMA,
            JobKind::Judge { .. } => JUDGE_SCHEMA,
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

/// The real model, through the user's own `claude` login.
pub struct ClaudeCli;

impl Llm for ClaudeCli {
    fn run(&self, job: &LlmJob) -> Result<String> {
        let mut child = Command::new("claude")
            .args(["-p", "--output-format", "json", "--json-schema"])
            .arg(job.schema())
            .args(["--no-session-persistence", "--permission-mode", "plan"])
            .current_dir(&job.cwd)
            // The user's Claude login, as in their own sessions; and no tmux,
            // so hooks do not take this for the user's live window.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting claude")?;
        // Every pipe on its own thread, so a full pipe never stalls the others.
        let mut stdin = child.stdin.take().context("claude stdin")?;
        let prompt = job.prompt.clone();
        let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let out = read_all(child.stdout.take().context("claude stdout")?);
        let err = read_all(child.stderr.take().context("claude stderr")?);
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().context("waiting for claude")? {
                break status;
            }
            if start.elapsed() >= TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                bail!("claude took longer than {} s", TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = writer.join();
        let out = out.join().unwrap_or_default();
        let err = err.join().unwrap_or_default();
        if !status.success() && out.trim().is_empty() {
            bail!("claude failed ({status}): {}", err.trim());
        }
        structured_output(&out)
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe.read_to_string(&mut s);
        s
    })
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
}
