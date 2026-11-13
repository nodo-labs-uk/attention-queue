# aq

![aq: an attention queue for Claude Code](media/banner.svg)

aq is a terminal app for people who run several Claude Code sessions in tmux. It watches every session and puts whatever needs you in one queue, shown one card at a time:

- a diff to review when a session finishes its turn,
- an edit to allow before it is applied,
- a permission prompt,
- a question, or Claude's own menu of options.

A card that comes in while you are on another one waits its turn. It never takes the screen. When nothing is waiting, aq shows every session live, and you can message one, start a new one, or ask where it is up to.

![Three sessions at work: a permission prompt is allowed, a finished turn is reviewed and the comment sent back, and a question is answered, all from aq](media/hero.svg)

## How it works

aq connects to Claude Code through [hooks](hooks.md). When Claude wants to edit a file, asks for a permission or shows an option menu, the hook asks aq over a Unix socket and waits for your answer. When a session finishes its turn, aq reads its diff and its last message. If aq is not running, the hooks answer nothing and Claude carries on as normal.

aq does not start or own your sessions. Start Claude however you like, in any tmux window, and aq finds it.

## Design principles

- **Your attention is the bottleneck.** The work comes to you in one queue, so you never cycle through windows to find the session that is waiting.
- **An easy review loop.** Comment on a line or a range of a diff, and send the comments to the session as one message, like a pull request review. The review is inspired by [tuicr](https://tuicr.dev/).
- **Checking to suit the project.** Each project has its own [mode](modes.md): review, step by step, quiz or vibe.
- **Unobtrusive.** aq runs beside your sessions. It needs tmux and the hooks, nothing else.
- **Keyboard first,** with vim keys.

## Requirements

- Linux (aq reads `/proc` to find Claude sessions).
- tmux, with aq and your Claude sessions running inside it.
- Claude Code, with the aq hooks added to its settings.

aq is open source under the MIT licence: [github.com/nodo-labs-uk/attention-queue](https://github.com/nodo-labs-uk/attention-queue).
