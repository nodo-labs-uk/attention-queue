# Attention queue (`aq`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust terminal app that adopts the user's tmux windows running Claude Code and presents everything needing a human decision as one card at a time, in three modes (review board, step by step, quiz), plus permission cards and side questions.

**Architecture:** One binary `aq`. Run bare, it is the TUI and listens on a Unix socket. Run as `aq hook <event>`, it is a Claude Code hook that forwards the event over the socket and waits for the answer. Agents are reached only through an `AgentConn` trait (tmux implementation now, cloud later). Model calls (quiz, judging, side questions) go through `claude -p` behind an `Llm` trait so tests use a fake.

**Tech Stack:** Rust stable (1.95 installed), ratatui + crossterm, syntect, tokio, serde/serde_json, toml, clap, anyhow, insta (snapshot tests), tempfile.

**Spec:** `docs/superpowers/specs/2026-11-06-attention-queue-design.md`

## Global Constraints

- Never kill, restart or rename tmux windows or sessions; never set tmux session options. Only read tmux and paste into one pane.
- If no `aq` is listening, every hook exits 0 with no output (Claude behaves exactly as today).
- Step-mode edits with no answer are refused, never silently allowed. Permission cards with no answer return no decision (the normal prompt stays).
- Never run tests against the live `main` tmux session. Tests use `tmux -L aqtest-<random>`.
- Nothing listens on the network. Socket at `$XDG_RUNTIME_DIR/aq.sock` (fallback `/tmp/aq-$UID/aq.sock`, dir mode 0700).
- Built-in themes: Tokyo Night (default), Catppuccin Mocha, Gruvbox Dark, Rosé Pine Dawn. User themes from `~/.config/aq/themes/*.toml`.
- Config: `~/.config/aq/config.toml`. Saved state (drafts, quiz progress, round snapshots): `~/.local/state/aq/`.
- Default mode for a checkout is review board.
- A 5,000-line diff must scroll without visible lag: render only visible rows; highlight lazily per visible line and cache.
- British English in all UI text. No em dashes in UI text.
- Do not add Claude attribution or Co-Authored-By lines to commits.

## Review Focus

1. **A refused step edit is retried by the agent.** The retry must reuse the existing card, and if the user already approved it while the hook was gone, the retry is allowed at once. (Task 8: `retry_of_approved_edit_is_allowed`.)
2. **An Edit whose `old_string` is not in the file** (file changed under the agent). The card must still show something useful: the raw old/new strings, marked "could not place in file". (Task 8: `edit_not_found_falls_back_to_raw`.)
3. **Binary, deleted, renamed and untracked files, and paths with spaces or non-UTF-8 bytes** in the diff. They must appear in the tree, not crash the parser. (Task 3: `parses_binary_rename_delete_and_spaces`.)
4. **The agent's pane closes while its card is open.** The card stays, says "agent gone", and sending is disabled instead of pasting into whatever pane later reuses that ID. (Task 2: `pane_identity_includes_claude_pid`; Task 6: `send_to_gone_agent_is_refused`.)
5. **`claude -p` fails, times out or returns invalid JSON** during quiz generation or judging. The quiz card shows the error and a retry key, and the review is not lost. (Task 11: `quiz_generation_failure_is_shown`.)

---

### Task 1: Project skeleton, model and config

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/model.rs`, `src/config.rs`, `.gitignore`, `README.md`
- Test: `src/config.rs` (unit tests)

**Interfaces:**
- Produces:
  - `enum Mode { Review, Step, Quiz }` (serde lowercase)
  - `struct Checkout(PathBuf)`: worktree root, canonicalised.
  - `struct AgentId { pane: String, claude_pid: u32 }` (`pane` like `%54`)
  - `enum CardKind { Review(ReviewCard), Step(StepCard), ShellChange(ShellCard), Quiz(QuizCard), Permission(PermissionCard) }` — inner structs defined by later tasks; start with empty placeholders.
  - `struct Card { id: u64, checkout: Checkout, agent: AgentId, created: SystemTime, kind: CardKind }`
  - `struct Config { theme: String, modes: BTreeMap<PathBuf, Mode> }` with `Config::load(path) -> Result<Config>`, `Config::save(&self, path) -> Result<()>`, `Config::mode_for(&self, &Checkout) -> Mode`, `Config::set_mode(&mut self, &Checkout, Mode)`.
  - `fn paths() -> Paths { config_file, themes_dir, state_dir, socket }` honouring `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`.
  - CLI (clap): `aq` runs the TUI; `aq hook pre-tool|post-tool|permission [--deadline-secs N]`; `aq --version`.

- [ ] **Step 1: `cargo init --name aq`, add dependencies listed in Tech Stack, `.gitignore` with `/target` and `.superpowers/`.**
- [ ] **Step 2: Write failing tests** in `config.rs`:
  - `missing_file_gives_defaults`: load from nonexistent path → `theme == "tokyo-night"`, `mode_for(any) == Mode::Review`.
  - `round_trip`: set mode Step for `/tmp/x`, save, load → `mode_for(/tmp/x) == Step`.
  - `paths_follow_xdg`: with `XDG_RUNTIME_DIR=/r`, `paths().socket == /r/aq.sock`.
- [ ] **Step 3: Run `cargo test` → FAIL.**
- [ ] **Step 4: Implement model, config, paths, CLI stubs** (`aq` prints "not yet" and exits 0).
- [ ] **Step 5: `cargo test` → PASS; `cargo clippy -- -D warnings` clean.**
- [ ] **Step 6: Commit** `git add -A && git commit -m "Skeleton: model, config, CLI"`

### Task 2: Agent discovery over tmux

**Files:**
- Create: `src/agents/mod.rs`, `src/agents/tmux.rs`, `src/agents/claude.rs`, `src/agents/procs.rs`, `tests/fixtures/list-panes.txt`, `tests/fixtures/agents.json`
- Test: unit tests in each file

**Interfaces:**
- Consumes: `AgentId`, `Checkout` (Task 1).
- Produces:
  - `struct Agent { id: AgentId, session: String, window_index: u32, window_name: String, cwd: PathBuf, checkout: Option<Checkout>, session_id: String, status: AgentStatus, waiting: bool }`
  - `enum AgentStatus { Busy, Idle, Unknown }` (from `claude agents --json` `status`)
  - `trait AgentConn: Send + Sync { fn list(&self) -> Result<Vec<Agent>>; fn send_text(&self, agent: &AgentId, text: &str) -> Result<()>; fn is_alive(&self, agent: &AgentId) -> bool; }`
  - `struct TmuxConn { socket: Option<String> }` (`Some(name)` adds `-L name`, used by tests).
  - `fn parse_list_panes(s: &str) -> Vec<Pane>`; `fn parse_agents_json(s: &str) -> Result<Vec<ClaudeProc>>`; `fn is_ancestor(ancestor: u32, pid: u32) -> bool` (walks `/proc/<pid>/stat` field 4).
  - `fn checkout_of(dir: &Path) -> Option<Checkout>` (`git -C dir rev-parse --show-toplevel`).

Facts the implementer needs (checked on the VPS):
- Pane line format: `tmux list-panes -a -F '#{session_name}|#{window_index}|#{window_name}|#{pane_id}|#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{@claude_waiting}'`. `@claude_waiting` is `1` or empty.
- `claude agents --json` returns an array of `{pid, cwd, kind, startedAt, sessionId, name, status}`; `status` seen as `busy` or `idle`. Ignore entries whose `kind` is not `interactive`.
- The claude pid is **not** the pane pid's direct child (there is an intermediate process). Join a claude process to the pane whose `pane_pid` is an ancestor of it.
- Two panes can share one checkout (windows 6 and 7 do today).

- [ ] **Step 1: Write failing tests:**
  - `parses_pane_lines` (fixture with 8 lines like the sample above; asserts pane `%54` has cwd `/home/saman/.local/share/chezmoi` and `waiting == false`, `%12` `waiting == true`).
  - `parses_agents_json_skips_non_interactive`.
  - `ancestor_walk_finds_grandparent` (spawn `sh -c 'sleep 5'` child-of-child, check `is_ancestor(our_pid, grandchild)`).
  - `pane_identity_includes_claude_pid`: two `AgentId`s with the same pane and different pid are not equal.
- [ ] **Step 2: `cargo test agents` → FAIL.**
- [ ] **Step 3: Implement.** `TmuxConn::send_text`: `tmux load-buffer -b aq -` (text on stdin), `tmux paste-buffer -p -d -b aq -t <pane>`, then `tmux send-keys -t <pane> Enter`. `is_alive`: pane exists and its claude pid is alive.
- [ ] **Step 4: Integration test** `tests/tmux_conn.rs::send_text_reaches_pane`: start `tmux -L aqtest-<rand> new -d 'cat > $tmpfile'`, send `"hello\nworld"`, assert file contains both lines; kill that test server at the end.
- [ ] **Step 5: `cargo test` → PASS.**
- [ ] **Step 6: Commit** "Agent discovery from tmux and claude agents".

### Task 3: Git diffs and round snapshots

**Files:**
- Create: `src/git/mod.rs`, `src/git/diff.rs`, `src/git/repo.rs`
- Test: unit tests in `diff.rs`; `tests/git_repo.rs`

**Interfaces:**
- Consumes: `Checkout`.
- Produces:
  - `struct FileDiff { old_path: Option<PathBuf>, new_path: Option<PathBuf>, status: FileStatus, binary: bool, hunks: Vec<Hunk> }`; `enum FileStatus { Added, Deleted, Modified, Renamed, Untracked }`
  - `struct Hunk { old_start: u32, old_len: u32, new_start: u32, new_len: u32, lines: Vec<DiffLine> }`; `enum DiffLine { Context{old:u32,new:u32,text:String}, Add{new:u32,text:String}, Del{old:u32,text:String} }`
  - `fn parse_unified(bytes: &[u8]) -> Vec<FileDiff>`: lossy UTF-8 for line text. Run git with `-c core.quotePath=false` so paths with spaces or non-ASCII come through unquoted; still unquote C-style quoted paths if present.
  - `fn default_branch(c: &Checkout) -> Result<String>` (`origin/HEAD` symbolic ref, else `main`, else `master`).
  - `fn base_tree(c: &Checkout) -> Result<String>` (merge-base commit of HEAD and default branch).
  - `fn snapshot_tree(c: &Checkout) -> Result<String>`: copy `.git/index` path into a temp file, set `GIT_INDEX_FILE`, `git add -A`, `git write-tree`. The user's index must not change.
  - `fn diff_trees(c: &Checkout, from: &str, to: &str) -> Result<Vec<FileDiff>>` (`git diff --no-color --no-ext-diff -M from to`).
  - `fn branch_diff(c) = diff_trees(c, base_tree, snapshot_tree)`.
  - `fn file_at(c: &Checkout, tree: &str, path: &Path) -> Result<Option<String>>`.

- [ ] **Step 1: Write failing tests:**
  - `parses_simple_modify` (asserts line numbers on Add/Del/Context).
  - `parses_binary_rename_delete_and_spaces` (fixture diff with `Binary files differ`, `rename from/to`, `deleted file mode`, a path `dir with space/ü.txt`).
  - `snapshot_does_not_touch_user_index` (temp repo: stage file A, leave B unstaged and C untracked; after `snapshot_tree`, `git diff --cached --name-only` is still only A; the snapshot tree contains A, B, C).
  - `round_two_diff_shows_only_new_change` (snapshot, change one more line, `diff_trees(snap1, snap2)` has exactly one Add).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** by shelling out to `git` (no libgit2).
- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** "Git diffs, base and round snapshots".

### Task 4: Themes and syntax highlighting

**Files:**
- Create: `src/theme.rs`, `src/highlight.rs`, `themes/` (four built-in TOML files embedded with `include_str!`)
- Test: unit tests

**Interfaces:**
- Produces:
  - `struct Theme { name, bg, panel, line, fg, dim, acc, hi, add, add_bg, del, del_bg, sel, key, ok: Color, syntax: String }` — field names and Tokyo Night values match the approved mockup (`--bg:#1a1b26` etc. in the brainstorm `modes-v2.html`, copied into `themes/tokyo-night.toml`; Catppuccin, Gruvbox and Rosé Pine Dawn likewise).
  - `fn builtin_themes() -> Vec<Theme>`; `fn load_themes(dir: &Path) -> Vec<Theme>` (built-ins plus user files; a bad user file is skipped with a warning, never fatal); `fn next_theme(current: &str, all: &[Theme]) -> &Theme`.
  - `struct Highlighter`; `fn highlight_line(&mut self, path: &Path, line_no: u32, text: &str) -> Vec<(Style, String)>` with an LRU cache keyed by (path, line_no, text hash). `syntax` maps to a syntect theme name; colours for add/del backgrounds come from `Theme`, foregrounds from syntect.

- [ ] **Step 1: Failing tests:** `four_builtins_named` (names exactly `tokyo-night`, `catppuccin-mocha`, `gruvbox-dark`, `rose-pine-dawn`); `bad_user_theme_is_skipped`; `rust_keyword_gets_colour` (`fn` in a `.rs` file gets a style differing from plain text); `unknown_extension_is_plain`.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Step 4: Run → PASS.**
- [ ] **Step 5: Commit** "Themes and syntax highlighting".

### Task 5: App shell, queue and queue bar

**Files:**
- Create: `src/queue.rs`, `src/app.rs`, `src/ui/mod.rs`, `src/ui/bar.rs`, `src/ui/keys.rs`, `src/ui/empty.rs`
- Modify: `src/main.rs` (run the TUI)
- Test: unit tests in `queue.rs`; `insta` snapshots via ratatui `TestBackend` in `src/ui/`

**Interfaces:**
- Consumes: `Card`, `Config`, `AgentConn`, `Theme`.
- Produces:
  - `struct Queue`: `push(Card)`, `current() -> Option<&Card>`, `current_mut()`, `remove(id)`, `skip()` (current to back), `iter()`, `find_mut(pred)`.
  - Ordering rule (spec call 11): permission cards first (oldest first); then, if any step or shell-change card exists, the oldest such checkout's cards only; then everything else oldest first.
  - `struct App { queue, agents: Vec<Agent>, config, themes, theme_idx, overlay: Option<Overlay>, status_msg }`; `enum Msg { Key(KeyEvent), Tick, Agents(Vec<Agent>), Ipc(IpcEvent), Llm(LlmEvent) }`; `fn update(&mut self, msg: Msg) -> Vec<Effect>`; `enum Effect { Send{agent, text}, Reply{req_id, decision}, Llm(LlmJob), SaveState }`. `update` is pure (no I/O) so it can be unit tested; a runtime loop executes effects.
  - Agent poll every 1 s on a blocking task; key events from crossterm; render at most 30 fps.
  - Review card creation (spec): an agent whose checkout is in Review mode, going from not-waiting to waiting, with a non-empty diff since its last snapshot → push a Review card (built in Task 6). Same for Quiz mode (Task 11).
  - Keys shared: `?` ask, `q` close overlay, `T` next theme (saved), `M` cycle mode for this checkout (saved), `s` skip.
  - Empty state screen: "Nothing needs you." plus one line per agent: window, project, working or waiting.
  - Two agents in one checkout: cards show "shared checkout: diff may mix work from windows X and Y".

- [ ] **Step 1: Failing tests:** `permission_cards_jump_ahead`; `step_checkout_blocks_others` (step cards for A and B plus a review card: order is A-steps, then B-steps, then review); `skip_moves_to_back`; snapshots `bar_shows_counts` and `empty_state_lists_agents`.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Step 4: Run → PASS** (`cargo insta review` to accept first snapshots after checking them by eye).
- [ ] **Step 5: Manual check:** run `aq` in a spare tmux window; it lists the live agents and never changes them. Quit with `Q`.
- [ ] **Step 6: Commit** "App shell, queue, queue bar".

### Task 6: Review board

**Files:**
- Create: `src/review.rs` (card state), `src/ui/review.rs`, `src/ui/diffview.rs`, `src/ui/tree.rs`, `src/ui/input.rs` (single and multi-line text input)
- Test: unit tests in `review.rs`; snapshots in `ui/`

**Interfaces:**
- Consumes: `FileDiff`, `snapshot_tree`, `diff_trees`, `branch_diff`, `Highlighter`, `AgentConn::send_text`.
- Produces:
  - `struct ReviewCard { round: u32, from_tree: String, to_tree: String, files: Vec<FileDiff>, comments: Vec<Comment>, view: DiffView, show_full_branch: bool, cursor: Cursor }`; `struct Comment { path: PathBuf, line: u32, end_line: u32, side: Side, body: String, round: u32 }`; `enum DiffView { Split, Unified }`.
  - `fn compose_message(card: &ReviewCard) -> String`. Exact format:
    ```
    Review comments (round {round}):

    {path}:{line}[-{end_line}]
    > {first changed line quoted, trimmed to 120 chars}
    {body}

    ...one block per comment, ordered by path then line
    ```
  - Approve message, exact: `Approved, carry on.`
  - Keys: `v` split/unified, `j/k` line, `]c`/`[c` next/previous change, `tab`/`shift-tab` file, `c` comment (on visual range if `V` selection active), `x` delete comment under cursor, `S` send all, `A` approve, `R` full branch diff toggle.
  - On send or approve: record `to_tree` as this checkout's last snapshot (saved to state dir), remove the card.
  - Comments from earlier rounds: shown dimmed with "old", not resent.
- [ ] **Step 1: Failing tests:** `compose_orders_by_path_then_line`; `range_comment_formats_as_range`; `send_to_gone_agent_is_refused` (`update` with a dead agent → no `Effect::Send`, status message "agent gone"); `round_two_uses_last_snapshot`; snapshots `split_view` and `unified_view` from a fixture diff (80×30).
- [ ] **Step 2: Run → FAIL. Step 3: Implement.** Split view aligns deletions and additions per hunk, padding the shorter side with blank rows. Render only visible rows.
- [ ] **Step 4: Run → PASS.** Perf check: `bench_render_5000_lines` (ignored test, run with `--ignored`) renders a 5,000-line diff frame in under 5 ms in release.
- [ ] **Step 5: Commit** "Review board with split and unified views".

### Task 7: Hook transport (prove the wait first)

**Files:**
- Create: `src/ipc.rs`, `src/hook.rs`, `docs/hooks.md` (the verified hook formats)
- Test: `tests/hook_ipc.rs`

**Interfaces:**
- Produces:
  - Wire format: one JSON line each way over the Unix socket.
    `enum HookRequest { PreTool{ session_id, transcript_path, cwd, tool_name, tool_use_id, tool_input: Value }, PostTool{ session_id, cwd, tool_name, tool_input: Value }, Permission{ session_id, cwd, tool_name, tool_input: Value, pane: Option<String> } }`
    `enum HookReply { Allow, Deny{ reason: String }, NoDecision }`
  - Hook side: `fn run_hook(event: HookEvent, stdin: impl Read, deadline: Duration) -> Result<Option<String>>` returns the stdout JSON to print, or `None` for "print nothing, exit 0".
    - Socket missing or connect refused → `None`.
    - PreTool: reply `Allow` → allow JSON; `Deny` → deny JSON with reason; `NoDecision` → `None`; no reply by deadline or socket closed mid-wait → deny with reason exactly `Waiting for human review in aq. Try this edit again shortly.`
    - Permission: no reply by deadline or socket closed → `None`.
    - The hook reads `$TMUX_PANE` to fill `pane`.
  - TUI side: `async fn serve(socket: PathBuf, tx: Sender<IpcEvent>)` where `IpcEvent { req_id: u64, req: HookRequest, reply: oneshot::Sender<HookReply> }`. Removes a stale socket file on start only if nothing answers on it; refuses to start if another `aq` is live ("aq is already running").
  - Default deadline: hook command configured with `timeout: 3600`, hook uses `--deadline-secs 3570`.

- [ ] **Step 1: Verify formats (spike, throwaway).** Read the hooks docs for Claude Code 2.1.319 (`claude-code-guide` agent or the docs site) and record in `docs/hooks.md`: PreToolUse input fields; PreToolUse output (expected `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"|"deny","permissionDecisionReason":"..."}}`); PermissionRequest input and output (expected `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"|"deny","message":"..."}}}`); the maximum `timeout`. Then prove a long wait: in a scratch dir with project-local `.claude/settings.local.json`, register a PreToolUse hook on `Write` that `sleep 300`s then allows, run `claude -p "write hi to x.txt" --permission-mode acceptEdits` and confirm the file appears after ~5 min, not before. If the wait fails, stop and report: the design's fallback (PermissionRequest) needs the user.
- [ ] **Step 2: Failing tests:** `no_socket_prints_nothing`; `allow_reply_prints_allow_json`; `deny_reply_carries_reason`; `deadline_denies_pre_tool` (server never replies, deadline 1 s → deny with the exact reason); `deadline_gives_no_decision_for_permission`; `server_dropped_mid_wait_denies_pre_tool`; `second_instance_refuses`.
- [ ] **Step 3: Run → FAIL. Step 4: Implement. Step 5: Run → PASS.**
- [ ] **Step 6: Commit** "Hook transport over Unix socket".

### Task 8: Step-by-step cards

**Files:**
- Create: `src/step.rs`, `src/edit.rs`, `src/transcript.rs`, `src/ui/step.rs`, `src/ui/fold.rs`
- Test: unit tests; `tests/fixtures/transcript.jsonl`

**Interfaces:**
- Consumes: `IpcEvent`, `HookReply`, `Config::mode_for`, `Highlighter`.
- Produces:
  - `fn apply_edit(tool_name: &str, input: &Value, current: Option<&str>) -> EditPreview` where `enum EditPreview { Placed{ before: String, after: String, changed: Vec<Range<u32>> }, Unplaced{ old: String, new: String, why: String } }`. Supports `Edit` (`old_string`, `new_string`, `replace_all`), `MultiEdit` (`edits` array applied in order), `Write` (`content`; before is current or empty), `NotebookEdit` (shown as Unplaced with the cell source).
  - `fn description_before(transcript: &Path, tool_use_id: &str) -> Option<String>`: last assistant text block before the `tool_use` with that id, first 300 chars.
  - `struct StepCard { req_ids: Vec<u64>, key: EditKey, path: PathBuf, preview: EditPreview, description: Option<String>, step_no: u32, decided: Option<HookReply> }`; `EditKey` = hash of (session_id, tool_name, canonical tool_input).
  - Behaviour on PreTool event: if checkout mode is not Step → reply `NoDecision` at once. If a card with the same `EditKey` exists: if `decided` is set, reply with it and remove the card; else attach this request to it (the older request's channel may be dead). Otherwise create a card. A request for a checkout not at the front of the queue is held (no reply) until it reaches the front or the hook's deadline.
  - Folding: always show the enclosing function or block of each changed range (syntect scope or indentation, implementer's choice) plus 3 lines either side; fold the rest as `▸ N lines`. `za` toggles the fold under the cursor; `zR` unfolds all.
  - Keys: `y` allow; `n` deny, input box for the reason (reason sent as `Rejected by reviewer: {text}`); `e` deny with `Please change this edit instead: {text}`; `tab` next changed range; `gd` search the identifier under the cursor in the checkout with `git grep -n -w` and offer results in an overlay; `m` (no description) sends nothing, it shows the raw tool input.
  - Left panel: "this change" (changed ranges) and "earlier" (this session's decided steps with ✓ or ✗).
- [ ] **Step 1: Failing tests:** `edit_places_change`; `replace_all_marks_every_range`; `multiedit_applies_in_order`; `write_new_file_has_empty_before`; `edit_not_found_falls_back_to_raw`; `description_is_text_before_tool_use` (fixture transcript); `retry_of_approved_edit_is_allowed`; `non_step_checkout_gets_no_decision`; snapshot `step_card_folded`.
- [ ] **Step 2: Run → FAIL. Step 3: Implement. Step 4: Run → PASS.**
- [ ] **Step 5: Commit** "Step-by-step cards".

### Task 9: Shell change cards

**Files:**
- Create: `src/shell.rs`, `src/ui/shell.rs`
- Modify: `src/hook.rs` (PostTool for `Bash` replies at once; never waits)

**Interfaces:**
- Consumes: `snapshot_tree`, `diff_trees`, diff view widgets from Task 6.
- Produces:
  - Per step-mode checkout, `aq` keeps `last_tree`. Taken when the checkout enters step mode, and after every allowed step edit is reported done (PostTool for edit tools).
  - On PostTool `Bash` in a step-mode checkout: `now = snapshot_tree`; if `diff_trees(last_tree, now)` is non-empty, push `ShellCard { command: String, files: Vec<FileDiff> }`; set `last_tree = now`. Reply `NoDecision` immediately in all cases.
  - Keys: `y` approve; `u` send `Please undo what this command changed: {command}`; diff view keys from Task 6.
- [ ] **Step 1: Failing tests:** `bash_that_changes_files_makes_card`; `bash_with_no_change_makes_no_card`; `edit_tool_post_updates_last_tree_without_card`.
- [ ] **Step 2: FAIL → Step 3: implement → Step 4: PASS. Step 5: Commit** "Shell change cards".

### Task 10: Permission cards

**Files:**
- Create: `src/permission.rs`, `src/ui/permission.rs`

**Interfaces:**
- Consumes: `IpcEvent` with `HookRequest::Permission`.
- Produces: `PermissionCard { req_id, tool_name, tool_input: Value, summary: String }`. Summary: Bash → the command; edit tools → the path; others → compact JSON, 200 chars. Keys: `y` allow, `n` deny with reason, `g` go to window (`tmux select-window -t <pane>` then `switch-client` if needed; this is the only window-changing action, and only on the user's key). When the agent's pane later shows it moved on (agent busy again, or the hook connection closed), the card is removed.
- [ ] **Step 1: Failing tests:** `permission_card_goes_first`; `closed_hook_removes_card`; `bash_summary_is_command`.
- [ ] **Step 2: FAIL → Step 3: implement → Step 4: PASS. Step 5: Commit** "Permission cards".

### Task 11: Quiz

**Files:**
- Create: `src/llm.rs`, `src/quiz.rs`, `src/ui/quiz.rs`, `prompts/quiz.md`, `prompts/judge.md`, `schemas/quiz.json`, `schemas/judge.json`

**Interfaces:**
- Produces:
  - `trait Llm: Send + Sync { fn run(&self, job: &LlmJob) -> Result<String>; }`; `struct ClaudeCli`: runs `claude -p --output-format json --json-schema <schema> --no-session-persistence --permission-mode plan` in the checkout with the prompt on stdin, timeout 180 s, and returns the `result` field (or `structured_output` if present; check which in the spike in Task 7's doc and note it). `struct FakeLlm(Vec<Result<String>>)` for tests.
  - Quiz schema: `{questions: [{kind: "choice"|"free", question, choices?: [string], answer_index?: int, explanation, must_cover?: [string], code_ref: {path, line}}]}`, 3 to 6 questions.
  - Judge schema: `{verdict: "right"|"partial"|"wrong", gap: string, gap_in: "code"|"understanding"|"none", explanation: string}`.
  - Prompts (in `prompts/`) tell the model: focus on behaviour, failure cases and risky parts; no trivia; mix both kinds; never ask about formatting.
  - `QuizCard { state: QuizState, questions, answers: Vec<Answer>, fixes: Vec<String>, from_tree, to_tree }`; `enum QuizState { Generating, Failed(String), Asking(usize), Done }`.
  - Generation starts in the background when the card is created (agent goes to waiting in a quiz-mode checkout with a non-empty diff since the last snapshot).
  - Choice question: the answer is checked locally against `answer_index`; wrong shows `explanation` and the code at `code_ref`. Free question: judged by a judge job; `gap_in == "code"` offers `c` "send a fix" (adds `Fix: {gap}` to `fixes`) and `y` keep; otherwise just the explanation and continue.
  - On Done: if `fixes` is non-empty send `Quiz follow-up. Please fix:\n- {fix}\n- ...`; else send `Approved, carry on.` Record `to_tree` as last snapshot.
  - Keys: `1-4` choose, `i` type answer (multi-line input, `ctrl-s` or `enter` on empty last line submits), `m` show code, `c` fix, `y` keep, `r` retry generation.
- [ ] **Step 1: Failing tests:** `choice_wrong_shows_explanation`; `free_answer_code_gap_offers_fix`; `understanding_gap_offers_no_fix`; `done_with_fixes_sends_list`; `done_without_fixes_approves`; `quiz_generation_failure_is_shown` (FakeLlm returns invalid JSON → `Failed`, `r` re-queues the job); snapshot `quiz_choice` and `quiz_free`.
- [ ] **Step 2: FAIL → Step 3: implement → Step 4: PASS.**
- [ ] **Step 5: Manual check:** one real `ClaudeCli` quiz on a small diff in a scratch repo; questions are about behaviour.
- [ ] **Step 6: Commit** "Quiz mode".

### Task 12: Side questions

**Files:**
- Create: `src/ask.rs`, `src/ui/ask.rs`

**Interfaces:**
- Consumes: `Llm`, the current card.
- Produces: `?` opens an input overlay. On submit, an `LlmJob::Ask { session_id, checkout, context: String, question }` runs `claude -p --resume <session_id> --fork-session --output-format json` (no schema) with a prompt that includes the card's context (the diff, the step preview, or the quiz question). If that exits non-zero, fall back to plain `claude -p` in the checkout with the same prompt. The answer shows in a scrollable overlay over the card; `q` closes it and the card's cursor is unchanged. Several questions can be asked in one overlay; each forks again from the live session (answers do not chain).
- [ ] **Step 1: Failing tests:** `ask_preserves_card_cursor`; `fork_failure_falls_back`; `context_for_step_card_contains_after_text`.
- [ ] **Step 2: FAIL → Step 3: implement → Step 4: PASS. Step 5: Commit** "Side questions".

### Task 13: Saved state, end-to-end test, install

**Files:**
- Create: `src/persist.rs`, `tests/e2e.rs`, `tests/fake-agent.sh`, `contrib/settings-hooks.json`
- Modify: `README.md`; `/home/saman/.local/share/chezmoi/dot_claude/settings.json.tmpl` (add hooks; not committed unless the user asks)

**Interfaces:**
- Produces:
  - `persist.rs`: saves draft comments, quiz progress and per-checkout last snapshots to `~/.local/state/aq/state.json` on each `Effect::SaveState` (debounced 1 s, written atomically via temp file + rename); loaded on start and matched back to cards by checkout and tree hashes.
  - `contrib/settings-hooks.json`: PreToolUse matcher `Edit|Write|MultiEdit|NotebookEdit` → `aq hook pre-tool --deadline-secs 3570`, `timeout: 3600`; PostToolUse matcher `Bash|Edit|Write|MultiEdit|NotebookEdit` → `aq hook post-tool`, `timeout: 30`; PermissionRequest → `aq hook permission --deadline-secs 3570`, `timeout: 3600`. Use the exact formats recorded in `docs/hooks.md`.
  - `tests/e2e.rs`: private tmux server `-L aqtest-<rand>`, a window running `fake-agent.sh` (which echoes pasted input to a file and lets the test trigger fake hook calls by running `aq hook` with recorded payloads), `aq` in another window with `AQ_SOCKET` and `XDG_*` pointed at a temp dir. Drive keys with `send-keys`; read screens with `capture-pane -p`.
- [ ] **Step 1: Failing tests:** `draft_comment_survives_restart`; `e2e_review_comment_reaches_agent`; `e2e_step_reject_reason_reaches_hook`; `e2e_no_aq_hook_is_silent`.
- [ ] **Step 2: FAIL → Step 3: implement → Step 4: PASS** (`cargo test --all`; `cargo clippy --all-targets -- -D warnings`).
- [ ] **Step 5: Install:** `cargo install --path .`; add the hooks to the chezmoi settings template next to the existing ones; `chezmoi diff` shows only the added entries; `chezmoi apply ~/.claude/settings.json`.
- [ ] **Step 6: Live smoke test** with the user's agreement: open `aq` in a new window of `main`, set one throwaway checkout to step mode, run a small task, confirm a card appears and approve works.
- [ ] **Step 7: Commit** "Saved state, end-to-end tests, hook install" (repo only; chezmoi changes left uncommitted for the user).
