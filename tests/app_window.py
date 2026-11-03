#!/usr/bin/env python3
"""The real app, a real window manager and real mouse input: does the title bar work?

The other tests can't tell. A browser has no window to move, and with no window manager
nothing moves or maximizes at all. This one found what only shows with both: a window
that takes hold of the mouse on the press never hears the release, so a double click
never arrives and a quick click can leave the window stuck to the pointer.

    cargo build --manifest-path app/src-tauri/Cargo.toml
    python3 tests/app_window.py [screenshot dir]

Linux only, X11 only. Needs Xvfb, openbox, xdotool, wmctrl and xprop (and ImageMagick's
import for screenshots). Runs twice: with the buttons at the right, as a desktop it
knows nothing about gets them, and at the left, from a KDE setting that says so.
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "app", "src-tauri", "target", "debug", "fatstick")
OUT = sys.argv[1] if len(sys.argv) > 1 else None
DISPLAY = ":77"
FAILS = 0


def check(name, ok, extra=""):
    global FAILS
    if not ok:
        FAILS += 1
    print(f"{'PASS' if ok else 'FAIL'} {name}{f'  ({extra})' if extra else ''}", flush=True)


def run(desktop, button):
    """button(window x, window y, window width, name) gives a button's middle."""
    work = tempfile.mkdtemp(prefix="fatstick-window-")
    env = dict(os.environ, HOME=work, XDG_DATA_HOME=f"{work}/d", XDG_CONFIG_HOME=f"{work}/c", XDG_CACHE_HOME=f"{work}/k",
               FATSTICK_DATA_DIR=f"{work}/data", FATSTICK_CACHE_DIR=f"{work}/cache", FATSTICK_NULL_AUDIO="1",
               DISPLAY=DISPLAY, GDK_BACKEND="x11", WEBKIT_DISABLE_DMABUF_RENDERER="1", WEBKIT_DISABLE_COMPOSITING_MODE="1",
               LIBGL_ALWAYS_SOFTWARE="1", NO_AT_BRIDGE="1")
    env.pop("XDG_CURRENT_DESKTOP", None)
    env.pop("WAYLAND_DISPLAY", None)
    if desktop:
        env["XDG_CURRENT_DESKTOP"] = desktop
        os.makedirs(f"{work}/c", exist_ok=True)
        with open(f"{work}/c/kwinrc", "w") as f:
            f.write("[org.kde.kdecoration2]\nButtonsOnLeft=XIA\nButtonsOnRight=HS\n")

    def sh(*a):
        return subprocess.run(a, env=env, capture_output=True, text=True, timeout=20).stdout.strip()

    xvfb = subprocess.Popen(["Xvfb", DISPLAY, "-screen", "0", "1500x1000x24"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    wm = subprocess.Popen(["openbox"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    app = subprocess.Popen([APP], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        win = ""
        for _ in range(60):
            time.sleep(0.5)
            win = sh("xdotool", "search", "--onlyvisible", "--name", "^Fatstick$").split("\n")[0]
            if win:
                break
        check("the window appears", bool(win))
        if not win:
            return
        time.sleep(5)

        def geo():
            m = re.search(r"X=(-?\d+)\nY=(-?\d+)\nWIDTH=(\d+)\nHEIGHT=(\d+)", sh("xdotool", "getwindowgeometry", "--shell", win))
            return tuple(int(v) for v in m.groups())

        def state():
            return sh("xprop", "-id", win, "_NET_WM_STATE")

        def shot(name):
            if OUT:
                sh("import", "-window", "root", os.path.join(OUT, f"window-{desktop or 'other'}-{name}.png"))

        def press_on(name):
            x, y, w, _ = geo()
            bx, by = button(x, y, w, name)
            sh("xdotool", "mousemove", str(bx), str(by))
            time.sleep(0.25)
            sh("xdotool", "click", "1")

        def drag(px, py, dx, dy, steps=8):
            sh("xdotool", "mousemove", str(px), str(py))
            time.sleep(0.3)
            sh("xdotool", "mousedown", "1")
            time.sleep(0.3)
            for i in range(1, steps + 1):
                sh("xdotool", "mousemove", str(px + dx * i // steps), str(py + dy * i // steps))
                time.sleep(0.06)
            sh("xdotool", "mouseup", "1")
            time.sleep(0.9)

        props = sh("xprop", "-id", win, "_NET_FRAME_EXTENTS")
        check("the system draws no title bar", "= 0, 0, 0, 0" in props, props)
        hints = sh("xprop", "-id", win, "WM_NORMAL_HINTS")
        check("the smallest window is 860 wide", re.search(r"minimum size: 860 by", hints), hints.replace("\n", " ")[:90])
        shot("start")

        x, y, w, h = geo()
        drag(x + w - 300, y + 34, -96, 56)
        x2, y2, w2, h2 = geo()
        check("dragging the bar moves the window", (x2, y2) != (x, y) and (w2, h2) == (w, h), f"{x},{y} to {x2},{y2}")
        x, y, w, h = x2, y2, w2, h2
        drag(x + 500, y + 34, 50, 30)
        check("so does the readout", geo()[:2] != (x, y), f"{geo()[:2]}")
        x, y, w, h = geo()
        drag(x + 90, y + 50, 40, 20)
        check("and the logo", geo()[:2] != (x, y), f"{geo()[:2]}")

        press_on("maximize")
        time.sleep(1.2)
        check("the maximize button maximizes it", "MAXIMIZED" in state(), state())
        _, _, gw, gh = geo()
        check("and it fills the screen", gw >= 1490 and gh >= 980, f"{gw}x{gh}")
        shot("maximized")
        press_on("maximize")
        time.sleep(1.2)
        check("the same button restores it", "MAXIMIZED" not in state(), state())

        x, y, w, h = geo()
        sh("xdotool", "mousemove", str(x + w - 300), str(y + 34))
        time.sleep(0.3)
        sh("xdotool", "click", "--repeat", "2", "--delay", "90", "1")
        time.sleep(1.4)
        check("double-clicking the bar maximizes it", "MAXIMIZED" in state(), state())
        x, y, w, h = geo()
        sh("xdotool", "mousemove", str(x + w - 300), str(y + 34))
        time.sleep(0.3)
        sh("xdotool", "click", "--repeat", "2", "--delay", "90", "1")
        time.sleep(1.4)
        check("and again restores it", "MAXIMIZED" not in state(), state())

        # A quick click on the bar mustn't leave the window following the pointer.
        x, y, w, h = geo()
        sh("xdotool", "mousemove", str(x + w - 300), str(y + 34))
        time.sleep(0.3)
        sh("xdotool", "click", "1")
        time.sleep(0.4)
        sh("xdotool", "mousemove", str(x + w - 200), str(y + 200))
        time.sleep(0.6)
        check("a click on the bar doesn't move it", geo()[:2] == (x, y), f"{(x, y)} to {geo()[:2]}")

        x, y, w, h = geo()
        drag(x + w - 2, y + h // 2, -90, 0, steps=6)
        check("dragging the right edge resizes it", geo()[2] < w and geo()[3] == h, f"{w} to {geo()[2]}")
        x, y, w, h = geo()
        drag(x + w // 2, y + h - 2, 0, -72, steps=6)
        check("and the bottom edge", geo()[3] < h, f"{h} to {geo()[3]}")
        shot("resized")

        press_on("minimize")
        time.sleep(1.0)
        check("the minimize button minimizes it", "HIDDEN" in state(), state())
        sh("wmctrl", "-i", "-a", win)
        time.sleep(1.0)
        check("and it comes back", "HIDDEN" not in state(), state())

        press_on("close")
        for _ in range(40):
            if app.poll() is not None:
                break
            time.sleep(0.25)
        check("the close button closes the app", app.poll() == 0, f"exit {app.poll()}")
    finally:
        if app.poll() is None:
            app.terminate()
        wm.terminate()
        xvfb.terminate()
        time.sleep(0.5)
        shutil.rmtree(work, ignore_errors=True)


# Their middles: 22 wide, 2 apart, 4 from the corner and 1 from the top.
AT_RIGHT = {"minimize": -63, "maximize": -39, "close": -15}
AT_LEFT = {"close": 15, "minimize": 39, "maximize": 63}

print("--- a desktop with no setting of its own: at the right")
run(None, lambda x, y, w, name: (x + w + AT_RIGHT[name], y + 12))
print("--- KDE, set to have them at the left")
run("KDE", lambda x, y, w, name: (x + AT_LEFT[name], y + 12))
print(f"{FAILS} FAILED" if FAILS else "ALL PASSED")
sys.exit(1 if FAILS else 0)
