//! `aq hook` with arguments it does not know still exits 0: a PreToolUse
//! hook that exits 2 would block every edit.

use std::process::{Command, Stdio};

const AQ: &str = env!("CARGO_BIN_EXE_aq");

fn hook(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(AQ)
        .arg("hook")
        .args(args)
        // Never near a real aq.
        .env("AQ_SOCKET", dir.path().join("none.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn unknown_event_exits_0_with_nothing_on_stdout() {
    let out = hook(&["bogus-event"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(!out.stderr.is_empty(), "the problem goes to stderr");
}

#[test]
fn bad_flag_exits_0() {
    for args in [
        &["pre-tool", "--deadline-secs", "abc"][..],
        &["permission", "--no-such-flag"],
        &[],
    ] {
        let out = hook(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }
}

#[test]
fn other_commands_still_fail_on_bad_arguments() {
    let out = Command::new(AQ).arg("--no-such-flag").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
