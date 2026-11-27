# Side questions, shell and fast reading

## Side questions

`b` asks the session a side question with Claude Code's own `/btw`: the session of the card on screen, or the picked session in the sessions view. You type the question in aq's window. aq types `/btw` and your question into the session's box, waits for the answer, copies it out of the `/btw` panel and closes the panel, so only the answer shows in aq. `/btw` answers from the conversation without adding to it, even while the session is working. The session's box must be empty: aq never types over a half-written message. The panel's copy also puts the answer on your clipboard.

`W` asks a copy of the session (`claude --resume --fork-session`) where it is up to: a short summary of what is done and what is left. It runs at low effort, so it answers in about half the time; the copy still uses the session's own model and its prompt cache, so it costs no more.

`T` sums up the latest turn only, in a short window titled TL;DR. The first line is the one thing you most need to know or decide. Up to four short lines follow with the facts you need to act on it: what changed, what Claude found, what is left, and the options if you must choose. It runs on Claude Haiku, through your own `claude` login, so it is quick and cheap. Haiku gets no tools: it reads the conversation and nothing else.

`b` in that window asks a `/btw` question of the same session, and `W` and `T` ask again.

## Shell

`!` opens the project's shell in a tmux popup, in the project's folder. Use it for things Claude cannot run itself: `sudo`, logins, or commands its permissions block.

Each project has one shell, and it stays alive. `Ctrl-Q` hides the popup and the shell keeps running, with what you started in it; the next `!` shows the same shell. So `sudo` asks for your password only as often as it does in any one terminal (15 minutes after your last `sudo` command, unless your sudo settings say otherwise). Leaving the shell (`exit`) ends it, and the next `!` starts a new one. The shells are tmux sessions, so they also outlive aq.

The command Claude last suggested is copied and typed in at the prompt, so `Enter` runs it. aq takes it from Claude's latest reply: a `! command`, or else a shell code block or a command that starts with `sudo`. If the latest reply has none, aq takes the last `! command` of an earlier reply. aq only copies the command, without typing it, when the shell is busy with another program (a root shell from `sudo -s` counts as one) or when the command has several lines in a shell that is already open.

![Claude asks for a command that needs sudo; ! opens a shell with it typed in](media/shell.svg)

## Quick session

`E` opens a new Claude session in a popup over aq, for a small job that has nothing to do with the work on screen, such as writing an email or looking something up. It runs in a folder of its own, away from your projects: `~/.local/share/aq/quick`, or `aq/quick` under `$XDG_DATA_HOME` if that is set. Every quick session uses the same folder, so Claude asks you to trust it only the first time.

The quick session is kept out of aq: it has no tab, and its prompts and questions show in the popup, not as cards. `Ctrl-Q` closes the popup and ends the session. If the folder is empty, aq then deletes it. If the session left files there, such as a draft, aq keeps them and says where; the next quick session starts with them in its folder.

## Fast reading

`r` shows a text one word at a time, in the same place, so your eyes do not move. It works on question cards, on messages between sessions, and on a session's last reply in the sessions view.

| Key | What it does |
|---|---|
| `Space` | pause or play |
| `↑` `↓` | faster or slower, by 25 words a minute |
| a number, then `Enter` | set the speed in words a minute |
| `←` `→` | back or on ten words |
| `Home` | back to the start |
| `Esc` | close |

The speed starts at 350 words a minute, between 100 and 1500, and aq remembers it. Each word's focus letter, a little left of its middle, sits under the guide marks. Words at the end of a sentence or clause, and long words, stay a little longer.

## Selecting text

Drag with the mouse to select text; letting go copies it. A double-click copies the word, and a triple-click the whole line.
