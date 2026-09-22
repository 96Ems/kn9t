#!/usr/bin/env python3
"""snake.py — Snake, playable inside the kn9t TUI.

`start_snake` (one tool) seats the game: it registers a plugin UI via
`ui_register_lua` and starts a heartbeat — one `ui_set_state` tick every 220ms,
which is both the game clock and the redraw trigger. The Lua view owns the whole
game (board, snake, food, kn9t.on_key); click the view to focus it, then play
with arrows or WASD.
"""
import json, sys, threading, time

LUA = r"""
-- snake view: the whole game lives here; the process just heartbeats state.
local W, H = 46, 15
local snake = {{x=9,y=8},{x=8,y=8},{x=7,y=8}}
local dir = {x=1, y=0}; local pending = nil
local food = {x=32, y=5}; local score = 0; local dead = false; local ticks = 0

local function place_food()
  ticks = ticks + 1
  math.randomseed(os.time() * 7 + ticks)
  food = {x = math.random(2, W-2), y = math.random(2, H-2)}
end

local function turn(dx, dy)
  return function()
    if (dir.x + dx == 0 and dir.y + dy == 0) then return false end
    pending = {x=dx, y=dy}
    return true
  end
end

for _, k in ipairs({"Up","up","<Up>","w","W"}) do kn9t.on_key(k, turn(0,-1)) end
for _, k in ipairs({"Down","down","<Down>","s","S"}) do kn9t.on_key(k, turn(0,1)) end
for _, k in ipairs({"Left","left","<Left>","a","A"}) do kn9t.on_key(k, turn(-1,0)) end
for _, k in ipairs({"Right","right","<Right>","d","D"}) do kn9t.on_key(k, turn(1,0)) end

local function step()
  if dead then return end
  if pending then dir = pending; pending = nil end
  local head = {x = snake[1].x + dir.x, y = snake[1].y + dir.y}
  if head.x < 2 or head.x > W-1 or head.y < 2 or head.y > H-1 then dead = true; return end
  for _, s in ipairs(snake) do
    if s.x == head.x and s.y == head.y then dead = true; return end
  end
  table.insert(snake, 1, head)
  if head.x == food.x and head.y == food.y then
    score = score + 1; place_food()
  else
    table.remove(snake)
  end
end

function render(state)
  step()   -- one game step per heartbeat
  local grid = {}
  for y = 1, H do
    grid[y] = {}
    for x = 1, W do grid[y][x] = "·" end
  end
  for x = 1, W do grid[1][x] = "─"; grid[H][x] = "─" end
  for y = 1, H do grid[y][1] = "│"; grid[y][W] = "│" end
  grid[1][1] = "┌"; grid[1][W] = "┐"; grid[H][1] = "└"; grid[H][W] = "┘"
  grid[food.y][food.x] = "●"
  for i, s in ipairs(snake) do grid[s.y][s.x] = (i == 1 and "█" or "▓") end
  local rows = {}
  for y = 1, H do rows[y] = table.concat(grid[y]) end
  local head_line
  if dead then
    head_line = "  SNAKE ▸ game over · score " .. score
  else
    head_line = "  SNAKE ▸ score " .. score .. " · arrows or WASD"
  end
  table.insert(rows, 1, head_line)
  return { type = "text", content = table.concat(rows, "\n") }
end
"""

TOOLS = [{
    "name": "start_snake",
    "description": "Seat the Snake game in the TUI: registers the game view (plugin UI) and starts its clock.",
    "schema": {"type": "object", "properties": {}, "required": []},
    "parallel_safe": False, "hidden": False, "effects": [],
    "policy": {"pattern_field": None, "default_policy": "allow", "builtin_allow": [], "builtin_deny": []},
}]

def send(m):
    sys.stdout.write(json.dumps(m) + "\n")
    sys.stdout.flush()

def heartbeat(sid):
    t0 = time.time()
    while True:
        time.sleep(0.22)
        send({"t": "request", "id": 0, "op": "ui_set_state",
              "payload": {"session": sid, "state": {"tick": int((time.time() - t0) * 1000)}}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    t = m.get("t")
    if t == "hello":
        send({"t": "hello", "name": "snake", "capabilities": ["tools", "host_api", "ui_interaction"],
              "hooks": [], "tools": TOOLS, "events": [], "provider": None})
    elif t == "hook" and m.get("hook") == "tool_call":
        sid = m.get("payload", {}).get("session", "")
        send({"t": "request", "id": 0, "op": "ui_register_lua",
              "payload": {"session": sid, "source": LUA, "placement": "bottom"}})
        threading.Thread(target=heartbeat, args=(sid,), daemon=True).start()
        send({"t": "done", "id": m["id"], "is_error": False,
              "content": [{"type": "text", "text": "game seated in the bottom slot — click it, then arrows or WASD"}]})
    elif t == "shutdown":
        sys.exit(0)
