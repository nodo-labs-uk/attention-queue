use std::process::Command;
use std::time::{Duration, Instant};

use aq::agents::{AgentConn, TmuxConn};
use aq::model::AgentId;

#[test]
fn send_text_reaches_pane() {
    let socket = format!("aqtest-{}", std::process::id());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.txt");
    let tmux = |args: &[&str]| {
        Command::new("tmux")
            .args(["-L", &socket])
            .args(args)
            .output()
            .unwrap()
    };
    let cmd = format!("cat > {}", file.display());
    let started = tmux(&["new-session", "-d", "-x", "80", "-y", "24", &cmd]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let pane = String::from_utf8(tmux(&["list-panes", "-a", "-F", "#{pane_id}"]).stdout)
        .unwrap()
        .trim()
        .to_string();

    let conn = TmuxConn {
        socket: Some(socket.clone()),
    };
    let id = AgentId {
        pane,
        claude_pid: std::process::id(),
    };
    let result = conn.send_text(&id, "hello\nworld");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = String::new();
    while Instant::now() < deadline {
        got = std::fs::read_to_string(&file).unwrap_or_default();
        if got.contains("world") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let alive = conn.is_alive(&id);
    tmux(&["kill-server"]);
    result.unwrap();
    assert!(alive);
    assert!(
        got.contains("hello") && got.contains("world"),
        "got {got:?}"
    );
}
