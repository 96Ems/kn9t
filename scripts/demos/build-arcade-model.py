#!/usr/bin/env python3
"""build-arcade-model.py — assemble build-arcade.model.json from scripts/demos/assets/.

The model script's `write` turns carry the full plugin sources; building the JSON
from the asset files keeps the sources testable and the escapes out of sight.
Usage: DEMO_WORK=/tmp/opencode/demo-home/work python3 build-arcade-model.py > build-arcade.model.json
"""
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = os.path.join(HERE, "assets")
WORK = os.environ.get("DEMO_WORK", "/tmp/opencode/demo-home/work")

def src(name):
    with open(os.path.join(ASSETS, name)) as f:
        return f.read()

def write(name):
    return {"call_id": "w_" + name.replace(".", "_"), "name": "write",
            "args": {"path": os.path.join(WORK, name), "content": src(name)}}

def load(name):
    return {"call_id": "l_" + name.replace(".", "_"), "name": "load_plugin",
            "args": {"path": os.path.join(WORK, name)}}

def usage(inp, out):
    return {"input": inp, "output": out}

script = {
    "model": {"id": "demo-1", "ctx_window": 200000,
              "price": {"input": 0.5, "output": 1.5, "cache_read": 0.05, "cache_write": 0.625}},
    "turns": [
        {
            "thinking": ["arcade = four plugins, one live theme change, one game — ",
                         "start with pm, the loader that installs the rest without curl."],
            "text": "Arcade boot sequence. First piece: pm — a plugin that installs other plugins, so nothing here needs curl after this. ",
            "tools": [write("pm.py")],
            "stop": "TOOL_CALL",
            "usage": usage(410, 520),
        },
        {
            "text": "Now the rest of the cabinet, one batch: ",
            "tools": [write("dice.py"), write("theme.py"), write("snake.py")],
            "stop": "TOOL_CALL",
            "usage": usage(1310, 1480),
        },
        {
            "text": "Four sources on disk. pm gets one bootstrap call — after this, installs go through its own tool: ",
            "tools": [{"call_id": "b_pm", "name": "bash", "args": {
                "cmd": "chmod +x pm.py dice.py theme.py snake.py && P=$(cat $KN9T_HOME/port) && T=$(cat $KN9T_HOME/token) && curl -sS -X POST http://127.0.0.1:$P/plugin/load -H \"Authorization: Bearer $T\" -H 'Content-Type: application/json' -d \"{\\\"cmd\\\":[\\\"" + os.path.join(WORK, "pm.py") + "\\\"]}\""}}],
            "stop": "TOOL_CALL",
            "usage": usage(2210, 150),
        },
        {
            "text": "pm is live. Installing the cabinet through it — three plugins, no restarts, registry rebuilds live: ",
            "tools": [load("dice.py"), load("theme.py"), load("snake.py")],
            "stop": "TOOL_CALL",
            "usage": usage(2740, 240),
        },
        {
            "text": "All registered. Lights on — recoloring the whole TUI through its Lua, and seating the game: ",
            "tools": [{"call_id": "t_c", "name": "recolor", "args": {"color": "cyan"}},
                      {"call_id": "t_s", "name": "start_snake", "args": {}}],
            "stop": "TOOL_CALL",
            "usage": usage(3120, 90),
        },
        {
            "text": "Chrome is recolored through the file watcher's hot reload. Dice check: ",
            "tools": [{"call_id": "t_r", "name": "roll", "args": {"sides": 20}}],
            "stop": "TOOL_CALL",
            "usage": usage(3380, 70),
        },
        {
            "text": "Arcade ready: pm loaded the cabinet live, theme rewrote the UI's Lua and the watcher reloaded it, and snake is on the bottom — click the game and play with the arrows.",
            "stop": "STOP",
            "usage": usage(3620, 96),
        },
    ],
}

json.dump(script, sys.stdout, indent=1)
