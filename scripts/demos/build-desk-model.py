#!/usr/bin/env python3
"""build-desk-model.py — assemble github-desk.model.json from assets/ghpanel.py.

Usage: DEMO_WORK=/tmp/opencode/demo-home/work python3 build-desk-model.py > github-desk.model.json
"""
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.environ.get("DEMO_WORK", "/tmp/opencode/demo-home/work")
REAL_HOME = os.environ.get("HOME", "/home/emericclement")   # gh keeps its login there

def src(name):
    with open(os.path.join(HERE, "assets", name)) as f:
        return f.read()

def usage(inp, out):
    return {"input": inp, "output": out}

def load_bash():
    payload = {"cmd": [os.path.join(WORK, "ghpanel.py")],
               "env": {"GH_HOME": REAL_HOME}}
    body = json.dumps(payload).replace('"', '\\"')
    return ("chmod +x ghpanel.py && P=$(cat $KN9T_HOME/port) && T=$(cat $KN9T_HOME/token) && "
            "curl -sS -X POST http://127.0.0.1:$P/plugin/load -H \"Authorization: Bearer $T\" "
            "-H 'Content-Type: application/json' -d \"" + body + "\"")

script = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {
            "thinking": ["a GitHub desk = three tools plus a live PR board in the UI — ",
                         "write it, load it with my gh login, then use it on real repos."],
            "text": "One plugin, the whole desk: PR and issue tools, plus a live PR board at the bottom of the UI. ",
            "tools": [{"call_id": "w_gh", "name": "write",
                       "args": {"path": os.path.join(WORK, "ghpanel.py"), "content": src("ghpanel.py")}}],
            "stop": "TOOL_CALL",
            "usage": usage(420, 940),
        },
        {
            "text": "Loading it into the running server (with my gh session so private repos show too) — the board seats itself on the first query: ",
            "tools": [{"call_id": "b_load", "name": "bash", "args": {"cmd": load_bash()}}],
            "stop": "TOOL_CALL",
            "usage": usage(1620, 150),
        },
        {
            "text": "Live. What's landing upstream in ratatui — the library drawing this very TUI: ",
            "tools": [{"call_id": "t_prs", "name": "list_prs", "args": {"repo": "ratatui/ratatui"}}],
            "stop": "TOOL_CALL",
            "usage": usage(2050, 240),
        },
        {
            "text": "Now our own shop — and then the pick of the week: ",
            "tools": [{"call_id": "t_mine", "name": "list_prs", "args": {"repo": "96Ems/kn9t-plugins"}},
                      {"call_id": "t_iss", "name": "list_issues", "args": {"repo": "96Ems/kn9t-plugins"}}],
            "stop": "TOOL_CALL",
            "usage": usage(2530, 210),
        },
        {
            "text": "Nothing in flight on our side — clean desk. Picking a contribution upstream: ",
            "tools": [{"call_id": "t_pick", "name": "pick_issue", "args": {"repo": "ratatui/ratatui"}}],
            "stop": "TOOL_CALL",
            "usage": usage(2870, 140),
        },
        {
            "text": "The board at the bottom is live: j/k walks the PRs, r refreshes. Desk is open — review, pick, or ship.",
            "stop": "STOP",
            "usage": usage(3090, 74),
        },
    ],
}

json.dump(script, sys.stdout, indent=1)
