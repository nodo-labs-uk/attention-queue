# Every key

This is the same list as aq's help page (`F1`).

## Anywhere

| Key | What it does |
|---|---|
| `F1` | help |
| `F2` | the tour again |
| `F3` | show or hide the keys row |
| `M` | next mode for this project |
| `t` | next theme |
| `Q` | quit aq |
| `?` | ask the session a side question; a copy answers |
| `W` | where are we: a short summary of what is done and what is left |
| `!` | a shell in a popup, in this project |
| `N` | a new Claude session in this project |
| `O` | open a workspace |
| drag | select text; letting go copies it |
| double-click, triple-click | copy the word, or the whole line |

## Every card

| Key | What it does |
|---|---|
| `Space` | skip this card |
| `B` | move it to the back of the queue |

## Moving around, vim style

| Key | Move |
|---|---|
| `j` `k` | line |
| `Ctrl-d` `Ctrl-u` | half a page |
| `Ctrl-f` `Ctrl-b` | whole page |
| `gg` `G` | first or last line |
| `{` `}` or `[c` `]c` | previous or next change |
| `H` `L` or `[[` `]]` | previous or next file; `Tab` also goes to the next one (on a step card, between changes) |

## Review card

| Key | What it does |
|---|---|
| `c` | comment |
| `x` | delete comment |
| `s` | send comments |
| `a` | approve all |
| `d` | mark the file done |
| `u` | show done files again |
| `V` | range |
| `v` | split or unified view |
| `r` | whole branch or this round |
| `m` | Markdown as written, or as read |
| `f` | hide or show the file list |
| `e` | open the file at this line in your editor |

## Step card

| Key | What it does |
|---|---|
| `y` | allow |
| `n` | reject with a reason |
| `e` | ask for a different change |
| `za` | fold |
| `zR` | unfold all |
| `w` `b` | word forward or back |
| `gd` | find the word in the project |
| `m` | raw input |

## Shell change card

| Key | What it does |
|---|---|
| `y` | keep the change |
| `u` | ask Claude to undo it |
| `v` | view |
| `f` | file list |

## Permission card

| Key | What it does |
|---|---|
| `y` | allow (once you have scrolled to the end) |
| `n` | deny with a reason |
| `PgUp` `PgDn` | page |
| `G` | end |
| `g` | go to the session's window |

## Question card

| Key | What it does |
|---|---|
| `a` | approve (sends "Approved, carry on.") |
| `Enter` | reply in your own words |
| `x` | stash the card |
| `S` | the stashed questions: `Enter` brings one back, `x` drops it |
| `r` | read it fast |
| `g` | go to the session's window |

## Option menu

| Key | What it does |
|---|---|
| `1`-`9` | pick (or tick, where several apply) |
| `j` `k` + `Enter` | pick |
| `Space` | tick |
| `o` | answer in your own words |
| `u` | back a question |
| `x` | let the menu show in the session's window |
| `g` | the same, and go there |

## Message card

| Key | What it does |
|---|---|
| `y` | send it |
| `e` | edit it first |
| `n` | drop it |
| `r` | read it fast |

## Quiz card

| Key | What it does |
|---|---|
| `Ctrl-S`, or `Enter` on an empty line | submit |
| `Esc` | put the box away |
| `Enter` | answer again after `Esc` |
| `m` | whole file |
| `y` | fine as is |
| `c` | ask for a fix |
| `e` | show the answer now |
| `r` | judge or generate again |
| `x` | dismiss a failed quiz |

## Fast reader

| Key | What it does |
|---|---|
| `Space` | pause or play |
| `↑` `↓` | speed |
| number, `Enter` | set the speed |
| `←` `→` | back or on ten words |
| `Home` | the start |
| `Esc` | close |

## Sessions view

| Key | What it does |
|---|---|
| `h` `l` (`j` `k`) | pick a session |
| `g` | go to its window |
| `Tab` | the session you last typed in; again for the one before |
| `Enter` | message it |
| `A` | message every session in its tmux session |
| `p` | ask it to open a PR |
| `m` | switch its model |
| `P` | switch its permission mode |
| `r` | read its last reply fast |
| `x` | close its pane |
| `Esc Esc` | stop it |
| `Ctrl-U` `Ctrl-D`, `PgUp` `PgDn`, wheel | scroll its screen back |
| `Home` | its oldest line |
| `G` `End` | back to the latest |

## Typing in a box

| Key | What it does |
|---|---|
| `Ctrl-U` | delete to the line start |
| `Ctrl-K` | delete to the line end |
| `Ctrl-W` | delete the word before |
| `Alt-Backspace` | the same, up to punctuation |
| `Ctrl-A` `Ctrl-E` | line start or end |
| `Alt-B` `Alt-F` | word back or forward |
