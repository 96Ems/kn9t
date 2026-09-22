#!/usr/bin/env python3
"""dice.py — a kn9t plugin in 40 lines. One tool: roll."""
import json, random, sys

TOOLS = [{
    "name": "roll",
    "description": "Roll an N-sided die and return the result.",
    "schema": {"type": "object",
               "properties": {"sides": {"type": "integer", "description": "Number of sides."}},
               "required": ["sides"]},
    "parallel_safe": True,
    "hidden": False,
    "effects": [],
    "policy": {"pattern_field": None, "default_policy": "allow",
               "builtin_allow": [], "builtin_deny": []},
}]

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        t = msg.get("t")
        if t == "hello":
            send({"t": "hello", "name": "dice", "capabilities": ["tools"],
                  "hooks": [], "tools": TOOLS, "events": [], "provider": None})
        elif t == "hook" and msg.get("hook") == "tool_call":
            args = msg.get("payload", {}).get("args", {})
            sides = int(args.get("sides", 6))
            send({"t": "done", "id": msg["id"], "is_error": False,
                  "content": [{"type": "text",
                               "text": "rolled a d%d -> %d" % (sides, random.randint(1, sides))}]})
        elif t == "shutdown":
            return

if __name__ == "__main__":
    main()
