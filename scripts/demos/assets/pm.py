#!/usr/bin/env python3
"""pm.py — the plugin manager: one tool, load_plugin.

Loads another plugin into the RUNNING server through the `plugin_load` host-op
(same path as POST /plugin/load, no restart, registry rebuilt live). Answers
"do I really need curl to install a plugin?" with "not once pm is in your
toolbelt".
"""
import json, sys

TOOLS = [{
    "name": "load_plugin",
    "description": "Hot-load a plugin binary into the running kn9t server: handshake, register its tools, rebuild the registry. No restart.",
    "schema": {"type": "object",
               "properties": {"path": {"type": "string", "description": "Path to the plugin executable."}},
               "required": ["path"]},
    "parallel_safe": False,
    "hidden": False,
    "effects": [],
    "policy": {"pattern_field": None, "default_policy": "allow",
               "builtin_allow": [], "builtin_deny": []},
}]

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

pending = {}          # host-api request id -> tool_call id to answer
next_req = [900000]   # our own id space, away from hook ids

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    t = m.get("t")
    if t == "hello":
        send({"t": "hello", "name": "pm", "capabilities": ["tools", "host_api"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        tid = m["id"]
        args = m.get("payload", {}).get("args", {})
        path = args.get("path", "")
        rid = next_req[0]; next_req[0] += 1
        pending[rid] = tid
        send({"t": "request", "id": rid, "op": "plugin_load", "payload": {"cmd": [path]}})
    elif t == "api_result":
        tid = pending.pop(m["id"], None)
        if tid is not None:
            ok = bool(m.get("ok"))
            body = json.dumps(m.get("result")) if ok else "load failed: %s" % m.get("error")
            send({"t": "done", "id": tid, "is_error": not ok,
                  "content": [{"type": "text", "text": body}]})
    elif t == "shutdown":
        sys.exit(0)
