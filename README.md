<h1 align="center"><img src="docs/media/banner.svg" width="640" alt="aq: an attention queue for Claude Code"></h1>

<p align="center"><b>Every Claude Code session that needs you, in one queue in your terminal.</b><br>
Diffs to review, edits to allow, permission prompts and questions, one card at a time.<br>
<sub>Linux · tmux · Rust · MIT</sub></p>

I run several Claude Code sessions in tmux and kept losing track of which one was waiting on me. aq watches all of them and puts whatever needs me (a diff to review, an edit to allow, a permission prompt, a question) in one queue, shown one card at a time. A card that comes in while I'm on another waits its turn; it never takes the screen. If I answer the card on screen in its own window instead, it stays, marked "answered elsewhere", until I press a key.

![Three sessions at work: a permission prompt is allowed, a finished turn is reviewed and the comment sent back, and a question is answered, all from aq](docs/media/hero.svg)

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
> 2. Add the three hooks from contrib/settings-hooks.json to
>    ~/.claude/settings.json. Back the file up first. Merge them into the
>    existing PreToolUse, PostToolUse and PermissionRequest arrays, keeping
>    every hook already there, and skip any that are already present. Show me
>    the diff before saving.
> 3. Check that tmux is installed, and tell me which running Claude sessions
>    need a restart to pick up the hooks.
>
> Don't change anything else.
> ```

**Contents:** [Design principles](#design-principles) · [Modes](#modes) · [Sessions](#sessions) · [Keys](#keys) · [Compared with other tools](#compared-with-other-tools) · [Install](#install) · [Files](#files) · [Development](#development) · [Licence](#licence)

## Design principles

- **Your attention is the bottleneck.** Instead of cycling through sessions to find the one waiting on you, aq uses Claude Code's hooks to push the work to one queue: let the work come to you, not the other way round. Permission prompts, questions and option menus are answered from there too, and a new card never takes the screen from the one you're on.
- **An easy change, review and comment loop.** Code review inspired by [tuicr](https://tuicr.dev/): comment on a line or a range, and send the comments to the agent as one message, like a PR review.
- **Checking to suit the project.** Each project has its own mode: vibe (nothing checked), review, step (every edit waits for a yes) or quiz. A script can just run while a payments service waits on every edit.
- **Quiz mode.** Answer design questions about the code to check and reinforce your understanding of the change.
- **Unobtrusive.** It runs alongside your tmux sessions and connects to Claude Code through hooks, with no complex dependencies. If aq isn't running, the hooks do nothing and Claude carries on as normal.
- **Your sessions from one screen.** When nothing is waiting you see every session live, and can message one, ask where it's up to, switch its model or permission mode, or start a new one.
- **Keyboard first,** with vim keybindings.

## Modes

Each project has a mode, and `M` switches it.

**Review** (the default): when a session finishes, its diff becomes a card. You can comment on a line or a range, `s` sends all the comments, `a` approves, and `d` marks a file as done so it's out of the way. Markdown files (plans, specs, READMEs) show as read, with headings, bold, code and bullets styled and their marks hidden; `m` shows them as written. `e` opens the file at the cursor's line in your editor, for a change you'd rather make yourself.

![A finished turn becomes a review card, and the comment goes back to the session](docs/media/review.svg)

**Step**: every edit waits for approval before it's applied. `y` lets it through, `n` sends it back with a reason.

![An edit waiting on a step card, allowed with y, then another rejected with a reason](docs/media/step.svg)

**Quiz**: when a session finishes, aq picks the parts of the diff that matter and asks you to explain what they do, or why they were done that way, in your own words. A judge marks the answer. If it isn't right, or you say you don't know, you get a question that leads towards what you missed and another try, twice at most, before the answer is shown (`e` shows it sooner). A gap the judge finds in the code itself can be sent back as a fix.

![A quiz question on part of the diff: a first try gets a leading question, the second is marked right](docs/media/quiz.svg)

**Vibe**: nothing is checked; the agent just codes.

**Questions**, in every mode: when a session ends its turn by asking you something (to approve a draft, to choose between two ways), its message becomes a card. `a` approves when the question asks for a go-ahead, `Enter` replies in your own words, and `x` puts the card away. When Claude asks with its own menu of options, the card lists them and a number key picks one; the answer goes back through the hook, so nothing is typed into the session. A turn that has both a diff and a question shows the question on the review card, and sending from there answers it.

![A session asks to approve a PR draft and a approves it; then Claude's option menu is answered from a card](docs/media/questions.svg)

## Sessions

When nothing is waiting, there is a tab per session with the selected one's screen live underneath. Each tab shows the session's permission mode as its footer does, such as `⏵⏵ auto` or `⏸ plan`. Sessions in different tmux sessions get a row each, led by the tmux session's name. `Enter` messages the picked session (sent once it's idle), `A` sends one message to every session in its row, `W` asks where it's up to without interrupting it, `p` asks it to open a PR, `m` switches its model, `P` its permission mode (auto, manual, accept edits or plan), and `Esc Esc` stops it. `N` starts a new Claude session in the same project, in a tmux window of its own, with what you type as its first prompt. `Tab` in that box gives the session a git worktree of its own (Claude's `--worktree`), so two sessions don't edit the same files. `Ctrl-O` picks another folder: type a path with the matching folders listed underneath (`Tab` completes, the arrows choose), and a folder that isn't there yet is made when the session starts. The first time Claude Code starts in a folder it asks whether to trust it; aq shows that question in its own box, where `y` trusts the folder and `n` closes the new session. When several sessions ask at once, they are asked about in turn, and `a` trusts them all.

`O` opens a workspace: several repos at once, for work that spans them. Pick the folder that holds them, and aq finds the git repos inside (up to two levels down) and ticks them all; `Space` unticks one. `Enter` starts a Claude session in each, as windows of one tmux session named after the folder, so they share a row of tabs. aq saves the set in its config, and `O` lists it next time; opening it again starts only the repos that have no session there yet.

Sessions can ask each other for work with `aq tell <name> "<message>"`, where the name is the other session's project, as its tab shows it; `aq tell` alone lists them. Each message becomes a card: `y` sends it, `e` lets you edit it first, `n` drops it, so nothing crosses between repos without you. Sessions opened from a workspace are told how to use it. For a session started some other way, add a line like this to its `CLAUDE.md`:

```
To ask a Claude session in another repo to do something, run `aq tell <name> "<message>"`; `aq tell` alone lists the sessions.
```

Claude asks before it runs `aq tell` unless the command is allowed, for example with `Bash(aq tell:*)` in `permissions.allow`.

![Switching between sessions, messaging one, and asking where it's up to](docs/media/sessions.svg)

![N starts a session in a worktree of its own, then Ctrl-O starts one in a folder that is made for it](docs/media/new.svg)

`!` opens a shell in a tmux popup, in the project's folder, for things Claude can't run itself: sudo, logins, or commands its permissions block. The last `! command` Claude suggested is copied and, in zsh, already typed in, so Enter runs it. The popup closes when the shell exits.

![Claude asks for a command that needs sudo; ! opens a shell with it typed in](docs/media/shell.svg)

## Keys

The first time aq starts, a tour has you try the workflow on made-up cards, telling you which key to press, one at a time. First a review (move, comment, mark done, send, approve), then `M` into step mode (allow one edit, refuse the next with a reason), then quiz mode (answer a question and send the gap the judge found back as a fix). Last come three made-up sessions: pick one, message it, ask where it's up to, start a new one and move its project through the modes. Nothing in the tour reaches a real session. `F2` runs the tour again, and `F1` lists every key.

![The first-run tour: practice cards for review, step and quiz, then made-up sessions, asking for each key](docs/media/tour.svg)

The ones worth knowing from day one:

| Key | Where | What it does |
|---|---|---|
| `M` | anywhere | next mode for this project: review, step, quiz, vibe |
| `?` | anywhere | ask the session a side question; a fork answers, so the session never sees it |
| `!` | anywhere | shell in a popup, with Claude's last `! command` typed in |
| `W` | anywhere | ask the session where it's up to |
| `N` | anywhere | start a new Claude session in this project; `Tab` puts it in its own worktree, `Ctrl-O` picks another folder |
| `O` | anywhere | open a workspace: a Claude session for each git repo in a folder, in one tmux session |
| drag | anywhere | select text; letting go copies it |
| double-click, triple-click | anywhere | copy the word, or the whole line, as in Ghostty |
| `Space` | card | skip it for now |
| `c` `s` `a` | review | comment, send the comments, approve the lot |
| `d` | review | mark a file done |
| `f` | review | hide or show the file list, for a wider diff |
| `e` | review | open the file at this line in `$VISUAL` or `$EDITOR` |
| `y` `n` | step, permission | allow, or refuse with a reason |
| `a` `Enter` | question | approve, or reply in your own words |
| `y` `e` `n` | message | send a session's `aq tell` message, edit it first, or drop it |
| `1`-`9` `o` | option menu | pick an option, or answer in your own words |
| `h` `l` | sessions | pick a session |
| `Tab` | sessions | back to the session shown before; again to return |
| `Enter` | sessions | message it |
| `m` | sessions | switch its model: fable, opus, sonnet or haiku |
| `P` | sessions | switch its permission mode: auto, manual, accept edits or plan |
| `Esc Esc` | sessions | stop it |

Moving around works as in vim: `j` `k`, `Ctrl-d` `Ctrl-u`, `gg` `G`, `]c` `[c` for changes and `H` `L` for files.

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

aq can start a session (`N`), and in a worktree of its own if you ask, but it leaves branches and merging to you and Claude: start Claude however you like and aq picks it up. If you'd rather have one app that manages every session's branch for you, one of the others is a better fit.

## Install

The prompt at the top of this page does all of this for you. By hand: aq runs on Linux, inside tmux. Either take the binary from the [latest release](https://github.com/nodo-labs-uk/attention-queue/releases/latest):

```sh
curl -fsSL https://github.com/nodo-labs-uk/attention-queue/releases/latest/download/aq-x86_64-linux.tar.gz | tar xz
install aq-x86_64-linux/aq ~/.local/bin/
```

or build it from source with `cargo install --path .`.

Then add the hooks from [`contrib/settings-hooks.json`](contrib/settings-hooks.json) to `~/.claude/settings.json`. Merge them into your existing `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays rather than replacing them, or your other hooks will stop running. [docs/hooks.md](docs/hooks.md) has the details.

Run `aq` in a tmux window.

## Files

- `~/.config/aq/config.toml`: the theme, each checkout's mode, whether the file list is hidden, and whether the tour has been seen (`t` cycles themes).
- `~/.local/state/aq/state.json`: review rounds, unsent comments and quiz progress, so a restart loses nothing.
- Socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`. `AQ_SOCKET` overrides it.

## Development

`cargo test` runs everything, including end-to-end tests against a private tmux server. `demo/record.py` re-records the videos with fake sessions and a stand-in `claude`, so it needs no login and costs nothing. Pushing a `v*` tag builds a release.

## Licence

[MIT](LICENSE): use it, change it and share it, for anything.

---

aq was built with [Claude Code](https://claude.com/claude-code). Issues, PRs and other contributions are welcome. You could also point your own agent at this repo and have it build you a version from scratch, but this one saves you the tokens.
