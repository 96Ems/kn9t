# kn9t Plugin Development Guide

> **Standalone guide for building kn9t plugins in any language.**
> No source code access required — this document is the complete reference.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Quick Start](#2-quick-start)
3. [Wire Protocol Reference](#3-wire-protocol-reference)
4. [Hook Reference](#4-hook-reference)
5. [Host API](#5-host-api)
6. [TUI Integration (Lua)](#6-tui-integration-lua)
7. [Error Handling & Debugging](#7-error-handling--debugging)
8. [Configuration & Installation](#8-configuration--installation)
9. [Examples](#9-examples)
10. [Troubleshooting](#10-troubleshooting)

---

## 1. Introduction

### What is a kn9t plugin?

A kn9t plugin is a **subprocess** that communicates with the kn9t host via
**newline-delimited JSON (NdJSON) over stdin/stdout**. This architecture provides:

- **Language agnostic** — Write in Rust, Python, TypeScript, Go, or any language
- **Crash isolation** — A plugin crash doesn't take down the host
- **Hot reload** — Plugins can be restarted without restarting kn9t
- **No shared memory** — Simple, debuggable protocol

### Plugin Types

A plugin can provide one or more of:

| Type | Purpose | Example |
|------|---------|---------|
| **Tool** | Expose tools to the agent | `bash`, `read`, `write`, custom tools |
| **Provider** | Implement an LLM provider | OpenAI, Anthropic, Bedrock |
| **Hook** | Intercept agent lifecycle | Approval gates, context injection, redaction |
| **Event Sink** | Observe events (read-only) | Logging, metrics, audit trails |

### When to build a plugin

- **Custom tools** — Add domain-specific capabilities (databases, APIs, etc.)
- **Custom providers** — Integrate with internal LLM deployments
- **Policy enforcement** — Block dangerous commands, require approval
- **Context injection** — Add steering messages, system prompts
- **Observability** — Log events, track metrics, audit actions

---

## 2. Quick Start

### Minimal Python Plugin

```python
#!/usr/bin/env python3
"""Minimal kn9t plugin that provides a 'greet' tool."""
import json
import sys

def read_msg():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    sys.stdout.write(json.dumps(msg, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def run():
    # 1. Wait for host hello
    hello = read_msg()
    if not hello or hello.get("t") != "hello":
        return
    
    # 2. Reply with our capabilities
    write_msg({
        "t": "hello",
        "name": "my-plugin",
        "capabilities": [],
        "tools": [{
            "name": "greet",
            "description": "Say hello to someone",
            "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            },
            "parallel_safe": True
        }],
        "hooks": [],
        "events": []
    })
    
    # 3. Main loop
    while True:
        msg = read_msg()
        if not msg:
            break
        
        if msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "tool_call":
            tool = msg["payload"]["tool"]
            args = msg["payload"]["args"]
            
            if tool == "greet":
                name = args.get("name", "World")
                write_msg({
                    "t": "done",
                    "id": msg["id"],
                    "content": [{"type": "text", "text": f"Hello, {name}!"}],
                    "is_error": False
                })

if __name__ == "__main__":
    run()
```

### Running Your Plugin

1. Save as `my_plugin/__main__.py`
2. Create `pyproject.toml`:
   ```toml
   [project]
   name = "my-plugin"
   version = "0.1.0"
   ```
3. Add to `~/.kn9t/config.toml`:
   ```toml
   [[plugin]]
   name = "my-plugin"
   cmd = ["python", "-m", "my_plugin"]
   
   [plugin.env]
   PYTHONPATH = "/path/to/my_plugin"
   ```
4. Restart kn9t or hot-reload:
   ```bash
   curl -X POST "http://localhost:$PORT/plugin/my-plugin/reload" \
        -H "Authorization: Bearer $TOKEN"
   ```

---

## 3. Wire Protocol Reference

### JSON Convention

**All JSON uses `snake_case`** for field names and enum variants.

### Message Flow

```
Host  → Plugin:  {"t":"hello","proto":1,"kn9t":"0.1.0"}
Plugin → Host:   {"t":"hello","name":"my-plugin",...}

Host  → Plugin:  {"t":"hook","id":1,"hook":"tool_call","payload":{...}}
Plugin → Host:   {"t":"done","id":1,"content":[...],"is_error":false}

Host  → Plugin:  {"t":"shutdown"}
```

### Host → Plugin Messages

| `t` | Fields | Description |
|-----|--------|-------------|
| `hello` | `proto`, `kn9t` | Handshake initiation |
| `hook` | `id`, `hook`, `payload` | Hook invocation |
| `cancel` | `id` | Cancel in-flight operation |
| `shutdown` | — | Graceful shutdown request |
| `apiresult` | `id`, `ok`, `result?`, `error?` | Response to plugin's `request` |

### Plugin → Host Messages

| `t` | Fields | Description |
|-----|--------|-------------|
| `hello` | `name`, `capabilities`, `tools`, `hooks`, `events`, `provider?` | Handshake reply |
| `result` | `id`, + flattened reply fields | Hook reply (non-streaming) |
| `chunk` | `id`, + streaming fields | Streaming progress |
| `done` | `id`, `content`, `is_error` | Final tool result |
| `request` | `id`, `op`, `payload` | Host API call (requires `host_api` capability) |
| `declare` | `tools`, `hooks`, `events` | Hot re-declaration |

### Handshake

**Host → Plugin:**
```json
{"t": "hello", "proto": 1, "kn9t": "0.1.0"}
```

**Plugin → Host:**
```json
{
  "t": "hello",
  "name": "my-plugin",
  "capabilities": ["streaming", "cancelable"],
  "tools": [...],
  "hooks": ["before_tool_call", "get_steering"],
  "events": ["turn_started", "turn_ended"],
  "provider": null
}
```

### Capabilities

| Flag | Meaning |
|------|---------|
| `streaming` | Plugin may send `chunk` messages before `done` |
| `cancelable` | Plugin listens for `cancel` messages |
| `host_api` | Plugin may send `request` messages to call host ops |
| `compactor` | Plugin provides context compaction |

### Tool Specification

```json
{
  "name": "my_tool",
  "description": "What the tool does",
  "schema": {
    "type": "object",
    "properties": {
      "arg1": {"type": "string", "description": "First argument"}
    },
    "required": ["arg1"]
  },
  "parallel_safe": true,
  "hidden": false,
  "effects": [
    {"field": "path", "kind": "fs_write"}
  ]
}
```

**Effect kinds:** `shell`, `fs_read`, `fs_write`, `network`

### Content Types

Tool results use a `content` array with typed elements:

| `type` | Fields | Example |
|--------|--------|---------|
| `text` | `text` | `{"type": "text", "text": "Hello"}` |
| `image` | `sha256`, `mime` | `{"type": "image", "sha256": "abc...", "mime": "image/png"}` |
| `tool_call` | `id`, `name`, `args_json` | For providers |
| `tool_result` | `id`, `content`, `is_error` | For providers |
| `thinking` | `text` | Extended thinking content |

### Flattened Body Fields

**IMPORTANT:** Hook reply fields are **flattened**, not nested under `"body"`:

```json
// ✅ Correct — fields at same level as "t" and "id":
{"t": "result", "id": 7, "action": "allow"}
{"t": "done", "id": 42, "content": [...], "is_error": false}

// ❌ Wrong — nested body:
{"t": "result", "id": 7, "body": {"action": "allow"}}
```

---

## 4. Hook Reference

Plugins subscribe to hooks in the handshake. Each hook has a specific payload and
expected reply format.

### Hook Invocation

```json
{"t": "hook", "id": 1, "hook": "before_tool_call", "payload": {...}}
```

### Hook Reply

```json
{"t": "result", "id": 1, "action": "allow"}
```

### Available Hooks

#### `tool_call` — Execute a tool

Called when the agent invokes a tool declared by this plugin.

**Payload:**
```json
{
  "tool": "my_tool",
  "args": {"arg1": "value"},
  "cwd": "/path/to/workspace",
  "session_id": "01ABC..."
}
```

**Reply (non-streaming):**
```json
{"t": "done", "id": 1, "content": [{"type": "text", "text": "result"}], "is_error": false}
```

**Reply (streaming):**
```json
{"t": "chunk", "id": 1, "text": "partial output..."}
{"t": "chunk", "id": 1, "text": "more output..."}
{"t": "done", "id": 1, "content": [{"type": "text", "text": "full output"}], "is_error": false}
```

---

#### `before_tool_call` — Gate tool execution

Called before any tool executes. Use for approval gates, policy enforcement.

**Payload:**
```json
{
  "tool": "bash",
  "args": {"cmd": "rm -rf /"},
  "cwd": "/workspace",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "action": "allow"}
{"t": "result", "id": 1, "action": "deny", "reason": "Dangerous command"}
{"t": "result", "id": 1, "action": "ask", "reason": "Requires approval"}
{"t": "result", "id": 1, "action": "replace", "args": {"cmd": "ls"}}
```

**Composition:** First-deny-wins. If any plugin denies, the tool is blocked.

**Failure posture:** Deny (fail closed).

**Timeout:** 30 seconds.

---

#### `after_tool_call` — Transform tool output

Called after a tool executes. Use for redaction, formatting.

**Payload:**
```json
{
  "tool": "bash",
  "args": {"cmd": "cat secret.txt"},
  "result": [{"type": "text", "text": "SECRET_KEY=abc123"}],
  "cwd": "/workspace",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "action": "keep"}
{"t": "result", "id": 1, "action": "replace", "content": [{"type": "text", "text": "[REDACTED]"}]}
```

**Composition:** Pipeline. Each plugin transforms in order.

**Failure posture:** Keep original.

---

#### `before_request` — Modify LLM request

Called before sending a request to the LLM provider.

**Payload:**
```json
{
  "messages": [...],
  "model": {"provider": "anthropic", "id": "claude-3"},
  "system": "You are a helpful assistant.",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "action": "keep"}
{"t": "result", "id": 1, "action": "replace", "messages": [...]}
```

**Composition:** Pipeline.

**Failure posture:** Use original.

---

#### `get_steering` — Inject context messages

Called every turn. Use for injecting steering messages, reminders.

**Payload:**
```json
{
  "cwd": "/workspace",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{
  "t": "result",
  "id": 1,
  "messages": [
    {"role": "user", "silent": true, "content": [{"type": "text", "text": "Remember: be concise."}]}
  ]
}
```

**Composition:** Concat. All plugins' messages are combined.

**Failure posture:** Empty (no messages injected).

---

#### `get_followup` — Queue follow-up messages

Called after a turn ends. Use for chaining prompts.

**Payload:**
```json
{
  "cwd": "/workspace",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{
  "t": "result",
  "id": 1,
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "Now run the tests."}]}
  ]
}
```

**Composition:** Concat.

**Failure posture:** Empty.

---

#### `should_stop_after_turn` — Control loop termination

Called after each turn. Use for budget limits, turn caps.

**Payload:**
```json
{
  "stop": "tool_use",
  "turn": 5,
  "usage": {"input": 1000, "output": 500, "cache_read": 0, "cache_write": 0},
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "action": "continue"}
{"t": "result", "id": 1, "action": "stop", "reason": "Turn limit reached"}
```

**Composition:** Any-says-stop. If any plugin says stop, the loop ends.

**Failure posture:** Continue.

---

#### `prepare_next_turn` — Switch model/thinking

Called before starting the next turn. Use for model switching.

**Payload:**
```json
{
  "stop": "tool_use",
  "usage": {"input": 1000, "output": 500, "cache_read": 0, "cache_write": 0},
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "action": "keep"}
{"t": "result", "id": 1, "action": "patch", "model": {"provider": "openai", "id": "gpt-4"}}
```

**Composition:** Pipeline.

**Failure posture:** No change.

---

#### `get_api_key` — Provide API keys

Called when a provider needs an API key.

**Payload:**
```json
{
  "provider": "openai",
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{"t": "result", "id": 1, "key": "sk-..."}
{"t": "result", "id": 1, "key": null}
```

**Composition:** First non-null wins.

**Failure posture:** Fall back to config/environment.

---

#### `provider_complete` — Custom LLM provider

Called when the host needs to complete a request via your provider.

**Payload:**
```json
{
  "model": "my-model",
  "messages": [...],
  "system": "...",
  "tools": [...],
  "session_id": "01ABC..."
}
```

**Reply (streaming chunks):**
```json
{"t": "chunk", "id": 1, "text_delta": "Hello "}
{"t": "chunk", "id": 1, "text_delta": "world!"}
{"t": "chunk", "id": 1, "tool_use_start": {"id": "call_1", "name": "bash"}}
{"t": "chunk", "id": 1, "tool_use_delta": {"id": "call_1", "delta": "{\"cmd\":"}}
{"t": "chunk", "id": 1, "tool_use_delta": {"id": "call_1", "delta": "\"ls\"}"}}
{"t": "chunk", "id": 1, "input_tokens": 100}
{"t": "done", "id": 1, "stop": "tool_use", "usage": {...}, "cost_usd": 0.001}
```

---

#### `compactor_compact` — Context compaction

Called when context needs compaction (requires `compactor` capability).

**Payload:**
```json
{
  "messages": [...],
  "target_tokens": 50000,
  "session_id": "01ABC..."
}
```

**Reply:**
```json
{
  "t": "result",
  "id": 1,
  "messages": [...],
  "summary": "Compacted 20 messages to 5"
}
```

---

### Hook Composition Classes

| Class | Behavior | Hooks |
|-------|----------|-------|
| **Pipeline** | Each plugin transforms in order | `before_request`, `after_tool_call`, `prepare_next_turn` |
| **Veto (First-deny-wins)** | Any deny blocks | `before_tool_call` |
| **Collect (Concat)** | All results combined | `get_steering`, `get_followup` |
| **Any-says-stop** | First stop wins | `should_stop_after_turn` |
| **First non-null** | First valid value wins | `get_api_key` |
| **Single handler** | One plugin handles | `tool_call`, `provider_complete` |

---

## 5. Host API

Plugins with `host_api` capability can call back to the host via `request` messages.

### Request Format

```json
{
  "t": "request",
  "id": 1,
  "op": "session_read",
  "payload": {"session": "01ABC..."}
}
```

### Response Format

```json
{"t": "apiresult", "id": 1, "ok": true, "result": {...}}
{"t": "apiresult", "id": 1, "ok": false, "error": "session not found"}
```

### Available Operations

| Op | Payload | Result | Description |
|----|---------|--------|-------------|
| `session_read` | `{session, start?, end?}` | `{messages}` | Projected messages; whole transcript by default |
| `session_create` | `{model?, cwd?}` | `{session}` | Brand-new independent session (no parent/fork) |
| `session_fork` | `{session, origin_seq?, copy_events?, model?, budget_usd?}` | `{session}` | Fork as a subagent; `copy_events: false` is a bare, task-only child |
| `session_prompt` | `{session, text, tools?}` | `{session, result}` | One synchronous turn; the parent's Cancel propagates, so ESC aborts the child |
| `provider_complete` | `{session, model, messages, system?, tools?}` | streaming | One real provider call on the session's model and credentials |
| `tool_list` | `{session}` | `{tools}` | Registry tool names, for composing a child toolset |
| `tool_execute` | `{session, name, args}` | `{content, is_error}` | Run a tool through the normal approval path |
| `tool_visibility` | `{hidden, tools?}` | `{tools}` | Hide or reveal **your own** tools; a `plugin` field is rejected |
| `interaction_request` | `{session, payload}` | `{payload}` | Ask the client a question; blocks until `POST /ui-respond` |
| `plugin_list` | `{}` | `{plugins}` | Plugin inventory: name, state, tools |
| `plugin_start` / `plugin_stop` / `plugin_reload` | `{name}` | — | Lifecycle for a plugin the server already knows |
| `plugin_load` | `{cmd?, env?, from_config?}` | `{loaded, tools}` | Load a new plugin without a restart |
| `plugin_health` | `{}` | `{plugins}` | What the *server* observed per subprocess: a silent plugin cannot self-report |
| `ui_register_lua` | `{session, source, placement?, title?, rows?, cols?}` | `{ok}` | Ship Lua defining `render(state)`; placement is a request, the TUI decides |
| `ui_set_state` | `{session, state}` | `{ok}` | Push opaque state to `render(state)` |
| `ui_clear` | `{session}` | `{ok}` | Drop the plugin's UI |
| `ui_directive` (alias `ui_push`) | `{target, op, payload?}` | `{ok}` | Structured plugin→TUI directive, session-scoped |

**Note:** Most ops take `session`; the SDK auto-injects it from hook context. This table is
checked against the host on every push (`scripts/check-contract.sh`) — if an op exists in the
code and not here, or here and not in the code, the push fails.

---

## 6. TUI Integration (Lua)

Plugins can display interactive UIs in the TUI.

### Architecture

1. **Register Lua source** via `ui_register_lua` — defines `render(state)`
2. **Push state** via `ui_set_state` — arbitrary JSON from plugin to TUI
3. **Handle keys** via `kn9t.on_key(key, fn)`; printable text via `kn9t.on_text(fn)`
4. **Modify local state directly** in handlers — the Lua `V` table persists
5. **Answer an interaction** via `kn9t.respond(payload)` — the host owns the
   transport (`POST /ui-respond`)

> **IMPORTANT:** Handlers run in the TUI process, not your plugin process, and
> there is no general reverse channel. The exceptions are `kn9t.notify()` (to
> your backend) and `kn9t.respond()` (to the host, for the interaction you are
> rendering). Everything else must be self-contained in the Lua source.

### Getting session_id

The host hello does NOT include `session_id`. Get it from the first hook payload:

```python
session_id = None
ui_registered = False

# In main loop:
if msg.get("t") == "hook":
    payload = msg.get("payload", {})
    if not session_id:
        session_id = payload.get("session_id")
    
    if session_id and not ui_registered:
        register_ui()
        ui_registered = True
```

### Register UI

```json
{
  "t": "request",
  "id": 1,
  "op": "ui_register_lua",
  "payload": {
    "session": "01ABC...",
    "source": "function render(s) ... end",
    "placement": "main",
    "title": "My Plugin"
  }
}
```

**Placements:** `main`, `sidebar`, `bottom`, `status`. `bottom` reserves rows between the
transcript and the prompt — the slot for an interaction that must not cover the transcript.

### Push State

```json
{
  "t": "request",
  "id": 2,
  "op": "ui_set_state",
  "payload": {
    "session": "01ABC...",
    "state": {"items": [...], "cursor": 0}
  }
}
```

### Lua Template

```lua
-- Local view state (persists across renders, NOT sent back to plugin)
local V = { cursor = 0, items = {} }

-- Called when plugin pushes state via ui_set_state
function on_state(s)
    -- Merge plugin data into V, but keep local UI state (cursor, scroll, etc.)
    V.items = s.items or V.items
    -- Don't overwrite V.cursor - that's local navigation state
end

-- Register key handlers - modify V directly, no messages to plugin!
kn9t.on_key("j", function()
    if V.cursor < #V.items - 1 then
        V.cursor = V.cursor + 1
    end
    return true  -- consumed
end)

kn9t.on_key("k", function()
    if V.cursor > 0 then
        V.cursor = V.cursor - 1
    end
    return true  -- consumed
end)

-- Text entry: one handler for every printable character. Return false to let
-- a character fall through to on_key or the host.
kn9t.on_text(function(ch)
    if not V.editing then return false end
    V.buffer = V.buffer .. ch
    return true
end)

-- Esc is intercepted by the host (it cancels a pending interaction), so a view
-- cannot bind it; offer an explicit "cancel" item instead.

-- Render function (required)
function render(s)
    on_state(s)
    local out = {}
    for i, item in ipairs(V.items) do
        table.insert(out, {
            type = "text",
            content = (i == V.cursor + 1) and "> "..item or "  "..item,
            fg = (i == V.cursor + 1) and "cyan" or "white",
            size = {fixed = 1}
        })
    end
    return {type = "split", direction = "vertical", children = out}
end
```

### Data Flow

```
Plugin (Python/Rust/etc)          TUI (Lua)
       │                              │
       │── ui_register_lua ──────────>│  (send Lua source once)
       │                              │
       │── ui_set_state ─────────────>│  (push data anytime)
       │                              │
       │                              │<── key events (handled locally)
       │                              │    modifies V directly
       │                              │
       X<─ NO REVERSE CHANNEL ────────│  (can't send back to plugin)
```

**Key insight:** The Lua runs in the TUI process. Your plugin process cannot
receive key events or UI interactions. Design your UI to be self-contained:

- **Read-only views:** Plugin pushes data, Lua renders it, navigation is local
- **Stateful views:** Store state in `V`, persist to temp files if needed
- **Actions:** Use `kn9t.write_file()` to write to a temp file your plugin can poll

### Widget Types

| Type | Properties |
|------|------------|
| `text` | `content`, `fg`, `bold`, `spans` |
| `split` | `direction` (`vertical`/`horizontal`), `children` |
| `spacer` | `size` |
| `box` | `title`, `border`, `child` |

**Return content, not a frame.** The layout wraps every plugin view in its own box — border,
title and focus ring included — so a view whose top-level node is a `box` nests two frames.
Return the `split`/`list`/`text` you want *inside* the frame; a `box` is still fine for a
sub-panel within your content.

**Register, then clear.** A view that is only useful for the duration of an operation (a
progress panel, a question) should `ui_clear` when it is done, so an idle session keeps no
panel and a second run re-registers it. Register lazily: the `session_id` arrives with the
first hook, not at handshake.

### Communicating Back to Plugin via `kn9t.notify()`

Plugins can receive UI interactions via `kn9t.notify()`. This sends an event
through the server to your plugin process.

**Step 1: Subscribe to `ui_interaction` events in your plugin's Hello:**

```json
{
  "t": "hello",
  "name": "my-plugin",
  "version": "0.1.0",
  "subscriptions": ["ui_interaction"]
}
```

**Step 2: Call `kn9t.notify()` from Lua key handlers:**

```lua
kn9t.on_key("Enter", function()
    if V.cursor >= 0 and V.cursor < #V.items then
        kn9t.notify({
            event = "select",
            item = V.items[V.cursor + 1],
            index = V.cursor
        })
    end
    return true
end)

kn9t.on_key("d", function()
    kn9t.notify({ event = "delete", index = V.cursor })
    return true
end)
```

**Step 3: Handle the event in your plugin's main loop:**

```python
# Plugin receives:
# {"t": "event", "kind": "ui_interaction", "plugin": "my-plugin",
#  "session_id": "01ABC...", "event": "select", "data": {"item": "...", "index": 0}}

elif msg.get("t") == "event" and msg.get("kind") == "ui_interaction":
    event = msg.get("event")
    data = msg.get("data", {})
    
    if event == "select":
        handle_selection(data.get("item"))
    elif event == "delete":
        handle_delete(data.get("index"))
    
    # Update UI state after handling
    push_state()
```

**Flow:**

```
Lua key handler                    Server                      Plugin
      │                              │                            │
      │── kn9t.notify({event}) ─────>│                            │
      │                              │── POST /plugin/X/ui_event ─>│
      │                              │                            │
      │                              │<── ui_set_state ───────────│
      │<── state update ─────────────│                            │
```

**Important:**
- The `event` field is required in the notify payload
- The plugin must be subscribed to `ui_interaction` events
- After handling, push updated state via `ui_set_state` to refresh the UI

---

## 7. Error Handling & Debugging

### stderr is for logging

Plugin stderr goes to the host's log. Use it for debugging:

```python
import sys
print("Debug: got message", msg, file=sys.stderr)
```

### Hook Errors

If a hook handler raises an exception, the host uses the **failure posture**:

| Hook | Failure Posture |
|------|-----------------|
| `before_tool_call` | **Deny** (fail closed) |
| `after_tool_call` | Keep original |
| `before_request` | Use original |
| `get_steering` | Empty |
| `get_followup` | Empty |
| `should_stop_after_turn` | Continue |
| `get_api_key` | Fall back to config |

### Timeouts

| Hook | Default Timeout |
|------|-----------------|
| `before_tool_call` | 30 seconds |
| `tool_call` | No limit (streaming) |
| `provider_complete` | No limit (streaming) |
| Others | 60 seconds |

### Cancellation

If your plugin declares `cancelable` capability, handle `cancel` messages:

```python
if msg.get("t") == "cancel":
    cancel_id = msg.get("id")
    # Stop the operation with that id
```

### Common Errors

| Error | Cause | Fix |
|-------|-------|-----|
| `malformed message` | Invalid JSON or unknown `t` | Check JSON syntax, use `t` not `type` |
| `protocol violation` | Wrong message type | Use `request` not `host_api` |
| `session not found` | Invalid session_id | Get session_id from hook payload |
| `plugin unhealthy` | Plugin crashed or timed out | Check stderr, add error handling |

---

## 8. Configuration & Installation

### config.toml Entry

```toml
[[plugin]]
name = "my-plugin"
cmd = ["python", "-m", "my_plugin"]

[plugin.env]
PYTHONPATH = "/path/to/my_plugin"
MY_API_KEY = "secret"
```

### Plugin Discovery

kn9t looks for plugins in `~/.kn9t/plugins/` (binaries) and `config.toml` entries.

**Binary plugins** (Rust, Go): Copy executable to `~/.kn9t/plugins/`

**Interpreted plugins** (Python, Node): Add `[[plugin]]` entry with `cmd` and `env`

### Hot Reload

Reload without restarting kn9t:

```bash
PORT=$(cat ~/.kn9t/port)
TOKEN=$(cat ~/.kn9t/token)
curl -X POST "http://localhost:$PORT/plugin/my-plugin/reload" \
     -H "Authorization: Bearer $TOKEN"
```

The host will:
1. Cancel in-flight operations
2. Send `shutdown` message
3. Respawn from `cmd`
4. Re-handshake

### Hot Re-declaration

Plugins can change their tools/hooks at runtime via `declare`:

```json
{
  "t": "declare",
  "tools": [...],
  "hooks": ["get_steering"],
  "events": []
}
```

---

## 9. Examples

### Policy Gate (Python)

```python
#!/usr/bin/env python3
"""Block dangerous commands."""
import json, sys, fnmatch

DANGEROUS = ["rm -rf*", "git reset --hard*", "git clean -fd*"]

def matches(cmd, patterns):
    return any(fnmatch.fnmatch(cmd, p) for p in patterns)

def read_msg():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def run():
    read_msg()  # hello
    write_msg({"t": "hello", "name": "policy", "hooks": ["before_tool_call"],
               "capabilities": [], "tools": [], "events": []})
    
    while True:
        msg = read_msg()
        if not msg or msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "before_tool_call":
            payload = msg["payload"]
            if payload.get("tool") == "bash":
                cmd = payload.get("args", {}).get("cmd", "")
                if matches(cmd, DANGEROUS):
                    write_msg({"t": "result", "id": msg["id"],
                               "action": "deny", "reason": f"Blocked: {cmd}"})
                    continue
            write_msg({"t": "result", "id": msg["id"], "action": "allow"})

if __name__ == "__main__":
    run()
```

### Context Injector (Python)

```python
#!/usr/bin/env python3
"""Inject reminders every turn."""
import json, sys

def read_msg():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def run():
    read_msg()
    write_msg({"t": "hello", "name": "injector", "hooks": ["get_steering"],
               "capabilities": [], "tools": [], "events": []})
    
    while True:
        msg = read_msg()
        if not msg or msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "get_steering":
            write_msg({
                "t": "result", "id": msg["id"],
                "messages": [{
                    "role": "user", "silent": True,
                    "content": [{"type": "text", "text": "Remember: be concise."}]
                }]
            })

if __name__ == "__main__":
    run()
```

---

## 10. Troubleshooting

### Plugin not loading

1. Check `~/.kn9t/server.log` for errors
2. Verify `cmd` path is correct
3. Test manually: `echo '{"t":"hello","proto":1,"kn9t":"test"}' | python -m my_plugin`

### Keys not working in TUI

1. Use `kn9t.on_key("j", fn)` not `function on_key(key)`
2. Return `true` to consume, `false` to pass through
3. Check plugin is focused (F10 to cycle)

### UI not appearing

1. Get `session_id` from hook payload, not hello
2. Register UI on first hook, not at startup
3. Use `"t": "request"` not `"t": "host_api"`
4. Include `session` in every payload

### Hook not being called

1. Verify hook name in handshake `hooks` array
2. Check spelling (`before_tool_call` not `beforeToolCall`)
3. Use `get_steering` to ensure early registration (called every turn)

### Hot reload fails

1. Check server log for errors
2. Verify Authorization header is correct
3. Plugin must handle `shutdown` gracefully

