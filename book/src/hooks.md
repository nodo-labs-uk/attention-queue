# Hooks

aq talks to Claude Code through three hooks. Each one runs `aq hook <event>`, which sends the event to the running aq over a Unix socket and prints aq's answer.

## The hooks

Add these to `~/.claude/settings.json`, merged into any arrays you already have. The same JSON is in [`contrib/settings-hooks.json`](https://github.com/nodo-labs-uk/attention-queue/blob/main/contrib/settings-hooks.json).

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit|AskUserQuestion",
        "hooks": [
          { "type": "command", "command": "aq hook pre-tool --deadline-secs 3570", "timeout": 3600 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash|Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "aq hook post-tool", "timeout": 30 }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          { "type": "command", "command": "aq hook permission --deadline-secs 3570", "timeout": 3600 }
        ]
      }
    ]
  }
}
```

| Hook | What aq does with it |
|---|---|
| `PreToolUse` on edits | In step mode, holds the edit until you answer its [step card](cards/step.md). In other modes, answers at once. |
| `PreToolUse` on `AskUserQuestion` | Shows Claude's option menu as a [card](cards/question.md#option-menu) and answers it. |
| `PostToolUse` | Notes which files each session touched, for review cards, and spots [shell changes](cards/shell-change.md) in step mode. Never blocks. |
| `PermissionRequest` | Shows the request as a [permission card](cards/permission.md). |

## When aq is not running

The hooks do nothing. With no answer from aq, Claude Code carries on as normal: edits go through the usual permission flow, and permission prompts and menus show in the session's window. Silence never approves anything.

## Timeouts

Claude Code cancels a hook after its `timeout`, and a cancelled hook does not block the call. So aq's hooks answer by themselves 30 seconds before the timeout (`--deadline-secs 3570` against `timeout: 3600`):

- an edit still waiting in step mode is refused, with a note telling Claude to try again shortly;
- a permission request or option menu gets no decision, so it shows in the session's window.

## Restart sessions after adding hooks

A Claude session reads its hooks when it starts. Restart sessions that were running before you added them, or step mode and the cards that come from hooks will not work for them.

## Hook formats

The JSON aq reads and writes for each event, checked against Claude Code's hooks reference with real runs, is in [docs/hooks.md](https://github.com/nodo-labs-uk/attention-queue/blob/main/docs/hooks.md) in the repo.
