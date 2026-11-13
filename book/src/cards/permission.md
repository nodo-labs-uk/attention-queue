# Permission card

When Claude asks for permission to run a tool, the `PermissionRequest` hook sends the request to aq, and it becomes a permission card. Permission cards come first in the queue, because the session is stuck until you answer.

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
