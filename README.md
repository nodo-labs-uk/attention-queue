# aq

i run a bunch of claude code sessions in tmux, and kept losing track of which one was waiting on me. aq watches all of them and puts whatever needs me (a diff to review, an edit to allow, a permission prompt) in one queue, one card at a time.

![a finished turn turns into a review card, the comment goes back to the session](docs/media/review.svg)

## why

- the agents aren't the slow part, i am. no point having 5 sessions if i'm flicking through windows to find the one that's stuck
- when a turn ends i want to see the diff, not scroll the chat. comments go back as one message, like a PR review
- not every project needs the same checking. a script can just run, a payments service should wait for me on every edit. so each checkout has its own mode
- skimming a diff and hitting approve isn't really reviewing it. quiz mode makes me explain bits of the change first
- it shouldn't get in the way. it talks to claude through hooks, and if aq isn't running (or breaks) the hooks do nothing and claude carries on as normal

## modes

each project has a mode, `M` switches it.

**review** (default): when a session finishes, its diff becomes a card. comment on a line or a range, `S` sends them all, `A` approves, `d` marks a file done so it's out of the way. that's the video above.

**step**: every edit waits for me before it's applied. `y` lets it through, `n` sends it back with a reason.

![an edit waiting on a step card, allowed with y, then one rejected with a reason](docs/media/step.svg)

**quiz**: when a session finishes, aq picks the bits of the diff that matter and asks me to explain what they do or why they're done that way, in my own words. a judge marks it, and if it finds a gap that can go back as a fix.

![a quiz question on a piece of the diff, answered and marked right](docs/media/quiz.svg)

**vibe**: nothing gets checked, it just codes.

when nothing's waiting there's a tab per session and the picked one's screen live underneath. `Enter` messages it (sent once it's idle), `W` asks where it's at without interrupting it, `P` gets it to open a PR, `Esc Esc` stops it.

![picking sessions, messaging one, asking where it's at](docs/media/sessions.svg)

`?` on any card asks the session a side question. a fork answers, so the session itself never sees it.

## vs other tools

| | aq | [claude squad](https://github.com/smtg-ai/claude-squad) | [conductor](https://www.conductor.build) | [vibe kanban](https://github.com/BloopAI/vibe-kanban) | [tuicr](https://github.com/agavra/tuicr) |
|---|---|---|---|---|---|
| runs in | terminal | terminal | mac app | browser | terminal |
| works with sessions already running in tmux | ✓ | starts its own | starts its own | starts its own | any repo |
| one queue of what needs you, across sessions | ✓ | list of sessions | list of workspaces | task board | – |
| line comments go back to the agent | ✓ directly | – | diff view | ✓ | ✓ clipboard or skill |
| approve each edit before it lands | ✓ | – | – | – | – |
| quiz on the change | ✓ | – | – | – | – |
| permission prompts in the same place | ✓ | – | – | – | – |
| starts agents in their own worktrees | – | ✓ | ✓ | ✓ | – |

aq doesn't start sessions or set up worktrees, you start claude however you want and it picks them up. if you want one app that does all of that, use one of the others.

## install

```sh
cargo install --path .
```

then add the hooks from [`contrib/settings-hooks.json`](contrib/settings-hooks.json) to `~/.claude/settings.json`. merge them into your existing `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays, don't replace them or your other hooks stop running. [docs/hooks.md](docs/hooks.md) has the details.

run `aq` in a tmux window. `F1` lists every key, and the bottom bar shows the ones that work right now.

## files

- `~/.config/aq/config.toml`: theme and each checkout's mode (`T` cycles themes)
- `~/.local/state/aq/state.json`: review rounds, unsent comments, quiz progress, so restarting loses nothing
- socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`. `AQ_SOCKET` overrides it

## dev

`cargo test` runs everything, end to end tests included (they use a private tmux server). `demo/record.py` re-records the videos with fake sessions and a fake `claude`, so no login and no cost.
