#!/usr/bin/env python3
"""build-concrete-models.py — assemble the three concrete demo model scripts.

Usage: python3 build-concrete-models.py
Writes relnotes.model.json, standup.model.json and triage.model.json here.
"""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.environ.get("DEMO_WORK", "/tmp/opencode/demo-home/work")
REAL_HOME = os.environ.get("HOME", "/home/emericclement")
KN9T_REPO = "/home/emericclement/dev/kn9t"

def src(name):
    with open(os.path.join(HERE, "assets", name)) as f:
        return f.read()

def usage(i, o):
    return {"input": i, "output": o}

def write(call, asset, path):
    return {"call_id": call, "name": "write", "args": {"path": path, "content": src(asset)}}

def load_bash(plugin):
    payload = {"cmd": [os.path.join(WORK, plugin)], "env": {"GH_HOME": REAL_HOME}}
    body = json.dumps(payload).replace('"', '\\"')
    return ("chmod +x %s && P=$(cat $KN9T_HOME/port) && T=$(cat $KN9T_HOME/token) && "
            "curl -sS -X POST http://127.0.0.1:$P/plugin/load -H \"Authorization: Bearer $T\" "
            "-H 'Content-Type: application/json' -d \"%s\"" % (plugin, body))

# ── 1 · relnotes: real git history → RELEASE_NOTES.md ────────────────────────
relnotes = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {"thinking": ["release prep = facts from git, then a draft — ",
                      "relnotes feeds the log and the churn, I write the notes."],
         "text": "Release prep. relnotes.py gives me the raw history and the churn, I do the writing: ",
         "tools": [write("w_rel", "relnotes.py", os.path.join(WORK, "relnotes.py"))],
         "stop": "TOOL_CALL", "usage": usage(380, 520)},
        {"text": "Loading it: ",
         "tools": [{"call_id": "b_rel", "name": "bash", "args": {"cmd": load_bash("relnotes.py")}}],
         "stop": "TOOL_CALL", "usage": usage(1040, 130)},
        {"text": "Facts please — the last 30 commits and what they churned: ",
         "tools": [{"call_id": "t_log", "name": "changelog", "args": {"repo": KN9T_REPO, "since": "HEAD~30"}},
                   {"call_id": "t_dif", "name": "diffstat", "args": {"repo": KN9T_REPO, "since": "HEAD~30"}}],
         "stop": "TOOL_CALL", "usage": usage(1420, 310)},
        {"text": "Writing the draft from that: ",
         "tools": [{"call_id": "w_notes", "name": "write", "args": {
             "path": os.path.join(WORK, "RELEASE_NOTES.md"),
             "content": "# kn9t — release notes (draft)\n\n## Highlights\n- one Event enum is the wire, the log and the truth\n- providers, tools, policy and compaction are all plugins\n- hot reload: UI Lua, plugins and config reload live\n\n## What changed (last 30 commits)\n- streamed from git: schema-first API, plugin host v2, SSE attach without gaps\n- TUI: Lua-owned layout, native transcript/input/status/explorer/viewer\n\n## Fixes\n- plugin reload contract: cancel, drain, respawn, re-handshake\n\n*(drafted by kn9t from `git log` — edit freely)*\n"}}],
         "stop": "TOOL_CALL", "usage": usage(2210, 240)},
        {"text": "RELEASE_NOTES.md is drafted from the real history — 30 commits distilled into highlights, changes and fixes. Edit freely, it's a draft.",
         "stop": "STOP", "usage": usage(2530, 62)},
    ],
}

# ── 2 · standup: the daily brief ─────────────────────────────────────────────
standup = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {"thinking": ["standup = branch, dirty tree, commits, TODOs, PRs — ",
                      "one tool for the brief, one for the debt, plus a live board."],
         "text": "Morning standup, automated: a standup tool for the state of the repo, find_todos for the debt, and a live board: ",
         "tools": [write("w_std", "standup.py", os.path.join(WORK, "standup.py"))],
         "stop": "TOOL_CALL", "usage": usage(360, 560)},
        {"text": "Loading: ",
         "tools": [{"call_id": "b_std", "name": "bash", "args": {"cmd": load_bash("standup.py")}}],
         "stop": "TOOL_CALL", "usage": usage(1080, 120)},
        {"text": "The brief for kn9t — and the debt: ",
         "tools": [{"call_id": "t_std", "name": "standup", "args": {"repo": KN9T_REPO}},
                   {"call_id": "t_todo", "name": "find_todos", "args": {"repo": KN9T_REPO}}],
         "stop": "TOOL_CALL", "usage": usage(1360, 260)},
        {"text": "Standup: work is on main, the tree still carries today's work untracked — that's the push cue. Board below is live, r refreshes it.",
         "stop": "STOP", "usage": usage(1720, 68)},
    ],
}

# ── 3 · triage: file the three real findings ─────────────────────────────────
bugs = [
    ("approval: \"Always\" kills a turn running on a provider plugin",
     "Repro: start a turn whose model is a `kind=\"plugin\"` provider, approve a tool with **Always**. "
     "The approval persists to `config.toml`, the config watcher reloads, and `reload_config()` reaps "
     "the old provider-plugin subprocess — the turn dies mid-prompt.\n\n"
     "Found while recording demos: the scripted provider was reaped between turns 1 and 2.\n\n"
     "Expected: a config reload should not reap a provider host still used by an in-flight turn "
     "(defer the reap, or let the cloned provider finish)."),
    ("non-interactive approval Deny hangs the turn forever",
     "Repro: `kn9t chat --json` (NonInteractiveApprover) with a policy plugin answering `ask` on a tool call. "
     "`Ask` maps to `Deny` — and the turn never emits `turn_ended`; the CLI waits forever.\n\n"
     "Found via the demo tooling (a scripted run hung 150s+ until killed).\n\n"
     "Expected: a denied call returns an error tool result and the loop continues/closes."),
    ("a provider plugin dying mid-stream hangs the turn until the 600s call timeout",
     "Repro: let a provider plugin panic during `chunk` streaming. The host waits on the reply channel "
     "and the turn hangs (observed 4m49s) instead of surfacing `plugin disconnected`.\n\n"
     "Expected: reader EOF/panic should fail the in-flight call immediately (same contract as "
     "`wait_on_channel`: a synthetic `disconnected` error at the call site)."),
]
triage = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {"thinking": ["the session found three real bugs — the desk files them properly: ",
                      "file_issue ×3 with repro steps, then label and track."],
         "text": "Bug desk. This session found three real kn9t bugs — I'll file them properly with repro steps, label them, and leave us a board: ",
         "tools": [write("w_tri", "triage.py", os.path.join(WORK, "triage.py"))],
         "stop": "TOOL_CALL", "usage": usage(440, 620)},
        {"text": "Loading with my gh session: ",
         "tools": [{"call_id": "b_tri", "name": "bash", "args": {"cmd": load_bash("triage.py")}}],
         "stop": "TOOL_CALL", "usage": usage(1240, 130)},
        {"text": "Filing all three now: ",
         "tools": [dict({"call_id": "i_%d" % n, "name": "file_issue",
                         "args": {"repo": "96Ems/kn9t-plugins", "title": ttl, "body": body,
                                  "label": "bug"}}, )
                   for n, (ttl, body) in enumerate(bugs, 1)],
         "stop": "TOOL_CALL", "usage": usage(1680, 320)},
        {"text": "Three issues filed with repro steps. The board below tracks them — r refreshes, and they'll follow the project to the kn9t tracker once it's pushed.",
         "stop": "STOP", "usage": usage(2150, 74)},
    ],
}

for name, script in [("relnotes", relnotes), ("standup", standup), ("triage", triage)]:
    with open(os.path.join(HERE, name + ".model.json"), "w") as f:
        json.dump(script, f, indent=1)
    calls = sum(len(t.get("tools", [])) for t in script["turns"])
    print("%s.model.json: %d turns, %d tool calls" % (name, len(script["turns"]), calls))
