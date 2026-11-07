use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aq::hook::{HookEvent, WAITING_REASON, run_hook_with};
use aq::ipc::{self, HookReply, HookRequest, IpcEvent};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const PRE_TOOL_INPUT: &str = r#"{
  "session_id": "s1",
  "transcript_path": "/t/s1.jsonl",
  "cwd": "/w/api",
  "permission_mode": "acceptEdits",
  "hook_event_name": "PreToolUse",
  "tool_name": "Write",
  "tool_input": {"file_path": "/w/api/x.txt", "content": "hi"},
  "tool_use_id": "toolu_1"
}"#;

const PERMISSION_INPUT: &str = r#"{
  "session_id": "s1",
  "cwd": "/w/api",
  "hook_event_name": "PermissionRequest",
  "tool_name": "Bash",
  "tool_input": {"command": "rm -rf node_modules"},
  "permission_suggestions": []
}"#;

/// A listening server on a fresh socket, and the events it receives.
fn start_server() -> (tempfile::TempDir, PathBuf, mpsc::Receiver<IpcEvent>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("aq.sock");
    let listener = ipc::listen(&socket).unwrap();
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(ipc::accept_loop(listener, tx));
    (dir, socket, rx)
}

/// Runs the blocking hook on its own thread.
async fn hook(
    socket: &Path,
    event: HookEvent,
    input: &'static str,
    deadline: Duration,
) -> Option<String> {
    let socket = socket.to_path_buf();
    tokio::task::spawn_blocking(move || {
        run_hook_with(
            &socket,
            Some("%7".to_string()),
            event,
            input.as_bytes(),
            deadline,
        )
    })
    .await
    .unwrap()
    .unwrap()
}

fn parsed(out: Option<String>) -> Value {
    serde_json::from_str(&out.expect("expected hook output")).unwrap()
}

/// Answers every request with `reply`.
fn answer_all(mut rx: mpsc::Receiver<IpcEvent>, reply: HookReply) {
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = ev.reply.send(reply.clone());
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn no_socket_prints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("aq.sock");
    let out = hook(
        &socket,
        HookEvent::PreTool,
        PRE_TOOL_INPUT,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_file_prints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("aq.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let out = hook(
        &socket,
        HookEvent::PreTool,
        PRE_TOOL_INPUT,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_reply_prints_allow_json() {
    let (_dir, socket, rx) = start_server();
    answer_all(rx, HookReply::Allow);
    let out = parsed(
        hook(
            &socket,
            HookEvent::PreTool,
            PRE_TOOL_INPUT,
            Duration::from_secs(5),
        )
        .await,
    );
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_reply_carries_reason() {
    let (_dir, socket, rx) = start_server();
    answer_all(
        rx,
        HookReply::Deny {
            reason: "use the helper".into(),
        },
    );
    let out = parsed(
        hook(
            &socket,
            HookEvent::PreTool,
            PRE_TOOL_INPUT,
            Duration::from_secs(5),
        )
        .await,
    );
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "use the helper"
        }})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_decision_reply_prints_nothing() {
    let (_dir, socket, rx) = start_server();
    answer_all(rx, HookReply::NoDecision);
    let out = hook(
        &socket,
        HookEvent::PreTool,
        PRE_TOOL_INPUT,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn pre_tool_request_reaches_server() {
    let (_dir, socket, mut rx) = start_server();
    let waiting = tokio::spawn({
        let socket = socket.clone();
        async move {
            hook(
                &socket,
                HookEvent::PreTool,
                PRE_TOOL_INPUT,
                Duration::from_secs(5),
            )
            .await
        }
    });
    let ev = rx.recv().await.unwrap();
    assert_eq!(
        ev.req,
        HookRequest::PreTool {
            session_id: "s1".into(),
            transcript_path: "/t/s1.jsonl".into(),
            cwd: "/w/api".into(),
            tool_name: "Write".into(),
            tool_use_id: "toolu_1".into(),
            tool_input: json!({"file_path": "/w/api/x.txt", "content": "hi"}),
        }
    );
    ev.reply.send(HookReply::NoDecision).unwrap();
    assert_eq!(waiting.await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_carries_pane_and_tolerates_missing_fields() {
    let (_dir, socket, mut rx) = start_server();
    let waiting = tokio::spawn({
        let socket = socket.clone();
        async move {
            hook(
                &socket,
                HookEvent::Permission,
                PERMISSION_INPUT,
                Duration::from_secs(5),
            )
            .await
        }
    });
    let ev = rx.recv().await.unwrap();
    assert_eq!(
        ev.req,
        HookRequest::Permission {
            session_id: "s1".into(),
            cwd: "/w/api".into(),
            tool_name: "Bash".into(),
            tool_input: json!({"command": "rm -rf node_modules"}),
            pane: Some("%7".into()),
        }
    );
    ev.reply
        .send(HookReply::Deny {
            reason: "not now".into(),
        })
        .unwrap();
    assert_eq!(
        parsed(waiting.await.unwrap()),
        json!({"hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {"behavior": "deny", "message": "not now"}
        }})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_allow_prints_allow_json() {
    let (_dir, socket, rx) = start_server();
    answer_all(rx, HookReply::Allow);
    let out = parsed(
        hook(
            &socket,
            HookEvent::Permission,
            PERMISSION_INPUT,
            Duration::from_secs(5),
        )
        .await,
    );
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_denies_pre_tool() {
    let (_dir, socket, mut rx) = start_server();
    // Hold every request without answering.
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(ev) = rx.recv().await {
            held.push(ev);
        }
    });
    let out = parsed(
        hook(
            &socket,
            HookEvent::PreTool,
            PRE_TOOL_INPUT,
            Duration::from_secs(1),
        )
        .await,
    );
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "Waiting for human review in aq. Try this edit again shortly."
        }})
    );
    assert_eq!(
        WAITING_REASON,
        "Waiting for human review in aq. Try this edit again shortly."
    );
    held.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_gives_no_decision_for_permission() {
    let (_dir, socket, mut rx) = start_server();
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(ev) = rx.recv().await {
            held.push(ev);
        }
    });
    let out = hook(
        &socket,
        HookEvent::Permission,
        PERMISSION_INPUT,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(out, None);
    held.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn server_dropped_mid_wait_denies_pre_tool() {
    let (_dir, socket, mut rx) = start_server();
    let waiting = tokio::spawn({
        let socket = socket.clone();
        async move {
            hook(
                &socket,
                HookEvent::PreTool,
                PRE_TOOL_INPUT,
                Duration::from_secs(30),
            )
            .await
        }
    });
    // The request arrives, then aq goes away without answering.
    drop(rx.recv().await.unwrap());
    let out = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("hook should not wait for the deadline")
        .unwrap();
    assert_eq!(
        parsed(out)["hookSpecificOutput"]["permissionDecisionReason"],
        WAITING_REASON
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn server_dropped_mid_wait_gives_no_decision_for_permission() {
    let (_dir, socket, mut rx) = start_server();
    let waiting = tokio::spawn({
        let socket = socket.clone();
        async move {
            hook(
                &socket,
                HookEvent::Permission,
                PERMISSION_INPUT,
                Duration::from_secs(30),
            )
            .await
        }
    });
    drop(rx.recv().await.unwrap());
    let out = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disconnect_is_observed() {
    let (_dir, socket, mut rx) = start_server();
    let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    use std::io::Write;
    let line = r#"{"kind":"PreTool","session_id":"s","transcript_path":"","cwd":"/w","tool_name":"Write","tool_use_id":"t","tool_input":{}}"#;
    writeln!(client, "{line}").unwrap();
    let mut ev = rx.recv().await.unwrap();
    assert!(!ev.reply.is_closed());
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), ev.reply.closed())
        .await
        .expect("the reply sender should see the hook go away");
}

#[tokio::test(flavor = "multi_thread")]
async fn second_instance_refuses() {
    let (_dir, socket, _rx) = start_server();
    let err = ipc::listen(&socket).unwrap_err();
    assert!(
        format!("{err:#}").contains("aq is already running"),
        "{err:#}"
    );
    // The live socket is left alone.
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("aq.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    let _listener = ipc::listen(&socket).unwrap();
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_is_private() {
    let (_dir, socket, _rx) = start_server();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_answers_requests() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("aq.sock");
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(ipc::serve(socket.clone(), tx));
    answer_all(rx, HookReply::Allow);
    let mut out = None;
    for _ in 0..50 {
        out = hook(
            &socket,
            HookEvent::PreTool,
            PRE_TOOL_INPUT,
            Duration::from_secs(5),
        )
        .await;
        if out.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        parsed(out)["hookSpecificOutput"]["permissionDecision"],
        "allow"
    );
}
