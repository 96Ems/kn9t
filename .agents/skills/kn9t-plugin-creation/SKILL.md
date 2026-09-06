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

> **⚠️ Always read `schema/plugin.json` for the canonical wire protocol.**
> This skill provides guidance, but the schema file is the source of truth.

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

## Wire Protocol v2

### Handshake

```
Host  → Plugin:  {"t":"hello","proto":1,"kn9t":"0.1.0"}
Plugin → Host:   {"t":"hello","name":"my-plugin","capabilities":["streaming","cancelable"],"tools":[...],"hooks":[...],"events":[...]}
```

### Capabilities

| Flag | Meaning |
|------|---------|
| `streaming` | Plugin may send `chunk` messages before `done` |
| `cancelable` | Plugin listens for `cancel` messages |
| `host_api` | Plugin may call host ops (provider_complete, session_fork, etc.) |
| `compactor` | Plugin provides context compaction |

### Tool Call Flow

**Non-streaming:**
```
Host → Plugin:  {"t":"hook","id":7,"hook":"tool_call","payload":{"tool":"echo","args":{"message":"hi"},"session":"01..."}}
Plugin → Host:  {"t":"done","id":7,"content":[{"type":"text","text":"hi"}],"is_error":false}
```

**Streaming (with `streaming` capability):**
```
Host → Plugin:  {"t":"hook","id":7,"hook":"tool_call","payload":{...}}
Plugin → Host:  {"t":"chunk","id":7,"text":"partial output..."}
Plugin → Host:  {"t":"chunk","id":7,"text":"more output..."}
Plugin → Host:  {"t":"done","id":7,"content":[{"type":"text","text":"full output"}],"is_error":false}
```

### Available Hooks

| Hook | Payload | Reply | Purpose |
|------|---------|-------|---------|
| `before_tool_call` | `{tool, args, cwd}` | `{action: allow/deny/replace}` | Gate tool execution |
| `after_tool_call` | `{tool, args, result}` | `{action: keep/replace}` | Transform output |
| `before_request` | `{messages, model, system}` | `{action: keep/replace}` | Modify LLM request |
| `should_stop_after_turn` | `{stop, usage, turn}` | `{action: continue/stop}` | Control loop termination |
| `prepare_next_turn` | `{stop, usage}` | `{action: keep/patch}` | Switch model/thinking |
| `get_steering` | `null` | `{messages: [...]}` | Inject context |
| `get_followup` | `null` | `{messages: [...]}` | Queue follow-up messages |
| `get_api_key` | `{provider}` | `{key: "..." or null}` | Provide API keys |

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

Available ops: `provider_complete`, `session_read`, `tool_execute`, `session_fork`, `session_prompt`

## Python Plugin Example

```python
#!/usr/bin/env python3
"""Minimal Python plugin."""
import json
import sys

def main():
    # Read host hello
    hello = json.loads(sys.stdin.readline())
    assert hello["t"] == "hello"
    
    # Send our hello
    print(json.dumps({
        "t": "hello",
        "name": "python-plugin",
        "capabilities": [],
        "tools": [{
            "name": "greet",
            "description": "Say hello",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "parallel_safe": True
        }],
        "hooks": [],
        "events": []
    }), flush=True)
    
    # Main loop
    while True:
        line = sys.stdin.readline()
        if not line:
            break
        msg = json.loads(line)
        
        if msg["t"] == "shutdown":
            break
        elif msg["t"] == "hook" and msg["hook"] == "tool_call":
            name = msg["payload"]["args"].get("name", "World")
            print(json.dumps({
                "t": "done",
                "id": msg["id"],
                "content": [{"type": "text", "text": f"Hello, {name}!"}],
                "is_error": False
            }), flush=True)

if __name__ == "__main__":
    main()
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

Plugins are discovered from `~/.kn9t/plugins/`:

```bash
# Rust plugin
cargo build --release -p my-plugin
cp target/release/my-plugin ~/.kn9t/plugins/

# Python plugin
chmod +x my_plugin.py
cp my_plugin.py ~/.kn9t/plugins/
```

Or pin in config (`~/.kn9t/config.toml`):

```toml
[[plugin]]
name = "my-plugin"
cmd = ["python", "-m", "my_plugin"]
env = { MY_API_KEY = "..." }
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

**Schemas (source of truth):**
- `schema/plugin.json` — **Canonical wire protocol schema** (read this for exact message formats)
- `schema/http.json` — HTTP API schema

**Specifications:**
- `spec/08b-plugin-redesign.md` — Full protocol specification with rationale

**Rust SDK:**
- `crates/kn9t-plugin-sdk/src/traits.rs` — Trait definitions (PluginTool, PluginProvider, etc.)
- `crates/kn9t-plugin-sdk/src/wire.rs` — Wire message types
- `crates/kn9t-plugin-sdk/src/plugin.rs` — Plugin container and main loop

**Example Plugins:**
- `plugins/kn9t-tools/` — Rust: bash, read, write, edit tools
- `plugins/kn9t-anthropic/` — Rust: LLM provider plugin
- `plugins/kn9t-subagent/` — TypeScript: sub-agent spawning with host_api
- `plugins/kn9t-skills/` — Python: Agent Skills integration
- `plugins/kn9t-mcp/` — TypeScript: MCP bridge

**Regenerate schemas after SDK changes:**
```bash
cargo run -p xtask -- generate
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
