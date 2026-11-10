#!/usr/bin/env python3
"""Records the README's demos as animated SVGs, with nothing but the
standard library, tmux and a debug build of aq.

Each scene runs aq in a private tmux server next to fake agents
(`fake-agent`) working in throwaway git repos, with `fake-claude` standing in
for `claude -p`. Keys go in with `send-keys`; the aq pane is read with
`capture-pane -e` every 0.1 s, and the frames become one SVG whose CSS
animation steps through them. Nothing touches your own tmux server, config
or Claude login.

usage: demo/record.py [scene ...]     (default: every scene)
"""

import html
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
OUT = ROOT / "docs" / "media"
AQ = ROOT / "target" / "debug" / "aq"
COLS, ROWS = 120, 30
SESSION_ID = "6f1d2c4e-8a3b-4c5d-9e7f-0a1b2c3d4e5f"

# The ghostty theme's colours, for text drawn in the terminal's defaults.
FG, BG = "#ffffff", "#282c34"

LIMITER_OLD = '''"""Rate limiting for the public API."""

import time


class Limiter:
    def __init__(self, per_minute):
        self.per_minute = per_minute
        self.calls = []

    def allow(self, key):
        now = time.time()
        self.calls = [t for t in self.calls if now - t < 60]
        if len(self.calls) >= self.per_minute:
            return False
        self.calls.append(now)
        return True
'''

LIMITER_NEW = '''"""Rate limiting for the public API."""

import threading
import time


class Limiter:
    """A token bucket per key: bursts up to `burst`, then `per_minute`."""

    def __init__(self, per_minute, burst=10):
        self.rate = per_minute / 60
        self.burst = burst
        self.buckets = {}
        self.lock = threading.Lock()

    def allow(self, key):
        now = time.monotonic()
        with self.lock:
            tokens, last = self.buckets.get(key, (self.burst, now))
            tokens = min(self.burst, tokens + (now - last) * self.rate)
            if tokens < 1:
                self.buckets[key] = (tokens, now)
                return False
            self.buckets[key] = (tokens - 1, now)
            return True
'''

TEST_OLD = '''from limiter import Limiter


def test_blocks_after_limit():
    lim = Limiter(per_minute=2)
    assert lim.allow("a") and lim.allow("a")
    assert not lim.allow("a")
'''

TEST_NEW = '''from limiter import Limiter


def test_burst_then_blocks():
    lim = Limiter(per_minute=60, burst=2)
    assert lim.allow("a") and lim.allow("a")
    assert not lim.allow("a")


def test_keys_have_their_own_bucket():
    lim = Limiter(per_minute=60, burst=1)
    assert lim.allow("a") and lim.allow("b")
'''

API_TALK = """\033[1m❯ the API's rate limit is one shared window; make it a token bucket per key\033[0m

● I'll replace the sliding window in limiter.py with a token bucket per
  key, keeping the per_minute argument, and update the tests.

● Update(limiter.py)
  ⎿  Updated limiter.py with 14 additions and 9 removals

● Bash(python -m pytest -q)
  ⎿  2 passed in 0.03s

● Done: each key now gets its own bucket of 10 that refills at per_minute.
"""

WEB_TALK = """\033[1m❯ the signup form loses its state on a validation error\033[0m

● Reading src/signup/Form.tsx…

● Update(src/signup/Form.tsx)
  ⎿  Updated src/signup/Form.tsx with 6 additions and 2 removals
"""

DOCS_TALK = """\033[1m❯ add a page on the new export format\033[0m

● Added docs/export.md and linked it from the index. The examples use
  the sample file in fixtures/, and the page builds without warnings.
"""


def run(*args, cwd=None, env=None, check=True, inp=None):
    r = subprocess.run(args, cwd=cwd, env=env, input=inp, capture_output=True, text=True)
    if check and r.returncode != 0:
        sys.exit(f"{args}: {r.stderr}")
    return r.stdout


def git(repo, *args):
    run("git", "-C", str(repo), "-c", "user.name=demo", "-c", "user.email=demo@example.com", *args)


# Every World made, so a failed scene still gets its tmux server shut down.
WORLDS = []


class World:
    """A private tmux server with fake agents and aq, removed on close."""

    def __init__(self, agents, modes=None, client=False, tour=False):
        WORLDS.append(self)
        self.client = None
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name).resolve()
        os.chmod(base, 0o700)
        self.base = base
        self.socket = f"aqdemo-{os.getpid()}"
        self.aq_socket = base / "run" / "aq.sock"
        (base / "run").mkdir(mode=0o700)
        (base / "config" / "aq").mkdir(parents=True)
        bin_dir = base / "bin"
        bin_dir.mkdir()
        (bin_dir / "claude").symlink_to(HERE / "fake-claude")
        conf = base / "tmux.conf"
        conf.write_text("set -g status off\nset -g escape-time 10\n"
                        "set -g default-terminal tmux-256color\n")
        listing = []
        self.panes = {}
        for i, a in enumerate(agents):
            repo = base / a["name"]
            repo.mkdir()
            git(repo, "init", "-q", "-b", "main")
            for path, text in a.get("before", {}).items():
                (repo / path).parent.mkdir(parents=True, exist_ok=True)
                (repo / path).write_text(text)
            git(repo, "add", "-A")
            git(repo, "commit", "-qm", "start", "--allow-empty")
            for path, text in a.get("after", {}).items():
                (repo / path).write_text(text)
            talk = base / f"{a['name']}.talk"
            talk.write_text(a["talk"])
            replies = base / f"{a['name']}.replies"
            replies.write_text("\n".join(a.get("replies", [])) + "\n")
            cmd = f"{HERE / 'fake-agent'} {talk} {replies}"
            if i == 0:
                self.tmux("-f", str(conf), "new-session", "-d", "-s", "demo", "-n", a["name"],
                          "-x", str(COLS), "-y", str(ROWS), "-c", str(repo), cmd)
            else:
                self.tmux("new-window", "-d", "-t", "demo", "-n", a["name"], "-c", str(repo), cmd)
            pane, pid = self.tmux("display-message", "-p", "-t", f"demo:{a['name']}",
                                  "#{pane_id} #{pane_pid}").split()
            self.panes[a["name"]] = pane
            if a.get("waiting"):
                self.wait(a["name"], True)
            listing.append({"pid": int(pid), "cwd": str(repo), "kind": "interactive",
                            "sessionId": SESSION_ID if i == 0 else f"{SESSION_ID[:-1]}{i}",
                            "name": a["name"], "status": "idle"})
            if mode := (modes or {}).get(a["name"]):
                modes_toml = f'{json.dumps(str(repo))} = "{mode}"\n'
                with open(base / "config" / "aq" / "config.toml", "a") as f:
                    f.write(modes_toml)
        cfg = base / "config" / "aq" / "config.toml"
        body = cfg.read_text() if cfg.exists() else ""
        tour = "false" if tour else "true"
        cfg.write_text(f'theme = "ghostty"\ntutorial_done = {tour}\n\n[modes]\n' + body)
        (base / "agents.json").write_text(json.dumps(listing))
        self.repos = {a["name"]: base / a["name"] for a in agents}
        env = {
            "AQ_SOCKET": self.aq_socket,
            "AQ_AGENTS_JSON": base / "agents.json",
            "XDG_CONFIG_HOME": base / "config",
            "XDG_STATE_HOME": base / "state",
            "XDG_RUNTIME_DIR": base / "run",
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            # Transcripts and the `!` shell's startup files come from here.
            "HOME": base,
        }
        # tmux runs the command through your shell, whose startup files may
        # put the real `claude` back in front; `env` sets PATH after them.
        args = ["new-window", "-d", "-t", "demo", "-n", "aq", "env"]
        args += [f"{k}={v}" for k, v in env.items()]
        self.tmux(*args, str(AQ))
        self.aq = self.tmux("display-message", "-p", "-t", "demo:aq", "#{pane_id}").strip()
        self.wait_for("aq's socket", lambda: self.aq_socket.exists())
        # A popup needs a client to show on: one attached through a pty.
        if client:
            env = {k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_PANE")}
            self.client = subprocess.Popen(
                ["script", "-qfec", f"stty cols {COLS} rows {ROWS}; "
                 f"exec tmux -L {self.socket} attach -t demo:aq", "/dev/null"],
                stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                env=env)
            self.wait_for("a client", lambda: self.tmux("list-clients").strip())
        # A pane laid over aq's frames where tmux draws a popup: (pane, title).
        self.overlay = None

    def tmux(self, *args):
        env = {k: v for k, v in os.environ.items() if k not in ("TMUX", "TMUX_PANE")}
        return run("tmux", "-L", self.socket, *args, env=env)

    def screen(self):
        return self.tmux("capture-pane", "-p", "-e", "-t", self.aq)

    def wait_for(self, what, test, timeout=20):
        end = time.time() + timeout
        while not test():
            if time.time() > end:
                raise TimeoutError(f"waiting for {what}:\n"
                                   f"{self.tmux('capture-pane', '-p', '-t', self.aq)}")
            time.sleep(0.05)

    def see(self, text, timeout=20):
        self.wait_for(repr(text), lambda: text in self.tmux("capture-pane", "-p", "-t", self.aq),
                      timeout)

    def wait(self, agent, waiting):
        """Marks the agent waiting at its prompt, as Claude Code's hook does."""
        pane = self.panes[agent]
        if waiting:
            self.tmux("set-option", "-p", "-t", pane, "@claude_waiting", "1")
        else:
            self.tmux("set-option", "-p", "-u", "-t", pane, "@claude_waiting")

    def keys(self, *keys, gap=0.6):
        for k in keys:
            self.tmux("send-keys", "-t", self.aq, k)
            time.sleep(gap)

    def type(self, text, delay=0.035):
        for c in text:
            self.tmux("send-keys", "-t", self.aq, "-l", c)
            time.sleep(delay)

    def hook(self, event, payload):
        """`aq hook <event>` as Claude Code would run it; join for its reply."""
        env = dict(os.environ, AQ_SOCKET=str(self.aq_socket))
        env.pop("TMUX_PANE", None)
        p = subprocess.Popen([str(AQ), "hook", event, "--deadline-secs", "60"], env=env,
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        p.stdin.write(json.dumps(payload))
        p.stdin.close()
        return p

    def close(self):
        if self in WORLDS:
            WORLDS.remove(self)
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)
        if self.client:
            self.client.kill()
            self.client.wait()
        self.tmp.cleanup()


class Recorder:
    def __init__(self, world):
        self.world, self.frames, self.stop = world, [], threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def loop(self):
        start = time.time()
        while not self.stop.is_set():
            s = self.world.screen()
            if self.world.overlay:
                pane, title = self.world.overlay
                popup = self.world.tmux("capture-pane", "-p", "-e", "-t", pane)
                s = composite(s, popup, title)
            if not self.frames or self.frames[-1][1] != s:
                self.frames.append((time.time() - start, s))
            time.sleep(0.1)
        self.end = time.time() - start

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join()


# ---- turning frames into an SVG ------------------------------------------

PALETTE = ["#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
           "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff"]


def colour_256(n):
    if n < 16:
        return PALETTE[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[n // 6 % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def sgr(params, st):
    ps = [int(p) if p else 0 for p in params.split(";")] if params else [0]
    i = 0
    while i < len(ps):
        p = ps[i]
        if p == 0:
            st.update(fg=None, bg=None, bold=False, dim=False, italic=False, under=False, rev=False)
        elif p == 1:
            st["bold"] = True
        elif p == 2:
            st["dim"] = True
        elif p == 3:
            st["italic"] = True
        elif p == 4:
            st["under"] = True
        elif p == 7:
            st["rev"] = True
        elif p == 22:
            st["bold"] = st["dim"] = False
        elif p == 23:
            st["italic"] = False
        elif p == 24:
            st["under"] = False
        elif p == 27:
            st["rev"] = False
        elif p in (38, 48):
            key = "fg" if p == 38 else "bg"
            if i + 1 < len(ps) and ps[i + 1] == 2:
                st[key] = "#%02x%02x%02x" % tuple(ps[i + 2:i + 5])
                i += 4
            elif i + 1 < len(ps) and ps[i + 1] == 5:
                st[key] = colour_256(ps[i + 2])
                i += 2
        elif p == 39:
            st["fg"] = None
        elif p == 49:
            st["bg"] = None
        elif 30 <= p <= 37:
            st["fg"] = PALETTE[p - 30]
        elif 90 <= p <= 97:
            st["fg"] = PALETTE[p - 82]
        elif 40 <= p <= 47:
            st["bg"] = PALETTE[p - 40]
        elif 100 <= p <= 107:
            st["bg"] = PALETTE[p - 92]
        i += 1


def cells(line):
    """(column, char, width, style) for each printed character."""
    st = dict(fg=None, bg=None, bold=False, dim=False, italic=False, under=False, rev=False)
    out, col, i = [], 0, 0
    while i < len(line):
        c = line[i]
        if c == "\x1b" and i + 1 < len(line) and line[i + 1] == "[":
            j = i + 2
            while j < len(line) and not line[j].isalpha():
                j += 1
            if j < len(line) and line[j] == "m":
                sgr(line[i + 2:j], st)
            i = j + 1
            continue
        if unicodedata.combining(c):
            i += 1
            continue
        w = 2 if unicodedata.east_asian_width(c) in "WF" else 1
        fg, bg = st["fg"] or FG, st["bg"] or BG
        if st["rev"]:
            fg, bg = bg, fg
        out.append((col, c, w, (fg, bg, st["bold"], st["dim"], st["italic"], st["under"])))
        col += w
        i += 1
    return out


FS, CW, LH, PAD = 14, 8.43, 18, 12


def row_svg(line):
    parts = []
    cs = cells(line)
    # Backgrounds, merged into runs.
    k = 0
    while k < len(cs):
        col, _, w, style = cs[k]
        bg, end = style[1], col + w
        j = k + 1
        while j < len(cs) and cs[j][3][1] == bg:
            end = cs[j][0] + cs[j][2]
            j += 1
        if bg != BG:
            parts.append(f'<rect x="{col * CW:.1f}" width="{(end - col) * CW:.1f}" '
                         f'height="{LH}" fill="{bg}"/>')
        k = j
    # Text, in runs of one style; each run pinned to its columns.
    k = 0
    while k < len(cs):
        col, _, _, style = cs[k]
        j, text, end = k, "", col
        while j < len(cs) and cs[j][3][0] == style[0] and cs[j][3][2:] == style[2:]:
            text += cs[j][1]
            end = cs[j][0] + cs[j][2]
            j += 1
        k = j
        if not text.strip():
            continue
        lead = len(text) - len(text.lstrip(" "))
        text = text.strip(" ")
        x = (col + lead) * CW
        fg, _, bold, dim, italic, under = style
        attrs = f'x="{x:.1f}" y="{FS}" fill="{fg}"'
        if bold:
            attrs += ' font-weight="bold"'
        if dim:
            attrs += ' fill-opacity="0.6"'
        if italic:
            attrs += ' font-style="italic"'
        if under:
            attrs += ' text-decoration="underline"'
        width = sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in text)
        if width > 1:
            attrs += f' textLength="{width * CW:.1f}" lengthAdjust="spacingAndGlyphs"'
        parts.append(f"<text {attrs}>{html.escape(text, quote=False)}</text>")
    return "".join(parts)


def to_svg(frames, total, hold=4.0, max_idle=2.5):
    # Long waits are cut short; the last frame stays up a while before looping.
    times, t, prev = [], 0.0, 0.0
    for at, _ in frames:
        t += min(at - prev, max_idle)
        prev = at
        times.append(t)
    duration = times[-1] + hold
    w, h = COLS * CW + 2 * PAD, ROWS * LH + 2 * PAD
    rows, defs, uses = {}, [], []
    for i, (_, screen) in enumerate(frames):
        lines = screen.split("\n")[:ROWS]
        frame = []
        for r, line in enumerate(lines):
            if not line.strip("\x1b[0m "):
                continue
            if line not in rows:
                rows[line] = f"r{len(rows)}"
                defs.append(f'<g id="{rows[line]}">{row_svg(line)}</g>')
            frame.append(f'<use href="#{rows[line]}" y="{r * LH}"/>')
        uses.append(f'<g transform="translate(0 {i * h})">{"".join(frame)}</g>')
    keys = []
    for i, t in enumerate(times):
        keys.append(f"{t / duration * 100:.3f}%{{transform:translateY({-i * h}px)}}")
    keys.append(f"100%{{transform:translateY({-(len(frames) - 1) * h}px)}}")
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" viewBox="0 0 {w:.0f} {h:.0f}">
<style>
text{{font-family:"JetBrains Mono","DejaVu Sans Mono",Menlo,Consolas,monospace;font-size:{FS}px;white-space:pre}}
.film{{animation:play {duration:.2f}s step-end infinite}}
@keyframes play{{{"".join(keys)}}}
</style>
<defs>{"".join(defs)}</defs>
<rect width="100%" height="100%" rx="8" fill="{BG}"/>
<svg x="{PAD}" y="{PAD}" width="{COLS * CW:.0f}" height="{ROWS * LH}">
<g class="film">{"".join(uses)}</g>
</svg>
</svg>
'''


# ---- a popup laid over a frame --------------------------------------------

# tmux's popup at -w 90% -h 85%, centred, as aq opens it.
POP_W, POP_H = COLS * 90 // 100, ROWS * 85 // 100
POP_X, POP_Y = (COLS - POP_W) // 2, (ROWS - POP_H) // 2


def grid(line):
    """A row as COLS cells of (char, style); the second half of a wide
    character is None."""
    row = [(" ", (FG, BG, False, False, False, False))] * COLS
    for col, c, w, style in cells(line):
        if col < COLS:
            row[col] = (c, style)
            if w == 2 and col + 1 < COLS:
                row[col + 1] = None
    return row


def put(row, col, cell):
    if row[col] is None and col > 0:
        row[col - 1] = (" ", row[col - 1][1] if row[col - 1] else cell[1])
    if row[col] is not None and row[col][0] and col + 1 < COLS and row[col + 1] is None:
        row[col + 1] = (" ", row[col][1])
    row[col] = cell


def sgr_of(style):
    fg, bg, bold, dim, italic, under = style
    rgb = lambda h: ";".join(str(int(h[i:i + 2], 16)) for i in (1, 3, 5))
    codes = ["0", f"38;2;{rgb(fg)}", f"48;2;{rgb(bg)}"]
    codes += [c for c, on in (("1", bold), ("2", dim), ("3", italic), ("4", under)) if on]
    return f"\x1b[{';'.join(codes)}m"


def serialise(row):
    out, cur = "", None
    for cell in row:
        if cell is None:
            continue
        if cell[1] != cur:
            out += sgr_of(cell[1])
            cur = cell[1]
        out += cell[0]
    return out + "\x1b[0m"


def composite(screen, popup, title):
    """`popup`'s screen in a bordered box over `screen`, as tmux shows it."""
    rows = [grid(l) for l in (screen.split("\n") + [""] * ROWS)[:ROWS]]
    inner = (popup.split("\n") + [""] * POP_H)[:POP_H - 2]
    border = (FG, BG, False, False, False, False)
    top = "┌" + title[:POP_W - 2].ljust(POP_W - 2, "─") + "┐"
    for y in range(POP_H):
        row = rows[POP_Y + y]
        if y in (0, POP_H - 1):
            text = top if y == 0 else "└" + "─" * (POP_W - 2) + "┘"
            for i, c in enumerate(text):
                put(row, POP_X + i, (c, border))
            continue
        put(row, POP_X, ("│", border))
        put(row, POP_X + POP_W - 1, ("│", border))
        cells_in = grid(inner[y - 1])
        for i in range(POP_W - 2):
            cell = cells_in[i]
            if cell is None:
                row[POP_X + 1 + i] = None
            else:
                put(row, POP_X + 1 + i, cell)
    return "\n".join(serialise(r) for r in rows)


# ---- the scenes ------------------------------------------------------------

def api_agent(**kw):
    return {"name": "api", "before": {"limiter.py": LIMITER_OLD, "tests/test_limiter.py": TEST_OLD},
            "after": {"limiter.py": LIMITER_NEW, "tests/test_limiter.py": TEST_NEW},
            "talk": API_TALK, **kw}


def scene_review():
    w = World([api_agent(replies=["Good catch. I'll drop buckets that have been full for an hour, "
                                  "in a sweep every 1,000 calls, and add a test for it."])])
    w.see("○")
    with Recorder(w) as rec:
        time.sleep(1.5)
        w.wait("api", True)
        w.see("limiter.py")
        time.sleep(2)
        w.keys("v", gap=1.5)
        w.keys(*["j"] * int(os.environ.get("DEMO_J", "13")), gap=0.25)
        time.sleep(0.6)
        w.keys("c")
        w.type("Buckets are never removed, so one per key ever seen stays in memory forever.")
        time.sleep(0.8)
        w.keys("C-s")
        time.sleep(1.5)
        w.keys("L")
        time.sleep(1.5)
        w.keys("S")
        w.see("Good catch")
        time.sleep(1)
    return w, rec


def scene_step():
    edit_old = "        now = time.time()\n"
    w = World([api_agent(after={})], modes={"api": "step"})
    w.see("○")
    repo = w.repos["api"]
    with Recorder(w) as rec:
        time.sleep(1)
        payload = {
            "session_id": SESSION_ID, "transcript_path": str(w.base / "none.jsonl"),
            "cwd": str(repo), "permission_mode": "acceptEdits", "hook_event_name": "PreToolUse",
            "tool_name": "Edit", "tool_use_id": "toolu_demo1",
            "tool_input": {"file_path": str(repo / "limiter.py"),
                           "old_string": "import time\n",
                           "new_string": "import threading\nimport time\n"},
        }
        first = w.hook("pre-tool", payload)
        w.see("threading")
        time.sleep(2.5)
        w.keys("y")
        first.wait()
        (repo / "limiter.py").write_text(LIMITER_OLD.replace("import time\n",
                                                             "import threading\nimport time\n"))
        time.sleep(1)
        payload.update(tool_use_id="toolu_demo2", tool_input={
            "file_path": str(repo / "limiter.py"), "old_string": edit_old,
            "new_string": "        now = time.monotonic()\n"})
        second = w.hook("pre-tool", payload)
        w.see("monotonic")
        time.sleep(2.5)
        w.keys("n")
        time.sleep(0.5)
        w.type("Keep time.time() for now: the tests freeze it with freezegun.")
        time.sleep(0.6)
        w.keys("Enter")
        second.wait()
        time.sleep(2)
    return w, rec


def scene_quiz():
    w = World([api_agent()], modes={"api": "quiz"})
    w.see("○")
    with Recorder(w) as rec:
        time.sleep(1)
        w.wait("api", True)
        w.see("Your answer")
        time.sleep(3)
        w.type("It tops the bucket up for the hour that passed, at rate tokens a second, "
               "but never past burst, so the key gets its full 10 back.")
        time.sleep(0.8)
        w.keys("C-s")
        w.see("Question 2 of 3")
        time.sleep(2)
        # The card behind the box shows the verdict on the last answer.
        w.keys("Escape")
        time.sleep(4)
    return w, rec


def scene_sessions():
    w = World([
        dict(name="api", talk=API_TALK, waiting=True,
             replies=["Updated README.md: the Rate limits section now describes the bucket of 10 "
                      "that refills at per_minute, per key."]),
        dict(name="web", talk=WEB_TALK),
        dict(name="docs", talk=DOCS_TALK, waiting=True),
    ])
    w.see("each key now gets")
    with Recorder(w) as rec:
        time.sleep(2)
        w.keys("l", gap=1.5)
        w.keys("l", gap=1.5)
        w.keys("h", "h", gap=1.2)
        w.keys("Enter")
        w.type("update the README's rate limit section to match")
        time.sleep(0.6)
        w.keys("Enter")
        w.see("Updated README.md")
        time.sleep(2)
        w.keys("W")
        w.see("Needs you")
        time.sleep(3.5)
        w.keys("q")
        time.sleep(1)
    return w, rec


SHELL_TALK = """\033[1m❯ put the new limits live on the server too\033[0m

● The config on the server is updated, but the running service still has
  the old limits. Restarting it needs sudo, which I can't run from here.
  Run this yourself:

  ! sudo systemctl restart api

  Then I'll make a few test calls to check the new limits.
"""


def scene_shell():
    w = World([dict(name="api", talk=SHELL_TALK, waiting=True)], client=True)
    base, repo = w.base, w.repos["api"]
    # The session's transcript, where aq finds the suggested command, and a
    # plain prompt for the shell.
    said = SHELL_TALK.split("● ", 1)[1].replace("\n  ", "\n").strip()
    project = base / ".claude" / "projects" / "-demo-api"
    project.mkdir(parents=True)
    # Compact, as Claude Code writes them.
    (project / f"{SESSION_ID}.jsonl").write_text("\n".join(json.dumps(line, separators=(",", ":")) for line in [
        {"type": "user", "message": {"content": "put the new limits live on the server too"}},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": said}]}},
    ]) + "\n")
    (base / ".zshrc").write_text("PS1='%F{blue}%~%f ❯ '\n")
    w.see("sudo systemctl")
    with Recorder(w) as rec:
        time.sleep(2.5)
        w.keys("!", gap=0)
        aq_files = base / "run" / "aq-shell"
        w.wait_for("aq's zsh files", lambda: (aq_files / ".zshrc").exists())
        # aq's popup is real but tmux draws it outside any pane, so the same
        # zsh setup runs in a hidden pane of the popup's size, laid over the
        # frames. Its own copy of the files, since the popup uses up `typed`.
        mine = base / "popup-shell"
        mine.mkdir(mode=0o700)
        for name in (".zshenv", ".zshrc"):
            text = (aq_files / name).read_text().replace(str(aq_files), str(mine))
            (mine / name).write_text(text)
        (mine / "typed").write_text("sudo systemctl restart api")
        w.tmux("new-window", "-d", "-n", "popup", "-c", str(repo),
               f"env HOME={base} ZDOTDIR={mine} AQ_ZDOTDIR={base} zsh")
        w.tmux("set-option", "-w", "-t", "demo:popup", "window-size", "manual")
        w.tmux("resize-window", "-t", "demo:popup", "-x", str(POP_W - 2), "-y", str(POP_H - 2))
        pane = w.tmux("display-message", "-p", "-t", "demo:popup", "#{pane_id}").strip()
        w.wait_for("the typed command", lambda: "restart api" in w.tmux("capture-pane", "-p", "-t", pane))
        w.overlay = (pane, " shell in api · exit or Ctrl-D closes it · Claude's last ! command is "
                           "typed in, Enter runs it ")
        time.sleep(5)
    return w, rec


def scene_tour():
    w = World([dict(name="api", talk=API_TALK, waiting=True)], tour=True)
    w.see("Welcome to aq")
    with Recorder(w) as rec:
        time.sleep(3)
        for _ in range(7):
            w.keys("Right", gap=3.2)
        time.sleep(1)
    return w, rec


SCENES = {"review": scene_review, "step": scene_step, "quiz": scene_quiz, "shell": scene_shell,
          "tour": scene_tour,
          "sessions": scene_sessions}


def main():
    names = sys.argv[1:] or list(SCENES)
    run("cargo", "build", "-q", cwd=ROOT)
    OUT.mkdir(parents=True, exist_ok=True)
    for name in names:
        try:
            w, rec = SCENES[name]()
            svg = to_svg(rec.frames, rec.end)
        finally:
            for w in list(WORLDS):
                w.close()
        (OUT / f"{name}.svg").write_text(svg)
        if dump := os.environ.get("DEMO_DUMP"):
            plain = [run("sed", "s/\x1b\\[[0-9;]*m//g", inp=f) for _, f in rec.frames]
            Path(dump, f"{name}.txt").write_text("\n=====\n".join(plain))
        print(f"{name}: {len(rec.frames)} frames, {len(svg) // 1024} KB")


if __name__ == "__main__":
    main()
