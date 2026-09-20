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

A plugin with the `host_api` capability calls back with `request`, answered by `api_result`.
The Rust SDK wraps this as `ctx.host.call(op, payload)`; a hand-rolled TS/Python client writes
the `request` itself.

`ctx.host` **auto-injects the enclosing `tool_call`'s `session`** into any payload that lacks
one. A hand-rolled client does not get that: send `session` yourself, or the host sees `None`
and falls back to the server's cwd and default model.

A sub-agent *is* a session running a turn — there is no separate concept:

| op | what it does |
|---|---|
| `session_create` | A brand-new independent session. Optional `model`, `cwd`. |
| `session_fork` | A child of the caller's session. `copy_events: false` gives a bare, task-only child; `copy_events: true` copies the parent transcript and captures `budget_usd` in the fork. |
| `session_prompt` | One synchronous turn on that session with `text`, an optional `tools` subset, and `timeout_s`. Returns `{session, result}`, `result` being the final assistant text. |

```rust
// 1. a bare child, 2. one turn, 3. read the text back.
let child = ctx.host.call("session_fork",
    json!({ "copy_events": false, "budget_usd": 0.5 }))?["session"]
    .as_str().unwrap().to_string();
let r = ctx.host.call("session_prompt", json!({
    "session": child,
    "text": "Summarize the auth module",
    "tools": ["read", "bash"],   // tool subset; omit to inherit the session's tools
    "timeout_s": 120,
}))?;
let answer = r["result"].as_str().unwrap_or_default();
```

**Do not fork with `copy_events: true` to run a task from inside a tool call.** The parent's
in-flight assistant message — the one carrying the very tool call being executed — is already
in the transcript, and its `tool_result` does not exist yet. The child inherits a dangling
tool call, and a real provider rejects it (OpenAI and Anthropic both require a `tool_result`
immediately after `tool_use`). Pass the context the child needs inside `text` instead.

**`tools` is a filter, not an addition:** the child sees exactly the named tools from the live
registry. Omit it and the child gets everything visible — including `subagent`, so a
sub-agent can fork its own children.

**Cancel propagates:** Esc on the parent aborts the sub-agent; the child has its own `Cancel`,
so it can never cancel its parent.

All host-API ops are listed in [`references/api.md`](references/api.md).

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

A plugin is a subprocess speaking NdJSON, so the honest test **spawns the built binary and
plays the host**: send `hello`, answer each `request` it makes, assert the `done` it writes.
That catches what a unit test cannot — a reply field named wrong, an `api_result` read and
discarded, a hook path that never returns. Keep the pure logic in a plain function and unit
test that separately.

```js
const proc = spawn("node", ["dist/main.js"], { stdio: ["pipe", "pipe", "inherit"] });
send({ t: "hello", proto: 1, kn9t: "test" });   // host → plugin, then a hook
// … read the plugin's hello, send the hook, answer its `request`s.
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

A plugin draws in the TUI by sending Lua source. There is no widget registry and no
per-plugin Rust: the layout draws the frame (border, title, focus ring) and the plugin draws
the content inside it.

### Lifecycle

1. **Register** with `ui_register_lua`. `source` defines `render(state)` returning a widget
   tree. `placement` is a *request*: `main`, `sidebar`, `bottom`, `status`. `bottom`
   reserves rows between the transcript and the prompt — the slot for a question that must
   not cover what it is about.
2. **Push data** with `ui_set_state` — arbitrary JSON, handed to `render(state)`.
3. **Handle input** in Lua with `kn9t.on_key` / `kn9t.on_text`.
4. **Tear down** with `ui_clear` when the view has nothing left to show. An idle session must
   keep no panel; a view that registers once and never clears is a leak.

Register lazily: the `session_id` arrives with the first hook, not at handshake. A view that
is only useful for the duration of one operation (a question, a progress bar) must clear
itself when that operation ends, so the next one re-registers cleanly.

### What a view may call

| Call | Purpose |
|---|---|
| `kn9t.on_key(key, fn)` | Bind a key while the view holds focus. Return `true` to consume, `false` to fall through. Exact `on_key` bindings are matched **before** `on_text`. |
| `kn9t.on_text(fn)` | Every printable character, Space included. Return `false` to fall through. This is how text entry works — never bind one `on_key` per glyph. |
| `kn9t.on_click(id, fn)` | A click on a widget carrying `id=`. |
| `kn9t.respond(payload)` | Answer the `interaction_request` this view renders. The host owns the transport (`POST /ui-respond`) and the Esc cancel. |
| `kn9t.notify({event = "...", ...})` | Send an event to *your plugin process*. Your plugin must subscribe to `ui_interaction` events. |
| `kn9t.insert_input(text)` | Append text to the user's prompt. |

There is **no other reverse channel**: the Lua runs in the TUI process, so view state
(cursor, input buffer, toggles) lives in the Lua and is mutated by its own handlers. Do not
expect to round-trip it through your plugin process.

### Rules that make a view behave

* **Return content, not a frame.** The layout already wraps every view in a `box`. A view
  whose top-level node is `{type="box"}` nests two borders and prints two titles.
* **Declare your placement and size.** Do not rely on the default; say
  `sidebar`/`bottom`/`main`, and pass `rows` so the slot fits the content when focused.
* **Reserve the height that must be visible.** A `{flex=1}` row can be given zero height, and
  the content silently disappears. Size the rows that matter with `{fixed=N}`, and make the
  requested `rows` match what `render` returns.
* **Advertise the keys.** A footer such as
  `up/down move - Space toggle - Enter submit - Esc cancel` is the difference between a
  usable panel and a guess.
* **A focused `bottom` view is an interaction:** Esc cancels it, so the view cannot bind Esc
  itself and should offer an explicit cancel answer through `kn9t.respond`.

### Wire protocol

**Register (once, lazily):**
```json
{"t":"request","id":1,"op":"ui_register_lua","payload":{
  "session":"<id>", "source":"-- Lua", "placement":"bottom", "title":"Question", "rows":8}}
```

**Push state (cheap, per update):**
```json
{"t":"request","id":2,"op":"ui_set_state","payload":{"session":"<id>","state":{}}}
```

**Tear down:**
```json
{"t":"request","id":3,"op":"ui_clear","payload":{"session":"<id>"}}
```

The three UI ops are **fire-and-forget**: none of their replies is needed. If you hand-roll
the transport, do not issue a *blocking* call while another request is in flight — a
blocking reader that discards replies it is not waiting for will swallow the other request's
reply and hang. Either buffer every reply you do not consume, or keep at most one request
outstanding.

### Lua template

```lua
-- View state: lives in the TUI process, reset when the data's identity changes.
local V = { cursor = 1, items = {}, text = "" }
local seen = nil

kn9t.on_key("Up",   function() V.cursor = math.max(1, V.cursor - 1) return true end)
kn9t.on_key("Down", function() V.cursor = math.min(#V.items, V.cursor + 1) return true end)
kn9t.on_key("Enter", function()
    kn9t.respond({ value = V.items[V.cursor] })
    return true
end)
kn9t.on_text(function(ch)
    if not V.typing then return false end   -- fall through when not typing
    V.text = V.text .. ch
    return true
end)

function render(state)
    state = state or {}
    -- The host re-renders every frame, so reset on the data's identity, not per call.
    if state.question ~= seen then
        seen = state.question
        V.items = state.items or {}
        V.cursor = 1
        V.text = ""
    end

    local out = {}
    table.insert(out, { type = "text", content = state.question or "",
                        wrap = true, size = { fixed = 2 } })
    for i, item in ipairs(V.items) do
        table.insert(out, {
            type = "text",
            content = (i == V.cursor and "> " or "  ") .. item,
            fg = (i == V.cursor) and "cyan" or "white",
            size = { fixed = 1 },
        })
    end
    table.insert(out, { type = "text", content = "up/down move - Enter submit - Esc cancel",
                        fg = "darkgray", size = { fixed = 1 } })
    return { type = "split", direction = "vertical", children = out }
end
```

### Widget reference

| Type | Properties |
|---|---|
| `text` | `content`, `spans`, `fg`, `bold`, `wrap`, `markdown`, `syntax`, `align` |
| `split` | `direction` (`vertical`/`horizontal`), `children` |
| `list` | `items`, `selected` (**0-based**), `offset` |
| `box` | `title`, `border`, `border_fg`, `padding`, `child` |
| `gauge` | `frac`, `label`, `fg`, `filled`, `empty` |
| `input` | `id` — a text field whose editing Rust owns |
| `float` | `x`, `y`, `w`, `h`, `clear`, `child` — a popup over the layout |
| `spacer` | `size` |

Any node also accepts `id="..."` to make it clickable (`kn9t.on_click`). Sizing is
`size = { fixed = N } | { percent = N } | { flex = N }`. An unknown `type` logs and renders
empty rather than erroring.

A `text` node (and each `list` item) is styled text: give it `content = "..."` or a bare
`text = "..."`, or `spans = {{ text = "...", fg = "..." }, ...}` for more than one colour.
An item's own `fg` overrides the node's. If a row renders blank, the key is wrong — a list
item that is silently empty is a bug that hides inside a rendered frame.

**Gotcha:** `list.selected` is **0-based**, while a cursor you keep yourself is usually
1-based (it indexes the items array). Passing your cursor straight through makes the first row
unreachable — and the rendered text alone cannot show it, because the row is highlighted in
the wrong place, not missing.

### Common mistakes

| Mistake | Correct approach |
|---|---|
| `"t": "host_api"` | Use `"t": "request"` |
| Registering the UI at startup | Wait for the first hook: it carries `session_id` |
| `function on_key(key)` | `kn9t.on_key("j", fn)` per key |
| One `on_key` binding per printable character | One `kn9t.on_text(fn)` handler |
| A blocking host call from inside another request's callback | Fire-and-forget, or buffer replies |
| Missing `session` in a payload | Include it in every request |
| `ui_set_state` without the `state` wrapper | `{"session": ..., "state": {}}` |
| Returning a top-level `box` | Return content; the layout draws the frame |
| Never calling `ui_clear` | Clear when the view has nothing to show |

### Rust SDK shape

The SDK exposes the same calls on the hook/tool context; there is no separate UI API:

```rust
use kn9t_plugin_sdk::{Plugin, PluginHook};
use serde_json::json;

const UI_LUA: &str = r#"
kn9t.on_key("j", function() kn9t.notify({ event = "down" }) return true end)
kn9t.on_text(function(ch) --[[ ... ]] return true end)
function render(s) return { type = "text", content = "hi", size = { fixed = 1 } } end
"#;

struct Panel;

impl PluginHook for Panel {
    fn hooks(&self) -> Vec<&'static str> { vec!["get_steering"] }

    fn call_with_ctx(&self, _hook: &str, payload: &serde_json::Value, ctx: &kn9t_plugin_sdk::ctx::HookCtx) -> serde_json::Value {
        // `get_steering` runs each turn, so this is a reliable place to obtain session_id.
        if let Some(session) = payload.get("session_id").and_then(|v| v.as_str()) {
            let _ = ctx.host.call("ui_register_lua", json!({
                "session": session, "source": UI_LUA, "placement": "bottom", "rows": 6,
            }));
            let _ = ctx.host.call("ui_set_state", json!({ "session": session, "state": {} }));
        }
        json!({ "messages": [] })
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
