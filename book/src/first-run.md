# First run

## The tour

The first time aq starts, a tour opens. It takes about four minutes, and most of it is you pressing keys on made-up cards:

1. A review: move around, comment, mark a file done, send, approve.
2. Step mode: allow one edit, refuse the next with a reason.
3. Quiz mode: answer a question, and send the gap the judge found back as a fix.
4. Three made-up sessions: pick one, message it, ask where it is up to, start a new one, and move its project through the modes.

Nothing in the tour reaches a real session. `F2` runs it again at any time.

![The first-run tour: practice cards for review, step and quiz, then made-up sessions](media/tour.svg)

## The screen

- **The top bar** shows how many cards are waiting behind the one on screen, how many questions are stashed (`S` lists them), and `F3 keys  F1 help` on the right.
- **The middle** is the card on screen, or, when nothing is waiting, the [sessions view](sessions.md).
- **The keys row** along the bottom lists the keys for what is on screen. It is hidden until `F3` shows it, and aq remembers the choice. While it is hidden, status messages show in the top bar.

## Help

- `F1` opens the help page with every key. `j` and `k` scroll it, `Esc` closes it.
- `F3` shows or hides the keys row.
- [Every key](keys.md) has the same list as this site.

## Quitting

`Q` quits. aq saves review rounds, unsent comments and quiz progress, so a restart loses nothing. Cards are not saved. A hook still waiting when aq quits gets no answer: a permission prompt then shows in the session's own window, and an edit waiting in step mode is refused, so Claude tries it again later.
