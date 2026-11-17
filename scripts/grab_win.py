#!/usr/bin/env python3
"""Grab a specific X window by id (or the first Speechnotes window whose
geometry matches a given size), so a popover can be captured on its own.

Usage:
    python3 scripts/grab_win.py <out.png> [wid]
    python3 scripts/grab_win.py <out.png> --popover
"""
import ctypes
import subprocess
import sys

from PIL import Image

x11 = ctypes.CDLL("libX11.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XGetImage.restype = ctypes.c_void_p
x11.XGetImage.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int,
                          ctypes.c_int, ctypes.c_uint, ctypes.c_uint,
                          ctypes.c_ulong, ctypes.c_int]
DISPLAY = x11.XOpenDisplay(b":1")
if not DISPLAY:
    raise SystemExit("no X display :1")


class XImage(ctypes.Structure):
    _fields_ = [("width", ctypes.c_int), ("height", ctypes.c_int),
                ("xoffset", ctypes.c_int), ("format", ctypes.c_int),
                ("data", ctypes.c_void_p), ("byte_order", ctypes.c_int),
                ("bitmap_unit", ctypes.c_int), ("bitmap_bit_order", ctypes.c_int),
                ("bitmap_pad", ctypes.c_int), ("depth", ctypes.c_int),
                ("bytes_per_line", ctypes.c_short), ("bits_per_pixel", ctypes.c_short),
                ("red_mask", ctypes.c_ulong), ("green_mask", ctypes.c_ulong),
                ("blue_mask", ctypes.c_ulong)]


def geometry(wid):
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


def grab(wid):
    g = geometry(wid)
    xi = ctypes.cast(x11.XGetImage(DISPLAY, wid, 0, 0, g["w"], g["h"],
                                   0xFFFFFF, 2),
                     ctypes.POINTER(XImage))
    if not xi:
        raise SystemExit("XGetImage failed")
    n = g["w"] * g["h"] * 4
    buf = (ctypes.c_char * n).from_address(xi.contents.data).raw
    img = Image.frombytes("RGBX", (g["w"], g["h"]), buf).convert("RGB")
    x11.XDestroyImage(ctypes.cast(xi, ctypes.POINTER(XImage)))
    return img


def list_windows():
    out = subprocess.run(["xwininfo", "-root", "-tree"],
                         capture_output=True, text=True).stdout
    wins = []
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("0x") and "speechnotes" in s.lower():
            wid = int(s.split(None, 1)[0], 16)
            g = geometry(wid)
            wins.append((wid, g))
    return wins


if __name__ == "__main__":
    out_path = sys.argv[1]
    wins = list_windows()
    if not wins:
        raise SystemExit("no speechnotes windows")
    if len(sys.argv) > 2 and sys.argv[2] == "--popover":
        # The popover is the child window that is not the main frame and is
        # not 1x1.
        cands = [(w, g) for w, g in wins if g["w"] > 1 and g["h"] > 1]
        if len(cands) < 2:
            raise SystemExit(f"no popover window among {cands}")
        wid, g = cands[-1]
    else:
        wid, g = wins[0]
    print(f"grabbing {hex(wid)} {g['w']}x{g['h']} -> {out_path}")
    grab(wid).save(out_path)
    print("saved")
