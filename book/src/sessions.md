# Sessions view

When nothing is waiting, aq shows a tab for each Claude session, with the picked session's screen live underneath.

![Switching between sessions, messaging one, and asking where it's up to](media/sessions.svg)

## Tabs

- `●` means the session is waiting for you; `○` means it is working. A session with only background jobs running counts as waiting, so messages still reach it.
- Each tab shows the session's permission mode (auto, plan, accept edits) as its footer does, and the project's mode when it is not review.
- Each [workspace](new-sessions.md#workspaces) gets a row of its own, and so does each other tmux session, led by its name.

While aq's window is showing, tmux's own status line is hidden, since the tabs list the sessions. It shows on your other windows, and comes back when aq quits.

## Keys

| Key | What it does |
|---|---|
| `h` `l` (or `j` `k`) | pick a session |
| `Tab` | the session you last typed in, here or in its own window; `Tab` again goes back to the one before. Moving with `h` `l` does not count. |
| `g` | go to its window |
| `Enter` | message it: sent now, or once it is idle (`Esc` cancels) |
| `a` | approve: sends "Approved, carry on." now, or once it is idle |
| `d` | done: sends "Done, continue." now, or once it is idle, for when it asked you to do something first |
| `A` | message every session in its row, each once idle |
| `W` | ask where it is up to |
| `T` | Haiku sums up its latest turn |
| `?` | ask it a [side question](tools.md#side-questions) |
| `r` | [read its last reply fast](tools.md#fast-reading) |
| `b` | that session in a popup over aq, with `/btw ` typed in its box for a side question; Ctrl-Q closes it, and the side-question panel in the session with it |
| `c` | the same popup with `/mcp ` typed in; Enter shows the session's MCP servers, to connect, reconnect or turn one off. Ctrl-Q closes it, and the menu in the session with it |
| `p` | ask an idle session to open a PR (`/create-pr`) and go there |
| `m` | switch its model: `f` fable, `o` opus, `s` sonnet, `h` haiku |
| `P` | switch its permission mode: `a` auto, `m` manual, `e` accept edits, `p` plan |
| `x` | close the session's pane, after a yes (`claude --resume` reopens it) |
| `Esc Esc` | stop what it is doing, as `Esc` in Claude Code; also drops a message queued for it |
| `M` | next mode for its project; the only way back from vibe mode |

## Messages

`Enter` opens a box to message the picked session. If the session is busy, the message waits and is sent once the session is idle at its prompt.

Claude Code's suggested next prompt, when it has one, shows in lighter text in the box. `Tab` or `Ctrl-F` fills it in.

`p` and `m` use Claude Code's own commands: `/create-pr`, and `/model`, which also makes the model the default for new sessions.

## Scrolling back

| Key | What it does |
|---|---|
| `Ctrl-U` `Ctrl-D`, `PgUp` `PgDn`, or the mouse wheel | scroll the session's screen back, into its conversation |
| `Home` | its oldest line |
| `G` or `End` | back to the latest |

Scrolling up past the screen reads the session's conversation from its transcript, so you can read further back than tmux keeps.
