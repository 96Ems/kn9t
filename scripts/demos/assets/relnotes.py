#!/usr/bin/env python3
"""relnotes.py — release-notes feed: real git history as tools.

  changelog {repo, since}   git log --oneline since..HEAD (the real story)
  diffstat  {repo, since}   what actually changed: files and churn

The agent writes the notes; this plugin only feeds it facts.
"""
import json, os, subprocess, sys

TOOLS = [
    {"name": "changelog",
     "description": "Oneline git log from a rev to HEAD (recent history of a repo).",
     "schema": {"type": "object",
                "properties": {"repo": {"type": "string", "description": "Path to the git repo."},
                               "since": {"type": "string", "description": "Start rev (e.g. HEAD~30, v0.1.0)."}},
                "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
    {"name": "diffstat",
     "description": "Diffstat from a rev to HEAD: which files changed and how much.",
     "schema": {"type": "object",
                "properties": {"repo": {"type": "string"},
                               "since": {"type": "string"}},
                "required": ["repo"]},
     "parallel_safe": True, "hidden": False, "effects": [],
     "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []}},
]

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def git(repo, args):
    out = subprocess.run(["git", "-C", repo] + args, capture_output=True, text=True, timeout=20)
    return out.stdout.strip() if out.returncode == 0 else ("git error: " + out.stderr.strip()[:120])

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
        send({"t": "hello", "name": "relnotes", "capabilities": ["tools"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        p = m.get("payload", {})
        name = p.get("tool", "")
        args = p.get("args", {})
        repo = args.get("repo", ".")
        since = args.get("since", "HEAD~30")
        if name == "changelog":
            done(m["id"], git(repo, ["log", "--oneline", "--no-merges", "-40", since + "..HEAD"]))
        elif name == "diffstat":
            done(m["id"], git(repo, ["diff", "--stat", since + "..HEAD"]))
        else:
            done(m["id"], "unknown tool %s" % name, err=True)
    elif t == "shutdown":
        sys.exit(0)
