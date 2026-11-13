# Cards

Each kind of card has its own keys. These keys work on every card:

| Key | What it does |
|---|---|
| `Space` | skip this card for now |
| `B` | move it to the back of the queue |
| `?` | ask the session a [side question](../tools.md#side-questions) |
| `W` | where are we: a short summary of what is done and what is left |
| `!` | a [shell](../tools.md#shell) in a popup, in this project |
| `N` | a [new Claude session](../new-sessions.md) in this project |
| `O` | open a [workspace](../new-sessions.md#workspaces) |
| `M` | next [mode](../modes.md) for this project |
| `t` | next theme |
| `F1` `F2` `F3` | help, the tour, the keys row |
| `Q` | quit aq |

Moving around works as in vim, on every card that scrolls:

| Keys | Move |
|---|---|
| `j` `k` | a line |
| `Ctrl-d` `Ctrl-u` | half a page |
| `Ctrl-f` `Ctrl-b` | a whole page |
| `gg` `G` | first or last line |
| `{` `}` or `[c` `]c` | previous or next change |
| `H` `L` or `[[` `]]` | previous or next file; `Tab` also goes to the next one |

The kinds of card:

- [Review](review.md): a finished turn's diff.
- [Step](step.md): one edit, waiting to be applied.
- [Shell change](shell-change.md): what a shell command changed, in step mode.
- [Permission](permission.md): Claude asks to run a tool.
- [Question and option menu](question.md): a session asks you something.
- [Message between sessions](message.md): one session asks another to do something.
- [Quiz](quiz.md): explain a piece of the diff.
