---
name: kn9t-plugin-creation
description: Create kn9t plugins in any language (Rust, Python, TypeScript, Go, etc.). Covers the plugin protocol v2, SDK usage, tool/provider/hook implementations, and testing. Use when building extensions for kn9t.
license: MIT
compatibility: Language-agnostic. Rust SDK available, but any language that can do stdin/stdout JSON works.
metadata:
  author: kn9t
  version: "1.0"
---

# Creating kn9t Plugins

> **⚠️ The wire contract is generated — read it, do not restate it.**
> `schema/plugin.json` is canonical; `references/api.md` is the readable rendering (and
> cannot be stale: `scripts/check-contract.sh` fails a push on any schema/host disagreement);
> `references/sdk/` is the Rust SDK byte-for-byte. This file is guidance only.

kn9t plugins are **subprocess binaries** that communicate via **newline-delimited JSON over stdin/stdout**. This design provides:

- **Language agnostic** — Write in Rust, Python, TypeScript, Go, or any language
- **Crash isolation** — A plugin crash doesn't take down the host
- **Hot reload** — Plugins can be restarted without restarting kn9t
- **No shared memory** — Simple, debuggable protocol

## Plugin Types

A plugin can provide one or more of:

| Type | Trait/Interface | Purpose |
|------|-----------------|---------|
| **Tool** | `PluginTool` | Expose tools to the agent (bash, read, edit, custom) |
| **Provider** | `PluginProvider` | Implement an LLM provider (OpenAI, Anthropic, custom) |
| **Hook** | `PluginHook` | Intercept agent lifecycle (approval, redaction, steering) |
| **Event Sink** | `PluginEventSink` | Observe events (logging, metrics, audit) |

## Quick Start (Rust)

### 1. Create standalone crate

```bash
mkdir my-plugin && cd my-plugin
cargo init --name my-plugin
```

### 2. Add SDK dependency

```toml
# Cargo.toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[workspace]  # Empty workspace — standalone crate

[[bin]]
name = "my-plugin"
path = "src/main.rs"

[dependencies]
kn9t-plugin-sdk = { path = "../../crates/kn9t-plugin-sdk" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### 3. Implement a tool

```rust
// src/main.rs
use kn9t_plugin_sdk::{Plugin, PluginTool, ToolOutput};
use kn9t_plugin_sdk::ctx::ToolCallCtx;
use kn9t_plugin_sdk::wire::ToolSpec;
use serde_json::{json, Value};

struct Echo;

impl PluginTool for Echo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "Returns input unchanged.".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "Text to echo"}
                },
                "required": ["message"]
            }),
            parallel_safe: true,
            hidden: false,
            effects: vec![],
            policy: Default::default(),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        // Check cancellation at checkpoints
        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }
        
        let msg = args["message"].as_str().unwrap_or("");
        ToolOutput::text(msg)
    }
}

fn main() {
    Plugin::new("my-plugin")
        .tool(Echo)
        .run();
}
```

### 4. Install and test

```bash
cargo build --release
cp target/release/my-plugin ~/.kn9t/plugins/
```

## The wire contract

**It is generated and verifiable — do not restate it here.** The authoritative sources,
in order:

| what | where |
|---|---|
| Canonical schema — message types, hook payloads, host-API ops, shared types | `schema/plugin.json` |
| Generated readable reference (adds the HTTP routes) | `references/api.md` |
| Rust SDK, byte-for-byte snapshot of `crates/kn9t-plugin-sdk` | `references/sdk/` |
| Guidance — hand-written, this file | here |

`scripts/check-contract.sh` fails a push when the schema and the host disagree about any op,
route or hook, so `references/api.md` cannot go stale. Reading this skill without the repo?
`references/api.md` is your contract.

One paragraph of orientation: the host spawns your plugin as a subprocess and you exchange
one JSON object per line (NdJSON) over stdin/stdout. Your plugin opens with a `hello`
declaring `capabilities`, `tools`, `hooks` and `events`; the host then drives it with `hook`
messages. Answer with `result` (atomic) or `chunk` … `done` (streaming, if you declared
`streaming`). With `host_api` you may also call back with `request` — the ops are listed in
`references/api.md`.

## Streaming Tools

For long-running tools (like `bash`), stream progress:

```rust
fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
    for line in run_command(args) {
        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }
        ctx.progress.send(&line);  // Sends {"t":"chunk","id":N,"text":"..."}
    }
    ToolOutput::text(all_output)
}
```

## Implementing a Provider

```rust
use kn9t_plugin_sdk::{PluginProvider, ProviderResult};
use kn9t_plugin_sdk::ctx::ProviderCallCtx;
use kn9t_plugin_sdk::wire::{ModelDecl, Usage};

struct MyProvider;

impl PluginProvider for MyProvider {
    fn id(&self) -> &str { "my-llm" }
    
    fn models(&self) -> Vec<ModelDecl> {
        vec![ModelDecl {
            id: "my-model".into(),
            ctx_window: 128000,
            price: None,
        }]
    }

    fn complete(&self, request: &Value, ctx: &ProviderCallCtx) -> ProviderResult {
        // Stream tokens
        ctx.chunk.text_delta("Hello ");
        ctx.chunk.text_delta("world!");
        
        // For tool calls:
        // ctx.chunk.tool_use_start("call_123", "bash");
        // ctx.chunk.tool_use_delta("call_123", "{\"cmd\":");
        // ctx.chunk.tool_use_delta("call_123", "\"ls\"}");

        ProviderResult {
            stop: "end_turn".into(),
            usage: Usage { input: 10, output: 5, cache_read: 0, cache_write: 0 },
            cost_usd: Some(0.0001),
            error: None,
        }
    }
}

fn main() {
    Plugin::new("my-provider")
        .provider(MyProvider)
        .run();
}
```

## Implementing Hooks

```rust
use kn9t_plugin_sdk::PluginHook;
use serde_json::{json, Value};

struct ApprovalGate;

impl PluginHook for ApprovalGate {
    fn hooks(&self) -> Vec<&'static str> {
        vec!["before_tool_call"]
    }

    fn call(&self, hook: &str, payload: &Value) -> Value {
        let tool = payload["tool"].as_str().unwrap_or("");
        
        // Block dangerous tools
        if tool == "bash" {
            let cmd = payload["args"]["cmd"].as_str().unwrap_or("");
            if cmd.contains("rm -rf") {
                return json!({"action": "deny", "reason": "Dangerous command blocked"});
            }
        }
        
        json!({"action": "allow"})
    }
}
```

## Host API (Subagents)

Plugins with `host_api` capability can call back to the host:

```rust
// Fork a session and run a turn
let fork_result = ctx.api.session_fork(session_id, Some(0.5), None)?;
let child_session = fork_result["session"].as_str().unwrap();

let prompt_result = ctx.api.session_prompt(
    child_session,
    "Summarize this code",
    Some(vec!["read", "bash"]),  // Tool subset
    Some(30),  // Timeout
)?;
```

All host-API ops are listed in [`references/api.md`](references/api.md); the snippet above
shows only the SDK shape.

## Python Plugin Structure

> **⚠️ REQUIRED:** Python plugins must follow this structure for `kn9t install-plugins` to detect and install them.

### Directory Layout

```
plugins/
└── kn9t-my-plugin/           # Plugin directory (in project plugins/)
    ├── pyproject.toml        # REQUIRED: Makes it detectable as Python plugin
    └── kn9t_my_plugin/       # Module dir (underscores, not hyphens)
        ├── __init__.py       # Can be empty
        └── __main__.py       # Entry point for `python -m kn9t_my_plugin`
```

### pyproject.toml (REQUIRED)

```toml
[project]
name = "kn9t-my-plugin"
version = "0.1.0"
description = "My kn9t plugin"
requires-python = ">=3.10"

[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"
```

### \_\_init\_\_.py

```python
"""kn9t-my-plugin package."""
```

### \_\_main\_\_.py

```python
#!/usr/bin/env python3
"""My kn9t plugin - entry point for python -m kn9t_my_plugin"""
import json
import sys

def read_msg():
    """Read JSON line from stdin."""
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    """Write JSON line to stdout."""
    sys.stdout.write(json.dumps(msg, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def run():
    # Handshake
    hello = read_msg()
    if not hello or hello.get("t") != "hello":
        return
    
    print(f"Connected to kn9t {hello.get('kn9t', '?')}", file=sys.stderr)
    
    write_msg({
        "t": "hello",
        "name": "kn9t-my-plugin",
        "capabilities": [],
        "tools": [{
            "name": "greet",
            "description": "Say hello",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "parallel_safe": True
        }],
        "hooks": [],
        "events": []
    })
    
    # Main loop
    while True:
        msg = read_msg()
        if not msg:
            break
        
        if msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "tool_call":
            name = msg["payload"]["args"].get("name", "World")
            write_msg({
                "t": "done",
                "id": msg["id"],
                "content": [{"type": "text", "text": f"Hello, {name}!"}],
                "is_error": False
            })

if __name__ == "__main__":
    run()
```

### Installation

After creating the plugin structure, run:

```bash
kn9t install-plugins
```

This will:
1. Detect `pyproject.toml` → Python plugin
2. Add a `[[plugin]]` entry to `~/.kn9t/config.toml`:

```toml
[[plugin]]
name = "kn9t-my-plugin"
cmd  = ["python", "-m", "kn9t_my_plugin"]

[plugin.env]
PYTHONPATH = "C:\\path\\to\\plugins\\kn9t-my-plugin"
```

### Hook-Only Plugin Example (Policy Gate)

For plugins that only implement hooks (no tools), like approval gates:

```python
# kn9t_policy/__main__.py
#!/usr/bin/env python3
"""Policy plugin - intercepts tool calls via before_tool_call hook."""
import json
import sys
import fnmatch

DANGEROUS_COMMANDS = ["git checkout*", "git reset*", "rm -rf*", "git clean*"]

def matches(value, patterns):
    return any(fnmatch.fnmatch(value, p) for p in patterns)

def read_msg():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    sys.stdout.write(json.dumps(msg, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def run():
    hello = read_msg()
    if not hello or hello.get("t") != "hello":
        return
    
    write_msg({
        "t": "hello",
        "name": "kn9t-policy",
        "capabilities": [],
        "hooks": ["before_tool_call"],  # Register for this hook
        "tools": [],
        "events": []
    })
    
    while True:
        msg = read_msg()
        if not msg:
            break
        
        if msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "before_tool_call":
            hook_id = msg.get("id", 0)
            payload = msg.get("payload", {})
            tool = payload.get("tool", "")
            args = payload.get("args", {})
            
            # Check bash commands
            if tool == "bash":
                cmd = args.get("cmd", "")
                if matches(cmd, DANGEROUS_COMMANDS):
                    write_msg({"t": "result", "id": hook_id, "action": "ask", 
                               "reason": f"Destructive command: {cmd}"})
                    continue
            
            write_msg({"t": "result", "id": hook_id, "action": "allow"})
        
        elif msg.get("t") == "hook":
            # Other hooks: allow by default
            write_msg({"t": "result", "id": msg.get("id", 0), "action": "allow"})

if __name__ == "__main__":
    run()
```

## TypeScript Plugin Example

```typescript
// Minimal TypeScript plugin using Node.js
import * as fs from "node:fs";

class LineReader {
  private buf = Buffer.alloc(0);
  readLine(): string | null {
    while (true) {
      const nl = this.buf.indexOf(0x0a);
      if (nl >= 0) {
        const line = this.buf.subarray(0, nl).toString("utf8");
        this.buf = this.buf.subarray(nl + 1);
        return line;
      }
      const chunk = Buffer.alloc(4096);
      const n = fs.readSync(0, chunk, 0, chunk.length, null);
      if (n <= 0) return null;
      this.buf = Buffer.concat([this.buf, chunk.subarray(0, n)]);
    }
  }
}

function writeMsg(msg: unknown): void {
  fs.writeSync(1, JSON.stringify(msg) + "\n");
}

const reader = new LineReader();

// Handshake
const hello = JSON.parse(reader.readLine()!);
console.error(`Connected to kn9t ${hello.kn9t}`);

writeMsg({
  t: "hello",
  name: "ts-plugin",
  capabilities: [],
  tools: [{
    name: "greet",
    description: "Say hello",
    schema: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
    parallel_safe: true
  }],
  hooks: [],
  events: []
});

// Main loop
while (true) {
  const line = reader.readLine();
  if (!line) break;
  const msg = JSON.parse(line);
  
  if (msg.t === "shutdown") break;
  if (msg.t === "hook" && msg.hook === "tool_call") {
    const name = msg.payload.args?.name ?? "World";
    writeMsg({
      t: "done",
      id: msg.id,
      content: [{ type: "text", text: `Hello, ${name}!` }],
      is_error: false
    });
  }
}
```

## Go Plugin Example

```go
package main

import (
    "bufio"
    "encoding/json"
    "fmt"
    "os"
)

func main() {
    scanner := bufio.NewScanner(os.Stdin)
    
    // Read host hello
    scanner.Scan()
    var hello map[string]interface{}
    json.Unmarshal(scanner.Bytes(), &hello)
    fmt.Fprintf(os.Stderr, "Connected to kn9t %v\n", hello["kn9t"])
    
    // Send our hello
    writeJSON(map[string]interface{}{
        "t": "hello",
        "name": "go-plugin",
        "capabilities": []string{},
        "tools": []map[string]interface{}{{
            "name": "greet",
            "description": "Say hello",
            "schema": map[string]interface{}{
                "type": "object",
                "properties": map[string]interface{}{
                    "name": map[string]string{"type": "string"},
                },
                "required": []string{"name"},
            },
            "parallel_safe": true,
        }},
        "hooks": []string{},
        "events": []string{},
    })
    
    // Main loop
    for scanner.Scan() {
        var msg map[string]interface{}
        json.Unmarshal(scanner.Bytes(), &msg)
        
        if msg["t"] == "shutdown" {
            break
        }
        if msg["t"] == "hook" && msg["hook"] == "tool_call" {
            payload := msg["payload"].(map[string]interface{})
            args := payload["args"].(map[string]interface{})
            name := "World"
            if n, ok := args["name"].(string); ok {
                name = n
            }
            writeJSON(map[string]interface{}{
                "t": "done",
                "id": msg["id"],
                "content": []map[string]string{{"type": "text", "text": fmt.Sprintf("Hello, %s!", name)}},
                "is_error": false,
            })
        }
    }
}

func writeJSON(v interface{}) {
    b, _ := json.Marshal(v)
    fmt.Println(string(b))
}
```

## Installation

### Using `kn9t install-plugins` (Recommended)

Place your plugin in `<project>/plugins/` and run:

```bash
kn9t install-plugins
```

Detection rules (from `cmd_install_plugins.rs`):
| File Present | Plugin Kind | Action |
|--------------|-------------|--------|
| `Cargo.toml` | Rust | `cargo build --release` → copy exe to `~/.kn9t/plugins/` |
| `go.mod` | Go | `go build` → copy exe to `~/.kn9t/plugins/` |
| `package.json` | Node | `npm install && npm run build` → add `[[plugin]]` to config |
| `pyproject.toml` | Python | Add `[[plugin]]` to config (no build needed) |

### Manual Installation

**Binary plugins (Rust/Go):** Copy to `~/.kn9t/plugins/`:

```bash
# Rust
cargo build --release -p my-plugin
cp target/release/my-plugin ~/.kn9t/plugins/

# Go
go build -o my-plugin .
cp my-plugin ~/.kn9t/plugins/
```

**Interpreted plugins (Python/Node):** Add to `~/.kn9t/config.toml`:

```toml
# Python plugin
[[plugin]]
name = "kn9t-my-plugin"
cmd  = ["python", "-m", "kn9t_my_plugin"]

[plugin.env]
PYTHONPATH = "/path/to/plugins/kn9t-my-plugin"

# Node plugin
[[plugin]]
name = "kn9t-node-plugin"
cmd  = ["node", "/path/to/plugins/kn9t-node-plugin/dist/main.js"]
```

### With Environment Variables

```toml
[[plugin]]
name = "my-plugin"
cmd = ["python", "-m", "my_plugin"]
env = { MY_API_KEY = "secret", DEBUG = "1" }
```

## Hot Reload

Reload a plugin without restarting kn9t:

```bash
# Get port and token
PORT=$(cat ~/.kn9t/port)
TOKEN=$(cat ~/.kn9t/token)

# Reload plugin
curl -X POST "http://localhost:$PORT/plugin/my-plugin/reload" \
     -H "Authorization: Bearer $TOKEN"
```

PowerShell:
```powershell
$port = Get-Content "$env:USERPROFILE\.kn9t\port"
$token = Get-Content "$env:USERPROFILE\.kn9t\token"
Invoke-RestMethod -Uri "http://localhost:$port/plugin/my-plugin/reload" `
    -Method POST -Headers @{Authorization="Bearer $token"}
```

The server will:
1. Cancel in-flight calls on that plugin
2. Send `{"t":"shutdown"}` and close pipes
3. Respawn from the original `cmd`
4. Re-handshake and re-register tools

## Testing

### Unit test tools

```rust
#[test]
fn test_echo() {
    let tool = Echo;
    let args = json!({"message": "test"});
    let ctx = ToolCallCtx::mock();  // Fake context
    
    let result = tool.execute(&args, &ctx);
    assert!(!result.is_error);
    assert_eq!(result.content[0].text(), Some("test"));
}
```

### Integration test with host

```bash
# Spawn plugin manually
echo '{"t":"hello","proto":1,"kn9t":"test"}' | ./my-plugin
```

## Reference Files

**Generated — do not edit, `xtask --check` enforces it:**
- `references/api.md` — the whole contract: HTTP routes, SSE, plugin protocol, hooks, host-API ops
- `references/sdk/` — the Rust SDK, byte-for-byte
- `references/README.md` — provenance for the two above

**Canonical schemas:** `schema/plugin.json` (plugin wire), `schema/http.json` (HTTP).

**In this repo:**
- SDK source: `crates/kn9t-plugin-sdk/src/` — `traits.rs`, `wire.rs`, `plugin.rs`, `ctx.rs`
- Worked examples: `plugins/kn9t-tools` (Rust tools), `plugins/kn9t-policy` (Python hooks),
  `plugins/kn9t-compactor` (TypeScript), `plugins/kn9t-agents-md` (Go), `plugins/kn9t-anthropic`
  (Rust provider)
- Protocol rationale: `spec/08b-plugin-redesign.md`

**After changing the SDK or a schema:** `cargo run -p xtask -- generate`

## Plugin TUI Integration

Plugins can register interactive UIs in the TUI. The UI is defined in Lua and sent to the host.

### Architecture

1. **Plugin sends Lua source** via `ui_register_lua` — defines `render(state)` returning a widget tree
2. **Plugin pushes state** via `ui_set_state` — arbitrary JSON that `render(state)` uses
3. **Keys are handled via `kn9t.on_key(key, fn)`** — registered in the Lua source
4. **User interactions sent back** via `kn9t.action("plugin_msg", {plugin, msg})` — plugin handles in main loop

### Key Concepts

| Concept | Description |
|---------|-------------|
| **Session-scoped** | UI is tied to a session, not global. Get `session_id` from hook payloads. |
| **Lazy registration** | Register UI on first hook that provides `session_id` (e.g., `get_steering`). |
| **Focus model** | Keys only reach a focused plugin (F10 cycles, Esc releases). |
| **Widget tree** | `render(state)` returns `{type, children, ...}` — see widget reference below. |

### Wire Protocol

**Register UI (once per session):**
```json
{
  "t": "request",
  "id": 1,
  "op": "ui_register_lua",
  "payload": {
    "session": "<session_id>",
    "source": "-- Lua code...",
    "placement": "main",
    "title": "My Plugin"
  }
}
```

**Push state (on every update):**
```json
{
  "t": "request",
  "id": 2,
  "op": "ui_set_state",
  "payload": {
    "session": "<session_id>",
    "state": { "items": [...], "cursor": 0 }
  }
}
```

### Lua UI Template

```lua
-- View state (survives state pushes)
local V = { cursor = 0, input = "" }

-- Update view state from host pushes
function on_state(s)
    V.items = s.items or {}
    V.cursor = s.cursor or 0
end

-- Helper to send messages back to plugin
local function send(t, extra)
    local msg = { t = t }
    if extra then for k, v in pairs(extra) do msg[k] = v end end
    kn9t.action("plugin_msg", { plugin = "my-plugin", msg = msg })
end

-- Register key handlers (NOT a global on_key function!)
kn9t.on_key("j", function()
    send("cursor_down")
    return true  -- consumed
end)

kn9t.on_key("k", function()
    send("cursor_up")
    return true
end)

kn9t.on_key("Enter", function()
    send("select_item")
    return true
end)

kn9t.on_key("Escape", function()
    return false  -- Let Esc release focus to TUI
end)

-- Render the UI
function render(s)
    on_state(s)
    local out = {}
    
    for i, item in ipairs(V.items) do
        local prefix = (i - 1 == V.cursor) and "> " or "  "
        local fg = (i - 1 == V.cursor) and "cyan" or "white"
        table.insert(out, {
            type = "text",
            content = prefix .. item,
            fg = fg,
            size = { fixed = 1 }
        })
    end
    
    -- Help bar at bottom
    table.insert(out, { type = "spacer", size = { flex = 1 } })
    table.insert(out, {
        type = "text",
        spans = {
            { text = "[j/k]", fg = "cyan" },
            { text = " nav  ", fg = "darkgray" },
            { text = "[Enter]", fg = "cyan" },
            { text = " select", fg = "darkgray" },
        },
        size = { fixed = 1 }
    })
    
    return { type = "split", direction = "vertical", children = out }
end
```

### Widget Reference

| Type | Properties | Description |
|------|------------|-------------|
| `text` | `content`, `fg`, `bold`, `spans` | Single line of text |
| `split` | `direction`, `children` | Vertical/horizontal layout |
| `spacer` | `size` | Flexible space |
| `box` | `title`, `border`, `child` | Bordered container |

**Spans** (styled text segments):
```lua
{ type = "text", spans = {
    { text = "[key]", fg = "cyan" },
    { text = " description", fg = "darkgray" }
}}
```

**Size** options:
- `{ fixed = N }` — exactly N rows/cols
- `{ flex = N }` — proportional space
- `{ min = N, max = M }` — constrained

### Python Example with UI

```python
#!/usr/bin/env python3
import json
import sys
from dataclasses import dataclass, asdict

# State
@dataclass
class State:
    items: list = None
    cursor: int = 0
    
    def __post_init__(self):
        self.items = self.items or ["Item 1", "Item 2", "Item 3"]

state = State()
session_id = None
_request_id = 0

# Lua UI source
UI_LUA = r'''
local V = { cursor = 0 }

function on_state(s)
    V.items = s.items or {}
    V.cursor = s.cursor or 0
end

local function send(t)
    kn9t.action("plugin_msg", { plugin = "my-plugin", msg = { t = t } })
end

kn9t.on_key("j", function() send("down"); return true end)
kn9t.on_key("k", function() send("up"); return true end)
kn9t.on_key("Escape", function() return false end)

function render(s)
    on_state(s)
    local out = {}
    for i, item in ipairs(V.items) do
        local prefix = (i - 1 == V.cursor) and "> " or "  "
        table.insert(out, { type = "text", content = prefix .. item,
                           fg = (i - 1 == V.cursor) and "cyan" or "white",
                           size = { fixed = 1 } })
    end
    return { type = "split", direction = "vertical", children = out }
end
'''

def read_msg():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def write_msg(msg):
    sys.stdout.write(json.dumps(msg, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def send_request(op, payload):
    global _request_id
    _request_id += 1
    write_msg({"t": "request", "id": _request_id, "op": op, "payload": payload})

def register_ui():
    if not session_id:
        return
    send_request("ui_register_lua", {
        "session": session_id,
        "source": UI_LUA,
        "placement": "main",
        "title": "My Plugin",
    })

def send_ui_state():
    if not session_id:
        return
    send_request("ui_set_state", {
        "session": session_id,
        "state": asdict(state),
    })

def handle_plugin_msg(msg):
    t = msg.get("t", "")
    if t == "down":
        state.cursor = min(state.cursor + 1, len(state.items) - 1)
    elif t == "up":
        state.cursor = max(state.cursor - 1, 0)
    send_ui_state()

def run():
    global session_id
    ui_registered = False
    
    # Handshake
    hello = read_msg()
    if not hello or hello.get("t") != "hello":
        return
    
    write_msg({
        "t": "hello",
        "name": "my-plugin",
        "capabilities": [],
        "hooks": ["get_steering"],  # Need a hook to get session_id
        "tools": [],
    })
    
    while True:
        msg = read_msg()
        if not msg:
            break
        
        t = msg.get("t", "")
        
        if t == "shutdown":
            break
        
        elif t == "hook":
            hook_id = msg.get("id", 0)
            payload = msg.get("payload", {})
            
            # Get session_id from hook payload
            if not session_id:
                session_id = payload.get("session_id")
            
            # Register UI on first hook
            if session_id and not ui_registered:
                register_ui()
                send_ui_state()
                ui_registered = True
            
            # Reply to hook
            write_msg({"t": "result", "id": hook_id, "messages": []})
        
        elif t == "plugin_msg":
            handle_plugin_msg(msg.get("msg", {}))

if __name__ == "__main__":
    run()
```

### Common Mistakes

| Mistake | Correct Approach |
|---------|------------------|
| Using `"t": "host_api"` | Use `"t": "request"` |
| Calling `ui_register_lua` at startup | Wait for first hook with `session_id` |
| Defining `function on_key(key)` | Use `kn9t.on_key("j", fn)` per key |
| Missing `session` in payload | Include `"session": session_id` in every request |
| Missing `"state"` wrapper | `ui_set_state` payload needs `{"session": ..., "state": ...}` |

### Rust SDK Example

```rust
use kn9t_plugin_sdk::{Plugin, PluginHook};
use kn9t_plugin_sdk::ctx::HookCtx;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

static UI_REGISTERED: AtomicBool = AtomicBool::new(false);

const UI_LUA: &str = r#"
local V = { count = 0 }
function on_state(s) V.count = s.count or 0 end
kn9t.on_key("j", function()
    kn9t.action("plugin_msg", { plugin = "counter", msg = { t = "inc" } })
    return true
end)
function render(s)
    on_state(s)
    return { type = "text", content = "Count: " .. V.count, fg = "cyan" }
end
"#;

struct Counter { count: u32 }

impl PluginHook for Counter {
    fn hooks(&self) -> Vec<&'static str> {
        vec!["get_steering"]
    }
    
    fn call_with_ctx(&self, _hook: &str, _payload: &Value, ctx: &HookCtx) -> Value {
        // Register UI on first call
        if !UI_REGISTERED.swap(true, Ordering::SeqCst) {
            let _ = ctx.host.call("ui_register_lua", json!({
                "source": UI_LUA,
                "placement": "main",
                "title": "Counter",
            }));
        }
        
        // Push state
        let _ = ctx.host.call("ui_set_state", json!({
            "state": { "count": self.count }
        }));
        
        json!({"messages": []})
    }
}
```

## Common Patterns

### Tool with file effects

```rust
ToolSpec {
    effects: vec![
        Effect { field: "path".into(), kind: EffectKind::Write },
    ],
    policy: ToolPolicy {
        default_policy: DefaultPolicy::Ask,
        ..Default::default()
    },
    ..
}
```

### Hook that injects context

```rust
impl PluginHook for Injector {
    fn hooks(&self) -> Vec<&'static str> { vec!["get_steering"] }
    
    fn call(&self, _: &str, _: &Value) -> Value {
        json!({
            "messages": [{
                "role": "user",
                "silent": true,
                "content": [{"type": "text", "text": "Remember: be concise."}]
            }]
        })
    }
}
```

### Event logging

```rust
impl PluginEventSink for Logger {
    fn event_filter(&self) -> Vec<&'static str> { vec!["*"] }
    
    fn on_event(&self, kind: &str, event: &Value) {
        eprintln!("[{}] {:?}", kind, event);
    }
}
```
