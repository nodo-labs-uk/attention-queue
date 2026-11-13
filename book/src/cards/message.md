# Message between sessions

A session can ask another session to do something with `aq tell`:

```sh
aq tell web "The API now returns created_at as an ISO date; update the client."
```

The name is the other session's project, as its tab shows it. `aq tell` with no arguments lists the sessions it can tell.

Nothing reaches the other session until you say so. Each message becomes a card:

| Key | What it does |
|---|---|
| `y` | send it, starting "From the <name> session:" |
| `e` | edit it first |
| `n` | drop it |
| `r` | [read it fast](../tools.md#fast-reading) |
| `j` `k` | scroll |

In vibe mode the message goes at once, with no card.

## Telling sessions about it

Sessions opened from a [workspace](../new-sessions.md#workspaces) are told how to use `aq tell`. For a session started another way, add a line like this to its `CLAUDE.md`:

```text
To ask a Claude session in another repo to do something, run `aq tell <name> "<message>"`; `aq tell` alone lists the sessions.
```

Claude asks before it runs `aq tell` unless the command is allowed, for example with `Bash(aq tell:*)` in `permissions.allow`.

`aq tell` exits with status 0 once the card is in aq's queue, and with 1 and a reason if it could not be (for example, aq is not running, or the name matches no session). Claude does not wait for your answer.
