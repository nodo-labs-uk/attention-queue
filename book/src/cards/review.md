# Review card

In review mode, when a session finishes a turn, its diff becomes a review card. You read the change, comment on lines, and send all the comments back to the session as one message, like a pull request review.

![A finished turn becomes a review card, and the comment goes back to the session](../media/review.svg)

## Keys

| Key | What it does |
|---|---|
| `c` | comment on the line, or on the range you picked with `V` |
| `x` | delete the comment under the cursor |
| `s` | send every comment to the session as one message |
| `a` | approve the lot |
| `d` | mark the file done: it is ticked and skipped |
| `u` | show done files again |
| `V` | start a range of lines |
| `v` | split or unified view |
| `r` | the whole branch, or only this round |
| `m` | Markdown as written, or as read (the default) |
| `f` | hide or show the file list, for a wider diff (remembered) |
| `e` | open the file at this line in `$VISUAL` or `$EDITOR`, in a popup |

Moving: `j` `k` by line, `]c` `[c` by change, `H` `L` or `Tab` by file.

## Rounds

Each time a session finishes a turn, aq starts a new round for that project. By default the card shows only what changed in this round. `r` switches to the whole branch, which helps when a fix touches code from an earlier round.

## Markdown

Markdown files (plans, specs, READMEs) show as read: headings, bold, code and bullets are styled and their marks hidden. `m` shows the file as written.

## Comments survive a restart

Unsent comments are saved. If aq restarts, they come back on the next review card for the same project and round.

## A question in the same turn

If the turn ended with a question as well as a diff, the question shows on the review card, and sending your comments answers it.
