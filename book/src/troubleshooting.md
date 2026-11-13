# Troubleshooting

## No tabs in the sessions view

- aq must run inside tmux, on the same tmux server as your sessions.
- aq finds sessions by looking for `claude` processes in tmux panes, through `/proc`. It runs on Linux only.

## Step mode lets edits through

The session was probably started before you added the hooks. Restart it. Check also that the `PreToolUse` matcher includes `Edit|Write|MultiEdit|NotebookEdit`.

## Permission prompts show in the window, not in aq

- aq is not running, or the session started before the hooks were added.
- The `PermissionRequest` hook is missing from `~/.claude/settings.json`.

## Option menus show in the window

The `PreToolUse` matcher must include `AskUserQuestion`.

## "is open to other users" on start

The socket's folder can be opened by other users. Make it private (`chmod 700`), or point `AQ_SOCKET` at a file in a folder only you can open.

## A message waits and is never sent

A message is sent once its session is idle at its prompt with an empty input box. If you have half-typed something in the session's own window, the message waits until the box is empty. `Esc Esc` on the session's tab drops a queued message.

## A system broadcast garbles the screen

A `wall` message (such as "The system will reboot at...") is written over every terminal, aq and the Claude panes included. aq repaints its own screen within 5 seconds, and Ctrl-L repaints it at once. A session whose screen shows a broadcast gets a Ctrl-L too, which makes Claude Code redraw without touching what is typed in its box. The broadcast is never read as text typed in that box.

## Report a problem

Open an issue at [github.com/nodo-labs-uk/attention-queue/issues](https://github.com/nodo-labs-uk/attention-queue/issues), with what you did, what you expected and what happened.
