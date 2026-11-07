# aq

Attention queue: a terminal app (ratatui) that collects decisions from Claude Code agents running in tmux, so you answer them in one place.

Build with `cargo build --release`; the binary is `target/release/aq`. Run the tests with `cargo test`.

## Installing the hooks

`aq` hears from Claude Code through three hooks. Install the binary so `aq` is on your `PATH` (`cargo install --path .`), then add the hooks from [`contrib/settings-hooks.json`](contrib/settings-hooks.json) to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "aq hook pre-tool --deadline-secs 3570", "timeout": 3600 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash|Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "aq hook post-tool", "timeout": 30 }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          { "type": "command", "command": "aq hook permission --deadline-secs 3570", "timeout": 3600 }
        ]
      }
    ]
  }
}
```

Merge these entries into the `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays you already have. Do not replace the arrays, or your existing hooks stop running.

When no `aq` is running, every hook exits at once and prints nothing, so Claude Code behaves as if the hooks were not there. The 3570 s deadline makes a waiting hook answer 30 s before Claude Code's 3600 s timeout would cancel it; [docs/hooks.md](docs/hooks.md) has the details.

## Files

- Settings: `~/.config/aq/config.toml` (theme, and the mode of each checkout).
- Saved state: `~/.local/state/aq/state.json`. It holds each checkout's last review round, review comments not sent yet and quiz progress. A saved draft or quiz comes back when the agent's card for the same snapshot comes back.
- Socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`. `AQ_SOCKET` overrides it.

`AQ_AGENTS_JSON` is a test hook: when it names a file, `aq` reads the agent list from that file instead of running `claude agents --json`. The end-to-end tests use it to make a fake agent look like Claude.
