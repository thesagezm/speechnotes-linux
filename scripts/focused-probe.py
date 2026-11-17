#!/usr/bin/env python3
"""Walk the live app's accessibility tree and report every FOCUSED node.

GTK4 dispatches a key event along the *focused widget's* ancestor chain,
and a window with no focused widget dispatches no key event at all — so
"is anything focused?" is the question that decides whether the reader's
keyboard works (a reader keyboard bug report, 2026-11-02). Before the fix this printed
"FOCUSED nodes: []" on the live app.

Usage: python3 scripts/focused-probe.py
"""
import sys

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402


def app_node():
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(i)
        if child is None:
            continue
        try:
            name = child.get_name()
        except Exception:
            continue
        if name == "speechnotes":
            return child
    raise SystemExit("the app is not in the tree")


def walk(node, depth=0, out=None, path=()):
    if out is None:
        out = []
    try:
        role = node.get_role_name()
        name = node.get_name()
        here = path + ((role, name),)
        focused = node.get_state_set().contains(Atspi.StateType.FOCUSED)
        if focused:
            ext = node.get_extents(Atspi.CoordType.WINDOW)
            out.append((here, (ext.x, ext.y, ext.width, ext.height)))
        for i in range(node.get_child_count()):
            child = node.get_child_at_index(i)
            if child is not None:
                walk(child, depth + 1, out, here)
    except Exception:
        pass
    return out


def main():
    app = app_node()
    found = walk(app)
    print(f"FOCUSED nodes: {len(found)}")
    for path, ext in found:
        print(f"  @{ext}")
        for role, name in path:
            print(f"    {role} {name!r}")
    return 0 if found else 1


if __name__ == "__main__":
    sys.exit(main())
