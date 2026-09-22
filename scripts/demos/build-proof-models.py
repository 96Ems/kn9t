#!/usr/bin/env python3
"""build-proof-models.py — model scripts for the two proof films:

  models.model.json   05 — Can I change models?   (catalog, F2 switch, live config add)
  replay.model.json   07 — Can it survive a session? (client dies mid-turn, session replays)

Usage: python3 build-proof-models.py
"""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.environ.get("DEMO_WORK", "/tmp/opencode/demo-home/work")

def usage(i, o):
    return {"input": i, "output": i // 4}

MODELS = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "extra_models": [
        {"id": "demo-2-flash", "ctx_window": 128000,
         "price": {"input": 0.1, "output": 0.4, "cache_read": 0.01, "cache_write": 0.1}},
    ],
    "turns": [
        {"thinking": ["the model layer is just config and declarations — ",
                      "show the catalog, switch live, add one by editing config."],
         "text": "The catalog: demo-1 (reasoning) and demo-2-flash (fast) — both declared by the provider plugin, switchable mid-session. Go ahead: step through them. ",
         "stop": "TOOL_CALL",
         "usage": {"input": 320, "output": 70}},
        {"text": "demo-3 is in the catalog now — added by editing config.toml while everything ran, no restart, and this sentence is being streamed through it. The model is not the architecture: it's a line of config.",
         "stop": "STOP",
         "usage": {"input": 640, "output": 88}},
    ],
}

REPLAY = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {"thinking": ["a long slow turn on purpose — the client will be killed mid-stream, ",
                      "the server must keep working and the session must replay exactly."],
         "text": "Writing the long status report now — take your time, there is a lot to say about the state of this project and its roadmap for the coming weeks. ",
         "tools": [{"call_id": "w_prog", "name": "write", "args": {
             "path": os.path.join(WORK, "progress.md"),
             "content": "# progress\n\n- the log is the truth: every durable action recorded\n- sessions replay from any cursor\n- tools, providers, policy: replaceable processes\n- the model is not the architecture\n\n(long report — written while the client was already gone)\n"}}],
         "stop": "TOOL_CALL",
         "usage": {"input": 410, "output": 240}},
        {"text": "Report written. Everything above happened while the client was gone — kill the screen and the work doesn't stop, because the session is the log, not the window.",
         "stop": "STOP",
         "usage": {"input": 830, "output": 74}},
    ],
}

for name, script in [("models", MODELS), ("replay", REPLAY)]:
    with open(os.path.join(HERE, name + ".model.json"), "w") as f:
        json.dump(script, f, indent=1)
    print("%s.model.json: %d turns" % (name, len(script["turns"])))
