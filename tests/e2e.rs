//! End to end: the real `aq` in a private tmux server, a fake agent in
//! another window, keys sent with `send-keys` and screens read with
//! `capture-pane`. Nothing touches the user's own tmux server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const AQ: &str = env!("CARGO_BIN_EXE_aq");
const FAKE_AGENT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake-agent.sh");
/// How long any one thing may take to show up.
const WAIT: Duration = Duration::from_secs(20);
const SESSION: &str = "e2e";
const AQ_WINDOW: &str = "e2e:aq";

/// Polls `f` until it gives a value; panics with `what` and the last
/// state seen once `WAIT` has passed.
fn wait_for<T>(what: &str, mut f: impl FnMut() -> Result<T, String>) -> T {
    let end = Instant::now() + WAIT;
    loop {
        match f() {
            Ok(v) => return v,
            Err(last) if Instant::now() >= end => panic!("timed out waiting for {what}:\n{last}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// A temp dir only this user can enter, as aq's socket directory must be.
fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// A private tmux server, killed on drop so a failed assertion still
/// cleans up.
struct Tmux {
    socket: String,
    /// The socket file, which `kill-server` leaves behind.
    socket_path: PathBuf,
}

impl Tmux {
    /// Starts the server with one window, `name`, running `cmd` in `cwd`.
    fn start(name: &str, cwd: &Path, cmd: &str) -> Tmux {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let mut tmux = Tmux {
            socket: format!("aqtest-{}-{nanos}", std::process::id()),
            socket_path: PathBuf::new(),
        };
        let cwd = cwd.to_str().unwrap();
        tmux.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-n",
            name,
            "-x",
            "160",
            "-y",
            "50",
            "-c",
            cwd,
            cmd,
        ]);
        let path = tmux.run(&["display-message", "-p", "#{socket_path}"]);
        tmux.socket_path = PathBuf::from(path.trim());
        tmux
    }

    fn command(&self) -> Command {
        let mut c = Command::new("tmux");
        // Started from inside tmux, the test must still not reach that server.
        c.env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args(["-L", &self.socket]);
        c
    }

    /// Runs a tmux command that must succeed; returns its stdout.
    fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn screen(&self, target: &str) -> String {
        self.run(&["capture-pane", "-p", "-t", target])
    }

    /// Waits until the pane shows `text`; returns the screen.
    fn wait_screen(&self, target: &str, text: &str) -> String {
        wait_for(&format!("{text:?} on {target}"), || {
            let s = self.screen(target);
            if s.contains(text) { Ok(s) } else { Err(s) }
        })
    }

    fn keys(&self, target: &str, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", target];
        args.extend(keys);
        self.run(&args);
    }

    fn type_text(&self, target: &str, text: &str) {
        self.run(&["send-keys", "-t", target, "-l", text]);
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// aq in a private tmux server next to a fake agent working in a git repo
/// with one commit and a change on top.
struct World {
    // Dropped first: the server goes before its files.
    tmux: Tmux,
    root: tempfile::TempDir,
    repo: PathBuf,
    /// What the fake agent was sent.
    out: PathBuf,
    socket: PathBuf,
    agent_pane: String,
}

impl World {
    /// `step` puts the repo in step mode.
    fn start(step: bool) -> World {
        let root = private_dir();
        let base = root.path().canonicalize().unwrap();
        let repo = base.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let config = base.join("config");
        let state = base.join("state");
        let run = base.join("run");
        std::fs::create_dir_all(config.join("aq")).unwrap();
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        if step {
            let toml = format!("[modes]\n{:?} = \"step\"\n", repo.display().to_string());
            std::fs::write(config.join("aq/config.toml"), toml).unwrap();
        }
        let socket = run.join("aq.sock");
        let out = base.join("agent-out.txt");
        std::fs::write(&out, "").unwrap();

        let fake = format!("sh {FAKE_AGENT} {}", out.display());
        let tmux = Tmux::start("agent", &repo, &fake);
        let pane = tmux.run(&[
            "display-message",
            "-p",
            "-t",
            "e2e:agent",
            "#{pane_id} #{pane_pid}",
        ]);
        let (agent_pane, pid) = pane.trim().split_once(' ').unwrap();
        let agents = base.join("agents.json");
        let list = json!([{
            "pid": pid.parse::<u32>().unwrap(),
            "cwd": repo,
            "kind": "interactive",
            "sessionId": "fake",
            "name": "fake",
            "status": "idle",
        }]);
        std::fs::write(&agents, list.to_string()).unwrap();

        let env = |k: &str, v: &Path| format!("{k}={}", v.display());
        tmux.run(&[
            "new-window",
            "-d",
            "-n",
            "aq",
            "-c",
            repo.to_str().unwrap(),
            "-e",
            &env("AQ_SOCKET", &socket),
            "-e",
            &env("AQ_AGENTS_JSON", &agents),
            "-e",
            &env("XDG_CONFIG_HOME", &config),
            "-e",
            &env("XDG_STATE_HOME", &state),
            "-e",
            &env("XDG_RUNTIME_DIR", &run),
            AQ,
        ]);
        let world = World {
            tmux,
            root,
            repo,
            out,
            socket,
            agent_pane: agent_pane.to_string(),
        };
        // Up, listening, and has seen the agent working.
        world.tmux.wait_screen(AQ_WINDOW, "working");
        wait_for("aq's socket", || {
            if world.socket.exists() {
                Ok(())
            } else {
                Err(String::new())
            }
        });
        world
    }

    fn sent_to_agent(&self) -> String {
        std::fs::read_to_string(&self.out).unwrap()
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Starts `aq hook <event>` with `input` on stdin; join for its output.
fn hook(socket: &Path, event: &str, input: &Value) -> std::thread::JoinHandle<Output> {
    let mut child = Command::new(AQ)
        .args(["hook", event, "--deadline-secs", "60"])
        .env("AQ_SOCKET", socket)
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    std::thread::spawn(move || child.wait_with_output().unwrap())
}

#[test]
fn e2e_review_comment_reaches_agent() {
    let w = World::start(false);
    w.tmux.run(&[
        "set-option",
        "-p",
        "-t",
        &w.agent_pane,
        "@claude_waiting",
        "1",
    ]);
    w.tmux.wait_screen(AQ_WINDOW, "a.txt");
    w.tmux.keys(AQ_WINDOW, &["c"]);
    w.tmux.type_text(AQ_WINDOW, "why two?");
    w.tmux.keys(AQ_WINDOW, &["C-s"]);
    w.tmux.wait_screen(AQ_WINDOW, "why two?");
    w.tmux.keys(AQ_WINDOW, &["S"]);
    let sent = wait_for("the comments at the agent", || {
        let s = w.sent_to_agent();
        if s.contains("why two?") {
            Ok(s)
        } else {
            Err(s)
        }
    });
    assert!(sent.starts_with("Review comments (round 1):"), "{sent:?}");
    assert!(sent.contains("a.txt:2"), "{sent:?}");
}

#[test]
fn e2e_step_reject_reason_reaches_hook() {
    let w = World::start(true);
    let file = w.repo.join("a.txt");
    let input = json!({
        "session_id": "fake",
        "transcript_path": w.root.path().join("none.jsonl"),
        "cwd": w.repo,
        "permission_mode": "acceptEdits",
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": {"file_path": file, "old_string": "two", "new_string": "deux"},
        "tool_use_id": "toolu_e2e",
    });
    let done = hook(&w.socket, "pre-tool", &input);
    w.tmux.wait_screen(AQ_WINDOW, "deux");
    // A step needs a moment on screen before a decision counts, so `n` is
    // pressed again if the reason box has not opened a second later.
    wait_for("the reason box", || {
        w.tmux.keys(AQ_WINDOW, &["n"]);
        let end = Instant::now() + Duration::from_secs(1);
        loop {
            let s = w.tmux.screen(AQ_WINDOW);
            if s.contains("tell Claude why") {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(s);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    w.tmux.type_text(AQ_WINDOW, "keep it in English");
    w.tmux.keys(AQ_WINDOW, &["Enter"]);
    let out = done.join().unwrap();
    assert!(out.status.success());
    let reply: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {:?}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(
        reply,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "Rejected by reviewer: keep it in English",
        }})
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\n");
}

#[test]
fn e2e_no_aq_hook_is_silent() {
    let dir = private_dir();
    let socket = dir.path().join("aq.sock");
    let edit = json!({
        "session_id": "s1",
        "cwd": "/",
        "tool_name": "Write",
        "tool_input": {"file_path": "/nowhere/x.txt", "content": "hi"},
        "tool_use_id": "toolu_1",
    });
    let bash = json!({
        "session_id": "s1",
        "cwd": "/",
        "tool_name": "Bash",
        "tool_input": {"command": "ls"},
    });
    for (event, input) in [
        ("pre-tool", &edit),
        ("post-tool", &bash),
        ("permission", &bash),
    ] {
        let started = Instant::now();
        let out = hook(&socket, event, input).join().unwrap();
        assert!(out.status.success(), "{event}: {out:?}");
        assert!(out.stdout.is_empty(), "{event}: {out:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{event} waited");
    }
}
