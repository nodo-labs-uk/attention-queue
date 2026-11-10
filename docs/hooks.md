# Claude Code hook formats used by `aq hook`

Checked against Claude Code 2.1.319 on 2026-11-07. Facts from the docs link to
the source. Facts from real runs say so and give the timings.

Sources:

- Hooks reference: <https://code.claude.com/docs/en/hooks>
- CLI reference: <https://code.claude.com/docs/en/cli-reference>

## Common input (all events)

Each hook gets one JSON object on stdin. Fields shared by every event
([hooks: common input fields](https://code.claude.com/docs/en/hooks#common-input-fields)):
`session_id`, `prompt_id` (absent before the first prompt), `transcript_path`,
`cwd`, `scratchpad_dir`, `permission_mode`, `effort`, `hook_event_name`, and
`agent_id` / `agent_type` inside subagents.

`aq hook` reads only the fields it needs and ignores the rest. Missing fields
become empty strings.

## PreToolUse (step mode)

Input ([hooks: PreToolUse input](https://code.claude.com/docs/en/hooks#pretooluse-input)):
the common fields plus `tool_name`, `tool_input` and `tool_use_id`. For `Write`,
`tool_input` is `{"file_path": "<absolute path>", "content": "..."}`.

Seen in a real run (`aq hook pre-tool` forwarded this to the socket):

```json
{"session_id":"8c35adc1-...","transcript_path":"/home/saman/.claude/projects/-tmp-aq-spike-deny-4kpu/8c35adc1-....jsonl",
 "cwd":"/tmp/aq-spike-deny.4kpu","tool_name":"Write","tool_use_id":"toolu_01DU...",
 "tool_input":{"content":"hi","file_path":"/tmp/aq-spike-deny.4kpu/x.txt"}}
```

Output ([hooks: PreToolUse decision control](https://code.claude.com/docs/en/hooks#pretooluse-decision-control)):

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"..."}}
```

### Option menus (AskUserQuestion)

The same hook, with `AskUserQuestion` in its matcher, catches Claude's
option menu before it shows. aq holds the call, shows the questions on a
card, and answers by handing the input back with an `answers` object added
([hooks: tools that require user interaction](https://code.claude.com/docs/en/hooks)):

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"questions":[...],"answers":{"<question text>":"<option label>"}}}}
```

Several picked labels are joined with ", ". An answer in your own words is
sent as the answer string; the docs do not say how the menu's own "Other"
is encoded, so that case rests on Claude reading the string as given. With
no answer (aq closed, `x` on the card, or the deadline) the hook prints
nothing and the menu shows in the window as usual.

- `permissionDecision` is `allow`, `deny`, `ask` or `defer`. `aq` uses only
  `allow` and `deny`.
- For `deny`, Claude reads `permissionDecisionReason`. For `allow`, the reason
  goes only to the debug log.
- Exit 0 with no stdout means "no decision": the call goes through the normal
  permission flow
  ([hooks: how a hook resolves](https://code.claude.com/docs/en/hooks#how-a-hook-resolves)).
  Silence never approves anything.

Real run: `aq hook pre-tool --deadline-secs 2` against a socket that never
answers. Claude did not write the file. It saw the reason as
`PreToolUse:Write hook error: Waiting for human review in aq. Try this edit again shortly.`
The call showed up in the result's `permission_denials`.

## PostToolUse (shell-change cards)

Input ([hooks: PostToolUse input](https://code.claude.com/docs/en/hooks#posttooluse-input)):
the common fields plus `tool_name`, `tool_input`, `tool_response`,
`tool_use_id` and an optional `duration_ms`. The tool has already run, so
`aq hook post-tool` prints nothing.

## PermissionRequest (permission cards)

Input ([hooks: PermissionRequest input](https://code.claude.com/docs/en/hooks#permissionrequest-input)):
like PreToolUse but with no `tool_use_id`. It may also carry an optional
`permission_suggestions` array, and an `mcp_server` object for MCP tools.

```json
{"session_id":"abc123","transcript_path":"...","cwd":"/Users/...","permission_mode":"default",
 "hook_event_name":"PermissionRequest","tool_name":"Bash",
 "tool_input":{"command":"rm -rf node_modules","description":"Remove node_modules directory"},
 "permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"rm -rf node_modules"}],"behavior":"allow","destination":"localSettings"}]}
```

Output ([hooks: PermissionRequest decision control](https://code.claude.com/docs/en/hooks#permissionrequest-decision-control)):

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"..."}}}
```

- `behavior` is `allow` or `deny`. An `allow` does not override a matching
  deny rule.
- `message` (deny only) tells Claude why. `interrupt: true` (deny only) stops
  Claude. `updatedInput` and `updatedPermissions` are for allow only. `aq`
  does not use them yet.
- With no output, the normal prompt stays.

`aq hook permission` reads `$TMUX_PANE` and sends it as `pane`.

## Timeout

- `timeout` is in seconds. The default is 600 for `command` hooks on most
  events ([hooks: common fields](https://code.claude.com/docs/en/hooks#common-fields)).
- The docs give **no general maximum**. The only caps they state are for
  `SessionEnd` (a budget of at most 60 s) and a 10 s kill for PostToolUse
  after Ctrl+Enter.
- `timeout: 3600` is accepted. See the real run below.
- **A PreToolUse hook that times out does not block the call.** Claude Code
  cancels it, throws away its output, and the call goes through the normal
  permission flow
  ([hooks: timeouts](https://code.claude.com/docs/en/hooks#timeouts)). So the
  hook must answer before `timeout`, and a refusal has to come from the hook
  itself.
- `"onFailure": "block"` (v2.1.295 and later) makes a failed or timed-out hook
  block the call, or deny it on PermissionRequest
  ([hooks: block the action when a hook fails](https://code.claude.com/docs/en/hooks#block-the-action-when-a-hook-fails)).
  `aq` does not use it. A hook that cannot start (for example, `aq` is not
  installed) would then block every edit, and that breaks the rule that
  Claude works as normal without `aq`.

`aq` is configured with `timeout: 3600` and `aq hook ... --deadline-secs 3570`
(the default), so the hook answers 30 s before Claude would cancel it.

## Proof that a PreToolUse hook can wait

Real run 1, a 300 s wait. In a scratch git repo, project-local
`.claude/settings.local.json` registered this on `Write`, with `timeout: 600`:
a script that runs `sleep 300` and then prints the allow JSON. Command:
`claude -p "write hi to x.txt" --permission-mode acceptEdits`, run with
`ANTHROPIC_API_KEY`, `TMUX` and `TMUX_PANE` unset.

| Event | Time (UTC) | Seconds from start |
|---|---|---|
| `claude` started | 09:55:07 | 0 |
| hook started | 09:55:10 | 3 |
| hook ended (allow) | 10:00:10 | 303 |
| `x.txt` appeared | 10:00:10.34 | 303 |
| `claude` exited 0 | 10:00:13 | 306 |

The edit landed only after the hook answered, 300 s later. Nothing killed the
hook early.

Real run 2, `timeout: 3600` outlasts the 600 s default. Same setup, but the
hook sleeps 700 s and is registered with `timeout: 3600`.

| Event | Time (UTC) | Seconds from start |
|---|---|---|
| `claude` started | 09:56:11 | 0 |
| hook started | 09:56:15 | 4 |
| hook ended (allow) | 10:07:55 | 704 |
| `x.txt` appeared | 10:07:55.34 | 704 |
| `claude` exited 0 | 10:07:58 | 707 |

The hook ran 700 s, well past the 600 s default, and was not cancelled. So
Claude Code accepts and honours `timeout: 3600`.

## `claude -p --output-format json --json-schema` (for Task 11)

`--json-schema` asks for validated JSON output that matches the schema, in
print mode only
([CLI reference: flags](https://code.claude.com/docs/en/cli-reference#cli-flags)).

Real run:
`claude -p --output-format json --json-schema '{"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"]}' "What is 2+3? ..."`.
The result object had both:

- `"structured_output": {"answer": 5}`, the parsed object. **Use this.**
- `"result": "{\"answer\":5}"`, the same data as a JSON string.

Other fields seen: `type: "result"`, `subtype: "success"`, `is_error: false`,
`session_id`, `total_cost_usd`, `usage`, `num_turns`.

### Locked-down child for the quiz (Task 11 fix round)

aq runs `claude -p` with `--tools Read,Grep,Glob --strict-mcp-config
--disable-slash-commands --settings '{"disableAllHooks":true}'` on top of
`--permission-mode plan`. A real run (`env -u ANTHROPIC_API_KEY -u TMUX -u
TMUX_PANE`, `--output-format stream-json --verbose` to see the init message)
showed `tools: [Glob, Grep, Read, StructuredOutput]`, `mcp_servers: []`,
`slash_commands: []`, `skills: []`, and `structured_output` still came back
(`{"answer": 5, "lines": 27}`). `--tools` is not in `claude --help` (2.1.319)
but is honoured.
