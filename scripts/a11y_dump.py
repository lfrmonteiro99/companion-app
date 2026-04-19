#!/usr/bin/env python3
"""
Dump the accessibility tree of the currently focused window as JSON.

Output stdout (one line):
  success: {"app":"teams-for-linux","title":"...","text":"...","nodes":42}
  error:   {"error":"no active window"}

Designed to be called once per tick by the Rust CLI.
"""
import json
import os
import subprocess
import sys
import signal

try:
    import pyatspi
    from pyatspi import Registry, STATE_ACTIVE, STATE_FOCUSED, STATE_SHOWING
except ImportError:
    print(json.dumps({"error": "pyatspi not installed"}))
    sys.exit(0)


def _xprop_topmost_title():
    """
    GNOME Wayland doesn't populate _NET_ACTIVE_WINDOW, but it does maintain
    _NET_CLIENT_LIST_STACKING for XWayland apps (Teams/Chrome/Electron). The
    last id in that list is the topmost X11 window — the one the user is
    currently looking at (for XWayland apps). Returns None for pure Wayland.
    """
    env = dict(os.environ)
    env.setdefault("DISPLAY", ":0")
    try:
        out = subprocess.run(
            ["xprop", "-root", "_NET_CLIENT_LIST_STACKING"],
            capture_output=True, text=True, timeout=1, env=env,
        )
        if out.returncode != 0:
            return None
        ids = [tok.rstrip(",") for tok in out.stdout.split() if tok.startswith("0x")]
        if not ids:
            return None
        topmost = ids[-1]
        out2 = subprocess.run(
            ["xprop", "-id", topmost, "_NET_WM_NAME"],
            capture_output=True, text=True, timeout=1, env=env,
        )
        if out2.returncode != 0:
            return None
        line = out2.stdout.strip()
        if "=" not in line:
            return None
        val = line.split("=", 1)[1].strip()
        if val.startswith('"') and val.endswith('"'):
            return val[1:-1]
        return None
    except Exception:
        return None



# Apps that register title-matching frames but hold no user content.
# mutter-x11-frames draws server-side decorations for XWayland apps, so it
# reports a frame with the same title as Teams/Chrome but with only a
# handful of decoration nodes.
_TITLE_MATCH_BLACKLIST = {
    "mutter-x11-frames",
    "gsd-xsettings",
    "gnome-shell",
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-gnome",
    "ibus-x11",
    "ibus-extension-gtk3",
}


def _find_by_title(title):
    if not title:
        return (None, None)
    desktop = Registry.getDesktop(0)
    # Collect ALL matches then pick the one with most children — the real
    # content app, not the decoration wrapper.
    matches = []
    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
            if app is None:
                continue
            app_name = (app.name or "").strip()
            if app_name in _TITLE_MATCH_BLACKLIST:
                continue
            for j in range(app.childCount):
                try:
                    win = app.getChildAtIndex(j)
                    if win is None:
                        continue
                    wname = (win.name or "").strip()
                    if not wname:
                        continue
                    if wname == title or title in wname or wname in title:
                        matches.append((app_name, win, win.childCount))
                except Exception:
                    continue
        except Exception:
            continue
    if not matches:
        return (None, None)
    # Prefer the match with the largest direct subtree.
    matches.sort(key=lambda t: t[2], reverse=True)
    return (matches[0][0], matches[0][1])


def _has_focused_descendant(node, depth=0, max_depth=6):
    """Cheap recursive check — limited depth to avoid walking entire trees."""
    if depth > max_depth:
        return False
    try:
        st = node.getState()
        if st.contains(STATE_FOCUSED):
            return True
    except Exception:
        pass
    try:
        for i in range(node.childCount):
            c = node.getChildAtIndex(i)
            if c is not None and _has_focused_descendant(c, depth + 1, max_depth):
                return True
    except Exception:
        pass
    return False


def find_active_window():
    """
    Return (app_name, active_window_node) for the focused window.

    Strategy (first hit wins):
      1. Top-level frame with STATE_ACTIVE (Wayland-native GTK, Chrome when
         it chooses to set it).
      2. Match by title from X11 _NET_CLIENT_LIST_STACKING's topmost entry —
         the reliable path for XWayland apps (Teams, Chrome, Electron).
      3. Top-level frame whose subtree has a STATE_FOCUSED descendant —
         desperate fallback for apps that don't set STATE_ACTIVE.
    """
    desktop = Registry.getDesktop(0)

    # Pass 1: STATE_ACTIVE on a window — skip decoration/WM wrappers that
    # report STATE_ACTIVE for XWayland apps they frame (mutter-x11-frames).
    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
            if app is None:
                continue
            app_name = (app.name or "").strip()
            if app_name in _TITLE_MATCH_BLACKLIST:
                continue
            for j in range(app.childCount):
                try:
                    win = app.getChildAtIndex(j)
                    if win is None:
                        continue
                    st = win.getState()
                    if st.contains(STATE_ACTIVE):
                        return (app_name, win)
                except Exception:
                    continue
        except Exception:
            continue

    # Pass 2: match by XWayland topmost window title.
    title = _xprop_topmost_title()
    if title:
        app_name, win = _find_by_title(title)
        if win is not None:
            return (app_name, win)

    # Pass 3: visible frames with a focused descendant.
    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
            if app is None:
                continue
            for j in range(app.childCount):
                try:
                    win = app.getChildAtIndex(j)
                    if win is None:
                        continue
                    try:
                        role = win.getRoleName()
                    except Exception:
                        role = ""
                    if role != "frame":
                        continue
                    st = win.getState()
                    if not st.contains(STATE_SHOWING):
                        continue
                    if _has_focused_descendant(win):
                        return (app.name or "", win)
                except Exception:
                    continue
        except Exception:
            continue

    return (None, None)


def _clean(s):
    # U+FFFC = object replacement character. Teams / Electron webviews use it
    # as a placeholder for every icon/avatar; without filtering, the dump
    # becomes dominated by empty anchors.
    return (s or "").replace("\ufffc", "").strip()


def collect_text(node, depth=0, max_depth=50, out=None):
    if out is None:
        out = []
    if depth > max_depth:
        return out
    try:
        try:
            ti = node.queryText()
            text = _clean(ti.getText(0, ti.characterCount))
            if len(text) > 1:
                out.append(text)
        except Exception:
            pass
        name = _clean(node.name)
        if len(name) > 1 and (not out or out[-1] != name):
            out.append(name)
    except Exception:
        pass
    try:
        for i in range(node.childCount):
            child = node.getChildAtIndex(i)
            if child is not None:
                collect_text(child, depth + 1, max_depth, out)
    except Exception:
        pass
    return out


def main():
    # Hard cap: if AT-SPI hangs, exit after 3 s.
    def _timeout(_signum, _frame):
        print(json.dumps({"error": "timeout"}))
        sys.exit(0)

    signal.signal(signal.SIGALRM, _timeout)
    signal.alarm(3)

    app_name, win = find_active_window()
    if win is None:
        print(json.dumps({"error": "no active window"}))
        return

    try:
        title = win.name or ""
        parts = collect_text(win)
        # De-duplicate while preserving order.
        seen = set()
        unique = []
        for p in parts:
            if p not in seen:
                seen.add(p)
                unique.append(p)
        text = "\n".join(unique)
        print(json.dumps({
            "app": app_name,
            "title": title,
            "text": text,
            "nodes": len(unique),
        }))
    except Exception as e:
        print(json.dumps({"error": str(e)}))


if __name__ == "__main__":
    main()
