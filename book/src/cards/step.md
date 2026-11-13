# Step card

In step mode, each edit Claude wants to make waits on a step card before it is applied. The card shows the edit as a diff against the file as it is now.

![An edit waiting on a step card, allowed with y, then another rejected with a reason](../media/step.svg)

## Keys

| Key | What it does |
|---|---|
| `y` | allow the edit |
| `n` | refuse it, with a reason Claude reads |
| `e` | ask for a different change |
| `j` `k` | move |
| `H` `L` or `Tab` | previous or next change |
| `za` | fold or unfold the context |
| `zR` | unfold all |
| `w` `b` | next or previous word |
| `gd` | find the word under the cursor in the project |
| `m` | show the raw tool input |

## How the wait works

The `PreToolUse` hook holds Claude's edit until you answer. Claude waits; it does not time out for almost an hour. If aq quits while an edit is waiting, the edit is refused with a note to try again, so nothing lands unchecked.

Step mode needs the aq hooks in the session's settings. A session started before you added them has to be restarted.
