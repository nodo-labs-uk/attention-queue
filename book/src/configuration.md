# Files and configuration

You rarely need to edit these by hand: aq writes them as you use it.

## Config: `~/.config/aq/config.toml`

| Setting | What it holds | Changed with |
|---|---|---|
| `theme` | the theme's name | `t` |
| `modes` | each project's mode | `M` |
| `hide_files` | whether review cards hide the file list | `f` |
| `show_keys` | whether the keys row shows | `F3` |
| `read_wpm` | the fast reader's speed | `↑` `↓` in the reader |
| `tutorial_done` | whether the tour has been finished or skipped | the tour |
| `workspaces` | saved workspaces: a root folder and its repos | `O` |

`XDG_CONFIG_HOME` moves the folder.

## Themes

aq has five themes built in: tokyo-night, catppuccin-mocha, gruvbox-dark, rose-pine-dawn and ghostty. `t` moves to the next one.

To add your own, put a `.toml` file in `~/.config/aq/themes/`. A theme with a built-in's name replaces it. Every colour is required:

```toml
name = "mine"
bg = "#1a1b26"      # background
panel = "#16161e"   # boxes and panels
line = "#232433"    # the cursor line
fg = "#c0caf5"      # text
dim = "#565f89"     # quiet text
acc = "#7aa2f7"     # accents and headings
hi = "#e0af68"      # highlights
add = "#9ece6a"     # added lines
add_bg = "#20303b"
del = "#f7768e"     # deleted lines
del_bg = "#37222c"
sel = "#283457"     # selection
key = "#7dcfff"     # key names in hints
ok = "#9ece6a"      # done and allowed
```

An optional `syntax` names one of the code highlighting themes bundled with aq; without it, aq builds one from the theme's own colours. A file with an error is skipped, with a warning.

## State: `~/.local/state/aq/state.json`

Review rounds, unsent comments and quiz progress, so a restart loses nothing. `XDG_STATE_HOME` moves the folder.

## Socket

The hooks reach aq over a Unix socket: `$XDG_RUNTIME_DIR/aq.sock`, or `/tmp/aq-$UID/aq.sock`. `AQ_SOCKET` overrides it. The socket's folder must be private to you; aq refuses to start if other users can open it.
