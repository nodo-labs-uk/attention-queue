# The queue

Everything that needs you becomes a card. aq shows one card at a time, and the top bar counts the cards behind it.

## Order

Cards are shown in this order:

1. Permission prompts and Claude's option menus. A session is stuck until you answer these.
2. Step and shell-change cards, grouped by project.
3. Everything else (reviews, questions, quizzes, messages), oldest first.

A card that comes in while you are on another one joins the queue. It never takes the screen from the card you are on.

## Moving cards

| Key | What it does |
|---|---|
| `Space` | Skip the card for now. It goes to the back of its group. |
| `B` | Move the card to the back of the whole queue, behind every other card. |
| `x` | On a question card: stash it. `S` lists stashed questions. |

## Answered elsewhere

If you answer a card's prompt in the session's own window instead, the card stays, marked "answered elsewhere", until you press a key. That way a card never disappears from under you.

## The 1.5 second guard

A card that takes the screen by itself ignores deciding keys (such as `y`, `n` and `a`) for 1.5 seconds. A key you meant for the previous card does not answer the new one by accident.

## When nothing is waiting

With an empty queue, aq shows the [sessions view](sessions.md): a tab per session, with the picked session's screen live underneath.
