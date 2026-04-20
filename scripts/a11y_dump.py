#!/usr/bin/env python3
"""
Dump the accessibility tree of the currently focused window as JSON.

Output stdout (one line):
  success: {"app":"teams-for-linux","title":"...","text":"...","nodes":42}
  error:   {"error":"no active window"}

Designed to be called once per tick by the Rust CLI.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
from typing import Any, List, Optional, Tuple

try:
    import pyatspi  # type: ignore[import-not-found]
    from pyatspi import (  # type: ignore[import-not-found]
        Registry,
        STATE_ACTIVE,
        STATE_FOCUSED,
        STATE_SHOWING,
    )
except ImportError:
    print(json.dumps({"error": "pyatspi not installed"}))
    sys.exit(0)


def _window_bbox(win: Any) -> Optional[Tuple[int, int, int, int]]:
    """Return (x, y, width, height) of the window in screen coordinates, or
    None if the Component interface / extents aren't exposed."""
    try:
        comp = win.queryComponent()
    except (NotImplementedError, RuntimeError, AttributeError):
        return None
    try:
        ext = comp.getExtents(pyatspi.DESKTOP_COORDS)  # type: ignore[attr-defined]
    except (NotImplementedError, RuntimeError, AttributeError):
        return None
    try:
        x, y, w, h = int(ext.x), int(ext.y), int(ext.width), int(ext.height)
    except (AttributeError, TypeError):
        return None
    if w <= 0 or h <= 0:
        return None
    return (x, y, w, h)


def _xprop_topmost_title() -> Optional[str]:
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
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return None
    if out.returncode != 0 or not out.stdout:
        return None
    ids = [tok.rstrip(",") for tok in out.stdout.split() if tok.startswith("0x")]
    if not ids:
        return None
    topmost = ids[-1]
    try:
        out2 = subprocess.run(
            ["xprop", "-id", topmost, "_NET_WM_NAME"],
            capture_output=True, text=True, timeout=1, env=env,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return None
    if out2.returncode != 0 or not out2.stdout:
        return None
    line = out2.stdout.strip()
    if "=" not in line:
        return None
    val = line.split("=", 1)[1].strip()
    if val.startswith('"') and val.endswith('"'):
        return val[1:-1]
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


def _find_by_title(title: str) -> Tuple[Optional[str], Any]:
    if not title:
        return (None, None)
    desktop = Registry.getDesktop(0)
    matches: List[Tuple[str, Any, int]] = []
    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
        except (RuntimeError, AttributeError):
            continue
        if app is None:
            continue
        app_name = (app.name or "").strip()
        if app_name in _TITLE_MATCH_BLACKLIST:
            continue
        for j in range(app.childCount):
            try:
                win = app.getChildAtIndex(j)
            except (RuntimeError, AttributeError):
                continue
            if win is None:
                continue
            wname = (win.name or "").strip()
            if not wname:
                continue
            if wname == title or title in wname or wname in title:
                matches.append((app_name, win, win.childCount))
    if not matches:
        return (None, None)
    # Prefer the match with the largest direct subtree.
    matches.sort(key=lambda t: t[2], reverse=True)
    return (matches[0][0], matches[0][1])


def _has_focused_descendant(node: Any, depth: int = 0, max_depth: int = 6) -> bool:
    """Cheap recursive check — limited depth to avoid walking entire trees."""
    if depth > max_depth:
        return False
    try:
        if node.getState().contains(STATE_FOCUSED):
            return True
    except (RuntimeError, AttributeError):
        pass
    try:
        child_count = node.childCount
    except (RuntimeError, AttributeError):
        return False
    for i in range(child_count):
        try:
            c = node.getChildAtIndex(i)
        except (RuntimeError, AttributeError):
            continue
        if c is not None and _has_focused_descendant(c, depth + 1, max_depth):
            return True
    return False


def find_active_window() -> Tuple[Optional[str], Any]:
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

    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
        except (RuntimeError, AttributeError):
            continue
        if app is None:
            continue
        app_name = (app.name or "").strip()
        if app_name in _TITLE_MATCH_BLACKLIST:
            continue
        for j in range(app.childCount):
            try:
                win = app.getChildAtIndex(j)
            except (RuntimeError, AttributeError):
                continue
            if win is None:
                continue
            try:
                if win.getState().contains(STATE_ACTIVE):
                    return (app_name, win)
            except (RuntimeError, AttributeError):
                continue

    title = _xprop_topmost_title()
    if title:
        app_name_opt, win = _find_by_title(title)
        if win is not None:
            return (app_name_opt, win)

    for i in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(i)
        except (RuntimeError, AttributeError):
            continue
        if app is None:
            continue
        for j in range(app.childCount):
            try:
                win = app.getChildAtIndex(j)
            except (RuntimeError, AttributeError):
                continue
            if win is None:
                continue
            try:
                role = win.getRoleName()
            except (RuntimeError, AttributeError):
                role = ""
            if role != "frame":
                continue
            try:
                st = win.getState()
            except (RuntimeError, AttributeError):
                continue
            if not st.contains(STATE_SHOWING):
                continue
            if _has_focused_descendant(win):
                return (app.name or "", win)

    return (None, None)


def _clean(s: Optional[str]) -> str:
    # U+FFFC = object replacement character. Teams / Electron webviews use it
    # as a placeholder for every icon/avatar.
    return (s or "").replace("\ufffc", "").strip()


def collect_text(
    node: Any,
    depth: int = 0,
    max_depth: int = 50,
    out: Optional[List[str]] = None,
) -> List[str]:
    if out is None:
        out = []
    if depth > max_depth:
        return out
    try:
        ti = node.queryText()
        text = _clean(ti.getText(0, ti.characterCount))
        if len(text) > 1:
            out.append(text)
    except (RuntimeError, AttributeError, NotImplementedError):
        pass
    try:
        name = _clean(node.name)
        if len(name) > 1 and (not out or out[-1] != name):
            out.append(name)
    except (RuntimeError, AttributeError):
        pass
    try:
        child_count = node.childCount
    except (RuntimeError, AttributeError):
        return out
    for i in range(child_count):
        try:
            child = node.getChildAtIndex(i)
        except (RuntimeError, AttributeError):
            continue
        if child is not None:
            collect_text(child, depth + 1, max_depth, out)
    return out


def main() -> None:
    def _timeout(_signum: int, _frame: Any) -> None:
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
        bbox = _window_bbox(win)
        parts = collect_text(win)
        seen: set[str] = set()
        unique: List[str] = []
        for p in parts:
            if p not in seen:
                seen.add(p)
                unique.append(p)
        text = "\n".join(unique)
        payload: dict[str, Any] = {
            "app": app_name,
            "title": title,
            "text": text,
            "nodes": len(unique),
        }
        if bbox is not None:
            payload["bbox"] = list(bbox)
        # When the subtree is thin (e.g. VS Code Monaco editor, most Electron
        # apps whose inner content is rendered to a canvas), the caller can
        # still use `bbox` to crop the screenshot before running OCR. Mark
        # the thin state explicitly so the Rust side knows to fall through
        # to OCR but keep the bbox.
        payload["thin"] = len(unique) == 0 or len(text) < 40
        print(json.dumps(payload))
    except (RuntimeError, AttributeError, OSError) as e:
        print(json.dumps({"error": str(e)}))


if __name__ == "__main__":
    main()
