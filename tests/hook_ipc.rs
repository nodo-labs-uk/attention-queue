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

/// A temp dir only this user can enter, as the socket's directory must be.
fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// A listening server on a fresh socket, and the events it receives.
fn start_server() -> (tempfile::TempDir, PathBuf, mpsc::Receiver<IpcEvent>) {
    let dir = private_dir();
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
    let dir = private_dir();
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
    let dir = private_dir();
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
    let dir = private_dir();
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
    let dir = private_dir();
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

/// A listener that never accepts, in `dir`, and the socket path.
fn silent_listener(dir: &Path) -> (std::os::unix::net::UnixListener, PathBuf) {
    let socket = dir.join("aq.sock");
    (
        std::os::unix::net::UnixListener::bind(&socket).unwrap(),
        socket,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn loose_socket_dir_prints_nothing() {
    let base = private_dir();
    for mode in [0o770, 0o707, 0o755] {
        let dir = base.path().join(format!("aq-{mode:o}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let (_listener, socket) = silent_listener(&dir);
        let out = hook(
            &socket,
            HookEvent::PreTool,
            PRE_TOOL_INPUT,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(out, None, "mode {mode:o}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn symlinked_socket_dir_prints_nothing() {
    let base = private_dir();
    let real = base.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_listener, _) = silent_listener(&real);
    let link = base.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let out = hook(
        &link.join("aq.sock"),
        HookEvent::PreTool,
        PRE_TOOL_INPUT,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(out, None);
}

#[test]
fn peer_uid_of_socketpair_is_ours() {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    assert_eq!(ipc::peer_uid(&a).unwrap(), aq::config::current_uid());
}

#[test]
fn listen_refuses_a_loose_dir() {
    let base = private_dir();
    let dir = base.path().join("open");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    assert!(ipc::listen(&dir.join("aq.sock")).is_err());
}

/// An aq that is stuck: its listen queue is full, so `connect` blocks.
#[tokio::test(flavor = "multi_thread")]
async fn stuck_aq_denies_pre_tool_by_the_deadline() {
    use std::os::fd::AsRawFd;
    let dir = private_dir();
    let (listener, socket) = silent_listener(dir.path());
    // SAFETY: a valid listening socket; listen again only shrinks its queue.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
    let _queued = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    let started = std::time::Instant::now();
    let out = hook(
        &socket,
        HookEvent::PreTool,
        PRE_TOOL_INPUT,
        Duration::from_secs(1),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        parsed(out)["hookSpecificOutput"]["permissionDecisionReason"],
        WAITING_REASON
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn post_tool_reports_and_returns_without_a_reply() {
    let (_dir, socket, mut rx) = start_server();
    let input = r#"{"session_id": "s1", "cwd": "/w/api", "tool_name": "Bash", "tool_input": {"command": "ls"}}"#;
    // Nobody answers, yet the hook returns at once.
    let out = hook(&socket, HookEvent::PostTool, input, Duration::from_secs(30)).await;
    assert_eq!(out, None);
    let ev = rx.recv().await.unwrap();
    assert_eq!(
        ev.req,
        HookRequest::PostTool {
            session_id: "s1".into(),
            cwd: "/w/api".into(),
            tool_name: "Bash".into(),
            tool_input: json!({"command": "ls"}),
        }
    );
}
