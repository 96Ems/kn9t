#!/usr/bin/env python3
"""triage.py — the bug desk: file, label and track real GitHub issues.

  file_issue   {repo, title, body, label}   gh issue create (a real issue)
  label_issue  {repo, number, label}        gh issue edit --add-label
  list_issues  {repo}                       the tracker's open issues

The first call seats a bug board in the TUI listing the open issues.
"""
import json, os, subprocess, sys, threading, time

TOOLS = [
    {"name": "file_issue",
     "description": "Create a GitHub issue (title, body) and return its URL.",
     "schema": {"type": "object",
                "properties": {"repo": {"type": "string"}, "title": {"type": "string"},
                               "body": {"type": "string"}, "label": {"type": "string"}},
                "required": ["repo", "title", "body"]},
     "parallel_safe": False, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "label_issue",
     "description": "Add a label to an existing issue.",
     "schema": {"type": "object",
                "properties": {"repo": {"type": "string"}, "number": {"type": "integer"},
                               "label": {"type": "string"}},
                "required": ["repo", "number", "label"]},
     "parallel_safe": False, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "list_issues",
     "description": "List the tracker's open issues.",
     "schema": {"type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
]

LUA = r"""
kn9t.on_key("r", function() kn9t.notify({event = "refresh"}); return true end)
kn9t.on_key("j", function() return true end)
function render(state)
  local rows = {"  BUG BOARD ▸ " .. ((state and state.repo) or "…") .. " · r refresh"}
  for _, l in ipairs((state and state.issues) or {}) do
    table.insert(rows, "  " .. l)
  end
  return {type = "text", content = table.concat(rows, "\n")}
end
"""

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def gh(args, home=None):
    env = {**os.environ}
    if home or os.environ.get("GH_HOME"):
        env["HOME"] = home or os.environ["GH_HOME"]
    out = subprocess.run(["gh"] + args, capture_output=True, text=True, timeout=25, env=env)
    return out.returncode == 0, out.stdout.strip() or out.stderr.strip()

state = {"sid": "", "repo": "", "issues": []}
board_on = False
lock = threading.Lock()

def issue_lines(repo):
    ok, out = gh(["issue", "list", "--repo", repo, "--state", "open", "--limit", "10",
                  "--json", "number,title,labels"])
    if not ok:
        return []
    try:
        return ["#%-5d [%s] %s" % (i["number"],
                                   ",".join(l.get("name", "") for l in i.get("labels", [])) or "-",
                                   i["title"][:52]) for i in json.loads(out)]
    except Exception:
        return []

def push_board():
    with lock:
        payload = {"session": state["sid"], "state": {"repo": state["repo"], "issues": state["issues"]}}
    send({"t": "request", "id": 0, "op": "ui_set_state", "payload": payload})

def seat_board(sid, repo):
    global board_on
    with lock:
        state["sid"] = sid
        state["repo"] = repo
    if not board_on:
        board_on = True
        send({"t": "request", "id": 0, "op": "ui_register_lua",
              "payload": {"session": sid, "source": LUA, "placement": "bottom"}})
    with lock:
        state["issues"] = issue_lines(repo)
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
        send({"t": "hello", "name": "triage", "capabilities": ["tools", "host_api", "ui_interaction"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        p = m.get("payload", {})
        name = p.get("tool", "")
        args = p.get("args", {})
        repo = args.get("repo", "96Ems/kn9t-plugins")
        if name == "file_issue":
            ok, out = gh(["issue", "create", "--repo", repo,
                          "--title", args.get("title", "untitled"),
                          "--body", args.get("body", ""),
                          "--label", args.get("label", "bug")])
            done(m["id"], out if ok else "gh failed: " + out, err=not ok)
            seat_board(p.get("session", ""), repo)
        elif name == "label_issue":
            ok, out = gh(["issue", "edit", str(args.get("number", 0)), "--repo", repo,
                          "--add-label", args.get("label", "bug")])
            done(m["id"], "labeled #%s -> %s" % (args.get("number"), args.get("label")) if ok
                 else "gh failed: " + out, err=not ok)
        elif name == "list_issues":
            seat_board(p.get("session", ""), repo)
            with lock:
                lines = list(state["issues"])
            done(m["id"], "\n".join(lines) or "no open issues")
        else:
            done(m["id"], "unknown tool %s" % name, err=True)
    elif t == "notify_plugin":
        if m.get("event") == "refresh":
            with lock:
                repo = state["repo"]
            if repo:
                with lock:
                    state["issues"] = issue_lines(repo)
                push_board()
    elif t == "shutdown":
        sys.exit(0)
