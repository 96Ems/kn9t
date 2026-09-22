#!/usr/bin/env python3
"""ghpanel.py — the GitHub desk: PRs and issues as tools AND as a live TUI board.

Tools:
  list_prs {repo}     open pull requests (number, title, author, +N/-M, age)
  list_issues {repo}  open issues (number, title, labels, age)
  pick_issue {repo}   pick the best issue to take: a good-first-issue if any,
                      otherwise the oldest unassigned one

The first tool call also seats a live PR board in the TUI (plugin UI via
ui_register_lua): it refreshes on its own clock, `j/k` moves the selection and
`r` forces a refresh. Uses the `gh` CLI (already signed in), curl as fallback.
"""
import json, os, subprocess, sys, threading, time

TOOLS = [
    {"name": "list_prs",
     "description": "List open pull requests of a GitHub repo (owner/name).",
     "schema": {"type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "list_issues",
     "description": "List open issues of a GitHub repo (owner/name).",
     "schema": {"type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "pick_issue",
     "description": "Pick the best open issue to work on: good-first-issue first, else oldest unassigned.",
     "schema": {"type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
]

LUA = r"""
-- ghpanel board: rows come from the host process (state.prs); j/k move, r refreshes
local sel = 1
local function move(n)
  return function()
    local total = #(STATE and STATE.prs or {})
    sel = math.max(1, math.min(total, sel + n))
    return true
  end
end
kn9t.on_key("j", move(1)); kn9t.on_key("Down", move(1))
kn9t.on_key("k", move(-1)); kn9t.on_key("Up", move(-1))
kn9t.on_key("r", function() kn9t.notify({event = "refresh"}); return true end)

function render(state)
  STATE = state
  local rows = {}
  local prs = (state and state.prs) or {}
  table.insert(rows, "  PULL REQUESTS ▸ " .. ((state and state.repo) or "…") .. " · j/k move · r refresh")
  if #prs == 0 then
    table.insert(rows, "  (no open PRs)")
  end
  for i, p in ipairs(prs) do
    local mark = (i == sel) and "▌ " or "  "
    local line = string.format("%s#%-5d +%-4d -%-4d @%-12s %s", mark, p.number, p.additions or 0,
                               p.deletions or 0, p.author or "?", (p.title or ""):sub(1, 60))
    table.insert(rows, line)
  end
  return {type = "text", content = table.concat(rows, "\n")}
end
"""

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def gh_json(args):
    """gh first (authed — GH_HOME points at the real login), curl fallback for
    public repos. Returns parsed JSON or None."""
    home = os.environ.get("GH_HOME") or os.environ.get("HOME")
    try:
        out = subprocess.run(["gh"] + args, capture_output=True, text=True, timeout=20,
                             env={**os.environ, "HOME": home})
        if out.returncode == 0 and out.stdout.strip():
            return json.loads(out.stdout)
    except Exception:
        pass
    # curl fallback: the public API (unauthenticated, fine for public repos)
    try:
        if args[:2] == ["pr", "list"]:
            repo = args[args.index("--repo") + 1]
            url = "https://api.github.com/repos/%s/pulls?state=open&per_page=10" % repo
        elif args[:2] == ["issue", "list"]:
            repo = args[args.index("--repo") + 1]
            url = "https://api.github.com/repos/%s/issues?state=open&per_page=30" % repo
        else:
            return None
        out = subprocess.run(["curl", "-sf", "-m", "15", url], capture_output=True, text=True)
        if out.returncode != 0 or not out.stdout.strip():
            return None
        data = json.loads(out.stdout)
        if args[:2] == ["issue", "list"]:
            data = [i for i in data if "pull_request" not in i]   # the issues API returns PRs too
        return data
    except Exception:
        return None

def pr_rows(repo):
    data = gh_json(["pr", "list", "--repo", repo, "--state", "open", "--limit", "10",
                    "--json", "number,title,author,additions,deletions,updatedAt"])
    if data is None:
        return []
    rows = []
    for p in data:
        rows.append({"number": p.get("number"), "title": p.get("title"),
                     "author": ((p.get("author") or p.get("user") or {}).get("login", "?")),
                     "additions": p.get("additions", 0), "deletions": p.get("deletions", 0),
                     "updated": p.get("updatedAt", "")})
    return rows

def fmt_prs(repo, prs):
    if not prs:
        return "no open PRs on %s" % repo
    return "\n".join("#%-5d +%-5d -%-5d  @%-12s  %s" %
                     (p["number"], p["additions"], p["deletions"], p["author"], p["title"][:64])
                     for p in prs)

def fmt_issues(issues):
    if not issues:
        return "no open issues"
    return "\n".join("#%-5d  [%s]  %s" %
                     (i["number"], ",".join(i.get("labels", [])) or "-", i["title"][:64]) for i in issues)

state = {"sid": "", "repo": "", "prs": []}
board_on = False
lock = threading.Lock()

def push_board():
    with lock:
        payload = {"session": state["sid"], "state": {"repo": state["repo"], "prs": state["prs"]}}
    send({"t": "request", "id": 0, "op": "ui_set_state", "payload": payload})

def seat_board(sid, repo):
    global board_on
    with lock:
        state["sid"] = sid
        state["repo"] = repo or "ratatui/ratatui"
        state["prs"] = pr_rows(state["repo"])
    if not board_on:
        board_on = True
        send({"t": "request", "id": 0, "op": "ui_register_lua",
              "payload": {"session": sid, "source": LUA, "placement": "bottom"}})
        threading.Thread(target=heartbeat, daemon=True).start()
    push_board()

def heartbeat():
    while True:
        time.sleep(6)
        with lock:
            repo = state["repo"]
        if repo:
            with lock:
                state["prs"] = pr_rows(repo)
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
        send({"t": "hello", "name": "ghpanel", "capabilities": ["tools", "host_api", "ui_interaction"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        p = m.get("payload", {})
        sid = p.get("session", "")
        name = p.get("tool", "")
        args = p.get("args", {})
        repo = args.get("repo", "ratatui/ratatui")
        if name == "list_prs":
            seat_board(sid, repo)
            with lock:
                prs = state["prs"] if state["repo"] == repo else pr_rows(repo)
            done(m["id"], fmt_prs(repo, prs))
        elif name == "list_issues":
            seat_board(sid, repo)
            data = gh_json(["issue", "list", "--repo", repo, "--state", "open", "--limit", "10",
                            "--json", "number,title,labels"]) or []
            issues = [{"number": i.get("number"), "title": i.get("title"),
                       "labels": [l.get("name") if isinstance(l, dict) else l for l in i.get("labels", [])]}
                      for i in data]
            done(m["id"], fmt_issues(issues))
        elif name == "pick_issue":
            seat_board(sid, repo)
            data = gh_json(["issue", "list", "--repo", repo, "--state", "open", "--limit", "30",
                            "--json", "number,title,labels,assignees"]) or []
            issues = [{"number": i.get("number"), "title": i.get("title"),
                       "labels": [l.get("name") if isinstance(l, dict) else l for l in i.get("labels", [])],
                       "assigned": bool(i.get("assignees") or i.get("assignee"))} for i in data]
            if not issues:
                done(m["id"], "no open issues on %s — nothing to pick" % repo)
            else:
                pick = next((i for i in issues
                             if any(g in ",".join(i["labels"]).lower()
                                    for g in ("good first issue", "help wanted")) and not i["assigned"]),
                            None) or next((i for i in issues if not i["assigned"]), issues[0])
                done(m["id"], "picked #%d — %s [%s]\nwhy: %s\nhttps://github.com/%s/issues/%d" % (
                    pick["number"], pick["title"], ",".join(pick["labels"]) or "no labels",
                    "good-first-issue match" if "good" in ",".join(pick["labels"]).lower()
                    else "oldest unassigned", repo, pick["number"]))
        else:
            done(m["id"], "unknown tool %s" % name, err=True)
    elif t == "notify_plugin":
        ev = m.get("event", "")
        if ev == "refresh":
            with lock:
                repo = state["repo"]
            if repo:
                with lock:
                    state["prs"] = pr_rows(repo)
                push_board()
    elif t == "shutdown":
        sys.exit(0)
