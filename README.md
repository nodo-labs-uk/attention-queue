# aq

**Attention queue for Claude Code.** Run as many Claude sessions in tmux as you like; aq gathers everything they need from you into one queue and shows it one card at a time.

![A finished turn becomes a review card; a line comment goes back to the session as one message](docs/media/review.svg)

## Why

- **Your attention is the bottleneck, not the agents.** Five sessions in five tmux windows means hunting for whichever one is waiting. aq watches them all and brings you the next thing that needs a decision, so you never go looking.
- **Read the change, not the chat.** When a turn ends you see its diff, not a wall of transcript. Comments go back as a single message, like a pull request review.
- **Choose how much to trust, per project.** A throwaway script can run free; a payment service can wait for you on every edit. Each git checkout has its own mode, switched with one key.
- **Understanding is the real review.** Approving a diff you skimmed is not the same as knowing what it does. Quiz mode asks you to explain pieces of the change before you accept it.
- **Stay out of the way.** aq talks to Claude Code through its hooks. When aq is not running, or anything goes wrong, the hooks print nothing and Claude carries on as if they were not there.

## What it does

Each project has one of four modes; `M` cycles them.

**Review board** (the default). When a session finishes a turn, its whole diff becomes a card: split or unified view, vim keys, comments on a line or a range. `S` sends all comments as one message; `A` approves; `d` ticks off a file you have read. Shown above.

**Step by step.** Every edit waits for you before it touches the file. `y` lets it through, `n` sends it back with your reason.

![An edit waits on a step card: y allows it, n rejects it with a reason Claude sees](docs/media/step.svg)

**Quiz.** When a turn ends, aq picks the parts of the diff that matter and asks you to explain, in your own words, what each one does or why it was done that way. A judge marks the answer, and a gap it finds can become a fix request.

![A quiz question on a piece of the diff, answered and marked right](docs/media/quiz.svg)

**Vibe.** Nothing is checked; the agent just codes.

**Nothing waiting?** You get a tab per session and the picked one's screen, live. `Enter` messages it (sent once it is idle), `W` asks where it is up to without disturbing it, `P` asks it to open a PR, and `Esc Esc` stops it.

![Picking sessions, sending one a message, and asking where it is up to](docs/media/sessions.svg)

Also on every card: `?` asks the session a side question (a forked copy answers, so the session never sees it), and permission requests from any session land in the same queue.

## Compared with

| | aq | [Claude Squad](https://github.com/smtg-ai/claude-squad) | [Conductor](https://www.conductor.build) | [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) | [tuicr](https://github.com/agavra/tuicr) |
|---|---|---|---|---|---|
| Runs in | terminal | terminal | macOS app | browser | terminal |
| Works with sessions you already run in tmux | ✓ | starts its own | starts its own | starts its own | any repo |
| One queue of what needs you, across sessions | ✓ | list of sessions | list of workspaces | board of tasks | – |
| Line comments sent back to the agent | ✓ directly | – | diff view | ✓ | ✓ via clipboard or skill |
| Approve each edit before it lands | ✓ | – | – | – | – |
| Quiz on the change | ✓ | – | – | – | – |
| Permission requests in the same place | ✓ | – | – | – | – |
| Starts agents in their own worktrees | – | ✓ | ✓ | ✓ | – |

aq does not launch or isolate agents: start Claude however you like (worktrees included) and aq finds it. The others are better if you want one app to create and run the sessions too.

## Install

```sh
cargo install --path .
```

Then add the hooks from [`contrib/settings-hooks.json`](contrib/settings-hooks.json) to `~/.claude/settings.json`, merging them into any `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays you already have (replacing the arrays would stop your existing hooks). [docs/hooks.md](docs/hooks.md) explains each one.

Run `aq` in a tmux window. `F1` shows every key, and the bottom row always shows the keys that work right now.

## Files

- Settings: `~/.config/aq/config.toml` (theme, and each checkout's mode). `T` cycles the themes.
- Saved state: `~/.local/state/aq/state.json`: review rounds, unsent comments and quiz progress, so a restart loses nothing.
- Socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`; `AQ_SOCKET` overrides it.

## Development

`cargo test` runs everything, including end-to-end tests against a private tmux server. `demo/record.py` re-records the videos above with fake sessions and a stand-in `claude`, so it needs no login and costs nothing.
