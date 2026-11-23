# Question and option menu

## Question card

When a session ends its turn by asking you something, such as approving a draft or choosing between two ways, its last message becomes a question card. If a message of yours is already waiting to go to that session, it answers the question, so no card comes up.

![A session asks to approve a PR draft and a approves it; then Claude's option menu is answered from a card](../media/questions.svg)

| Key | What it does |
|---|---|
| `a` | approve: sends "Approved, carry on." (only when the question asks for a go-ahead or names its own pick) |
| `d` | done: sends "Done, continue.", once you have done what it asked |
| `Enter` | reply in your own words; Claude's suggested next prompt shows in lighter text, and `Tab` or `Ctrl-F` fills it in |
| `x` | stash the card |
| `r` | [read it fast](../tools.md#fast-reading), a word at a time |
| `g` | go to the session's window |
| `j` `k` | scroll |

## Stashed questions

`x` puts a question aside without answering it. The top bar shows how many are stashed, and `S` lists them:

| Key | What it does |
|---|---|
| `Enter` | bring the question back to the front of the queue |
| `x` | drop it |
| `j` `k` | choose |
| `Esc` | close the list |

A stashed question goes once its session moves on.

## Option menu

When Claude asks with its own menu of options (its `AskUserQuestion` tool), the `PreToolUse` hook catches the menu before it shows, and the card lists the options. The answer goes back through the hook, so nothing is typed into the session.

| Key | What it does |
|---|---|
| `1`-`9` | pick an option |
| `j` `k` and `Enter` | the same |
| `Space` or `1`-`9` | tick an option, where several apply; `Enter` answers |
| `o` | answer in your own words |
| `u` | back a question |
| `x` | let the menu show in the session's window instead |
| `g` | the same, and go there |

This needs `AskUserQuestion` in the `PreToolUse` hook's matcher. The hooks in `contrib/settings-hooks.json` have it.

## Flagged questions and menus

A question or menu that leads to a [flagged action](flagged.md), such as merging a PR or pushing, carries a band too, so you see the stakes before you choose. When the text mentions a PR, an issue, a merge, a push or the like, the card keeps one row, `Checking whether this involves others…`, while Haiku and `gh` look. If it is flagged, the row becomes the band above the text; if not, the row goes.

You can answer while the check runs, and an answer never approves the action. Each action that follows is held on its own card.
