# Install

## Let Claude do it

Paste this into Claude Code:

```text
Set up aq (https://github.com/nodo-labs-uk/attention-queue) for me.

1. Install it: download aq-x86_64-linux.tar.gz from the latest release
   (https://github.com/nodo-labs-uk/attention-queue/releases/latest), or
   clone the repo and run `cargo install --path .`.
   Make sure `aq` is on my PATH and `aq --help` runs.
2. Add the three hooks from contrib/settings-hooks.json to
   ~/.claude/settings.json. Back the file up first. Merge them into the
   existing PreToolUse, PostToolUse and PermissionRequest arrays, keeping
   every hook already there, and skip any that are already present. Show me
   the diff before saving.
3. Check that tmux is installed, and tell me which running Claude sessions
   need a restart to pick up the hooks.

Don't change anything else.
```

## By hand

1. Get the binary. Either take it from the [latest release](https://github.com/nodo-labs-uk/attention-queue/releases/latest):

   ```sh
   curl -fsSL https://github.com/nodo-labs-uk/attention-queue/releases/latest/download/aq-x86_64-linux.tar.gz | tar xz
   install aq-x86_64-linux/aq ~/.local/bin/
   ```

   or build it from source:

   ```sh
   git clone https://github.com/nodo-labs-uk/attention-queue
   cd attention-queue
   cargo install --path .
   ```

2. Add the hooks to `~/.claude/settings.json`. The three hooks are in [`contrib/settings-hooks.json`](https://github.com/nodo-labs-uk/attention-queue/blob/main/contrib/settings-hooks.json), and [Hooks](hooks.md) explains each one.

   > **Warning:** merge them into your existing `PreToolUse`, `PostToolUse` and `PermissionRequest` arrays. If you replace the arrays, your other hooks stop running.

3. Restart any Claude sessions that were running before you added the hooks. A session reads its hooks when it starts.

4. Run `aq` in a tmux window of its own.

## Check it works

- `aq --help` prints the commands.
- In aq, the sessions view shows a tab for each Claude session in tmux.
- Switch a project to step mode (`M` twice) and ask its session to edit a file. A step card shows the edit before it is applied.

## Update

Download the new release, or run `git pull` and `cargo install --path .` again. Then quit aq with `Q` and start it again. Sessions keep running while aq restarts; a hook that fires while aq is down gets no answer, so Claude carries on as normal.
