//! `aq hook <event>`: a Claude Code hook that forwards the event to the
//! running `aq` and prints its decision. Blocking std I/O only.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::current_uid;
use crate::flag::candidate::{candidates, is_flag_tool};
use crate::ipc::{HookReply, HookRequest};

/// What a step-mode edit with no answer tells the agent.
pub const WAITING_REASON: &str = "Waiting for human review in aq. Try this edit again shortly.";

/// What a flagged action with no answer tells the agent.
pub const FLAG_WAITING_REASON: &str = "Held in aq: this action needs the developer's answer there. Do not try another way to do the same thing; tell them it is waiting in aq.";

/// Seconds the hook waits by default; the hook command's `timeout` is 3600.
pub const DEFAULT_DEADLINE_SECS: u64 = 3570;

/// The most a PostToolUse hook spends handing its report to aq.
const POST_TOOL_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HookEvent {
    PreTool,
    PostTool,
    Permission,
}

/// Runs the hook against the configured socket, with `$TMUX_PANE` as the pane.
pub fn run_hook(event: HookEvent, stdin: impl Read, deadline: Duration) -> Result<Option<String>> {
    let socket = crate::config::paths().socket;
    run_hook_with(&socket, tmux_pane, event, stdin, deadline)
}

/// The tmux pane this runs in, if any: `$TMUX_PANE`, or else the pane
/// found from the process's parents.
pub fn tmux_pane() -> Option<String> {
    std::env::var("TMUX_PANE")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| crate::agents::tmux::pane_of_process(std::process::id()))
}

/// How long `aq tell` waits for aq.
const TELL_WAIT: Duration = Duration::from_secs(5);

/// `aq tell`: sends `req` to aq at `socket`, and returns whether it
/// worked and what to print.
pub fn tell(socket: &Path, req: &HookRequest) -> (bool, String) {
    let unanswered = || (false, "aq did not answer, so nothing was sent".to_string());
    match connect(socket, TELL_WAIT) {
        Connect::NoAq => (false, "aq is not running, so nothing was sent".to_string()),
        Connect::TimedOut => unanswered(),
        Connect::Connected(stream) => match exchange(stream, req, Instant::now() + TELL_WAIT) {
            Some(HookReply::Note { ok, text }) => (ok, text),
            _ => unanswered(),
        },
    }
}

/// Returns the JSON to print on stdout, or `None` to print nothing. `pane`
/// is called only once the call is known to go to aq, so an ordinary Bash
/// call never looks for its tmux pane.
pub fn run_hook_with(
    socket: &Path,
    pane: impl FnOnce() -> Option<String>,
    event: HookEvent,
    mut stdin: impl Read,
    deadline: Duration,
) -> Result<Option<String>> {
    let mut input = String::new();
    stdin
        .read_to_string(&mut input)
        .context("reading hook input")?;
    let input: Value = serde_json::from_str(&input).context("parsing hook input")?;
    let tool_name = input["tool_name"].as_str().unwrap_or_default();
    let flag_tool = event == HookEvent::PreTool && is_flag_tool(tool_name);
    if flag_tool {
        match is_candidate(|| !candidates(tool_name, &input["tool_input"]).is_empty()) {
            // Ordinary commands are the common case: leave without contacting aq.
            Some(false) => return Ok(None),
            Some(true) => {}
            // The check itself failed: silence must not let the call through.
            None => return Ok(output(event, true, None)),
        }
    }
    let req = request(event, &input, pane());

    if event == HookEvent::PostTool {
        // The tool already ran: report it and go, so nothing waits on aq.
        send_only(socket, &req);
        return Ok(None);
    }
    let end = Instant::now() + deadline;
    let reply = match connect(socket, deadline) {
        // No aq of ours listening: Claude behaves as if there were no hook.
        Connect::NoAq => return Ok(None),
        Connect::TimedOut => None,
        Connect::Connected(stream) => exchange(stream, &req, end),
    };
    // An option menu with no answer shows in the window as usual.
    if reply.is_none() && input["tool_name"] == crate::choice::TOOL {
        return Ok(None);
    }
    Ok(output(event, flag_tool, reply))
}

/// Runs the candidate check, or `None` if it panicked.
fn is_candidate(check: impl FnOnce() -> bool) -> Option<bool> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)).ok()
}

enum Connect {
    NoAq,
    /// Our aq's socket exists but did not take the connection in time.
    TimedOut,
    Connected(UnixStream),
}

/// Connects to our own user's aq, waiting at most `wait`. The socket's
/// directory must be private, and the listener must run as this user.
fn connect(socket: &Path, wait: Duration) -> Connect {
    let private = socket
        .parent()
        .is_some_and(|dir| crate::ipc::check_private_dir(dir).is_ok());
    if !private {
        return Connect::NoAq;
    }
    // A blocking connect to a stuck aq can hang, so it runs on a thread that
    // is left behind if it does not finish in time.
    let (tx, rx) = mpsc::channel();
    let path = socket.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(path));
    });
    match rx.recv_timeout(wait) {
        Ok(Ok(stream)) if crate::ipc::peer_uid(&stream).ok() == Some(current_uid()) => {
            Connect::Connected(stream)
        }
        Ok(_) => Connect::NoAq,
        Err(_) => Connect::TimedOut,
    }
}

/// Sends the request and waits for one reply line until `end`. `None` means
/// no answer: the deadline passed, the connection closed, or the reply was
/// unreadable.
fn exchange(mut stream: UnixStream, req: &HookRequest, end: Instant) -> Option<HookReply> {
    // A zero timeout is an error for the socket, so wait at least 1 ms.
    let left = || {
        end.saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1))
    };
    stream.set_write_timeout(Some(left())).ok()?;
    let mut line = serde_json::to_string(req).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    stream.set_read_timeout(Some(left())).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    serde_json::from_str(&reply).ok()
}

/// Sends the request and does not wait for a reply. Failures are ignored: a
/// missed report only means a missed card.
fn send_only(socket: &Path, req: &HookRequest) {
    let end = Instant::now() + POST_TOOL_WAIT;
    if let Connect::Connected(mut stream) = connect(socket, POST_TOOL_WAIT)
        && let Ok(mut line) = serde_json::to_string(req)
    {
        line.push('\n');
        let left = end.saturating_duration_since(Instant::now());
        let _ = stream.set_write_timeout(Some(left.max(Duration::from_millis(1))));
        let _ = stream.write_all(line.as_bytes());
    }
}

fn request(event: HookEvent, input: &Value, pane: Option<String>) -> HookRequest {
    let s = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    let tool_input = input.get("tool_input").cloned().unwrap_or(Value::Null);
    match event {
        HookEvent::PreTool => HookRequest::PreTool {
            session_id: s("session_id"),
            transcript_path: s("transcript_path"),
            cwd: s("cwd"),
            tool_name: s("tool_name"),
            tool_use_id: s("tool_use_id"),
            tool_input,
            pane,
        },
        HookEvent::PostTool => HookRequest::PostTool {
            session_id: s("session_id"),
            cwd: s("cwd"),
            tool_name: s("tool_name"),
            tool_input,
        },
        HookEvent::Permission => HookRequest::Permission {
            session_id: s("session_id"),
            cwd: s("cwd"),
            tool_name: s("tool_name"),
            tool_input,
            pane,
        },
    }
}

/// The hook's stdout for a reply (`None` when no reply came).
fn output(event: HookEvent, flag_tool: bool, reply: Option<HookReply>) -> Option<String> {
    let specific = match (event, reply) {
        (HookEvent::PreTool, Some(HookReply::Allow)) => json!({
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
        }),
        (HookEvent::PreTool, Some(HookReply::Answer { input })) => json!({
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": input,
        }),
        // Only a PreToolUse hook is ever given an answer.
        (HookEvent::Permission, Some(HookReply::Answer { .. })) => return None,
        (HookEvent::PreTool, Some(HookReply::Deny { reason })) => pre_tool_deny(&reason),
        (HookEvent::PreTool, Some(HookReply::NoDecision)) => return None,
        // A flagged action with no answer is refused, never let through.
        (HookEvent::PreTool, None) if flag_tool => pre_tool_deny(FLAG_WAITING_REASON),
        // Step-mode edits with no answer are refused, never let through.
        (HookEvent::PreTool, None) => pre_tool_deny(WAITING_REASON),
        (HookEvent::Permission, Some(HookReply::Allow)) => json!({
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "allow"},
        }),
        (HookEvent::Permission, Some(HookReply::Deny { reason })) => json!({
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": reason},
        }),
        // No answer leaves the normal prompt waiting in the window.
        (HookEvent::Permission, Some(HookReply::NoDecision) | None) => return None,
        // Shell-change cards come after the fact; the hook only reports.
        (HookEvent::PostTool, _) => return None,
        // Only `aq tell` is answered with a note.
        (_, Some(HookReply::Note { .. })) => return None,
    };
    Some(json!({ "hookSpecificOutput": specific }).to_string())
}

fn pre_tool_deny(reason: &str) -> Value {
    json!({
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    use super::*;

    fn bash(command: &str) -> String {
        json!({
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": command},
        })
        .to_string()
    }

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn run(socket: &Path, input: &str) -> Option<String> {
        run_hook_with(
            socket,
            || None,
            HookEvent::PreTool,
            input.as_bytes(),
            Duration::from_secs(5),
        )
        .unwrap()
    }

    /// A socket that accepts one connection and closes it without replying.
    fn closing_listener(dir: &Path) -> std::path::PathBuf {
        let socket = dir.join("aq.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            let _ = listener.accept();
        });
        socket
    }

    #[test]
    fn ordinary_bash_never_connects() {
        let missing = Path::new("/nonexistent-aq-dir/aq.sock");
        assert_eq!(run(missing, &bash("ls")), None);

        // A listener that never accepts would stall a connect; ordinary
        // commands must not even try.
        let dir = private_dir();
        let socket = dir.path().join("aq.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let start = Instant::now();
        assert_eq!(run(&socket, &bash("ls -la && echo hi")), None);
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_candidate_with_no_answer_is_denied_with_the_flag_reason() {
        let dir = private_dir();
        let socket = closing_listener(dir.path());
        let out = run(&socket, &bash("git push origin main")).expect("a deny");
        let v: Value = serde_json::from_str(&out).unwrap();
        let specific = &v["hookSpecificOutput"];
        assert_eq!(specific["hookEventName"], "PreToolUse");
        assert_eq!(specific["permissionDecision"], "deny");
        assert_eq!(specific["permissionDecisionReason"], FLAG_WAITING_REASON);
    }

    #[test]
    fn the_pane_is_looked_up_only_for_a_call_that_goes_to_aq() {
        let missing = Path::new("/nonexistent-aq-dir/aq.sock");
        let looked = std::cell::Cell::new(false);
        let pane = || {
            looked.set(true);
            None
        };
        let out = run_hook_with(
            missing,
            pane,
            HookEvent::PreTool,
            bash("ls").as_bytes(),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out, None);
        assert!(!looked.get());
        let pane = || {
            looked.set(true);
            None
        };
        run_hook_with(
            missing,
            pane,
            HookEvent::PreTool,
            bash("git push").as_bytes(),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(looked.get());
    }

    #[test]
    fn a_check_that_panics_reads_as_no_answer_and_is_denied() {
        assert_eq!(is_candidate(|| true), Some(true));
        assert_eq!(is_candidate(|| false), Some(false));
        assert_eq!(is_candidate(|| panic!("bad parse")), None);
        let v: Value =
            serde_json::from_str(&output(HookEvent::PreTool, true, None).unwrap()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            FLAG_WAITING_REASON
        );
    }

    #[test]
    fn a_candidate_with_no_aq_prints_nothing() {
        let missing = Path::new("/nonexistent-aq-dir/aq.sock");
        assert_eq!(run(missing, &bash("git push")), None);
    }

    #[test]
    fn an_edit_with_no_answer_keeps_its_old_reason() {
        let dir = private_dir();
        let socket = closing_listener(dir.path());
        let input = json!({
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/w/x", "content": "hi"},
        })
        .to_string();
        let out = run(&socket, &input).expect("a deny");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            WAITING_REASON
        );
    }
}
