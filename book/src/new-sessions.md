# New sessions and workspaces

## Reopened after a restart

aq keeps a list of the Claude sessions open in tmux. When it starts and finds none open, as after the machine restarts, it reopens each one with `claude --resume`, in its old tmux session (made again if it is gone) and under its old window name. A session whose folder is gone, such as a removed worktree, is left out. If you close every session while aq is running, there is nothing to reopen next time.

## A new session: N

`N` starts a new Claude session in the project in front of you, in a tmux window of its own. What you type in the box is its first prompt. With no session open, it starts in the folder you started aq in.

![N starts a session in a worktree of its own, then Ctrl-O starts one in a folder that is made for it](media/new.svg)

| Key in the box | What it does |
|---|---|
| `Tab` | give the session a git worktree of its own (`claude --worktree`), so two sessions do not edit the same files |
| `Ctrl-O` | pick another folder |
| `Enter` | start the session |

After `Ctrl-O`, type a path. The matching folders are listed underneath: `Tab` completes and the arrows choose. A folder that is not there yet is made when the session starts.

## Trusting a folder

The first time Claude Code starts in a folder, it asks whether you trust it. aq shows the question in its own box:

| Key | What it does |
|---|---|
| `y` | trust the folder |
| `n` | close the new session |
| `a` | trust every folder waiting |

When several sessions ask at once, they are asked about in turn.

## Workspaces: O

A workspace is a set of git repos in one folder, for work that spans them. `O` opens one:

1. Pick the folder that holds the repos. aq finds the git repos inside it, up to two levels down, and ticks them all.
2. `Space` unticks a repo you do not want; `a` ticks all or none.
3. `Enter` starts a Claude session in each repo, as windows of one tmux session named after the folder. They share a row of tabs in the sessions view.

aq saves the set in its config, and `O` lists it next time. Opening it again starts only the repos that have no session in that tmux session yet.

Sessions opened from a workspace are told about each other and about [`aq tell`](cards/message.md), so they can ask each other for work.
