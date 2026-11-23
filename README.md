<h1 align="center"><img src="docs/media/banner.svg" width="640" alt="aq: an attention queue for Claude Code"></h1>

<p align="center"><b>Every Claude Code session that needs you, in one queue in your terminal.</b><br>
Diffs to review, edits to allow, permission prompts and questions, one card at a time.<br>
<sub>Linux · tmux · Rust · MIT</sub></p>

<p align="center"><a href="#key-features">Key features</a> · <a href="#get-started">Get started</a> · <a href="#first-keys">First keys</a> · <a href="#see-it-work">See it work</a> · <a href="#compared-with-other-tools">Compared</a> · <a href="https://nodo-labs-uk.github.io/attention-queue/">Documentation</a></p>

I run several Claude Code sessions in tmux and kept losing track of which one was waiting on me. aq watches all of them and puts whatever needs me in one queue, shown one card at a time. A card that comes in while I'm on another waits its turn, so nothing takes the screen from under me.

![Three sessions at work: a permission prompt is allowed, a finished turn is reviewed and the comment sent back, and a question is answered, all from aq](docs/media/hero.svg)

## Key features

- **One queue for every session.** Diffs, edits, permission prompts and questions from all your Claude sessions arrive as cards. [The queue](https://nodo-labs-uk.github.io/attention-queue/queue.html)
- **Review like a pull request.** Comment on lines of a finished turn's diff and send them back as one message. [Review card](https://nodo-labs-uk.github.io/attention-queue/cards/review.html)
- **As much checking as each project needs.** Each project has a mode: review each turn, approve each edit, get quizzed on the change, or check nothing. [Modes](https://nodo-labs-uk.github.io/attention-queue/modes.html)
- **Permission prompts with a summary.** A line from Claude Haiku says what the call does and why; `y` allows it. [Permission card](https://nodo-labs-uk.github.io/attention-queue/cards/permission.html)
- **A hold on anything done in your name.** Merges, pushes to main, force pushes and comments posted as you wait for a yes, in every mode. [Flagged actions](https://nodo-labs-uk.github.io/attention-queue/cards/flagged.html)
- **GitHub PR review.** `R` opens any open PR as a review card. Your comments go to the session to fix, or to GitHub as one review. [PR review](https://nodo-labs-uk.github.io/attention-queue/cards/pr-review.html)
- **Every session on one screen.** When nothing is waiting, watch your sessions live in tabs. Message one, approve it, tell it you're done, switch its model, open its `/btw` or `/mcp` in a popup, or stop it. [Sessions view](https://nodo-labs-uk.github.io/attention-queue/sessions.html)
- **Quick answers without interrupting.** `W` asks a session where it's up to, `T` has Haiku sum up its latest turn, and `?` asks anything. A copy of the session answers, so the session never sees it. [Side questions](https://nodo-labs-uk.github.io/attention-queue/tools.html)
- **New sessions and workspaces.** `N` starts a session, in a worktree of its own if you like. `O` starts one for each repo in a folder, in a row of tabs of their own. [New sessions](https://nodo-labs-uk.github.io/attention-queue/new-sessions.html)
- **Sessions that ask each other.** A session runs `aq tell` to ask another for work, and you approve the message first. [Messages](https://nodo-labs-uk.github.io/attention-queue/cards/message.html)
- **Nothing lost on a restart.** Unsent comments survive aq restarting, and after a reboot aq reopens the sessions that were open.
- **Keyboard first.** It has vim keys, and a short tour on the first run teaches them on made-up cards.

aq connects to Claude Code through hooks. If aq isn't running, the hooks do nothing and Claude carries on as normal. [Why it works this way](https://nodo-labs-uk.github.io/attention-queue/introduction.html#design-principles)

## Get started

aq needs Linux, tmux and Claude Code. The GitHub features also need `gh`, logged in as you.

> [!TIP]
> **Quickest install: paste this into Claude Code and let it do the setup.**
>
> ```text
> Set up aq (https://github.com/nodo-labs-uk/attention-queue) for me.
>
> 1. Install it: download aq-x86_64-linux.tar.gz from the latest release
>    (https://github.com/nodo-labs-uk/attention-queue/releases/latest), or
>    clone the repo and run `cargo install --path .`.
>    Make sure `aq` is on my PATH and `aq --help` runs.
> 2. Add the hooks from contrib/settings-hooks.json to
>    ~/.claude/settings.json. Back the file up first. Merge them into the
>    existing PreToolUse, PostToolUse and PermissionRequest arrays, keeping
>    every hook already there, and skip any that are already present. Show me
>    the diff before saving.
> 3. Check that tmux is installed, and tell me which running Claude sessions
>    need a restart to pick up the hooks.
>
> Don't change anything else.
> ```

Then run `aq` in a tmux window. The tour starts on its own.

<details>
<summary>Install by hand</summary>

1. Take the binary from the [latest release](https://github.com/nodo-labs-uk/attention-queue/releases/latest):

   ```sh
   curl -fsSL https://github.com/nodo-labs-uk/attention-queue/releases/latest/download/aq-x86_64-linux.tar.gz | tar xz
   install aq-x86_64-linux/aq ~/.local/bin/
   ```

   or build it from source with `cargo install --path .`.

2. Add the hooks from [`contrib/settings-hooks.json`](contrib/settings-hooks.json) to `~/.claude/settings.json`. Merge them into your existing `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays. If you replace the arrays, your other hooks stop running. [Hooks](https://nodo-labs-uk.github.io/attention-queue/hooks.html) explains each one.
3. Restart Claude sessions that were running before you added the hooks.
4. Run `aq` in a tmux window.

</details>

## First keys

| Key | What it does |
|---|---|
| `y` `n` | allow or refuse an edit or a permission prompt |
| `a` `d` | approve, or say you're done with what it asked ("Done, continue.") |
| `c` `s` `a` | on a review: comment on a line, send the comments, approve |
| `Enter` | reply to a question, or message the picked session |
| `Space` | skip this card for now |
| `M` | switch this project's mode: review, step, quiz, vibe |
| `W` `T` `?` | where is the session up to, sum up its last turn, ask it anything |
| `N` | start a new Claude session |
| `R` | review a GitHub PR |
| `!` | a shell in a popup, with Claude's last `! command` typed in |
| `Ctrl-Q` | close any window or popup |
| `F1` `F2` `F3` | every key, the tour again, the keys row along the bottom |

Moving around works as in vim. [Every key](https://nodo-labs-uk.github.io/attention-queue/keys.html)

## See it work

<details>
<summary>Review a finished turn and send the comment back</summary>

![A finished turn becomes a review card, and the comment goes back to the session](docs/media/review.svg)
</details>

<details>
<summary>Allow or refuse each edit in step mode</summary>

![An edit waiting on a step card, allowed with y, then another rejected with a reason](docs/media/step.svg)
</details>

<details>
<summary>Explain the change in quiz mode</summary>

![A quiz question on part of the diff: a first try gets a leading question, the second is marked right](docs/media/quiz.svg)
</details>

<details>
<summary>Answer a question or Claude's own menu</summary>

![A session asks to approve a PR draft and a approves it; then Claude's option menu is answered from a card](docs/media/questions.svg)
</details>

<details>
<summary>Switch between sessions and message one</summary>

![Switching between sessions, messaging one, and asking where it's up to](docs/media/sessions.svg)
</details>

<details>
<summary>Start a session in a worktree or a new folder</summary>

![N starts a session in a worktree of its own, then Ctrl-O starts one in a folder that is made for it](docs/media/new.svg)
</details>

<details>
<summary>Run a command Claude can't, in a shell popup</summary>

![Claude asks for a command that needs sudo; ! opens a shell with it typed in](docs/media/shell.svg)
</details>

<details>
<summary>The first-run tour</summary>

![The first-run tour: practice cards for review, step and quiz, then made-up sessions, asking for each key](docs/media/tour.svg)
</details>

## Compared with other tools

| | aq | [Claude Squad](https://github.com/smtg-ai/claude-squad) | [Conductor](https://www.conductor.build) | [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) | [tuicr](https://github.com/agavra/tuicr) |
|---|---|---|---|---|---|
| Runs in | terminal | terminal | macOS app | browser | terminal |
| Works with sessions already running in tmux | ✓ | starts its own | starts its own | starts its own | any repo |
| One queue of what needs you, across sessions | ✓ | list of sessions | list of workspaces | task board | – |
| Line comments go back to the agent | ✓ directly | – | diff view | ✓ | ✓ clipboard or skill |
| Approve each edit before it lands | ✓ | – | – | – | – |
| Quiz on the change | ✓ | – | – | – | – |
| Permission prompts in the same place | ✓ | – | – | – | – |
| Starts new sessions | ✓ in a tmux window | ✓ | ✓ | ✓ | – |
| Sets up a worktree per session | optional, `Tab` on `N` | ✓ | ✓ | ✓ | – |

aq leaves branches and merging to you and Claude: start Claude however you like and aq picks it up. If you'd rather have one app that manages every session's branch for you, one of the others is a better fit.

## Files, development and licence

aq keeps its settings in `~/.config/aq/config.toml` and its state in `~/.local/state/aq/`. [Files and configuration](https://nodo-labs-uk.github.io/attention-queue/configuration.html) lists each file, and how to add your own theme.

`cargo test` runs everything, including end-to-end tests against a private tmux server. `demo/record.py` re-records the videos with fake sessions and a stand-in `claude`, so it needs no login and costs nothing. Pushing a `v*` tag builds a release.

[MIT](LICENSE): use it, change it and share it, for anything.

---

aq was built with [Claude Code](https://claude.com/claude-code). Issues, PRs and other contributions are welcome. You could also point your own agent at this repo and have it build you a version from scratch, but this one saves you the tokens.
