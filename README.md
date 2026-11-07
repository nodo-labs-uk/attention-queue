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

When no `aq` is running, or on any internal error (including arguments it does not understand), `aq hook` prints nothing and exits 0, so Claude Code behaves as if the hooks were not there. The 3570 s deadline makes a waiting hook answer 30 s before Claude Code's 3600 s timeout would cancel it; [docs/hooks.md](docs/hooks.md) has the details.

## Keys

On every card: `?` asks a side question, `s` skips the card, `M` changes the project's mode, `T` the theme, `Q` quits. The bottom row always shows the keys that work right now.

A card that takes the screen by itself (a permission request jumping ahead, or the next card after one closed in the window) ignores the deciding keys below for 1.5 s, and the top bar shows "new card". Messages to an agent are sent only when it is waiting at its prompt, idle, and has no permission menu open; until then the card shows them as queued.

| Card | Decide | Look around |
|---|---|---|
| Review | `c` comment, `x` delete comment, `S` send comments, `A` approve | `j`/`k` line, `V` range, `]c`/`[c` change, `tab` file, `v` split or unified, `R` whole branch |
| Step | `y` allow, `n` reject with a reason, `e` ask for a different change | `j`/`k`, `tab` next change, `za` fold, `zR` unfold all, `w`/`b` word, `gd` find the word, `m` raw input |
| Shell change | `y` keep, `u` ask Claude to undo it | `j`/`k`, `]c`/`[c`, `tab` file, `v` view |
| Permission | `y` allow (after scrolling to the end), `n` deny with a reason | `j`/`k`, `PgUp`/`PgDn`, `g` go to the window |
| Quiz | `1`-`4` pick, `i` type an answer, `y` fine as is, `c` ask for a fix, `Enter` send | `m` show the code, `r` judge or generate again, `x` dismiss a failed quiz |

## Files

- Settings: `~/.config/aq/config.toml` (theme, and the mode of each checkout).
- Saved state: `~/.local/state/aq/state.json`. It holds each checkout's last review round, review comments not sent yet and quiz progress. Saved review comments come back on the review card for the same round, even if the agent changed more since; a comment whose file has left the diff is dropped, and the status line says so. A saved quiz comes back on the quiz card for the same snapshot.
- Socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`. `AQ_SOCKET` overrides it.

`AQ_AGENTS_JSON` is a test hook, honoured by debug builds only: when it names a file, `aq` reads the agent list from that file instead of running `claude agents --json`. The end-to-end tests use it to make a fake agent look like Claude.
