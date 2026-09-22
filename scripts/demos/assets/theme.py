#!/usr/bin/env python3
"""theme.py — recolor the TUI LIVE by writing a Lua override.

`recolor cyan` drops ~/.kn9t/tui/89_brand.lua; the TUI's file watcher hot-reloads
the whole Lua layout and the accent changes on screen — no restart, no prompt.
`uncolor` deletes the file and the default palette comes back the same way.
"""
import json, os, sys

BRAND = os.path.join(os.environ.get("HOME", os.path.expanduser("~")), ".kn9t", "tui", "89_brand.lua")

TOOLS = [{
    "name": "recolor",
    "description": "Recolor the kn9t TUI accent live (writes a Lua theme override that hot-reloads).",
    "schema": {"type": "object",
               "properties": {"color": {"type": "string", "description": "A Lua color: cyan, yellow, magenta, green, blue, red…"}},
               "required": ["color"]},
    "parallel_safe": False, "hidden": False, "effects": [],
    "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []},
}, {
    "name": "uncolor",
    "description": "Remove the live theme override — the default palette hot-reloads back.",
    "schema": {"type": "object", "properties": {}, "required": []},
    "parallel_safe": False, "hidden": False, "effects": [],
    "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []},
}]

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def done(tid, text, err=False):
    send({"t": "done", "id": tid, "is_error": err,
          "content": [{"type": "text", "text": text}]})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    t = m.get("t")
    if t == "hello":
        send({"t": "hello", "name": "theme", "capabilities": ["tools"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        payload = m.get("payload", {})
        name = payload.get("tool", "")
        args = payload.get("args", {})
        if name == "recolor":
            color = args.get("color", "cyan")
            os.makedirs(os.path.dirname(BRAND), exist_ok=True)
            with open(BRAND, "w") as f:
                f.write("-- theme.py: live accent override (hot-reloaded by the TUI watcher)\n")
                f.write('TUI.color.accent = "%s"\n' % color)
            done(m["id"], "accent -> %s (89_brand.lua written, watcher reloads the UI now)" % color)
        elif name == "uncolor":
            if os.path.exists(BRAND):
                os.remove(BRAND)
            done(m["id"], "theme override removed — default palette back")
        else:
            done(m["id"], "unknown tool %s" % name, err=True)
    elif t == "shutdown":
        sys.exit(0)
