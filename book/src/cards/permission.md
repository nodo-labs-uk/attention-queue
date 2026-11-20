# Permission card

When Claude asks for permission to run a tool, the `PermissionRequest` hook sends the request to aq, and it becomes a permission card. Permission cards come first in the queue, because the session is stuck until you answer.

## The summary

Above the request, a line starting `Haiku:` says what the call does and what the agent wants it for. aq asks Claude Haiku for it when the card arrives, through your own `claude` login, as a read-only fork of the session, so Haiku knows what the agent is working on. It takes a second or two; until then the line reads `Haiku: summarising…`.

The summary is a model's reading of the request and it can be wrong. The request below it is what Claude will run, and `y` still needs you to have seen all of it. A screen too short to show both leaves the summary out.

## Keys

| Key | What it does |
|---|---|
| `y` | allow, once you have scrolled to the end |
| `n` | deny, with a reason Claude reads |
| `j` `k` | scroll |
| `PgUp` `PgDn` | scroll a page |
| `G` | go to the end |
| `g` | go to the session's window |

`y` works only once you have seen the whole request, so a long command cannot hide something at the end.

## If you answer in the window

You can still answer the prompt in the session's own window. The card then stays, marked "answered elsewhere", until you press a key.

If aq is not running, or quits while a request waits, the normal permission prompt shows in the session's window.
