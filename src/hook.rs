//! `aq hook <event>`: a Claude Code hook that forwards the event to the
//! running `aq` and prints its decision. Blocking std I/O only.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::ipc::{HookReply, HookRequest};

/// What a step-mode edit with no answer tells the agent.
pub const WAITING_REASON: &str = "Waiting for human review in aq. Try this edit again shortly.";

/// Seconds the hook waits by default; the hook command's `timeout` is 3600.
pub const DEFAULT_DEADLINE_SECS: u64 = 3570;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HookEvent {
    PreTool,
    PostTool,
    Permission,
}

/// Runs the hook against the configured socket, with `$TMUX_PANE` as the pane.
pub fn run_hook(event: HookEvent, stdin: impl Read, deadline: Duration) -> Result<Option<String>> {
    let socket = crate::config::paths().socket;
    let pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty());
    run_hook_with(&socket, pane, event, stdin, deadline)
}

/// Returns the JSON to print on stdout, or `None` to print nothing.
pub fn run_hook_with(
    socket: &Path,
    pane: Option<String>,
    event: HookEvent,
    mut stdin: impl Read,
    deadline: Duration,
) -> Result<Option<String>> {
    let mut input = String::new();
    stdin
        .read_to_string(&mut input)
        .context("reading hook input")?;
    let input: Value = serde_json::from_str(&input).context("parsing hook input")?;
    let req = request(event, &input, pane);

    // No aq listening: Claude behaves as if there were no hook.
    let Ok(stream) = UnixStream::connect(socket) else {
        return Ok(None);
    };
    let reply = exchange(stream, &req, deadline);
    Ok(output(event, reply))
}

/// Sends the request and waits for one reply line. `None` means no answer:
/// the deadline passed, the connection closed, or the reply was unreadable.
fn exchange(mut stream: UnixStream, req: &HookRequest, deadline: Duration) -> Option<HookReply> {
    // A zero timeout is an error for the socket, so wait at least 1 ms.
    let deadline = deadline.max(Duration::from_millis(1));
    stream.set_read_timeout(Some(deadline)).ok()?;
    stream.set_write_timeout(Some(deadline)).ok()?;
    let mut line = serde_json::to_string(req).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    serde_json::from_str(&reply).ok()
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
fn output(event: HookEvent, reply: Option<HookReply>) -> Option<String> {
    let specific = match (event, reply) {
        (HookEvent::PreTool, Some(HookReply::Allow)) => json!({
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
        }),
        (HookEvent::PreTool, Some(HookReply::Deny { reason })) => pre_tool_deny(&reason),
        (HookEvent::PreTool, Some(HookReply::NoDecision)) => return None,
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
