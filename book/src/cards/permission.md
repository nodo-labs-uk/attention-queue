# Permission card

When Claude asks for permission to run a tool, the `PermissionRequest` hook sends the request to aq, and it becomes a permission card. Permission cards come first in the queue, because the session is stuck until you answer.

## What the card shows

The card shows Haiku's summary, then the tool's name. The request itself is hidden until `v` shows it under the tool's name. A shell command's time limit and own description follow the tool's name, so the request below is only the command. A plan that Claude asks you to approve shows as its text. Other fields show as `key: value` lines. Long lines wrap at spaces.

## The summary

Above the request, a line starting `Haiku:` says what the call does and what the agent wants it for. aq asks Claude Haiku for it when the card arrives, through your own `claude` login, as a read-only fork of the session, so Haiku knows what the agent is working on. It takes a second or two; until then the line reads `Haiku: summarising…`.

On a plan, Haiku lists the plan's key points instead, one per line under `Haiku's key points:`, in up to eight rows. Anything risky, or a question the plan leaves for you, comes first.

## Approving a plan

![A plan comes up as a card with Haiku's key points; y approves it and picks yes in Claude Code's own plan menu](../media/plan.svg)

Claude Code shows its own "Would you like to proceed?" menu for a plan as well as asking aq. aq keeps that menu off the queue while the plan's card waits. `y` approves the plan and picks "Yes, and use auto mode" in the menu for you. `n` sends your reason, and Claude stays in plan mode and changes the plan. To pick another option, such as approving edits by hand, answer the menu in the session's window.

The summary is a model's reading of the request and it can be wrong. The request is what Claude will run: `v` shows it.

`y` allows on the summary alone. Until the summary arrives, `y` waits for it. If you show the request with `v`, `y` needs you to have scrolled to its end, and the same holds when the summary failed or the screen is too short to show it, because the card then shows the request. A [flagged action](flagged.md) always shows its whole request.

## Keys

| Key | What it does |
|---|---|
| `y` | allow, on Haiku's summary; with the request shown, once you have scrolled to its end. On a plan, `y` also answers Claude's own plan menu with "Yes, and use auto mode" |
| `n` | deny, with a reason Claude reads |
| `v` | show the whole request, or hide it again |
| `j` `k` | scroll the request |
| `PgUp` `PgDn` | scroll a page |
| `G` | go to the end |
| `g` | go to the session's window |
| `c` | copy the command of the request; press it again for each web link in it, last first |

`y` works only once you have seen the whole request, so a long command cannot hide something at the end.

## If you answer in the window

You can still answer the prompt in the session's own window. The card then stays, marked "answered elsewhere", until you press a key.

If aq is not running, or quits while a request waits, the normal permission prompt shows in the session's window.
