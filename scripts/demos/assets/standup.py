#!/usr/bin/env python3
"""standup.py — the daily standup: repo state as a tool and as a live panel.

  standup    {repo}   branch, dirty tree, today's/this week's commits, PRs
  find_todos {repo}   TODO/FIXME/HACK hits with file:line

The first call also seats a standup board in the TUI (plugin UI), r refreshes.
"""
import json, os, subprocess, sys, threading, time

TOOLS = [
    {"name": "standup",
     "description": "Daily brief for a repo: branch, dirty files, recent commits, open PRs.",
     "schema": {"type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "find_todos",
     "description": "Find TODO/FIXME/HACK markers in a repo's source, with file:line.",
     "schema": {"type": "object",
                "properties": {"repo": {"type": "string"},
                               "pattern": {"type": "string", "description": "Regex (default TODO|FIXME|HACK)"}},
                "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
]

LUA = r"""
-- standup board: the brief comes from the process; r refreshes
kn9t.on_key("r", function() kn9t.notify({event = "refresh"}); return true end)
function render(state)
  local rows = {"  STANDUP ▸ " .. ((state and state.repo) or "…") .. " · r refresh"}
  for _, l in ipairs((state and state.brief) or {}) do
    table.insert(rows, "  " .. l)
  end
  return {type = "text", content = table.concat(rows, "\n")}
end
"""

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def sh(cmd):
    home = os.environ.get("GH_HOME") or os.environ.get("HOME")
    out = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=20,
                         env={**os.environ, "HOME": home})
    return out.stdout.strip()

def brief_for(repo):
    branch = sh("git -C %s rev-parse --abbrev-ref HEAD 2>/dev/null" % repo) or "?"
    dirty = sh("git -C %s status --porcelain 2>/dev/null | head -8" % repo).splitlines()
    today = sh("git -C %s log --oneline --since=midnight 2>/dev/null | wc -l" % repo) or "0"
    week = sh("git -C %s log --oneline --since=1.week 2>/dev/null | wc -l" % repo) or "0"
    brief = ["branch   %s · %s commit(s) today · %s this week" % (branch, today, week)]
    if dirty:
        brief.append("dirty    %d path(s) — %s" % (len(dirty), " ".join(d.strip(" M?")[:18] for d in dirty[:3])))
    else:
        brief.append("clean    tree, nothing pending")
    prs = sh("gh pr list --limit 4 --json number,title 2>/dev/null")
    try:
        for p in json.loads(prs or "[]"):
            brief.append("pr       #%d %s" % (p["number"], p["title"][:44]))
    except Exception:
        pass
    return brief

state = {"sid": "", "repo": "", "brief": []}
board_on = False
lock = threading.Lock()

def push_board():
    with lock:
        payload = {"session": state["sid"], "state": {"repo": state["repo"], "brief": state["brief"]}}
    send({"t": "request", "id": 0, "op": "ui_set_state", "payload": payload})

def seat_board(sid, repo):
    global board_on
    with lock:
        state["sid"] = sid
        state["repo"] = repo
        state["brief"] = brief_for(repo)
    if not board_on:
        board_on = True
        send({"t": "request", "id": 0, "op": "ui_register_lua",
              "payload": {"session": sid, "source": LUA, "placement": "bottom"}})
    push_board()

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
        send({"t": "hello", "name": "standup", "capabilities": ["tools", "host_api", "ui_interaction"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        p = m.get("payload", {})
        name = p.get("tool", "")
        args = p.get("args", {})
        repo = args.get("repo", ".")
        if name == "standup":
            seat_board(p.get("session", ""), repo)
            with lock:
                brief = list(state["brief"])
            done(m["id"], "\n".join(brief))
        elif name == "find_todos":
            pat = args.get("pattern", "TODO|FIXME|HACK")
            seat_board(p.get("session", ""), repo)
            hits = sh("grep -rnE '%s' --include='*.rs' %s 2>/dev/null | head -12" % (pat, repo))
            done(m["id"], hits or "no %s markers — disciplined codebase" % pat)
        else:
            done(m["id"], "unknown tool %s" % name, err=True)
    elif t == "notify_plugin":
        if m.get("event") == "refresh":
            with lock:
                repo = state["repo"]
            if repo:
                with lock:
                    state["brief"] = brief_for(repo)
                push_board()
    elif t == "shutdown":
        sys.exit(0)
