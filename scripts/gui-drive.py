#!/usr/bin/env python3
"""Drive the speechnotes GUI from outside: click by name, read state, capture.

Why this exists
===============
This box (Cosmic/Wayland, no AT-SPI registry by default, ydotool blocked,
ZCode's own window constantly taking the foreground) makes the usual
GUI-automation paths fail one by one. What actually works, proven on
2026-10-24, is a combination the whole family should reuse:

  * **AT-SPI semantic clicks.** The app is GTK4; when the accessibility
    bus is up at launch it bridges its whole widget tree, and every
    button exposes a `click` action that works *without focus, without
    the window being on top, without coordinates*. The reader toolbar is
    fully named by tooltip: "Contents", "Read the extracted text instead
    of the pages", "Zoom out", "Fit the page to the window", "Zoom in",
    "Original size (100%)", "Speak this chapter (F5)", "Previous
    chapter", "Next chapter" — plus the sidebar, note actions and window
    controls.
  * **AT-SPI state reads.** Status labels ("page 36 / 1295"), toggle
    states, the whole tree — also fully background.
  * **Window capture by X11 window id** (`XGetImage`), which works while
    the window is *occluded* — you get the last drawn frame. A fresh
    frame requires the window to be topmost at grab time, which is only
    reliable inside a single shell call (between calls the ZCode client
    raises itself).
  * **XTest** for raw keys/clicks on `:1` — needs the window focused /
    topmost (see `raise`).
  * **Raising**: launching a second `speechnotes` process activates the
    primary window (its "later activate" path). Caveats: it takes
    several seconds to land, it *closes any open popover* (the focus
    change), and Cosmic re-tiles the window to a random slot — never
    assume geometry, always re-measure.

If `speechnotes` is missing from the tree, the accessibility bus is
wedged again (its dbus-broker stops accepting connections while the
socket still listens — `Connection refused` on connect). `doctor`
restarts it; the app must then be *relaunched* to bridge.

Usage
-----
    python3 scripts/gui-drive.py doctor          # fix + verify the a11y bus
    python3 scripts/gui-drive.py buttons         # every named button + role
    python3 scripts/gui-drive.py labels          # every text label (state)
    python3 scripts/gui-drive.py status          # the reader's page status
    python3 scripts/gui-drive.py click 'Zoom in' # semantic, background
    python3 scripts/gui-drive.py key F5          # XTest key (needs focus)
    python3 scripts/gui-drive.py shot /tmp/s.png # capture by window id
    python3 scripts/gui-drive.py shotname 'Speaker view' /tmp/console.png
                                # capture a show window by title
                                # (the presenter work: 'Speechnotes — Show',
                                #  'Speechnotes — Speaker view')
    python3 scripts/gui-drive.py raise           # second-instance activate

    Precise controls (MERGE-VERIFY 2026-11-12, additive; all background):
    python3 scripts/gui-drive.py tick 'Ulysses · Kindle'   # a check box /
                                      # toggle / switch, to a state, idempotent
                                      # (`tick NAME off` unticks)
    python3 scripts/gui-drive.py press 'Send'    # a push button by its EXACT
                                      # name, frontmost dialog first — no
                                      # `clicki` numbering to go stale
    python3 scripts/gui-drive.py fill 'Peer address' 192.168.1.50
                                                 # type into a named entry/text

    Mapping / verification verbs (background unless noted):
    python3 scripts/gui-drive.py tree            # indented tree w/ states+extents
    python3 scripts/gui-drive.py map [FILE]      # write the full UI map + numbering
    python3 scripts/gui-drive.py clicki 7        # click actionable node #7 of `map`
    python3 scripts/gui-drive.py find NAME       # every node matching, with state
    python3 scripts/gui-drive.py state NAME      # toggles' checked/selected states
    python3 scripts/gui-drive.py rows [NAME]     # every list's rows (index+text)
    python3 scripts/gui-drive.py select 3        # row 3 of the first list, background
    python3 scripts/gui-drive.py combo Voice     # a DropDown's popup rows
    python3 scripts/gui-drive.py combo Voice kokoro  # …and pick one
    python3 scripts/gui-drive.py text NAME       # read a Text-interface node
    python3 scripts/gui-drive.py value NAME      # read a Value-interface node
    python3 scripts/gui-drive.py geo             # the main window's geometry
    python3 scripts/gui-drive.py extents NAME    # window-coord extents of matches
    python3 scripts/gui-drive.py wait NAME [S]   # poll until a node appears
    python3 scripts/gui-drive.py control NAME [VERB] [ARG]
                                      # drive any control: click, toggle,
                                      # set, type, state, text, value
    python3 scripts/gui-drive.py controls [ROLE]  # every operable control
    python3 scripts/gui-drive.py speak [WATCH]    # press the voice, watch the page
    python3 scripts/gui-drive.py ttsstate         # the transport's speech state

    Timing verbs (beta 2026-10-31, round-4 loading pass):
    python3 scripts/gui-drive.py time-startup ./target/release/speechnotes
                                      # launch + time bootstrap/to-tree
    python3 scripts/gui-drive.py time click 'A note' until 'preview words'
                                      # time one action in-process,
                                      # settle = latency to NAME appearing

Dependencies: python3-gi + at-spi2-core (both preinstalled here),
PIL for `shot`. No root, no installs.
"""

import ctypes
import os
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

try:
    from PIL import Image  # noqa: E402
except ImportError:
    Image = None

# A launch of a *renamed copy* of the binary (e.g. the /tmp/alpha-split-
# speechnotes trick to dodge a pkill loop) registers in the a11y tree under
# its own program name. Override it for such a session:
#   SPEECHNOTES_GUI_DRIVE_APP=alpha-split-speechnotes
APP_NAME = os.environ.get("SPEECHNOTES_GUI_DRIVE_APP", "speechnotes")
A11Y_SOCKET = "/run/user/%d/at-spi/bus_1" % os.getuid()

# ---------------------------------------------------------------- AT-SPI ---


def socket_alive() -> bool:
    import socket as s

    try:
        c = s.socket(s.AF_UNIX)
        c.connect(A11Y_SOCKET)
        c.close()
        return True
    except OSError:
        return False


def doctor() -> None:
    """Verify the accessibility bus, restarting its processes if wedged.

    The wedge seen in the wild: `ss -xl` shows the socket LISTENing yet
    every connect gets ECONNREFUSED — the dbus-broker behind it stopped
    accepting. Killing the stale launcher/broker is enough: both are
    dbus-activatable (`org.a11y.Bus`) and respawn on demand. The app
    bridges its tree only at *launch*, so after a restart the app must
    be relaunched to appear in the tree.
    """
    if socket_alive():
        print("a11y bus: alive")
        return
    print("a11y bus: wedged (socket present, connections refused) — restarting")
    out = subprocess.run(["pgrep", "-u", str(os.getuid()), "-f", "at-spi"],
                         capture_output=True, text=True).stdout.split()
    for pid in out:
        try:
            os.kill(int(pid), 15)
        except (ProcessLookupError, ValueError):
            pass
    time.sleep(1)
    # Force dbus to re-activate the bus, then wait for the socket.
    subprocess.run(["busctl", "--user", "call", "org.a11y.Bus",
                    "/org/a11y/bus", "org.a11y.Bus", "GetAddress"],
                   capture_output=True)
    for _ in range(20):
        if socket_alive():
            print("a11y bus: restarted — now RELAUNCH the app to bridge it")
            return
        time.sleep(0.5)
    raise SystemExit("a11y bus did not come back; is org.a11y.Bus activatable?")


def app_node() -> Atspi.Accessible:
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        a = desktop.get_child_at_index(i)
        # Prefix match: the presenter work launches a renamed copy of the
        # binary (speechnotes-present) so a concurrent session's pkill cannot
        # take this instance down; the a11y application name follows the
        # binary. On the private a11y bus this instance is alone anyway.
        if (a.get_name() or "").lower().startswith(APP_NAME):
            return a
    raise SystemExit(
        f"{APP_NAME!r} is not in the a11y tree. Run `doctor`, then relaunch "
        "the app (it bridges only at launch).")


# --------------------------------------------------------------- launching ---
# The app is a GApplication: a second launch hands its activation to the
# first and exits. That is a problem when more than one agent is working on
# the box — the second agent's window *is* the first agent's window, and a
# `pkill -x speechnotes` from either side kills both.
#
# `launch` starts an instance that belongs to this agent alone:
#
#   * its own XDG_RUNTIME_DIR, so it gets its own D-Bus connection and its
#     own `org.a11y.Bus` activation — the a11y bridge is per-session, and a
#     private runtime dir is what keeps one agent's bus restart from
#     unbridging another agent's app;
#   * its own XDG_CONFIG_HOME / XDG_CACHE_HOME, so the prefs the shelf and
#     the settings pages read are this agent's, not whoever wrote last;
#   * its own GApplication uniqueness key — `G_APPLICATION_NON_UNIQUE`
#     turns off the single-instance rule, so this instance is really its
#     own and another agent's launch cannot steal its activation.
#
# The data dir (books, notes, models) is shared on purpose: the library is
# the library.


def launch_isolated(extra_args=None) -> None:
    """Start a private instance of the app and wait for it to bridge."""
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    # ALPHA 2026-10-30 (additive): two agents launching at once shared one
    # base dir and trampled each other — the second launch overwrote
    # ui-agent.bus, the first app's bus connection died with it ("A
    # connection to the bus can't be made"). Set SPEECHNOTES_GUI_DRIVE_BASE
    # to get your own; the default stays the historical path.
    base = os.environ.get("SPEECHNOTES_GUI_DRIVE_BASE",
                          "/tmp/speechnotes-gui-drive")
    runtime = os.path.join(base, "rt")
    # Fix:  XDG_DATA_HOME was never re-pointed — an
    # isolated instance opened the USER'S LIVE notes store (measured: the
    # first gamma launch listed the live library). The agents' shell-env
    # recipes (`XDG_DATA_HOME=$base/xdg launch`) had masked it; bake the
    # recipe in. Config/cache were already scratch.
    data_home = os.path.join(base, "xdg")
    for d in (runtime, os.path.join(base, "cfg"), os.path.join(base, "cache"),
              data_home):
        os.makedirs(d, exist_ok=True)
    log = open(os.path.join(base, "app.log"), "ab")
    busfile = os.path.join(base, "ui-agent.bus")
    # alpha-3 2026-10-31 (additive): SPEECHNOTES_GUI_DRIVE_RUNTIME re-points
    # the APP's XDG_RUNTIME_DIR only — the export happens inside the session,
    # after the bus and a11y bootstrap, so the private sockets stay private
    # and the app alone gets the real runtime dir (its pipewire socket).
    # Set it to /run/user/1000 to give the instance the desktop's audio — a
    # TTS run in a plain isolated instance dies with "ALSA snd_pcm_open
    # failed: Host is down" because the pipewire socket lives in the real
    # runtime dir.
    app_runtime = os.environ.get("SPEECHNOTES_GUI_DRIVE_RUNTIME", "")
    app_runtime_line = (
        f'export XDG_RUNTIME_DIR={app_runtime}\n' if app_runtime else ""
    )
    a11yfile = os.path.join(base, "ui-agent.a11ybus")
    inner = f'''
set -eu
printf %s "$DBUS_SESSION_BUS_ADDRESS" > {busfile}
a11y=""
for i in $(seq 1 40); do
  out=$(busctl --address="$DBUS_SESSION_BUS_ADDRESS" call org.a11y.Bus \\
        /org/a11y/bus org.a11y.Bus GetAddress 2>/dev/null || true)
  case "$out" in 's "unix:'*) a11y=$(printf %s "$out" | sed -e 's/^s "//' -e 's/"$//'); break;; esac
  sleep 0.4
done
if [ -n "$a11y" ]; then
  printf %s "$a11y" > {a11yfile}
  if ! busctl --address="$a11y" list --no-pager 2>/dev/null | grep -q at-spi2-registr; then
    AT_SPI_BUS_ADDRESS="$a11y" XDG_RUNTIME_DIR={runtime} \\
      DISPLAY={os.environ.get("DISPLAY", ":1")} \\
      nohup /usr/libexec/at-spi2-registryd >> {base}/registry.log 2>&1 &
  fi
  sleep 0.5
fi
{app_runtime_line}env | sort > /tmp/sn-env-dump.txt
{app_runtime_line}exec "$@"
'''
    env = dict(
        os.environ,
        # SN_GDK_BACKEND overrides the launch backend: the presenter work
        # needs the Wayland-native path (fullscreen-on-monitor is a real
        # protocol hint there, while XWayland/COSMIC ignores its X11 twin —
        # measured 2026-11-12). The a11y bridge works on both; only the
        # X11 window grabs do not (no X windows on a Wayland app).
        GDK_BACKEND=os.environ.get("SN_GDK_BACKEND", "x11"),
        DISPLAY=os.environ.get("DISPLAY", ":1"),
        # The private XDG_RUNTIME_DIR below hides the desktop's Wayland
        # socket (wayland-1 resolves inside the private rt, where nothing
        # listens). On the Wayland backend the app gets the socket by
        # absolute path instead — WAYLAND_DISPLAY accepts one.
        **(
            {"WAYLAND_DISPLAY": os.environ.get("SN_WAYLAND_DISPLAY",
                                               "/run/user/1000/wayland-1")}
            if os.environ.get("SN_GDK_BACKEND") == "wayland" else {}
        ),
        XDG_RUNTIME_DIR=runtime,
        XDG_CONFIG_HOME=os.path.join(base, "cfg"),
        XDG_CACHE_HOME=os.path.join(base, "cache"),
        XDG_DATA_HOME=data_home,
    )
    env.pop("G_APPLICATION_NON_UNIQUE", None)  # was never a real GIO env var
    env.pop("DBUS_SESSION_BUS_ADDRESS", None)
    # A private **session bus**, not just a private runtime dir. The app is a
    # GApplication: on the shared desktop bus a second launch hands its
    # activation to whichever instance is already primary (measured twice —
    # the process exits 0 in ~0.3 s and every later gui-drive command drives
    # the OTHER agent's window). Under dbus-run-session the app is the only
    # instance on its own bus, so it IS the primary, and no launch can ever
    # hand off. org.a11y.Bus activates on that bus too (the standard service
    # files still load), so the a11y bridge comes up here and nowhere else —
    # which is also what keeps another agent's `doctor` from unbridging us.
    # The driver side must talk on the same bus: `scripts/gui-drive-mine`
    # exports the address before exec'ing this script; `launch` itself sets
    # it below, before wait_for_app.
    # The ordering matters and lives INSIDE the session: a11y bus up, the
    # registry started (a systemd user unit on the desktop's bus; a private
    # bus has no systemd, so nothing activates `org.a11y.atspi.Registry` and
    # the tree stays empty — measured "unit failed"), and only then the app,
    # whose a11y bridge registers with the registry once, at startup, and
    # does not retry. bash does the whole bootstrap, then execs the binary.
    binary = os.path.join(root, "target/debug/speechnotes")
    if not os.path.exists(binary):
        binary = os.path.expanduser("~/.local/bin/speechnotes")
    # OMEGA 2026-10-30 (additive): `launch [BINARY] [app-args...]` — an
    # optional first positional names the binary to run (e.g. the release
    # build for a release-gate smoke test). Non-positional (leading `-`)
    # args stay app arguments, exactly as before.
    if extra_args:
        first = str(extra_args[0])
        if not first.startswith("-") and os.path.isfile(first) and \
                os.access(first, os.X_OK):
            binary = os.path.abspath(first)
            extra_args = extra_args[1:]
    argv = (["dbus-run-session", "--", "bash", "-c", inner, "bash", binary]
            + list(extra_args or []))
    proc = subprocess.Popen(
        argv,
        env=env,
        cwd=root,
        stdout=log,
        stderr=log,
        stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    # The driver-side env for the wait below — and for every later
    # gui-drive-mine call, which re-reads the same two files.
    deadline = time.time() + 15
    while time.time() < deadline and not os.path.exists(busfile):
        time.sleep(0.1)
    if os.path.exists(busfile):
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = open(busfile).read().strip()
    if os.path.exists(a11yfile) and os.path.getsize(a11yfile) > 0:
        os.environ["AT_SPI_BUS_ADDRESS"] = open(a11yfile).read().strip()
    print(f"launched pid {proc.pid} (private runtime {runtime}, "
          f"private bus at {busfile})")
    wait_for_app(timeout=30)


def wait_for_app(timeout: float = 30.0) -> Atspi.Accessible:
    """Wait for the app to appear in the a11y tree."""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            app = app_node()
            if app is not None:
                print("app is in the a11y tree")
                return app
        except SystemExit as e:
            last = e
        time.sleep(1.0)
    raise SystemExit(f"the app never appeared in the a11y tree ({last})")


def walk(node, depth=0, max_depth=22):
    if depth > max_depth:
        return
    try:
        n = node.get_child_count()
    except Exception:
        return
    for i in range(min(n, 800)):
        c = node.get_child_at_index(i)
        if c is None:
            continue
        yield c
        yield from walk(c, depth + 1, max_depth)


def walk_with_depth(node, depth=0, max_depth=22):
    """The same walk, yielding `(depth, node)` — `press` sorts by it so a
    frontmost dialog's button wins over the window toolbar's same-named
    one (the walk visits parents before children; the dialog is deeper).
    Depth cap 22 = dump_tree's: the reader toolbar sits deeper than the
    old 14, which is why `press 'Present the deck'` could not see a button
    `tree` clearly showed (PRESENTER-VERIFY 2026-11-12)."""
    if depth > max_depth:
        return
    try:
        n = node.get_child_count()
    except Exception:
        return
    for i in range(min(n, 800)):
        c = node.get_child_at_index(i)
        if c is None:
            continue
        yield depth, c
        yield from walk_with_depth(c, depth + 1, max_depth)


def find(app, name_sub, role_sub=None):
    out = []
    for c in walk(app):
        try:
            role, name = c.get_role_name(), (c.get_name() or "")
        except Exception:
            continue
        if name_sub.lower() in name.lower() and (
            role_sub is None or role_sub in role
        ):
            out.append(c)
    return out


def find_best(app, name_sub, role_sub=None):
    """The match a human would mean.

    A control's accessible name is often a *prefix* of a longer name
    (GTK appends the popover's contents to a MenuButton; a rail button
    carries its state in the wording). Substring search alone picks the
    longest such name first and misses the real target, so rank:

      1. an exact name match
      2. the shortest name that contains the query
      3. everything else, in walk order

    Ties keep walk order, so the first of several identical buttons is
    still the first.
    """
    matches = find(app, name_sub, role_sub)
    if not matches:
        return []

    def rank(node):
        name = (node.get_name() or "")
        exact = 0 if name.strip().lower() == name_sub.strip().lower() else 1
        # On a name tie the ACTIONABLE node wins: a FlowBox cell and the
        # toggle button inside it share the child's name, and the cell has
        # no action of its own (the accent swatches measured this). sorted()
        # is stable, so equal ranks keep walk order.
        try:
            actionable = 0 if node.get_n_actions() > 0 else 1
        except Exception:
            actionable = 1
        return (exact, actionable, len(name))

    return sorted(matches, key=rank)


def semantic_click(app, name_sub, role_sub=None) -> str:
    """Click the first matching control. Returns what was clicked."""
    # MenuButton exposes no action itself; its inner toggle does.
    matches = find_best(app, name_sub, role_sub)
    with_action = [n for n in matches if n.get_n_actions() > 0]
    if not with_action:
        raise SystemExit(f"no actionable control matching {name_sub!r}")
    n = with_action[0]
    n.do_action(0)
    return f"{n.get_role_name()} {n.get_name()!r}"


# --------------------------------------------------------- mapping helpers ---
# The map verbs exist so every agent can review every UI feature from
# outside: `map` writes the whole tree (roles, names, states, extents) to a
# file and numbers every actionable node; `clicki` clicks a node by that
# number; `rows`/`select`/`combo` reach the widgets that expose no `click`
# action (ListBox rows, DropDown popups) through AT-SPI's Selection
# interface, which works in the background like `click` does.

STATE_NAMES = ["CHECKED", "SELECTED", "EXPANDED", "ACTIVE", "SHOWING",
               "SENSITIVE", "FOCUSABLE", "SELECTABLE", "EDITABLE",
               "READ_ONLY", "INVISIBLE"]

# The roles a person can operate. Everything else in the tree is a
# container and is not an "option".
INTERESTING_ROLES = {
    "button", "toggle button", "check box", "radio button", "switch",
    "combo box", "spin button", "slider", "scroll bar", "scale",
    "text", "entry", "password text", "list", "list item", "table",
    "table cell", "menu", "menu item", "link", "edit bar", "tool bar",
}


def node_states(node) -> list:
    out = []
    try:
        s = node.get_state_set()
    except Exception:
        return out
    for name in STATE_NAMES:
        t = getattr(Atspi.StateType, name, None)
        if t is not None and s.contains(t):
            out.append(name.lower())
    return out


def node_extents(node):
    try:
        e = node.get_extents(Atspi.CoordType.WINDOW)
        return (e.x, e.y, e.width, e.height)
    except Exception:
        return None


def first_label_text(node, max_depth=4) -> str:
    """A row's visible text: the first label at or under `node`."""
    try:
        if node.get_role_name() == "label":
            return node.get_name() or ""
    except Exception:
        return ""
    for c in walk(node, max_depth=max_depth):
        try:
            if c.get_role_name() == "label" and (c.get_name() or "").strip():
                return c.get_name() or ""
        except Exception:
            pass
    return ""


def node_text(node) -> str:
    """The Text interface's full contents (editor, status line)."""
    try:
        t = node.query_text()
        return t.get_text(0, t.get_character_count())
    except Exception:
        return ""


def model_row_button(app, engine_sub, want="Download"):
    """The Download/Remove button of the model row whose title matches.

    The Models page lays one row per model set: a text box (title + state)
    and one button labelled Download/Remove. Every button on the page shares
    the same accessible name, so the engine is picked by the *row's title*
    ("kokoro — 354 MB", "RapidOCR — page recognition for scanned books"): walk
    up from each candidate button to the row and read its first label.
    """
    for b in find(app, want, "button"):
        node = b
        for _ in range(4):
            node = node.get_parent()
            if node is None:
                break
            title = first_label_text(node, max_depth=6) or ""
            if engine_sub.lower() in title.lower():
                return b
    return None


def install_model(app, engine_sub, verb="Download") -> str:
    """Press a model row's Download (or Remove) button."""
    b = model_row_button(app, engine_sub, verb)
    if b is None:
        raise SystemExit(
            f"no {verb} button for a model row matching {engine_sub!r}")
    states = ",".join(node_states(b))
    b.do_action(0)
    return f"pressed {verb} on the row matching {engine_sub!r} [{states}]"


def node_value_repr(node) -> str:
    """One comparable snapshot of a node's displayed value.

    Value interface first (spin/scale), then the Text interface, then the
    accessible name — whichever the widget answers, so `poll` works for
    spin rows and status labels alike.
    """
    try:
        return str(node.get_current_value())
    except Exception:
        pass
    t = node_text(node)
    if t.strip():
        return t
    return node.get_name() or ""


def poll_value(name_sub, seconds: float, interval: float = 0.05) -> None:
    """Poll a control's value and timestamp every change.

    `poll NAME [SECS]` — this is how "the slider feels slow" becomes a
    number: click the stepper from another shell and this prints
    `+<elapsed>s  <old> -> <new>` per change. Re-walks the tree each tick
    (a changed label may be a new node), so the reported latency includes
    the AT-SPI walk (~0.1-0.4 s of observer cost — read the DELTA between
    changes, not the absolute). (alpha 2026-10-30)
    """
    deadline = time.time() + seconds
    last = None
    started = time.time()
    while time.time() < deadline:
        app = None
        try:
            app = app_node()
        except SystemExit:
            time.sleep(interval)
            continue
        matches = find_best(app, name_sub)
        if matches:
            now = node_value_repr(matches[0])
            if now != last:
                if last is not None:
                    print(f"+{time.time() - started:6.2f}s  {last!r} -> {now!r}",
                          flush=True)
                last = now
        time.sleep(interval)
    print(f"final {last!r} after {seconds:.0f}s", flush=True)


def spin_row(app, label_sub):
    """Find a spin row by its label: (spin, down_button, up_button).

    The settings' numeric knobs ("Reader text scale", "Book line height",
    …) are a label + an UNNAMED spin button + two unnamed stepper buttons,
    so none of them is findable by name. They share a row: locate the
    label's extents, then take the spin button and the buttons whose
    vertical centre sits on the same row and whose x is right of the
    label — the rightmost button is `+`, the other `−`. (alpha 2026-10-30)
    """
    labels = find_best(app, label_sub, "label")
    if not labels:
        raise SystemExit(f"no label matching {label_sub!r}")
    lext = node_extents(labels[0])
    if not lext:
        raise SystemExit(f"label {label_sub!r} has no extents")
    lcy = lext[1] + lext[3] / 2.0
    spin = down = up = None
    for n in walk(app):
        try:
            role = n.get_role_name()
        except Exception:
            continue
        if role not in ("spin button", "button"):
            continue
        e = node_extents(n)
        if not e or e[3] == 0:
            continue
        cy = e[1] + e[3] / 2.0
        if abs(cy - lcy) > max(lext[3], e[3]) / 2.0 or e[0] <= lext[0]:
            continue
        if role == "spin button" and spin is None:
            spin = n
        elif role == "button":
            if down is None or e[0] < node_extents(down)[0]:
                if up is None:
                    down = n
                elif e[0] < node_extents(up)[0]:
                    up = n
            elif up is None or e[0] > node_extents(up)[0]:
                up = n
    # Two unnamed buttons were expected: sort them out by x if both found.
    if down is not None and up is not None and \
            node_extents(down)[0] > node_extents(up)[0]:
        down, up = up, down
    if spin is None or down is None or up is None:
        raise SystemExit(
            f"row {label_sub!r}: spin={spin is not None} "
            f"down={down is not None} up={up is not None}")
    return spin, down, up


def spin_read(spin) -> str:
    try:
        v = spin.get_current_value()
        if 1e-6 < abs(v) < 1e6:  # the Value iface sometimes answers garbage
            return str(v)
    except Exception:
        pass
    return node_text(spin).strip() or (spin.get_name() or "")


def pick(app, combo_sub, value_sub) -> str:
    """Pick a DropDown row with a real mouse click and verify.

    `combo NAME VALUE` uses AT-SPI's Selection interface, which the popup
    rows silently ignore (select_child returns without error and nothing
    changes — re-measured 2026-10-30 on the Speech engine popup). The rows
    ARE clickable by mouse, so do what a user does: open the popup, find
    the X surface that appeared, click the row's band inside it, and
    re-read the combo's name (a DropDown's accessible name is its current
    value) to prove the pick landed. (alpha 2026-10-30)
    """
    def combo_by_desc(a):
        for c in walk(a):
            try:
                if c.get_role_name() == "combo box" and (
                        combo_sub.lower() in (c.get_name() or "").lower()
                        or combo_sub.lower() in (c.get_description() or "").lower()):
                    return c
            except Exception:
                pass
        return None

    combo = combo_by_desc(app)
    if combo is None:
        raise SystemExit(f"no combo matching {combo_sub!r}")
    before_name = combo.get_name() or ""
    # A popup left open by an earlier probe makes "new surface" detection
    # wrong (the toggle would only close it). Click twice: any open popup
    # closes, and the second click opens a fresh one to diff against.
    # Walk from `combo`, never from the app: the transport bar carries a
    # SECOND toggle with the same accessible name, and the first match on
    # an app-wide walk is not this combo's.
    def toggle_combo(a):
        for c in walk(combo, max_depth=8):
            try:
                if c.get_role_name() == "toggle button" and c.get_n_actions():
                    c.do_action(0)
                    return True
            except Exception:
                pass
        return False
    # Drive to a KNOWN closed state first: click until the a11y rows are
    # gone (a popup left open by an earlier probe made blind toggling
    # flip-flop), snapshot the surfaces, then open and diff.
    def rows_in_combo(a):
        for c in walk(combo, max_depth=8):
            try:
                if c.get_role_name() == "list item":
                    return True
            except Exception:
                pass
        return False

    app = app_node()
    for _ in range(4):
        if not rows_in_combo(app):
            break
        toggle_combo(app)
        time.sleep(0.6)
        app = app_node()
    before = {hex(w) for w, _ in popup_windows()}
    toggle_combo(app)
    time.sleep(0.8)
    app = app_node()
    if not rows_in_combo(app):
        raise SystemExit("combo would not open (no a11y rows after toggle)")
    new = [(w, g2) for w, g2 in popup_windows() if hex(w) not in before]
    # The popup is the new surface whose width fits a row list.
    new.sort(key=lambda wg: wg[1]["w"] * wg[1]["h"])
    pg = new[0][1]
    # Which row to click: the popup shows the full model in order; find the
    # index from the model rows the combo's a11y subtree carries.
    rows = [c.get_name() or "" for c in walk(combo, max_depth=8)
            if c.get_role_name() == "list item"]
    idx = next((i for i, r in enumerate(rows)
                if value_sub.lower() in r.lower()), None)
    if idx is None:
        for c in walk(combo, max_depth=6):
            try:
                if c.get_role_name() == "toggle button" and c.get_n_actions():
                    c.do_action(0)  # close it again
            except Exception:
                pass
        raise SystemExit(f"no popup row matching {value_sub!r} in {rows!r}")
    n_rows = max(len(rows), 1)
    xtest_click(int(pg["x"] + pg["w"] / 2),
                int(pg["y"] + pg["h"] * (idx + 0.5) / n_rows))
    time.sleep(0.8)
    app = app_node()
    after = combo_by_desc(app)
    after_name = after.get_name() if after else "?"
    return (f"clicked row {idx} of {n_rows} at {pg}: "
            f"{before_name!r} -> {after_name!r}")


def read_page_status(app) -> str:
    """The reader's page indicator, e.g. "page 7 / 46".

    The label is the one text that reads "page N / M". The walk that finds
    it is the same one the `status` verb uses; it is a function here so the
    TTS verbs can poll it without shelling out per poll.
    """
    for n in walk(app):
        try:
            if n.get_role_name() != "label":
                continue
            name = n.get_name() or ""
            if "page" in name.lower() and name.count("/") == 1:
                return name
        except Exception:
            continue
    return ""


def current_page() -> str:
    """The page indicator, read from a freshly-walked tree.

    The page label is rebuilt when the page changes, so a node read before
    the change can keep reporting the old text. Every poll takes a new
    tree.

    The tree is searched to depth 14 — the reader's page label sits at
    depth 12 on a loaded book, and the full walk of a book shelf runs to
    several thousand nodes (over a second per poll, too slow to time a
    page change). The search stops at the first match, so the common case
    is a short walk.

    An app that has quit takes its whole tree with it, and AT-SPI raises
    rather than returning an empty tree. That is a normal end of a run,
    not an error: the caller gets an empty string.
    """
    try:
        app = app_node()
    except SystemExit:
        return ""
    try:
        for n in walk(app, max_depth=14):
            try:
                if n.get_role_name() != "label":
                    continue
                name = n.get_name() or ""
                if "page" not in name.lower() or name.count("/") != 1:
                    continue
                # The OCR warm-up writes "recognising 3/12 — page 12" into
                # the same status line while it runs. A page watcher that
                # read it would report page 12 for a reader standing on
                # page 3 — the TTS gate would measure the wrong number.
                if name.lower().startswith("recognising"):
                    continue
                if name.lower().startswith("recognised"):
                    continue
                return name
            except Exception:
                continue
    except Exception:
        # The application went away mid-walk.
        return ""
    return ""


def actionable_nodes(app) -> list:
    """Every node with an action, in walk order — the `map` numbering."""
    out = []
    for c in walk(app):
        try:
            if c.get_n_actions() > 0 and c.get_role_name() != "label":
                out.append(c)
        except Exception:
            pass
    return out


def value_nodes(app) -> list:
    """Every node exposing the Value interface, in walk order — the `seti`
    numbering. Scroll bars expose a value but no AT-SPI action, so they are
    invisible to `map`/`clicki`; this walk reaches them (BETA 2026-10-30,
    additive)."""
    out = []
    for c in walk(app):
        try:
            c.get_current_value()
            out.append(c)
        except Exception:
            pass
    return out


def dump_tree(node, file, depth=0, max_depth=22, counter=None) -> None:
    if depth > max_depth:
        return
    try:
        role = node.get_role_name()
        name = (node.get_name() or "").replace("\n", " ")[:64]
        desc = (node.get_description() or "")[:32]
        nact = node.get_n_actions()
    except Exception:
        return
    tag = ""
    if counter is not None and nact > 0 and role != "label":
        counter[0] += 1
        tag = f" #{counter[0]}"
    line = f"{'  ' * depth}{role}{tag}"
    if name:
        line += f" {name!r}"
    if desc:
        line += f" ({desc})"
    st = node_states(node)
    if st:
        line += f" [{','.join(st)}]"
    ext = node_extents(node)
    if ext and (ext[2] or ext[3]):
        line += f" @{ext[0]},{ext[1]} {ext[2]}x{ext[3]}"
    if role == "text":
        body = node_text(node).replace("\n", " ")[:60]
        if body:
            line += f" text={body!r}"
    file.write(line + "\n")
    try:
        n = node.get_child_count()
    except Exception:
        return
    for i in range(min(n, 400)):
        c = node.get_child_at_index(i)
        if c is not None:
            dump_tree(c, file, depth + 1, max_depth, counter)


def lists_in(app) -> list:
    out = []
    for c in walk(app):
        try:
            if c.get_role_name() == "list":
                out.append(c)
        except Exception:
            pass
    return out


def list_rows(lst) -> list:
    """[(index, visible text, extents)] for a list's rows."""
    rows = []
    try:
        n = lst.get_child_count()
    except Exception:
        return rows
    for i in range(min(n, 600)):
        c = lst.get_child_at_index(i)
        if c is None:
            continue
        rows.append((i, first_label_text(c), node_extents(c)))
    return rows


def popup_lists(app, baseline_lists) -> list:
    """Lists that appeared after a combo opened (not in `baseline_lists`)."""
    before = {id(x) for x in baseline_lists}
    return [l for l in lists_in(app) if id(l) not in before]


def open_combo(app, name_sub):
    """Open the DropDown named `name_sub` (name or description match).
    Returns (toggle, popup list)."""
    matches = find(app, name_sub, "combo")
    if not matches:
        # A DropDown's own name is its current value; the group's label
        # ("Speech engine", "Voice") sits on the description.
        for c in walk(app):
            try:
                if (c.get_role_name() == "combo box"
                        and name_sub.lower() in (c.get_description() or "").lower()):
                    matches = [c]
                    break
            except Exception:
                pass
    if not matches:
        matches = find(app, name_sub, "toggle")
    if not matches:
        raise SystemExit(f"no combo matching {name_sub!r}")
    combo = matches[0]
    before = lists_in(app)
    toggle = None
    for c in walk(combo, max_depth=6):
        try:
            if c.get_role_name() == "toggle button" and c.get_n_actions():
                toggle = c
                break
        except Exception:
            pass
    if toggle is None:
        raise SystemExit(f"combo {name_sub!r} exposes no toggle")
    toggle.do_action(0)
    time.sleep(0.8)
    fresh = popup_lists(app, before)
    if not fresh:
        raise SystemExit("combo opened but no popup list appeared")
    return toggle, fresh


def combo_pick(app, name_sub, value_sub=None):
    """Open a combo; with `value_sub`, select the popup row matching it.
    Prints the rows otherwise."""
    toggle, fresh = open_combo(app, name_sub)
    if value_sub is None:
        for lst in fresh:
            for i, text, ext in list_rows(lst):
                print(f"{i:3d} {text!r} @{ext}")
        toggle.do_action(0)  # close it again
        time.sleep(0.4)
        return
    for lst in fresh:
        for i, text, _ext in list_rows(lst):
            if value_sub.lower() in text.lower():
                lst.select_child(i)
                time.sleep(0.6)
                toggle.do_action(0)
                time.sleep(0.4)
                print(f"selected {text!r}")
                return
    toggle.do_action(0)
    raise SystemExit(f"no row matching {value_sub!r}")


# ---------------------------------------------------------- control verbs ---
# "Map every option with gui-drive" means two things: seeing every control
# the app has, and being able to drive each one from outside. A button is
# easy (AT-SPI's `click` action). The rest needs the matching interface:
# a toggle/check has a state, a spin has a value, a radio is a selected
# child, a text entry has editable text, a slider has a value.
#
# `control` drives whichever interface the widget exposes, so a reviewer
# never has to know which one that is.


def _set_value(node, value) -> bool:
    """Set a Value-interface widget (spin, slider, scale)."""
    try:
        node.set_current_value(value)
        return True
    except Exception:
        pass
    # GtkSpinButton exposes the value on its entry child.
    for c in walk(node, max_depth=4):
        try:
            if c.get_role_name() in ("text", "entry", "spin button"):
                c.set_text(str(value))
                return True
        except Exception:
            continue
    return False


def select_text_range(app, name_sub, start: int, end: int) -> str:
    """ set a text selection on a matching text node.

    The speak-from-selection paths need the copy/cut gesture, and XTEST is
    dead — so the selection is made through the AT-SPI Text interface's
    setSelection, which GTK's bridge maps onto the buffer's selection mark.
    A TextView pill pops ~300 ms after the mark settles; a WebKit text node
    reports the same way the page's own selectionchange sees it.
    """
    target = None
    for n in walk(app):
        try:
            if n.get_role_name() not in ("text",):
                continue
        except Exception:
            continue
        name = n.get_name() or ""
        desc = n.get_description() or ""
        if name_sub.lower() in name.lower() or name_sub.lower() in desc.lower():
            target = n
            break
    if target is None:
        # Fall back: the text node holding the most characters (the editor
        # or a read-along view dominates the tree's other text nodes).
        best = None
        best_len = -1
        for n in walk(app):
            try:
                if n.get_role_name() != "text":
                    continue
                # gamma 2026-10-31 fix: the fallback still called
                # query_text() — pyatspi-only, absent in the GI binding this
                # driver uses (fefece1's own note says so) — so the fallback
                # never matched anything and name-less text nodes (the
                # webview's book page) were unreachable. The Accessible
                # carries the Text interface directly.
                length = n.get_character_count()
            except Exception:
                continue
            if length > best_len:
                best, best_len = n, length
        target = best
        if target is None:
            raise SystemExit("no text node found")
    try:
        # This GI binding exposes the Text interface straight on the
        # Accessible (query_text is the pyatspi spelling and does not exist
        # here); set_selection's GI order is (selection_num, start, end).
        count = target.get_character_count()
        if end > count:
            end = count
        if start >= end:
            raise SystemExit(f"range {start}..{end} empty (node holds {count})")
        target.set_selection(0, start, end)
        got = target.get_text_selections()
        return f"selected {start}..{end} of {count} (now {got})"
    except SystemExit:
        raise
    except Exception as e:
        raise SystemExit(f"set_selection failed: {e}")


def biggest_editable_text(app):
    """ the largest editable text node — the note
    body's TextView has no accessible name, so 'type' needs this to reach
    it."""
    best, best_len = None, -1
    for n in walk(app):
        try:
            if n.get_role_name() != "text":
                continue
            if not n.get_state_set().contains(Atspi.StateType.EDITABLE):
                continue
            length = n.get_character_count()
        except Exception:
            continue
        if length > best_len:
            best, best_len = n, length
    return best


def body_editable_text(app):
    """Additive (2026-11-11):  the note BODY TextView.

    `type-biggest` breaks on a fresh note: the title GtkEntry and the body
    TextView both sit at 0 characters, the walk hits the title first, and
    the whole string lands in the title (a literal \n renders as a tab
    glyph there and the note stays "No text yet" — measured twice). The
    body is the one editable node advertising MULTI_LINE (the title entry
    advertises NEITHER single- nor multi-line, measured, so a "not
    single-line" filter cannot tell them apart) — prefer MULTI_LINE.
    """
    fallback = None
    for n in walk(app):
        try:
            if n.get_role_name() != "text":
                continue
            ss = n.get_state_set()
            if not ss.contains(Atspi.StateType.EDITABLE):
                continue
            if ss.contains(Atspi.StateType.MULTI_LINE):
                return n
            if fallback is None:
                fallback = n
        except Exception:
            continue
    return fallback


def _set_text(node, text) -> bool:
    """Type into an editable text node."""
    try:
        node.set_text(text)
        return True
    except Exception:
        pass
    for c in walk(node, max_depth=4):
        try:
            if c.get_role_name() in ("text", "entry"):
                c.set_text(text)
                return True
        except Exception:
            continue
    return False


def dropdown_rows(app, name_sub):
    """Every row a DropDown offers, read from its own model.

    The popup cannot be *selected* through AT-SPI (measured four ways -- see
    the Pomodoro cards spec 2.2), but the *model* holds the same strings
    the popup shows. Reading them from the model is what makes "map every
    option" possible: the reviewer sees the full list of choices, and the
    stepper buttons beside the dropdown are what actually changes one.
    """
    combo = None
    for c in walk(app):
        try:
            if c.get_role_name() != "combo box":
                continue
        except Exception:
            continue
        name = (c.get_name() or "")
        desc = (c.get_description() or "")
        if (name_sub.lower() in name.lower()
                or name_sub.lower() in desc.lower()):
            combo = c
            break
    if combo is None:
        raise SystemExit(f"no combo box matching {name_sub!r}")
    rows = []
    for c in walk(combo, max_depth=8):
        try:
            if c.get_role_name() == "list item":
                rows.append(c.get_name() or "")
        except Exception:
            pass
    return combo, rows


def pick_index(app, combo_sub, idx) -> str:
    """`picki COMBO N` — pick a DropDown row by position, not by name.

    Additive (2026-11-11): GTK4 popup rows carry NO
    accessible name on this stack (measured: the Speech-engine popup's five
    list items all read ''), so `pick`'s value match can never fire. The
    rows are the model in order, and the caller knows the order it filled
    (`available_engines()` = espeak + installed catalog order), so an index
    pick with the same real-mouse machinery is the honest driver for it.
    Everything except the row choice is `pick`'s own logic (same popup
    surface diff, same click band, same verify-by-reread).
    """
    def combo_by_desc(a):
        for c in walk(a):
            try:
                if c.get_role_name() == "combo box" and (
                        combo_sub.lower() in (c.get_name() or "").lower()
                        or combo_sub.lower() in (c.get_description() or "").lower()):
                    return c
            except Exception:
                pass
        return None

    combo = combo_by_desc(app)
    if combo is None:
        raise SystemExit(f"no combo matching {combo_sub!r}")
    before_name = combo.get_name() or ""

    def toggle_combo():
        for c in walk(combo, max_depth=8):
            try:
                if c.get_role_name() == "toggle button" and c.get_n_actions():
                    c.do_action(0)
                    return True
            except Exception:
                pass
        return False

    def rows_in_combo(a):
        for c in walk(combo, max_depth=8):
            try:
                if c.get_role_name() == "list item":
                    return True
            except Exception:
                pass
        return False

    for _ in range(4):
        if not rows_in_combo(app):
            break
        toggle_combo()
        time.sleep(0.6)
        app = app_node()
    toggle_combo()
    time.sleep(0.8)
    app = app_node()
    if not rows_in_combo(app):
        raise SystemExit("combo would not open (no a11y rows after toggle)")
    # The popup is the SMALLEST speechnotes-titled X surface — the row list
    # is far smaller than any main frame, and (unlike pick's before/after
    # diff) this survives a popup window that GTK reuses across opens:
    # a failed earlier pick leaves its surface mapped, the combo reopens
    # into the SAME X window, and a diff for "new" surfaces finds nothing
    # (measured 2026-11-11 on the Speech-engine popup).
    wins = popup_windows()
    if len(wins) < 2:
        for c in walk(combo, max_depth=6):
            try:
                if c.get_role_name() == "toggle button" and c.get_n_actions():
                    c.do_action(0)
            except Exception:
                pass
        raise SystemExit(f"no popup surface among {wins}")
    wins.sort(key=lambda wg: wg[1]["w"] * wg[1]["h"])
    pg = wins[0][1]
    n_rows = len([c for c in walk(combo, max_depth=8)
                  if c.get_role_name() == "list item"]) or idx + 1
    if idx < 0 or idx >= n_rows:
        for c in walk(combo, max_depth=6):
            try:
                if c.get_role_name() == "toggle button" and c.get_n_actions():
                    c.do_action(0)
            except Exception:
                pass
        raise SystemExit(f"row {idx} out of range: popup shows {n_rows} rows")
    xtest_click(int(pg["x"] + pg["w"] / 2),
                int(pg["y"] + pg["h"] * (idx + 0.5) / n_rows))
    time.sleep(0.8)
    app = app_node()
    after = combo_by_desc(app)
    after_name = after.get_name() if after else "?"
    return (f"clicked row {idx} of {n_rows} at {pg}: "
            f"{before_name!r} -> {after_name!r}")


def drive(node, verb: str = "click", arg=None) -> str:
    """Drive one node and report what happened.

    Returns a one-line description of the action actually taken, so a
    reviewer can see from the log that the widget really moved.
    """
    role = node.get_role_name()
    name = node.get_name() or ""

    if verb == "click":
        if node.get_n_actions() > 0:
            node.do_action(0)
            return f"clicked {role} {name!r}"
        # No click action: a row in a list is selected instead.
        try:
            node.get_selection_iface().select_child(0)
            return f"selected {role} {name!r}"
        except Exception:
            pass
        return f"NO ACTION on {role} {name!r}"

    if verb == "toggle":
        # Toggle a check/toggle button: click it if it is off, and report
        # the resulting state.
        before = node_states(node)
        if node.get_n_actions() > 0:
            node.do_action(0)
            after = node_states(node)
            return (f"toggled {role} {name!r} "
                    f"[{','.join(before)}] -> [{','.join(after)}]")
        return f"NO ACTION on {role} {name!r}"

    if verb == "set":
        if _set_value(node, arg):
            return f"set {role} {name!r} = {arg}"
        return f"NO VALUE on {role} {name!r}"

    if verb == "type":
        if _set_text(node, arg):
            return f"typed into {role} {name!r}: {arg!r}"
        return f"NOT EDITABLE {role} {name!r}"

    if verb == "state":
        return f"{role} {name!r} [{','.join(node_states(node))}]"

    if verb == "text":
        return f"{role} {name!r}: {node_text(node)!r}"

    if verb == "value":
        try:
            return (f"{role} {name!r} = {node.get_current_value()} "
                    f"[{node.get_minimum_value()}..{node.get_maximum_value()}]")
        except Exception as e:
            return f"{role} {name!r}: no Value ({e})"

    if verb == "extents":
        return f"{role} {name!r} @{node_extents(node)}"

    return f"unknown verb {verb!r}"


# ------------------------------------------------------------- X11 layer ---

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XGetImage.restype = ctypes.c_void_p
x11.XGetImage.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int,
                          ctypes.c_int, ctypes.c_uint, ctypes.c_uint,
                          ctypes.c_ulong, ctypes.c_int]
DISPLAY = x11.XOpenDisplay(os.environ.get("DISPLAY", ":1").encode() or b":1")
if not DISPLAY:
    raise SystemExit("no X display (set DISPLAY=:1)")


class XImage(ctypes.Structure):
    _fields_ = [("width", ctypes.c_int), ("height", ctypes.c_int),
                ("xoffset", ctypes.c_int), ("format", ctypes.c_int),
                ("data", ctypes.c_void_p), ("byte_order", ctypes.c_byte),
                ("bitmap_unit", ctypes.c_byte),
                ("bitmap_bit_order", ctypes.c_byte),
                ("bitmap_pad", ctypes.c_byte), ("depth", ctypes.c_byte),
                ("bytes_per_line", ctypes.c_short),
                ("bits_per_pixel", ctypes.c_short),
                ("red_mask", ctypes.c_ulong),
                ("green_mask", ctypes.c_ulong),
                ("blue_mask", ctypes.c_ulong)]


x11.XDestroyImage.argtypes = [ctypes.c_void_p]


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int),
                ("serial", ctypes.c_ulong),
                ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p),
                ("window", ctypes.c_ulong),
                ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int),
                ("data", ctypes.c_long * 5)]


class XEvent(ctypes.Union):
    _fields_ = [("type", ctypes.c_int),
                ("xclient", XClientMessageEvent),
                ("pad", ctypes.c_long * 24)]


x11.XInternAtom.restype = ctypes.c_ulong
x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x11.XSendEvent.restype = ctypes.c_int
x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                           ctypes.c_long, ctypes.c_void_p]
x11.XDefaultRootWindow.restype = ctypes.c_ulong
x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x11.XResizeWindow.restype = ctypes.c_int
x11.XResizeWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_uint,
                              ctypes.c_uint]

# Xlib's default error handler calls exit(1) on a protocol error, so a
# surface that disappears mid-grab would kill the driver. Swallow them and
# let the caller notice the missing picture.
_ERRORS = []

XErrorHandler = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)


def _swallow_x_error(_display, event) -> int:
    _ERRORS.append(event)
    return 0


x11.XSetErrorHandler.restype = ctypes.c_void_p
x11.XSetErrorHandler.argtypes = [XErrorHandler]
_X_HANDLER = XErrorHandler(_swallow_x_error)  # keep the callback alive
x11.XSetErrorHandler(_X_HANDLER)


def xtest_key(name: str) -> None:
    code = x11.XKeysymToKeycode(
        DISPLAY, x11.XStringToKeysym(name.encode()))
    if not code:
        raise SystemExit(f"no keycode for {name}")
    xtst.XTestFakeKeyEvent(DISPLAY, code, True, 0)
    xtst.XTestFakeKeyEvent(DISPLAY, code, False, 0)
    x11.XFlush(DISPLAY)


def xtest_click(x: int, y: int) -> None:
    """Click at absolute root coordinates."""
    xtst.XTestFakeMotionEvent(DISPLAY, -1, x, y, 0)
    x11.XFlush(DISPLAY)
    xtst.XTestFakeButtonEvent(DISPLAY, 1, True, 0)
    xtst.XTestFakeButtonEvent(DISPLAY, 1, False, 0)
    x11.XFlush(DISPLAY)


def xtest_wheel(x: int, y: int, dy: int = -1) -> None:
    """Scroll the wheel at absolute root coordinates.

    Button 4 is "wheel up", button 5 is "wheel down" in the core X11
    protocol, and both are what a real mouse sends. The pointer is moved
    first, because the scroll lands wherever the cursor is.
    """
    button = 4 if dy < 0 else 5
    xtst.XTestFakeMotionEvent(DISPLAY, -1, x, y, 0)
    x11.XFlush(DISPLAY)
    for _ in range(abs(dy)):
        xtst.XTestFakeButtonEvent(DISPLAY, button, True, 0)
        xtst.XTestFakeButtonEvent(DISPLAY, button, False, 0)
    x11.XFlush(DISPLAY)


def popup_windows() -> list:
    """The app's popup surfaces: every X window that is not the main frame
    and not 1x1, with their geometry."""
    out = subprocess.run(["xwininfo", "-root", "-tree"],
                         capture_output=True, text=True).stdout
    wins = []
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("0x") and "speechnotes" in s.lower():
            wid = int(s.split(None, 1)[0], 16)
            g = win_geometry(wid)
            if g.get("w", 0) > 1 and g.get("h", 0) > 1:
                wins.append((wid, g))
    return wins


def popup_rect() -> dict:
    """The geometry of the popover: the second-largest popup surface (the
    largest is the main window)."""
    wins = popup_windows()
    if len(wins) < 2:
        raise SystemExit(f"no popover surface among {wins}")
    wins.sort(key=lambda wg: wg[1]["w"] * wg[1]["h"])
    return wins[-2][1]


def find_x_window() -> int:
    out = subprocess.run(["xwininfo", "-root", "-tree"],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("0x") and "Speechnotes" in s:
            return int(s.split(None, 1)[0], 16)
    raise SystemExit("no Speechnotes X window")


def x_windows_of_pid(pid: int) -> list:
    """The X surfaces owned by `pid`.

    `shot` picks the FIRST window titled Speechnotes, and X window ids are
    recycled the moment an instance dies — with several agent instances on
    X11 the grab lands on whichever instance reused the id, and the
    "screenshot" is of someone else's app. (alpha 2026-10-30, after exactly
    that ate two diagnostic rounds.)
    """
    import ctypes as _ct
    out = subprocess.run(["xwininfo", "-root", "-tree"],
                         capture_output=True, text=True).stdout
    x11 = _ct.CDLL("libX11.so.6")
    x11.XOpenDisplay.restype = _ct.c_void_p
    d = x11.XOpenDisplay(b":1")
    atom = x11.XInternAtom(d, b"_NET_WM_PID", False)
    wins = []
    for line in out.splitlines():
        s = line.strip()
        if not (s.startswith("0x") and "speechnotes" in s.lower()):
            continue
        wid = int(s.split(None, 1)[0], 16)
        t = _ct.c_ulong(); fmt = _ct.c_int(); n = _ct.c_ulong()
        ret = _ct.c_void_p(); bl = _ct.c_ulong()
        x11.XGetWindowProperty(d, wid, atom, 0, 1, False, 0,
                               _ct.byref(t), _ct.byref(fmt), _ct.byref(n),
                               _ct.byref(bl), _ct.byref(ret))
        if n.value and _ct.cast(ret, _ct.POINTER(_ct.c_ulong))[0] == pid:
            wins.append(wid)
    return wins


def shot_pid(pid: int, path: str) -> int:
    """Grab the main window of the instance `pid` owns."""
    wins = x_windows_of_pid(pid)
    if not wins:
        raise SystemExit(f"pid {pid} owns no X window")
    best = max(wins, key=lambda w: (lambda g: g.get("w", 0) * g.get("h", 0))(
        win_geometry(w)))
    grab(best).save(path)
    return best


def win_geometry(wid: int) -> dict:
    out = subprocess.run(["xwininfo", "-id", hex(wid)],
                         capture_output=True, text=True).stdout
    g = {}
    for line in out.splitlines():
        for k, p in [("x", "Absolute upper-left X:"),
                     ("y", "Absolute upper-left Y:"),
                     ("w", "Width:"), ("h", "Height:")]:
            if line.strip().startswith(p):
                g[k] = int(line.split(":")[1])
    return g


def grab(wid: int):
    """Capture the window by id. Works occluded; the frame is the last
    drawn one, so only trust it as *fresh* when the window was topmost.

    A surface can vanish between the geometry read and the grab (a popover
    closing), and XGetImage then raises a BadMatch X error that kills the
    process. Treat that as "no picture" and report it instead.
    """
    g = win_geometry(wid)
    if not g.get("w") or not g.get("h"):
        raise SystemExit(f"window {hex(wid)} has no size")
    x11.XGetImage.restype = ctypes.c_void_p
    xi = ctypes.cast(x11.XGetImage(DISPLAY, wid, 0, 0, g["w"], g["h"],
                                   0xFFFFFF, 2),
                     ctypes.POINTER(XImage))
    if not xi:
        raise SystemExit(f"XGetImage failed for {hex(wid)} (surface gone?)")
    n = g["w"] * g["h"] * 4
    buf = (ctypes.c_char * n).from_address(xi.contents.data).raw
    # X11's ZPixmap on a 24-bit TrueColor visual stores each pixel as
    # B, G, R, X — *not* R, G, B, X. Reading it as RGBX swaps red and blue,
    # which turns the app's blue accent (#3584e4) into orange and makes a
    # correct widget look mis-styled. Swap the channels back.
    img = Image.frombytes("RGBX", (g["w"], g["h"]), buf)
    r, g, b = img.split()[:3]
    img = Image.merge("RGB", (b, g, r))
    x11.XDestroyImage(ctypes.cast(xi, ctypes.c_void_p))
    return img


def raise_window() -> None:
    """Activate the primary window via a second instance. Takes seconds
    to land, closes open popovers, and Cosmic will re-tile the window —
    re-measure geometry afterwards."""
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    subprocess.Popen(
        [os.path.join(root, "target/debug/speechnotes")],
        env=dict(os.environ, GDK_BACKEND="x11", DISPLAY=":1"),
        cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True)
    print("activator launched; allow ~8s before measuring/clicking")


def raise_id(wid: int) -> None:
    """Raise one specific X window (alpha-2 2026-10-30, additive).

    `raise` activates the *first* Speechnotes window via a second launch —
    with 2+ agent instances that is whoever's instance is oldest, and the
    activator instance itself lands on the shared desktop profile. This
    raises an exact window id instead: the grab in `grab()` is only a FRESH
    frame when the window is topmost, and with several instances every
    agent needs its own window fronted for its own screenshots. Returns
    whether the server accepted it — the tiling WM may still re-tile.
    """
    x11.XRaiseWindow(DISPLAY, wid)
    x11.XFlush(DISPLAY)
    time.sleep(0.4)


def resize_window(width: int, height: int) -> None:
    """Resize the app's window to an exact size.

    Needed for the narrow-window gate: the toolbar row's surplus buttons
    have to scroll rather than clip, and that only shows at a width the
    row does not fit.

    This box's window manager tiles the app's window and refuses both
    `XConfigureWindow` on the client and the `_NET_WM_MOVERESIZE`
    client message. What does work is resizing the *frame* — the window
    the manager put around the client — which is the parent's own X
    window: the manager honours a configure on its own frame, and the
    client follows the frame's size.

    The frame is the client window's parent, found by walking the tree.
    """
    wid = find_x_window()
    frame = _frame_of(wid)
    x11.XResizeWindow(DISPLAY, frame, width, height)
    x11.XFlush(DISPLAY)
    time.sleep(0.6)
    after = win_geometry(wid)
    print(f"requested {width}x{height}, window is now "
          f"{after['w']}x{after['h']}")


def _frame_of(wid: int) -> int:
    """The window manager's frame around `wid` — the client window's
    parent, read out of the X tree."""
    out = subprocess.run(["xwininfo", "-root", "-tree"],
                         capture_output=True, text=True).stdout
    target = hex(wid)
    lines = out.splitlines()
    for index, line in enumerate(lines):
        if target in line:
            # The client is a child of the frame; the frame is the line
            # above it, indented one level less.
            for back in range(index - 1, max(-1, index - 4), -1):
                candidate = lines[back].strip()
                if not candidate.startswith("0x"):
                    continue
                if "child:" in lines[back]:
                    continue
                parent = int(candidate.split(None, 1)[0], 16)
                if parent != wid:
                    return parent
    return wid


def _atom(name: str) -> int:
    """The X atom id for `name`, created if it does not exist yet."""
    return x11.XInternAtom(DISPLAY, name.encode(), False)


# ------------------------------------------------------------------ main ---

def load_private_bus() -> None:
    """Point the driver at the isolated instance's buses when launched
    standalone.

    `launch` writes the session and a11y bus addresses of its private
    instance to /tmp/speechnotes-gui-drive/ui-agent.bus{,.a11ybus} — but
    shell state does not survive between two driver invocations, so every
    later call would otherwise land on the *shared desktop* bus and drive
    whatever instance lives there. When the caller has not exported the
    addresses themselves (an explicit env always wins) and the files
    exist, load them. Same base-dir override as `launch`. (alpha 2026-10-30)
    """
    base = os.environ.get("SPEECHNOTES_GUI_DRIVE_BASE",
                          "/tmp/speechnotes-gui-drive")
    for var, fname in [("DBUS_SESSION_BUS_ADDRESS", "ui-agent.bus"),
                       ("AT_SPI_BUS_ADDRESS", "ui-agent.a11ybus")]:
        if os.environ.get(var):
            continue
        path = os.path.join(base, fname)
        if os.path.exists(path) and os.path.getsize(path) > 0:
            addr = open(path).read().strip()
            if addr:
                os.environ[var] = addr


def repeat_timed(args) -> None:
    """Run another gui-drive action N times, timing each run. (beta
    2026-10-30, additive)

    `repeat COUNT CMD [ARGS...]` — each iteration is a fresh driver process
    (the CLI is stateless), so the numbers are end-to-end wall times for that
    action: process start, AT-SPI dispatch, and the app's own handling.
    Prints one line per run and a min/median/mean/max summary. This is the
    reader-scroll and pomodoro responsiveness probe: e.g.
    `repeat 30 wheel 900 500 -3` times 30 scroll ticks in the reader.
    """
    import statistics
    import subprocess
    if not args:
        print("usage: repeat COUNT CMD [ARGS...]")
        return
    count = int(args[0])
    rest = args[1:]
    times = []
    for i in range(count):
        t0 = time.monotonic()
        proc = subprocess.run(
            [sys.executable, os.path.abspath(__file__), *rest],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        dt = time.monotonic() - t0
        times.append(dt)
        lines = [l for l in (proc.stdout or "").strip().splitlines() if l.strip()]
        print(f"run {i + 1:3d}  {dt * 1000:8.1f} ms  rc={proc.returncode}  "
              f"{lines[-1] if lines else ''}")
    times.sort()
    med = statistics.median(times)
    mean = statistics.fmean(times)
    print(f"summary of {count} runs: min {times[0] * 1000:.1f} ms  "
          f"median {med * 1000:.1f} ms  mean {mean * 1000:.1f} ms  "
          f"max {times[-1] * 1000:.1f} ms")


def time_startup(args) -> None:
    """Measure launch → first frame, phase by phase. (beta 2026-10-31,
    additive, for the round-4 loading-time pass)

    `time-startup BINARY [app-args...]` — the binary is required (it is
    launched exactly like `launch`: isolated private-bus session). Phases:

        bootstrap       — command start → private bus address file exists
        exec→activate   — binary exec → the app's log line "first activate:
                          building the window" (dynamic linking + GTK init)
        exec→first frame— binary exec → the app's log line "window presented
                          mapped=true" (THE startup number)
        to-tree         — exec → the app is enumerable on the private a11y
                          bus, probed with fresh short-lived connections
                          (a long-lived client that polls during the bridge
                          window gets poisoned by a transient cache error
                          and never sees the app — measured, see board)
        first-walk      — one full tree walk after the app is visible (what
                          the first click/status pay)

    The exec timestamp comes from a wrapper script that stamps
    <base>/exec.marker then execs the binary. Log timestamps are the app's
    own (tracing RFC3339 UTC) — precise to microseconds, no polling race.

    Usage: python3 scripts/gui-drive.py time-startup ./target/release/speechnotes
    """
    import re as _re
    from datetime import datetime, timezone
    if not args:
        print("usage: time-startup BINARY [app-args...]")
        return
    binary = str(args[0])
    if not (os.path.isfile(binary) and os.access(binary, os.X_OK)):
        raise SystemExit(f"time-startup needs an executable binary, got {binary!r}")
    binary = os.path.abspath(binary)
    base = os.environ.get("SPEECHNOTES_GUI_DRIVE_BASE",
                          "/tmp/speechnotes-gui-drive")
    busfile = os.path.join(base, "ui-agent.bus")
    a11yfile = os.path.join(base, "ui-agent.a11ybus")
    logfile = os.path.join(base, "app.log")
    os.makedirs(base, exist_ok=True)
    for f in (busfile, a11yfile, logfile):
        if os.path.exists(f):
            os.unlink(f)  # stale addresses / old runs would pollute markers
    # The exec marker: a wrapper that stamps wall-clock epoch, then execs.
    marker = os.path.join(base, "exec.marker")
    if os.path.exists(marker):
        os.unlink(marker)
    wrapper = os.path.join(base, "exec-stamp.sh")
    with open(wrapper, "w") as f:
        f.write("#!/bin/bash\ndate +%s.%N > " + marker + "\nexec \"$@\"\n")
    os.chmod(wrapper, 0o755)
    t0 = time.monotonic()
    argv = [sys.executable, os.path.abspath(__file__), "launch", wrapper,
            binary, *args[1:]]
    # The private session has no secret service: the portal's own secret
    # proxy request then times out (~25 s) and blocks GTK init behind it.
    # The desktop's gnome-keyring answers instantly, so this is a harness
    # artifact, not app startup — turn the portal off for the measured run.
    env = dict(os.environ, GTK_USE_PORTAL="0")
    proc = subprocess.Popen(argv, env=env, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    deadline = t0 + 40
    t_bus = None
    while time.monotonic() < deadline:
        if os.path.exists(busfile) and os.path.getsize(busfile) > 0:
            t_bus = time.monotonic()
            break
        time.sleep(0.005)
    if t_bus is None:
        print("no bus file within 40 s — launch failed")
        return
    # a11y address + registry, so the tree probes below have a working bus.
    while time.monotonic() < deadline:
        if os.path.exists(a11yfile) and os.path.getsize(a11yfile) > 0:
            break
        time.sleep(0.01)
    a11y_addr = ""
    if os.path.exists(a11yfile) and os.path.getsize(a11yfile) > 0:
        a11y_addr = open(a11yfile).read().strip()
    t_reg = None
    while time.monotonic() < deadline and a11y_addr:
        out = subprocess.run(["busctl", "--address=" + a11y_addr, "list",
                              "--no-pager"], capture_output=True, text=True)
        if "at-spi2-registr" in (out.stdout or ""):
            t_reg = time.monotonic()
            break
        time.sleep(0.05)

    def log_epoch() -> dict:
        # The app log's RFC3339 UTC timestamps → epoch seconds.
        out = {}
        try:
            with open(logfile, "r", errors="replace") as f:
                for line in f:
                    line = _re.sub(r"\x1b\[[0-9;]*m", "", line)
                    m = _re.search(
                        r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)", line)
                    if not m:
                        continue
                    ts = datetime.fromisoformat(
                        m.group(1).replace("Z", "+00:00")).timestamp()
                    if "first activate" in line:
                        out["activate"] = ts
                    elif "window presented" in line and "mapped=true" in line:
                        out["mapped"] = ts
                    elif "book open requested" in line:
                        out["openreq"] = ts
                    elif "reader opened a chapter" in line:
                        out["chapter"] = ts
        except FileNotFoundError:
            pass
        return out

    def tree_epoch():
        # Fresh connection per probe: the bridge window poisons a client
        # that connects too early (cache GetItems fails, the app list never
        # recovers in that process). ~0.25 s quantisation.
        env = dict(os.environ)
        if a11y_addr:
            env["AT_SPI_BUS_ADDRESS"] = a11y_addr
        try:
            out = subprocess.run(
                [sys.executable, "-c",
                 "import gi; gi.require_version('Atspi','2.0');"
                 "from gi.repository import Atspi;"
                 "d=Atspi.get_desktop(0);"
                 "print('FOUND' if any('speechnotes' in (d.get_child_at_index(i).get_name() or '').lower() for i in range(d.get_child_count())) else 'NO')"],
                env=env, capture_output=True, text=True, timeout=10)
            return "FOUND" in (out.stdout or "")
        except Exception:
            return False

    t_exec = None
    while time.monotonic() < deadline:
        if os.path.exists(marker) and os.path.getsize(marker) > 0:
            t_exec = float(open(marker).read().strip())
            break
        time.sleep(0.005)
    if t_exec is None:
        print("the exec marker never appeared — the binary never ran")
        return
    logs = {}
    t_frame = None
    t_tree = None
    # beta (round 4, session 2, additive fix): with an open flag the run's
    # POINT is the exec->chapter-open number, but the open completes ~0.3 s
    # AFTER the tree is visible — the loop below used to break at tree-found
    # and the chapter line never got read (measured: number silently absent).
    # When the app args carry an open flag, the tree-found break waits for
    # the chapter marker too.
    opens_the_book = any(
        str(a).startswith(("--open", "--play", "--read")) for a in args[1:])
    while time.monotonic() < deadline:
        logs = log_epoch()
        if t_frame is None and "mapped" in logs:
            t_frame = logs["mapped"]
        if t_tree is None and tree_epoch():
            t_tree = time.time()
        if t_frame is not None and t_tree is not None and (
                not opens_the_book or "chapter" in logs):
            break
        time.sleep(0.05)
    t_walk0 = time.monotonic()
    n = -1
    try:
        load_private_bus()
        app = app_node()
        n = sum(1 for _ in walk(app))
    except SystemExit:
        pass
    t_walk = time.monotonic()
    parts = [f"startup: bootstrap {1000 * (t_bus - t0):7.1f} ms"]
    if t_reg is not None:
        parts.append(f"registry {1000 * (t_reg - t_bus):7.1f} ms")
    if "activate" in logs:
        parts.append(f"exec->activate {1000 * (logs['activate'] - t_exec):7.1f} ms")
    if "chapter" in logs:
        # A --open-book run: the reader finished opening the book/chapter.
        parts.append(f"exec->chapter open {1000 * (logs['chapter'] - t_exec):7.1f} ms")
        if "openreq" in logs:
            parts.append(f"(open itself {1000 * (logs['chapter'] - logs['openreq']):7.1f} ms)")
    if t_frame is not None:
        parts.append(f"exec->first frame {1000 * (t_frame - t_exec):7.1f} ms")
    else:
        parts.append("first frame NOT LOGGED")
    if t_tree is not None:
        parts.append(f"exec->a11y tree {1000 * (t_tree - t_exec):7.1f} ms")
    else:
        parts.append("a11y tree NOT SEEN")
    parts.append(f"first-walk {1000 * (t_walk - t_walk0):7.1f} ms ({n} nodes)")
    print(" | ".join(parts))


# One-shot actions `time` refuses to wrap (they launch or loop by design).
_TIME_FORBIDDEN = {"launch", "repeat", "time", "time-startup"}


def time_action(args) -> None:
    """Time one driver action in-process: no fresh Python/AT-SPI-process
    cost, so the numbers are the action's own. (beta 2026-10-31, additive,
    for the round-4 loading-time pass)

    `time CMD [ARGS...] [until NAME [ROLE]]` — runs `CMD` through main()'s
    dispatch after a timed warm-up connect, then, if `until NAME` is given,
    polls the tree (20 ms) until a node whose a11y name contains NAME
    appears; the settle time is the real user-visible latency of the action
    (e.g. `time click 'Some note' until 'first words of its preview'`).

    Prints `connect / action / settle / total` in ms.
    """
    rest = list(args)
    until_name = None
    until_role = None
    if "until" in rest:
        i = rest.index("until")
        until_name = rest[i + 1]
        until_role = rest[i + 2] if len(rest) > i + 2 else None
        rest = rest[:i]
    if not rest:
        print("usage: time CMD [ARGS...] [until NAME [ROLE]]")
        return
    if rest[0] in _TIME_FORBIDDEN:
        raise SystemExit(f"time cannot wrap {rest[0]!r}")
    t0 = time.monotonic()
    t_connect = None
    try:
        if app_node() is not None:
            t_connect = time.monotonic()
    except SystemExit:
        pass
    old_argv = sys.argv
    try:
        sys.argv = [old_argv[0], *rest]
        main()
    finally:
        sys.argv = old_argv
    t_action = time.monotonic()
    anchor = t_connect if t_connect is not None else t0
    connect_ms = (1000 * (t_connect - t0)) if t_connect is not None else -1.0
    parts = [f"time {rest[0]}: connect {connect_ms:7.1f} ms" if t_connect is not None
             else f"time {rest[0]}: connect (failed)",
             f"action {1000 * (t_action - anchor):7.1f} ms"]
    if until_name is None:
        print(" | ".join(parts), f"| total {1000 * (t_action - t0):7.1f} ms")
        return
    t_settle = None
    deadline = t_action + 30
    while time.monotonic() < deadline:
        try:
            if find(app_node(), until_name, until_role):
                t_settle = time.monotonic()
                break
        except SystemExit:
            pass
        time.sleep(0.02)
    if t_settle is None:
        parts.append(f"until {until_name!r} NOT SEEN within 30 s")
        parts.append(f"total {1000 * (time.monotonic() - t0):7.1f} ms")
    else:
        parts.append(f"settle {1000 * (t_settle - t_action):7.1f} ms")
        parts.append(f"total {1000 * (t_settle - t0):7.1f} ms")
    print(" | ".join(parts))


def main() -> None:
    if len(sys.argv) < 2:
        print(__doc__)
        return
    load_private_bus()
    cmd = sys.argv[1]
    if cmd == "doctor":
        doctor()
    elif cmd == "repeat":
        repeat_timed(sys.argv[2:])
    elif cmd == "time":
        time_action(sys.argv[2:])
    elif cmd == "time-startup":
        time_startup(sys.argv[2:])
    elif cmd == "launch":
        # Start a private, bridged instance of the app. Everything after
        # this works against that instance, so another agent's app (and
        # another agent's `pkill`) cannot interfere.
        launch_isolated(sys.argv[2:])
    elif cmd == "wait-app":
        wait_for_app(float(sys.argv[2]) if len(sys.argv) > 2 else 30.0)
    elif cmd == "buttons":
        app = app_node()
        for c in walk(app):
            try:
                if "button" in c.get_role_name():
                    print(f"{c.get_role_name():14s} {c.get_name()!r}")
            except Exception:
                pass
    elif cmd == "labels":
        app = app_node()
        for c in walk(app):
            try:
                if c.get_role_name() == "label":
                    print(repr(c.get_name()))
            except Exception:
                pass
    elif cmd == "status":
        app = app_node()
        hits = [c.get_name() for c in walk(app)
                if (c.get_name() or "").count("/") == 1
                and "page" in (c.get_name() or "").lower()]
        for h in hits:
            print(h)
    elif cmd == "options":
        # Every option a DropDown offers, read from its model -- the full
        # list of choices, even though the popup itself cannot be selected
        # from outside (the Pomodoro cards spec 2.2).
        app = app_node()
        combo, rows = dropdown_rows(app, sys.argv[2])
        print(f"combo {combo.get_name()!r} "
              f"({combo.get_description() or 'no description'})")
        for i, r in enumerate(rows):
            print(f"{i:3d} {r!r}")
    elif cmd == "click":
        app = app_node()
        what = semantic_click(app, sys.argv[2],
                              sys.argv[3] if len(sys.argv) > 3 else None)
        print("clicked", what)
    elif cmd == "tick":
        # MERGE-VERIFY 2026-11-12 (additive, user ask): set a check box /
        # toggle button / switch by name, to a wanted state. `click` reaches
        # the node with any action — on a CheckButton that is often the
        # LABEL (which forwards, but reads ambiguously and cannot be
        # asserted) — and nothing said whether the tick landed. `tick`
        # filters to the stateful roles, reads the current state, and only
        # clicks when it differs, so the verb is idempotent:
        #     python3 scripts/gui-drive.py tick 'Ulysses · Kindle'
        #     python3 scripts/gui-drive.py tick 'Ulysses · Kindle' off
        app = app_node()
        name = sys.argv[2]
        want_on = (sys.argv[3].lower() != "off") if len(sys.argv) > 3 else True
        stateful = []
        for n in find_best(app, name, None):
            try:
                if n.get_role_name() in ("check box", "toggle button", "switch"):
                    stateful.append(n)
            except Exception:
                continue
        if not stateful:
            raise SystemExit(f"no check box / toggle / switch matching {name!r}")
        n = stateful[0]
        checked = node_states(n)
        is_on = "CHECKED" in checked or "ACTIVE" in checked
        if is_on == want_on:
            print(f"already {'on' if is_on else 'off'}: "
                  f"{n.get_role_name()} {n.get_name()!r}")
        else:
            n.do_action(0)
            time.sleep(0.3)
            now = "CHECKED" in node_states(n) or "ACTIVE" in node_states(n)
            print(f"ticked {'on' if now else 'off'}: "
                  f"{n.get_role_name()} {n.get_name()!r}")
    elif cmd == "press":
        # MERGE-VERIFY 2026-11-12 (additive, user ask): click a push button
        # by its EXACT accessible name. The send flow has 'Send' inside a
        # dialog and 'Send files to a nearby device…' behind it — a
        # substring `click 'Send'` is a coin toss over which the tree's
        # walk order picks, and `clicki` numbering shifts whenever the
        # 700 ms app poll rebuilds a list between `map` and the click.
        # Exact name, role-filtered, dialog-first: the frontmost dialog's
        # buttons sit deeper in the tree than the window's toolbar ones,
        # and the walk visits parents before children, so sort by depth
        # descending — the deepest match (the dialog's) is pressed first.
        app = app_node()
        name = sys.argv[2].strip().lower()
        # The role is 'push button' on the stock bridge and plain 'button'
        # on this box's GTK build (measured side by side, 2026-11-12) —
        # accept both, and retry twice: the a11y cache churns while the
        # tree settles and a fresh walk can miss nodes an earlier one had.
        button_roles = ("push button", "button")
        candidates = []
        for _attempt in range(3):
            candidates = []
            for depth, n in walk_with_depth(app):
                try:
                    if n.get_role_name() not in button_roles:
                        continue
                    if (n.get_name() or "").strip().lower() == name:
                        candidates.append((depth, n))
                except Exception:
                    continue
            if candidates:
                break
            time.sleep(0.7)
        if not candidates:
            raise SystemExit(f"no push button named exactly {sys.argv[2]!r}")
        candidates.sort(key=lambda d: d[0], reverse=True)
        n = candidates[0][1]
        if n.get_n_actions() < 1:
            raise SystemExit(f"{n.get_name()!r} exposes no action")
        n.do_action(0)
        print(f"pressed {n.get_name()!r}")
    elif cmd == "fill":
        # MERGE-VERIFY 2026-11-12 (additive, user ask): set an entry / text
        # box by name — the BookDrop peer chooser's address box, which no
        # existing verb could reach (`type-body` targets the note body by
        # its MULTI_LINE state, `seti` the Value interface). The entry is
        # found by accessible name (the app gives the address box one), and
        # `_set_text` does the typing:
        #     python3 scripts/gui-drive.py fill 'Peer address' 192.168.1.50
        app = app_node()
        name = sys.argv[2]
        text = sys.argv[3] if len(sys.argv) > 3 else ""
        target = None
        for n in find_best(app, name, None):
            try:
                if n.get_role_name() in ("entry", "text", "edit bar",
                                         "password text"):
                    target = n
                    break
            except Exception:
                continue
        if target is None:
            raise SystemExit(f"no entry/text matching {name!r}")
        if not _set_text(target, text):
            raise SystemExit(f"could not set text on {target.get_name()!r}")
        print(f"filled {target.get_role_name()} {target.get_name()!r} = {text!r}")
    elif cmd == "control":
        # Drive any control by name, whatever kind it is:
        #   control NAME              -> click it
        #   control NAME toggle       -> flip a toggle/check, print the state
        #   control NAME set 5        -> set a spin/slider value
        #   control NAME type hello   -> type into a text entry
        #   control NAME state        -> print its states
        #   control NAME text         -> print its text
        #   control NAME value        -> print its value
        #   control NAME extents      -> print its geometry
        # A role filter may be given as a 4th argument.
        app = app_node()
        name = sys.argv[2]
        verb = sys.argv[3] if len(sys.argv) > 3 else "click"
        arg = sys.argv[4] if len(sys.argv) > 4 else None
        role = sys.argv[5] if len(sys.argv) > 5 else None
        matches = find_best(app, name, role)
        if not matches:
            raise SystemExit(f"no control matching {name!r}")
        if verb in ("set", "type") and arg is None:
            raise SystemExit(f"{verb} needs a value")
        print(drive(matches[0], verb, arg))
    elif cmd == "controls":
        # Every interactive control in the tree, one per line, with the
        # verbs that can drive it. This is the "map every option" view.
        #
        # Only roles a user can actually operate are listed: a plain panel
        # is not an option, and listing one per row buries the controls.
        app = app_node()
        wanted = sys.argv[2] if len(sys.argv) > 2 else None
        for n in walk(app):
            try:
                r = n.get_role_name()
                if wanted and wanted.lower() not in r.lower():
                    continue
                if r not in INTERESTING_ROLES:
                    continue
                verbs = []
                if n.get_n_actions() > 0:
                    verbs.append("click")
                st = node_states(n)
                if r in ("check box", "toggle button", "radio button",
                         "switch"):
                    verbs.append("toggle")
                if r in ("spin button", "slider", "scroll bar", "scale"):
                    verbs.append("set")
                if r in ("text", "entry", "password text"):
                    verbs.append("type")
                verbs += ["state", "text", "value", "extents"]
                # A combo box has no action of its own: its popup rows
                # cannot be selected from outside, so it is *readable* but
                # not *drivable* unless a stepper button sits beside it.
                # Marking that gap here is what keeps the map honest.
                if r == "combo box":
                    verbs.append("options")
                print(f"{r:16s} {(n.get_name() or '')!r:64s} "
                      f"[{','.join(st)}] {'|'.join(verbs)}")
            except Exception:
                continue
    elif cmd == "focus":
        # Fill-the-window on: the toolbar toggle (its a11y name is the
        # tooltip "Fill the whole window (F11)"). XTest F11 is dead on this
        # box, so the gate drives the control the reader drives.
        app = app_node()
        what = semantic_click(app, "Fill the whole window", "toggle")
        print("clicked", what)
    elif cmd == "unfocus":
        # Fill-the-window off: the header's leave button.
        app = app_node()
        what = semantic_click(app, "Leave fill-the-window", "button")
        print("clicked", what)
    elif cmd == "key":
        xtest_key(sys.argv[2])
    elif cmd == "clickat":
        xtest_click(int(sys.argv[2]), int(sys.argv[3]))
        print("clicked at", sys.argv[2], sys.argv[3])
    elif cmd == "wheel":
        # Scroll at absolute root coordinates: `wheel X Y [DY]`.
        xtest_wheel(int(sys.argv[2]), int(sys.argv[3]),
                    int(sys.argv[4]) if len(sys.argv) > 4 else -1)
        print(f"wheel {sys.argv[2]},{sys.argv[3]} "
              f"{sys.argv[4] if len(sys.argv) > 4 else -1}")
    elif cmd == "tree":
        app = app_node()
        import io
        buf = io.StringIO()
        dump_tree(app, buf)
        print(buf.getvalue())
    elif cmd == "map":
        app = app_node()
        path = sys.argv[2] if len(sys.argv) > 2 else "docs/UI-MAP.md"
        counter = [0]
        with open(path, "w") as f:
            f.write("# UI map — generated by `scripts/gui-drive.py map`\n\n")
            f.write(f"Regenerate with `python3 scripts/gui-drive.py map {path}`. "
                    "Actionable nodes carry `#N`; `clicki N` clicks node N.\n\n")
            f.write("```\n")
            dump_tree(app, f, counter=counter)
            f.write("```\n")
        print(f"wrote {path} ({counter[0]} actionable nodes)")
    elif cmd == "clicki":
        app = app_node()
        want = int(sys.argv[2])
        nodes = actionable_nodes(app)
        if want < 1 or want > len(nodes):
            raise SystemExit(f"#1..#{len(nodes)} exist (run `map`)")
        n = nodes[want - 1]
        n.do_action(0)
        print(f"clicked #{want}: {n.get_role_name()} {n.get_name()!r}")
    elif cmd == "seti":
        # BETA 2026-10-30 (additive): set a Value-interface node by its
        # `seti` numbering — a walk over every node exposing Value, so the
        # unnamed scroll bars (which carry no AT-SPI actions and are absent
        # from `map`'s #N numbering) and every slider are reachable. For a
        # scroll bar the value is 0..scrollable extent, not 0..1 — read the
        # range first with `valuei`.
        #     python3 scripts/gui-drive.py valuei 1     # read range
        #     python3 scripts/gui-drive.py seti 1 400   # scroll/set there
        app = app_node()
        want = int(sys.argv[2])
        value = float(sys.argv[3])
        nodes = value_nodes(app)
        if want < 1 or want > len(nodes):
            raise SystemExit(f"#1..#{len(nodes)} exist (run `valuei 0` to list)")
        n = nodes[want - 1]
        n.set_current_value(value)
        print(f"set #{want} {n.get_role_name()} {n.get_name()!r} = {value}")
    elif cmd == "valuei":
        # BETA 2026-10-30 (additive): list/read Value-interface nodes.
        # `valuei 0` lists every one with its index; `valuei N` reads #N.
        app = app_node()
        nodes = value_nodes(app)
        if len(sys.argv) > 2 and int(sys.argv[2]) > 0:
            want = int(sys.argv[2])
            if want > len(nodes):
                raise SystemExit(f"#1..#{len(nodes)} exist")
            n = nodes[want - 1]
            cur = n.get_current_value()
            lo, hi = n.get_minimum_value(), n.get_maximum_value()
            print(f"#{want} {n.get_role_name()} {n.get_name()!r} = {cur} [{lo}..{hi}]")
        else:
            for i, n in enumerate(nodes, 1):
                try:
                    cur = n.get_current_value()
                    lo, hi = n.get_minimum_value(), n.get_maximum_value()
                    print(f"#{i} {n.get_role_name()} {n.get_name()!r} = {cur} [{lo}..{hi}]")
                except Exception as e:
                    print(f"#{i} {n.get_role_name()} {n.get_name()!r}: {e}")
    elif cmd == "install-model":
        # The Models page's download, driven by row title, not button name:
        # `install-model kokoro` / `install-model RapidOCR`.
        app = app_node()
        print(install_model(app, sys.argv[2], "Download"))
    elif cmd == "remove-model":
        # The same row's Remove — for testing the delete path.
        app = app_node()
        print(install_model(app, sys.argv[2], "Remove"))
    elif cmd == "find":
        app = app_node()
        for n in find_best(app, sys.argv[2],
                           sys.argv[3] if len(sys.argv) > 3 else None):
            st = ",".join(node_states(n))
            ext = node_extents(n)
            print(f"{n.get_role_name():14s} {n.get_name()!r} "
                  f"[{st}] @{ext}")
    elif cmd == "state":
        app = app_node()
        for n in find_best(app, sys.argv[2],
                           sys.argv[3] if len(sys.argv) > 3 else None):
            st = ",".join(node_states(n))
            print(f"{n.get_role_name():14s} {n.get_name()!r} [{st}]")
    elif cmd == "rows":
        app = app_node()
        for lst in lists_in(app):
            rows = list_rows(lst)
            named = [t for _i, t, _e in rows if t.strip()]
            if sys.argv[2:3] and not any(
                    sys.argv[2].lower() in t.lower() for t in named):
                continue
            print(f"== list with {len(rows)} rows")
            for i, text, ext in rows:
                print(f"{i:3d} {text!r} @{ext}")
    elif cmd == "select":
        app = app_node()
        lists = lists_in(app)
        which = int(sys.argv[2]) if len(sys.argv) > 3 else 0
        row = int(sys.argv[3] if len(sys.argv) > 3 else sys.argv[2])
        if which >= len(lists):
            raise SystemExit(f"only {len(lists)} list(s) in the tree")
        lists[which].select_child(row)
        print(f"selected row {row} of list {which}")
    elif cmd == "cell":
        # alpha-2 2026-10-30, additive: select a shelf card (a table cell —
        # cards-mode books are a FlowBox, exposed as a table, not a list) by
        # a name it carries. `cell NAME [ORDINAL]` selects the ordinal'th
        # matching cell; the app opens the book on selection, the same path
        # a real click takes.
        app = app_node()
        name = sys.argv[2]
        ordinal = int(sys.argv[3]) if len(sys.argv) > 3 else 0
        cells = [c for c in walk(app)
                 if c.get_role_name() == "table cell"
                 and name.lower() in (c.get_name() or "").lower()]
        if not cells:
            raise SystemExit(f"no table cell matching {name!r}")
        if ordinal >= len(cells):
            raise SystemExit(f"only {len(cells)} cell(s) matching {name!r}")
        target = cells[ordinal]
        parent = target.get_parent()
        sel = None
        for node in (parent, target):
            try:
                sel = node.querySelection()
                break
            except Exception:
                continue
        if sel is None:
            raise SystemExit("the cell (and its parent) expose no Selection")
        index = 0
        try:
            index = target.get_index_in_parent()
        except Exception:
            pass
        sel.select_child(index)
        print(f"selected cell {index} ({target.get_name()!r})")
    elif cmd == "combo":
        app = app_node()
        combo_pick(app, sys.argv[2],
                   sys.argv[3] if len(sys.argv) > 3 else None)
    elif cmd == "text":
        app = app_node()
        for n in find_best(app, sys.argv[2],
                           sys.argv[3] if len(sys.argv) > 3 else None):
            body = node_text(n)
            print(f"--- {n.get_role_name()} {n.get_name()!r}")
            print(body[:4000])
    elif cmd == "value":
        app = app_node()
        for n in find_best(app, sys.argv[2],
                           sys.argv[3] if len(sys.argv) > 3 else None):
            try:
                cur = n.get_current_value()
                lo, hi = n.get_minimum_value(), n.get_maximum_value()
                print(f"{n.get_role_name()} {n.get_name()!r} = {cur} [{lo}..{hi}]")
            except Exception as e:
                print(f"{n.get_role_name()} {n.get_name()!r}: no Value ({e})")
    elif cmd == "geo":
        g = win_geometry(find_x_window())
        print(g)
    elif cmd == "extents":
        app = app_node()
        for n in find_best(app, sys.argv[2],
                           sys.argv[3] if len(sys.argv) > 3 else None):
            print(f"{n.get_role_name():14s} {n.get_name()!r} @{node_extents(n)}")
    elif cmd == "wait":
        app = app_node()
        deadline = time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 20.0)
        while time.time() < deadline:
            if find(app, sys.argv[2]):
                print("found", sys.argv[2])
                break
            time.sleep(0.5)
        else:
            raise SystemExit(f"timeout waiting for {sys.argv[2]!r}")
    elif cmd == "popup":
        print(popup_rect())
    elif cmd == "popups":
        for wid, g in popup_windows():
            print(hex(wid), g)
    elif cmd == "shot":
        if Image is None:
            raise SystemExit("PIL missing: pip install pillow")
        grab(find_x_window()).save(sys.argv[2] if len(sys.argv) > 2
                                   else "/tmp/speechnotes.png")
        print("saved")
    elif cmd == "shotid":
        # Grab one X surface by its id — the popovers are separate windows,
        # so `shot` (the main frame) never shows them.
        if Image is None:
            raise SystemExit("PIL missing: pip install pillow")
        wid = int(sys.argv[2], 0)
        grab(wid).save(sys.argv[3] if len(sys.argv) > 3
                      else "/tmp/speechnotes.png")
        print(f"saved {hex(wid)}")
    elif cmd == "shotpid":
        # `shotpid PID [FILE]` — the main window of the instance `pid`
        # owns. `shot` grabs the first Speechnotes-titled X window, and X
        # window ids are recycled as agent instances die, so with more
        # than one instance alive the grab lands on whoever reused the id.
        # alpha 2026-10-30 (additive).
        if Image is None:
            raise SystemExit("PIL missing: pip install pillow")
        wid = shot_pid(int(sys.argv[2]),
                       sys.argv[3] if len(sys.argv) > 3
                       else "/tmp/speechnotes.png")
        print(f"saved pid {sys.argv[2]} window {hex(wid)}")
    elif cmd == "shotname":
        # `shotname SUBSTR FILE` — grab the window whose TITLE contains
        # SUBSTR (case-insensitive). PRESENTER-VERIFY 2026-11-12 (additive):
        # the show opens its own windows the main window's grabs never see
        # ("Speechnotes — Show" on the audience screen, "Speechnotes —
        # Speaker view" on the console screen), and `shotpid` picks only
        # the LARGEST surface of the pid — fullscreen windows on two
        # screens tie, and the console must be seen, not guessed at.
        if Image is None:
            raise SystemExit("PIL missing: pip install pillow")
        title = sys.argv[2].lower()
        path = sys.argv[3] if len(sys.argv) > 3 else "/tmp/speechnotes.png"
        out = subprocess.run(["xwininfo", "-root", "-tree"],
                             capture_output=True, text=True).stdout
        wins = []
        for line in out.splitlines():
            ls = line.strip()
            if ls.startswith("0x") and title in ls.lower():
                try:
                    wid = int(ls.split(None, 1)[0], 16)
                except ValueError:
                    continue
                wins.append((wid, ls))
        if not wins:
            raise SystemExit(f"no X window titled ~{sys.argv[2]!r}")
        wid, found = wins[0]
        grab(wid).save(path)
        print(f"grabbed {found.split(':')[0]} ({found.split(chr(34))[1] if chr(34) in found else '?'}) -> {path}")
    elif cmd == "shotall":
        # Every app surface, each to <prefix>-<wid>.png.
        if Image is None:
            raise SystemExit("PIL missing: pip install pillow")
        prefix = sys.argv[2] if len(sys.argv) > 2 else "/tmp/sn"
        for wid, g in popup_windows():
            path = f"{prefix}-{hex(wid)}.png"
            try:
                grab(wid).save(path)
                print(f"{path} {g}")
            except SystemExit as e:
                print(f"{hex(wid)} {g}: {e}")
    elif cmd == "raise":
        raise_window()
    elif cmd == "raiseid":
        # alpha-2 2026-10-30, additive: `raiseid WID` — raise one exact X
        # window (find it via `shotpid PID`), so an agent fronts its OWN
        # instance for a fresh screenshot instead of `raise`'s first window.
        raise_id(int(sys.argv[2], 0))
        print(f"raised {sys.argv[2]}")
    elif cmd == "resize":
        resize_window(int(sys.argv[2]), int(sys.argv[3]))
    elif cmd == "speak":
        # The TTS gate: press the voice and measure what actually happened.
        #
        #   speak                 -> press the reader's speak button
        #   speak WATCH [SECS]    -> press it and watch the page advance
        #
        # The numbers that matter for "is the TTS fast and does it keep
        # going" are the *page* it started on, the page it moved to, and
        # how long that took — a voice that speaks but never advances is
        # indistinguishable from one that works unless the page is watched.
        # The reader's status line ("page 7 / 46") is the page indicator.
        app = app_node()
        # `speak 90` puts the watch length in argv[2]; argv[3] would be a
        # second argument the verb does not take.
        watch = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
        before = read_page_status(app)
        started = time.time()
        clicked = semantic_click(app, "Speak this chapter")
        print(f"pressed {clicked} at page {before!r}")
        if watch <= 0:
            return
        deadline = started + watch
        seen = []
        last = before
        while time.time() < deadline:
            time.sleep(1.0)
            # The tree is re-read every poll: the page label is rebuilt when
            # the page changes, and a node held from before the change can
            # report the old text forever.
            now = current_page()
            if not now:
                # The app quit (or the reader closed). The run is over; say
                # so rather than spinning to the deadline.
                print("  the app is no longer in the a11y tree", flush=True)
                break
            if now != last:
                elapsed = time.time() - started
                seen.append(now)
                print(f"  +{elapsed:5.1f}s  page {now!r}", flush=True)
                last = now
        print(f"final page {current_page()!r} after {watch:.0f}s", flush=True)
    elif cmd == "poll":
        # `poll NAME [SECS]` — timestamp every change of a control's value
        # (see poll_value). Run one poll in the background while clicking
        # the control from another shell; the change latencies are the
        # "the slider feels slow" measurement.
        poll_value(sys.argv[2],
                   float(sys.argv[3]) if len(sys.argv) > 3 else 10.0)
    elif cmd == "stepper":
        # `stepper LABEL`            -> read the row's spin value
        # `stepper LABEL up [N]`     -> click `+` N times (default 1)
        # `stepper LABEL down [N]`   -> click `−` N times
        # Drives the settings' unnamed numeric rows by their label
        # (see spin_row); prints the value after each click.
        app = app_node()
        spin, down, up = spin_row(app, sys.argv[2])
        if len(sys.argv) > 3 and sys.argv[3] in ("up", "down"):
            times = int(sys.argv[4]) if len(sys.argv) > 4 else 1
            for _ in range(times):
                (up if sys.argv[3] == "up" else down).do_action(0)
                time.sleep(0.3)
            print(f"{sys.argv[3]} x{times} -> {spin_read(spin)!r}")
        else:
            print(f"value = {spin_read(spin)!r}")
    elif cmd == "pick":
        # `pick COMBO VALUE` — pick a DropDown row with a real mouse click
        # (AT-SPI's select_child is a silent no-op on GTK popups; see pick).
        app = app_node()
        print(pick(app, sys.argv[2], sys.argv[3]))
    elif cmd == "picki":
        # `picki COMBO N` — pick a DropDown row by position (0-based): GTK4
        # popup rows carry no accessible name here, so a value match can
        # never fire; see pick_index.
        app = app_node()
        print(pick_index(app, sys.argv[2], int(sys.argv[3])))
    elif cmd == "select-text":
        #  the speak-from-selection driver — set a
        # selection on a text node (NAME_SUB START END) via AT-SPI.
        app = app_node()
        print(select_text_range(app, sys.argv[2], int(sys.argv[3]),
                                int(sys.argv[4])))
    elif cmd == "type-biggest":
        #  type into the largest editable text node
        # (the note body; its TextView carries no accessible name).
        app = app_node()
        node = biggest_editable_text(app)
        if node is None:
            raise SystemExit("no editable text node")
        node.set_text_contents(sys.argv[2])
        print(f"typed {node.get_character_count()} chars into the biggest editable text")
    elif cmd == "type-body":
        # Additive (2026-11-11):  type into the note BODY
        # (the multi-line editable text node), not the title entry — see
        # body_editable_text for why type-biggest cannot reach it on a
        # fresh note.
        app = app_node()
        node = body_editable_text(app)
        if node is None:
            raise SystemExit("no multi-line editable text node")
        node.set_text_contents(sys.argv[2])
        print(f"typed {node.get_character_count()} chars into the note body")
    elif cmd == "ttsstate":
        # The speech state, read the way a person reads it: the transport
        # bar's status label ("Speaking", "Finished", "Stopped") and the
        # position line. A session that ends "Stopped" instead of
        # "Finished" is the failure that silences the book walk.
        #
        # The match is exact: the transport bar's *controls* carry these
        # words too ("Speaking speed" is the rate control's label, "Paused"
        # is a state the widget offers), and a substring match picks those
        # up instead of the status line.
        app = app_node()
        for state in ("Speaking", "Finished", "Stopped", "Paused", "Failed"):
            for n in find(app, state):
                if (n.get_name() or "").strip() == state:
                    print(f"status: {state}")
        page = read_page_status(app)
        if page:
            print(f"page: {page}")
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
