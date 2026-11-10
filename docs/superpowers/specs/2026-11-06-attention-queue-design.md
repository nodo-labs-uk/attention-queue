# Attention queue: design

Working name `aq`. Status: draft for approval, 6 November 2026.

## Intent

Several Claude Code sessions run at once in tmux windows on one VPS. Human attention is sequential, but today the human has to go looking: switch windows, notice which agent is waiting, read a diff in another tool, then type feedback back into the right window.

`aq` turns that around. The user stays on one screen. Whatever needs a human decision arrives there as a card, one at a time, and the answer goes back to the right agent without leaving the screen. The user can always ask a question about the current card before deciding.

Success: the user reviews and steers all their agents from one tmux window, and understands and agrees with every change that lands.

## Goals

1. One screen, one card at a time, with a queue bar that shows what is waiting behind it. A new card joins the queue and never displaces the card on screen.
2. Three modes, chosen per project:
   - **Review board.** The agent finishes, then the user reviews the whole diff, comments on lines, and sends all the comments back as one message. The next round arrives as a new card.
   - **Step by step.** Each change arrives before it is applied, shown inside its full file, with a one-line description and the reason. The agent waits for approve, reject with a reason, or "change it like this". One project is worked through at a time.
   - **Quiz.** The agent finishes, then `aq` asks the user questions about what the change does: multiple choice, or free text that is judged. A wrong or partial answer shows the code and an explanation. Any gap becomes a decision: keep it as is, or send a fix.
3. `?` on any card asks the agent a side question without losing the user's place.
4. Beautiful and fast: good themes, syntax highlighting, no lag on large diffs.

## Non-goals for version 1

- Cloud containers (E2B, Daytona and the rest). The original brief wanted them. They are left out of version 1 so the three modes can be proved locally first. Nothing in the design may assume the agent is local beyond the single "agent connection" layer, so cloud can be added behind it later.
- Multiplayer and teammates watching.
- Starting sessions, creating worktrees, merging. The user keeps doing this with `cw`, lazygit and plain git.
- A browser or desktop view.
- Agents other than Claude Code.

## Anti-goals

- **Changing how sessions are launched.** `aq` adopts the user's existing tmux windows running `claude`. It must not require a wrapper or restart.
- **Touching sessions destructively.** The agent-deck test showed a manager killing the user's whole tmux session with no prompt. `aq` never kills, restarts or renames windows or sessions, and never changes tmux session options. It reads tmux and, when the user sends something, types into one pane.
- **More reading, not less.** The rules test showed a review layer can double what the user has to read. `aq` shows what needs a decision. Anything extra, like a generated summary, sits behind a key.
- **Blocking agents when `aq` is not running.** If `aq` is closed or crashes, every session behaves exactly as it does today.

## How it fits the current setup

- One tmux session `main`, several windows, each running interactive `claude`. Reached over ssh or mosh.
- Hooks already in `~/.claude/settings.json` (managed by chezmoi in `dot_claude/settings.json.tmpl`) set and clear the window option `@claude_waiting` on Stop, Notification, UserPromptSubmit and PostToolUse. `tmux-next-waiting` on Alt+; jumps to a waiting window.
- `claude agents --json` lists running sessions with status, `waitingFor` and pid.
- tuicr and lazygit are installed and stay in use.

## Decided by the user

- Terminal tool, written in Rust.
- Single screen with sequential cards and a queue bar.
- The three modes above.
- Review board: a whole-diff reader in the style of tuicr or diffview.nvim, with **split (two-column) and unified (+/−) views**, switchable.
- Step by step: the change shown **in the context of its file**, not just the hunk.
- Quiz: **multiple-choice and free-text** questions.
- Colour themes, syntax highlighting, and a polished look. The user approved the mockups in `.superpowers/brainstorm/*/content/modes-v2.html` in this folder: file tree on the left, vim-style keys, a key strip along the bottom, inline comments, folded file context, approved steps listed on the left in step mode, and verdict boxes in the quiz.

## Calls made by me (check these)

1. **Stack.** Rust with ratatui and crossterm. Syntax highlighting with syntect, or tree-sitter if it is clearly better for folding. Diffs from the `git` command line or `gix`; builder's choice.
2. **Fonts.** A terminal program cannot choose its font. `aq` uses colour, bold and italics; the font comes from the user's terminal. Mockup font buttons only previewed this.
3. **Themes.** Built in: Tokyo Night (default), Catppuccin Mocha, Gruvbox Dark, Rosé Pine Dawn (light). Users can add themes in a TOML file. Switch with a key at runtime; the choice is remembered.
4. **Finding agents.** Read tmux (`list-panes` across all sessions, pane command, cwd, `@claude_waiting`) joined with `claude agents --json` by pid. A pane running `claude` is an agent. Its project is its git root; the diff base is the merge-base with the repo's default branch, plus uncommitted and untracked changes.
5. **Sending text to an agent.** Load the message into a tmux buffer and paste it into that one pane with bracketed paste, then send Enter. This is how comments, rejections, "fix it" and quiz follow-ups reach the agent. Only send when the agent is waiting for input; otherwise hold the message and show it as queued.
6. **How step-by-step mode holds a change.** Through a Claude Code hook. A `PreToolUse` hook on `Edit|Write|MultiEdit|NotebookEdit` runs `aq hook`. It connects to the running `aq` over a Unix socket in `$XDG_RUNTIME_DIR` and sends the proposed edit. `aq` computes the file as it would be after the edit, and shows that as the card. The hook waits for the answer. It returns allow, or deny with the user's reason as the message the agent reads. If no `aq` is listening, or the project is not in step mode, the hook exits at once with no decision, so Claude behaves as normal.

   **When there is no answer (decided by the user): refuse and wait.** If `aq` was listening but the answer does not come (the hook nears its timeout, `aq` crashes mid-wait, or the edit is queued behind another step-mode project), the hook refuses the edit with a message such as "waiting for human review; try this edit again shortly". The proposed edit stays as a card, so nothing lands unseen. A step-mode project that is not at the front of the queue has its edits refused this way until its turn comes; the builder may choose a gentler hold if the agent loops badly on refusals.

   **Shell commands (decided by the user): edit tools are held; other changes are shown after.** Bash is not held. After each Bash call in a step-mode project, a `PostToolUse` hook asks `aq` to compare the checkout with its state before the command. Any files that changed become a card after the fact, showing the command and the diff, with approve, or "undo this" (sent to the agent as an instruction). The hook needs a long timeout, since the user may take minutes. The builder must check the current hook input and output format and the timeout limit against the docs for Claude Code 2.1.319 before relying on this. If `PreToolUse` cannot wait long enough, use `PermissionRequest` instead and run step-mode projects in a permission mode that asks before edits.
7. **Where the one-line description comes from.** The assistant text just before the tool call, read from the transcript (`transcript_path` in the hook input). If there is none, the card shows "no description" and `m` asks for one. No extra model call by default, to keep cards instant.
8. **Quiz questions and judging.** When a quiz-mode agent finishes (`@claude_waiting` set after a Stop), `aq` runs `claude -p` in the project folder with the diff and asks for 3 to 6 questions as JSON: a mix of multiple choice (with the right answer and an explanation) and free text (with what a good answer must cover). Free-text answers are judged by a second `claude -p` call that returns right, partial or wrong, plus the gap. Questions focus on behaviour, failure cases and anything risky, not trivia. Questions are generated in the background, so the card is ready when it reaches the front of the queue.
9. **Side questions (`?`).** Answered by `claude -p --resume <session-id> --fork-session`, so the answer knows the conversation but the live session is not interrupted or changed. The answer shows in a panel over the card. If forking is not available, fall back to a plain `claude -p` with the diff.
10. **Mode per project.** Set in `aq` with a key and stored in a small config file keyed by git root. The default is review board.
11. **Ordering.** Oldest card first. Step-mode projects are worked through one at a time: while a step-mode project has cards, other projects' cards wait behind it. A key skips the current card to the back.

    **The card on screen stays until the user acts on it (decided by the user).** A new card never replaces the card being shown, whatever its kind or priority. It joins the queue, and the queue bar shows it. The ordering rules here and under Cards decide only which waiting card comes next. The shown card leaves the screen only when the user answers it (approve, send, reject, allow, deny, finish the quiz) or skips it.

    **Stale cards (decided by the user).** A card goes stale when the thing it asks about is settled elsewhere: a permission or option-menu hook times out, or the user answers the prompt or question directly in the tmux window. A stale card that is still waiting in the queue is removed without notice. A stale card that is on screen stays there, marked "answered elsewhere", and nothing it does can reach the agent. Moving keys still scroll it; any other card key dismisses it and sends nothing.

    **A step card whose file changes under it** is replaced by a fresh card for the same edit. The fresh card takes the old card's place, on screen if the old one was there, and ignores decision keys for a moment so a key meant for the old version cannot decide it.
12. **Where it runs.** In its own tmux window, for example `main:aq`. One instance per user. The hook and the TUI talk over the socket; nothing listens on the network.
13. **Installing the hook.** Added to `dot_claude/settings.json.tmpl` in the chezmoi repo, next to the existing hooks, once the hook works.
14. **Location.** New repo at `~/devel/attention-queue`. This design moves there when the repo is created.
15. **Later review rounds.** Round 2 onwards shows only what changed since the previous round by default; `R` switches to the whole branch diff. Each time a review card is sent, `aq` records the tree state (a tree object hash, written with `git write-tree` on a temporary index so the user's index is untouched). Old comments stay attached to their file and nearest matching line, and are marked old.
16. **`e` in step mode.** The user types an instruction; the edit is refused with that instruction as the reason, and the agent tries again. Editing the proposed code directly is not in version 1.
17. **Quiz gaps and closing.** The judge says whether a gap is in the code (something worth fixing) or in the user's understanding (the code is fine; here is the explanation). Only code gaps offer "send a fix". When the last question is answered, any fixes the user chose go to the agent as one message. If none were chosen, the card closes and sends "approved, carry on".
18. **Permission prompts (decided by the user: answer them in `aq`).** When Claude asks for permission or shows a choice, it becomes a card with the tool, its input and the choices, answered with keys. This uses the `PermissionRequest` hook in the same way as the step hook: `aq hook` waits for the answer over the socket and returns allow or deny. If `aq` is not running, the hook exits with no decision and the normal prompt appears in the window. Unlike step mode, if the user does not answer before the timeout, the hook gives no decision, so the normal prompt is still waiting in the window and nothing is approved silently. Menus that are not permission prompts (for example a plan approval) are shown with a key that jumps to the window; the builder should check whether the hook covers them.
19. **Two agents in one checkout.** Cards and modes belong to the checkout (worktree root), not the git repository, so separate worktrees of one repo are separate projects. If two agent panes share one checkout, `aq` shows a warning on their cards that the diff may mix their work, and does not try to split it.

## Cards

Each card has: project, agent window, mode, kind (review, step, shell change, quiz, permission), creation time, and its content. Permission cards go next in the queue, behind the card on screen, since an agent is stopped until they are answered. Cards are kept in memory. Comments being drafted and quiz progress are saved to disk under `~/.local/state/aq/`, so a restart of `aq` loses nothing the user typed.

A review card is created when an agent in review mode goes from working to waiting and its diff is not empty. If the agent sent a new round, the card shows the round number, and comments from earlier rounds are visible but marked as old.

## Keys (from the mockups)

Shared: `?` ask, `tab` next file or place, `j/k` move, `q` back, `T` theme, `M` mode for this project, `s` skip card.

- Review: `v` split/unified, `]c`/`[c` next/previous change, `c` comment on line or range, `S` send all comments, `A` approve (sends a short "approved, carry on" message if the agent is waiting, otherwise just closes the card).
- Step: `y` approve, `n` reject and type why, `e` ask for a different change, `za` fold/unfold, `gd` jump to definition (best effort: search for the name in the project).
- Permission: `y` allow, `n` deny with a reason, `g` go to the window.
- Quiz: `1-4` pick, `i` type an answer, `enter` submit, `m` show code, `c` ask for a fix, `y` keep as is.

## Left to the builder

- Module layout, crate choices within the stack above, async runtime.
- Exact folding rules for file context, as long as the change and its enclosing function are always visible.
- Performance targets, as long as a 5,000-line diff scrolls without visible lag over ssh.
- Test approach. Use a fake agent (a script in a tmux pane plus recorded hook payloads) for end-to-end tests. Never run tests against the user's live `main` session.

## Order of work (suggested)

1. Agent list and queue bar from tmux and `claude agents --json`.
2. Review board with both diff views, comments and send.
3. Themes and syntax highlighting.
4. Step-by-step hook and cards, then shell-change cards. Prove the hook can wait before building the card screen.
5. Permission cards (same hook machinery).
6. Quiz generation, both question types, judging.
7. Side questions.
