# 08b — Plugin System Redesign (Protocol v2)

**Status:** replaces the protocol section of `08-plugin.md`. All `R-PLUG-*` requirements
from stage 08 that touch the wire format, the SDK, or the crate layout are superseded by
this file. Behavioural requirements (composition classes, failure postures, hook surface,
spawn tool) remain valid and are not repeated here.

**Decision log:** this spec was derived from a full design-challenge session recorded in
`CHANGELOG.md` (2026-08-26 — Plugin redesign). Every branch in that session maps to a
section here.

**Language neutrality:** this document is the canonical protocol reference. It is written
in terms of JSON structures and observable behaviour, not Rust types. Any language can
implement a conforming plugin. The Rust crate `kn9t-plugin-sdk` is one reference
implementation of this protocol; Python, Node, and Go SDKs are tracked follow-ups that
MUST conform to the same protocol without changes.

**SDK availability:**

| Language | Package | Status |
|---|---|---|
| Rust | `kn9t-plugin-sdk` on crates.io | v1 target |
| Python | `kn9t-plugin` on PyPI | future |
| Node / TypeScript | `@kn9t/plugin` on npm | future |
| Go | `kn9t.dev/plugin` module | future |

Any conforming implementation of §2 (wire protocol) is a valid SDK regardless of language.

---

## 0. Motivation

Stage 08 implemented a plugin host but exposed three gaps that prevent the built-in tools
(`bash`, `read`, `edit`) from being moved into the plugin system:

1. **No streaming** — the protocol has one request → one response. `bash` needs to emit
   progress lines while the child process runs; a provider plugin needs to stream tokens.
2. **No cancellation** — there is no way for the host to interrupt a running plugin call.
   `bash` polls `Cancel` in-process; over a pipe this is impossible without a new message.
3. **No provider plugin type** — providers are hard-wired in-process. The plugin system
   cannot replace or extend a provider.

The fix is a protocol revision (v2) plus a new SDK crate that hides all wire ceremony from
plugin authors, with the explicit long-term goal of publishing `kn9t-plugin-sdk` to
crates.io so the community can build and share kn9t plugins as ordinary Rust crates.

---

## 1. Crate layout

```
crates/
  kn9t-plugin-sdk/              # zero workspace deps; publishable
  kn9t-plugin/                  # host side — protocol v2

plugins/                        # EXTERNAL standalone crates (outside workspace)
  kn9t-tools/                   # bash + read + write + edit as a plugin binary (auto-discovered)
  kn9t-custom-provider/         # example external provider plugin
  kn9t-anthropic/               # anthropic provider plugin
```

All plugins are **external** standalone crates with an empty `[workspace]` and a
`path = "../../crates/kn9t-plugin-sdk"` dependency only. The repo's `plugins/`
directory is **build source**; `~/.kn9t/plugins/` is the **install target**
(ADR-0004). Bootstrap copies `plugins/kn9t-tools` into `~/.kn9t/plugins/` on first
run when a build artifact is found.

> **R-PLUG2-010**
> `kn9t-plugin-sdk` MUST have zero workspace member dependencies. Its only external deps
> are `serde` and `serde_json`. This keeps it independently publishable and gives plugin
> authors a single small dep.
> **Accept:** `cargo tree -p kn9t-plugin-sdk` contains no path to any `kn9t-*` crate.

> **R-PLUG2-020**
> `plugins/kn9t-tools` (the default tools plugin) MUST depend only on
> `kn9t-plugin-sdk`. It MUST compile to a binary (`[[bin]]`) named `kn9t-tools`.
> It MUST NOT link the old `kn9t-tools` library crate (now removed). Its binary is
> installed to `~/.kn9t/plugins/` (the user plugin dir), not shipped as a
> sibling of the server executable.
> **Accept:** `cargo tree -p kn9t-tools` (run as `cd plugins/kn9t-tools && cargo tree`) contains no path to any `kn9t-*` crate
> except `kn9t-plugin-sdk`; `ls ~/.kn9t/plugins/kn9t-tools` exists after bootstrap or `cd plugins/kn9t-tools && cargo build` + install.

> **R-PLUG2-030**
> `kn9t-plugin` (host side) retains `kn9t-core` as its single workspace dep (GI-1).

---

## 2. Wire protocol v2

### 2.0 Transport

- **Channel:** the plugin's standard input (stdin) and standard output (stdout).
- **Framing:** every message is one JSON object serialised on a single line, terminated by
  a newline character (`\n`, U+000A). Carriage returns inside a line are not permitted.
- **Encoding:** UTF-8 throughout. JSON strings MUST be valid UTF-8.
- **Multiplexing:** multiple concurrent calls share the same stdin/stdout pair, identified
  by the `"id"` field. The host and plugin MUST treat message ordering per `id` as FIFO;
  messages for different `id`s MAY interleave freely.
- **Standard error (stderr):** reserved for human-readable diagnostic text. The host MAY
  log it; it MUST NOT parse it as protocol messages.
- **Backpressure:** if the host's stdin pipe buffer fills (because the plugin is not
  reading), the host blocks. Plugin implementations MUST read stdin continuously — spinning
  a dedicated reader thread is the standard pattern.

Protocol version is negotiated via capability flags in the hello (§2.1) — no integer
version bump — so a v1 plugin (no `"streaming"` capability declared) continues to work
alongside v2 plugins on the same host.

### 2.1 Capability flags

The plugin declares its capabilities in the hello reply:

```json
{
  "t": "hello",
  "name": "my-plugin",
  "capabilities": ["streaming", "cancelable"],
  "tools":    [{"name":"bash","description":"...","schema":{}}],
  "provider": {"id":"my-llm","models":[{"id":"...","ctx_window":128000}]},
  "hooks":    ["before_tool_call","after_tool_call"],
  "events":   ["MessageAppended","TurnEnded"]
}
```

| capability | meaning |
|---|---|
| `streaming` | plugin may send `chunk` messages before `done` |
| `cancelable` | plugin listens for `cancel` messages on a dedicated thread |

A plugin without `"streaming"` MUST reply with `result` only (v1 behaviour, unchanged).
A plugin without `"cancelable"` will not receive `cancel` messages; on abort the host
applies the hook's failure posture after the timeout expires and then sends `shutdown`.

Unrecognised capability strings MUST be ignored by the host. Future capabilities will be
added without a protocol version bump.

### 2.2 Message type table

**Host → Plugin:**

| `t` | when | payload |
|---|---|---|
| `hello` | once, on spawn | `{"proto":1,"kn9t":"<ver>"}` |
| `hook` | per hook invocation | `{"id":N,"hook":"<name>","payload":{}}` |
| `event` | fire-and-forget bus event | `{"id":N,...event fields...}` |
| `cancel` | abort a running call | `{"id":N}` |
| `shutdown` | graceful stop | — |

**Plugin → Host:**

| `t` | when | payload |
|---|---|---|
| `hello` | once, in reply to host hello | declaration (see §2.1) |
| `result` | atomic (non-streaming) reply | `{"id":N,...reply fields...}` |
| `chunk` | partial streaming output | `{"id":N,...partial fields...}` |
| `done` | final streaming reply + accounting | `{"id":N,...final fields...}` |

> **R-PLUG2-040**
> A plugin that declared `streaming` MUST use `chunk`/`done` for any call that produces
> incremental output. It MAY still use `result` for calls that complete atomically (e.g.
> `get_api_key`). `chunk` and `done` for the same `id` MUST arrive in order.
> **Accept:** `cargo test plug2::streaming_tool_chunks_then_done`

> **R-PLUG2-050**
> When the host sends `{"t":"cancel","id":N}`, a plugin that declared `cancelable` MUST
> stop work for call `id:N` and send `{"t":"done","id":N,"is_error":true,"content":[
> {"type":"text","text":"cancelled"}]}` within its `before_tool_call` timeout window.
> A non-cancelable plugin receives no `cancel` message; the host applies the hook's
> failure posture after the timeout expires.
> **Accept:** `cargo test plug2::cancel_in_flight`

### 2.3 Tool call wire shapes

A tool call is a hook invocation with `hook: "tool_call"`. The host sends:

```json
{"t":"hook","id":7,"hook":"tool_call","payload":{"name":"bash","args":{"cmd":"ls"}}}
```

**Atomic reply (non-streaming):**
```json
{"t":"result","id":7,"content":[{"type":"text","text":"file1\nfile2"}],"is_error":false}
```

**Streaming reply (`streaming` capability declared):**
```json
{"t":"chunk","id":7,"text":"file1\n"}
{"t":"chunk","id":7,"text":"file2\n"}
{"t":"done","id":7,"content":[{"type":"text","text":"file1\nfile2"}],"is_error":false}
```

The host accumulates `chunk.text` fields for live display (TUI progress). The final
`done.content` is the authoritative result stored in the transcript — it MAY differ from
the accumulated chunks (e.g. the tool may add structured details only at the end).

### 2.4 Provider call wire shapes

A provider call uses `hook: "provider_complete"`. The host sends the full `Request`:

```json
{"t":"hook","id":12,"hook":"provider_complete","payload":{
  "model":{"provider":"my-llm","id":"gpt-4o"},
  "messages":[...],
  "system":"...",
  "tools":[...],
  "policy":{...}
}}
```

The plugin streams token deltas as `chunk` messages, followed by a single `done`:

```json
{"t":"chunk","id":12,"kind":"text_delta","text":"Hello"}
{"t":"chunk","id":12,"kind":"text_delta","text":" world"}
{"t":"chunk","id":12,"kind":"thinking_delta","thinking":"Let me think...","signature":"sig123"}
{"t":"chunk","id":12,"kind":"tool_use_start","call_id":"c1","name":"bash","args_json":""}
{"t":"chunk","id":12,"kind":"tool_use_delta","call_id":"c1","args_json":"{\"cmd\":"}
{"t":"chunk","id":12,"kind":"tool_use_delta","call_id":"c1","args_json":"\"ls\"}"}
{"t":"done","id":12,"stop":"end_turn",
 "usage":{"input":10,"output":4,"cache_read":0,"cache_write":0},"cost_usd":0.00012}
```

**Chunk kinds — exhaustive table:**

| `kind` | required fields | meaning |
|---|---|---|
| `text_delta` | `text: string` | incremental assistant text |
| `thinking_delta` | `thinking: string`, `signature: string` | incremental thinking block; signature is provider-opaque |
| `tool_use_start` | `call_id: string`, `name: string`, `args_json: string` | start of a tool call; `args_json` is the first fragment (may be `""`) |
| `tool_use_delta` | `call_id: string`, `args_json: string` | continuation of tool arguments JSON |
| `input_tokens` | `count: integer` | pre-generation token count (sent before any text, optional) |

No other `kind` values are defined. The host MUST ignore unknown kinds to allow future
extension without breaking existing plugins.

**`done` fields for a provider call:**

| field | type | required | meaning |
|---|---|---|---|
| `stop` | string | yes | stop reason: `"end_turn"`, `"max_tokens"`, `"stop_sequence"`, `"tool_use"` |
| `usage` | object | yes | `{"input":N,"output":N,"cache_read":N,"cache_write":N}` — all integers, token counts |
| `cost_usd` | number | no | provider-computed cost in USD; omit if unknown |

The host assembles the streamed chunks into a complete `Message` and records a
`UsageRecorded` event from the `done` fields, exactly as it does for in-process providers.

> **R-PLUG2-060**
> A provider plugin MUST emit only the `kind` values in the table above. The `done`
> message MUST include `stop` and `usage`. Unknown kinds arriving at the host MUST be
> silently ignored (forward compatibility).
> **Accept:** `cargo test plug2::provider_chunks_assembled`

### 2.5 Complete message schema reference

This section is the single authoritative schema for every message in the protocol. It is
sufficient to implement a conforming plugin in any language without reading any kn9t
source code.

All field names are lowercase snake_case strings. All `id` values are unsigned 64-bit
integers. The `"t"` field is always present and identifies the message type.

---

**`host_hello`** — first message sent by the host on plugin spawn.

```json
{
  "t": "hello",
  "proto": 1,
  "kn9t": "<semver string>"
}
```

---

**`plugin_hello`** — plugin's reply; declares everything the plugin provides.

```json
{
  "t": "hello",
  "name": "<plugin name string>",
  "capabilities": ["streaming", "cancelable"],
  "tools": [
    {
      "name": "<string>",
      "description": "<string>",
      "schema": { "<JSON Schema object>" },
      "parallel_safe": false
    }
  ],
  "provider": {
    "id": "<string>",
    "models": [
      { "id": "<string>", "ctx_window": 128000, "price": {
        "input": 0.0, "output": 0.0, "cache_read": 0.0, "cache_write": 0.0
      }}
    ]
  },
  "hooks": ["before_tool_call", "after_tool_call"],
  "events": ["MessageAppended", "TurnEnded"]
}
```

`tools`, `provider`, `hooks`, `events` are all optional — omit keys that are not used.
`capabilities` is optional; omit or use `[]` for a v1-only plugin.
`parallel_safe` defaults to `false` if omitted.
`price` fields are cost per million tokens in USD; omit or use `0.0` if unknown.

---

**`hook`** — host invokes a hook or tool call on the plugin.

```json
{ "t": "hook", "id": 7, "hook": "<hook_name or tool_call or provider_complete>", "payload": {} }
```

Hook names: `before_tool_call`, `after_tool_call`, `before_request`,
`should_stop_after_turn`, `prepare_next_turn`, `get_steering`, `get_followup`,
`get_api_key`, `tool_call`, `provider_complete`.

---

**`event`** — host delivers a bus event to subscribed plugins (fire-and-forget).

```json
{ "t": "event", "kind": "<EventKind>", "<...event fields...>": "..." }
```

The plugin MUST NOT reply to event messages.

---

**`cancel`** — host aborts a specific in-flight call (only sent to `cancelable` plugins).

```json
{ "t": "cancel", "id": 7 }
```

---

**`shutdown`** — host requests graceful termination. Plugin MUST flush stdout and exit.

```json
{ "t": "shutdown" }
```

---

**`result`** — plugin's atomic (non-streaming) reply to a hook or tool call.

```json
{ "t": "result", "id": 7, "<...reply fields...>": "..." }
```

Reply fields vary by hook — see §2.6.

---

**`chunk`** — plugin's partial streaming output (only from `streaming` plugins).

```json
{ "t": "chunk", "id": 7, "<...chunk fields...>": "..." }
```

For tool calls: `"text": "<progress string>"`.
For provider calls: `"kind": "<chunk kind>"` plus kind-specific fields (see §2.4).

---

**`done`** — plugin's final streaming reply. Replaces `result` when streaming.

```json
{ "t": "done", "id": 7, "<...final fields...>": "..." }
```

Final fields vary by call type — see §2.6.

### 2.6 Hook payload and reply schemas

**`before_tool_call`**

Payload:
```json
{ "tool": "bash", "args": { "cmd": "ls" }, "cwd": "/home/user" }
```
Reply (`result` or `done`):
```json
{ "action": "allow" }
{ "action": "deny", "reason": "not permitted outside sandbox" }
{ "action": "replace", "args": { "cmd": "ls -la" } }
```

---

**`after_tool_call`**

Payload:
```json
{ "tool": "bash", "args": { "cmd": "ls" }, "result": [ {"type":"text","text":"..."} ] }
```
Reply:
```json
{ "action": "keep" }
{ "action": "replace", "content": [ {"type":"text","text":"redacted"} ] }
```

---

**`before_request`**

Payload:
```json
{ "messages": [ {"role":"user","content":[...]} ], "model": {"provider":"p","id":"m"}, "system": "..." }
```
Reply:
```json
{ "action": "keep" }
{ "action": "replace", "messages": [ ... ] }
```

---

**`should_stop_after_turn`**

Payload:
```json
{ "stop": "end_turn", "usage": {"input":10,"output":4,"cache_read":0,"cache_write":0}, "turn": 3 }
```
Reply:
```json
{ "action": "continue" }
{ "action": "stop" }
```

---

**`prepare_next_turn`**

Payload:
```json
{ "stop": "end_turn", "usage": { ... } }
```
Reply:
```json
{ "action": "keep" }
{ "action": "patch", "model": {"provider":"p","id":"m"}, "thinking": {"enabled":true,"budget_tokens":1024} }
```
`model` and `thinking` are both optional inside `patch`.

---

**`get_steering`** / **`get_followup`**

Payload: `null`

Reply:
```json
{ "messages": [] }
{ "messages": [ {"role":"user","content":[{"type":"text","text":"be concise"}]} ] }
```

---

**`get_api_key`**

Payload:
```json
{ "provider": "openai" }
```
Reply:
```json
{ "key": null }
{ "key": "sk-..." }
```

---

**`tool_call`**

Payload:
```json
{ "name": "bash", "args": { "cmd": "ls" } }
```
Atomic reply:
```json
{ "content": [ {"type":"text","text":"file1\nfile2"} ], "is_error": false }
```
Streaming: chunks with `"text"` fields, then `done` with same fields as atomic reply.

---

**`provider_complete`**

Payload: full request object (see §2.4).
Streaming only (provider plugins MUST declare `"streaming"`).
Final `done`:
```json
{ "stop": "end_turn", "usage": {"input":10,"output":4,"cache_read":0,"cache_write":0}, "cost_usd": 0.00012 }
```

---

## 3. Plugin SDK contract

This section defines what any SDK in any language MUST provide to be a conforming
implementation. The Rust crate `kn9t-plugin-sdk` implements this contract; future Python,
Node, and Go packages MUST implement the same contract adapted to the idioms of their
language.

**Core responsibilities of any SDK:**

1. **Handshake** — send host hello, receive and validate plugin hello reply, send plugin hello.
2. **Main dispatch loop** — read lines from stdin, route each message by `t` to the
   right handler. MUST NOT block the reader on handler execution.
3. **Cancel listener** — if `cancelable` declared, run a second concurrent reader (thread,
   goroutine, coroutine, or equivalent) that watches for `{"t":"cancel",...}` and delivers
   cancellation to the matching in-flight call context. MUST share the same stdin channel
   safely (mutex, channel, or language equivalent).
4. **Streaming writer** — if `streaming` declared, provide a handle the handler uses to
   send `chunk` messages and then emit the final `done`.
5. **Event dispatch** — deliver `event` messages to registered sinks without blocking the
   main loop.
6. **Shutdown** — on `{"t":"shutdown"}`, finish in-flight calls (or cancel them), flush
   stdout, and exit cleanly.

**What the SDK MUST NOT do:**

- Parse or validate domain objects (messages, schemas) beyond what is needed for routing.
- Impose a threading model on plugin authors beyond what is needed for the cancel listener.
- Require plugin authors to handle raw JSON directly.

**Language-specific SDK notes:**

| Language | Cancel mechanism | Streaming |
|---|---|---|
| Rust | `Mutex<BufReader<Stdin>>` shared between dispatch thread and cancel thread | `Sender<ChunkPayload>` passed to handler |
| Python | `threading.Thread` + `queue.Queue`; or asyncio with `asyncio.Queue` | callback / async generator |
| Node / TS | `readline` on stdin; EventEmitter for cancel; `stream.write()` for chunks | async iterator or callback |
| Go | goroutine per handler; `sync.Mutex` on stdin reader; channel for cancel | channel of chunk structs |

In all cases the pattern is identical: one concurrent reader feeds the dispatcher; cancel
messages are intercepted and delivered out-of-band to the matching call context.

---

## 4. Rust SDK (`kn9t-plugin-sdk`)

The Rust reference implementation of the §3 contract. A Rust plugin author imports this
one crate and implements traits. Zero workspace deps — only `serde` and `serde_json`.

### 4.1 The four traits

```rust
/// A tool the plugin exposes to the agent.
pub trait PluginTool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    /// Execute the tool. Send progress via `ctx.progress`. Check `ctx.cancel`
    /// to stop early. Return the final authoritative content.
    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput;
}

/// A provider the plugin implements (replaces an in-process provider).
pub trait PluginProvider: Send + Sync {
    fn id(&self) -> &str;
    fn models(&self) -> Vec<ProviderModel>;
    /// Stream token deltas via `ctx.chunk`. Return usage + stop reason when done.
    fn complete(&self, req: &ProviderRequest, ctx: &ProviderCallCtx) -> ProviderResult;
}

/// A hook interceptor.
pub trait PluginHook: Send + Sync {
    fn hooks(&self) -> Vec<&'static str>;
    /// Called synchronously. Return a JSON reply body matching the hook's reply schema.
    fn call(&self, hook: &str, payload: &Value) -> Value;
}

/// A bus event observer (fire-and-forget, no reply).
pub trait PluginEventSink: Send + Sync {
    fn event_filter(&self) -> Vec<&'static str>; // event kind strings; "*" = all
    fn on_event(&self, kind: &str, event: &Value);
}
```

### 4.2 Context types

```rust
pub struct ToolCallCtx {
    /// Fires when host sends {"t":"cancel","id":N}. Check before long operations.
    pub cancel: &'_ CancelToken,
    /// Send a progress chunk to the host for live TUI display.
    pub progress: &'_ ProgressSender,
}

pub struct ProviderCallCtx {
    pub cancel: &'_ CancelToken,
    /// Send a typed chunk (text_delta, thinking_delta, tool_use_start, …).
    pub chunk: &'_ ChunkSender,
}

/// Opaque. Call .is_cancelled() to poll; SDK delivers it from the cancel thread.
pub struct CancelToken { /* private */ }
impl CancelToken {
    pub fn is_cancelled(&self) -> bool;
}

pub struct ProgressSender { /* private */ }
impl ProgressSender {
    /// Emits {"t":"chunk","id":N,"text":"..."} to the host.
    pub fn send(&self, text: impl Into<String>);
}

pub struct ChunkSender { /* private */ }
impl ChunkSender {
    pub fn text_delta(&self, text: &str);
    pub fn thinking_delta(&self, thinking: &str, signature: &str);
    pub fn tool_use_start(&self, call_id: &str, name: &str);
    pub fn tool_use_delta(&self, call_id: &str, args_json: &str);
}
```

### 4.3 The Plugin container and main loop

```rust
pub struct Plugin {
    name:        String,
    tools:       Vec<Box<dyn PluginTool>>,
    provider:    Option<Box<dyn PluginProvider>>,
    hooks:       Vec<Box<dyn PluginHook>>,
    event_sinks: Vec<Box<dyn PluginEventSink>>,
}

impl Plugin {
    pub fn new(name: impl Into<String>) -> Self;
    pub fn tool(mut self, t: impl PluginTool + 'static) -> Self;
    pub fn provider(mut self, p: impl PluginProvider + 'static) -> Self;
    pub fn hook(mut self, h: impl PluginHook + 'static) -> Self;
    pub fn event_sink(mut self, s: impl PluginEventSink + 'static) -> Self;

    /// Consume self, do the handshake, then block dispatching messages forever.
    /// Returns only on shutdown or stdin EOF.
    pub fn run(self);
}
```

A complete plugin `main.rs`:

```rust
fn main() {
    Plugin::new("kn9t-tools")
        .tool(Bash::new())
        .tool(Read::new())
        .tool(Edit::new())
        .run();
}
```

`Plugin::run()` internally:
1. Sends `{"t":"hello","proto":1,"kn9t":"<ver>"}` to stdout.
2. Reads the host hello reply (validates proto).
3. Sends its own hello declaration derived from registered tools/provider/hooks/sinks.
4. Spawns a **cancel listener thread** (if `cancelable` capability is declared) that reads
   stdin for `{"t":"cancel",...}` messages and delivers them to in-flight call contexts.
5. Enters the main dispatch loop: reads one line at a time from stdin, dispatches to the
   appropriate handler. Hook and tool calls are dispatched on a thread-pool (bounded, size
   configurable, default = number of CPUs) so multiple parallel tool calls are served
   concurrently. Events are dispatched on a dedicated event thread.

> **R-PLUG2-070**
> `Plugin::run()` MUST NOT block the cancel listener behind the main dispatch loop.
> The cancel listener MUST run on its own OS thread reading from the same stdin pipe.
> Both threads share stdin via a `Mutex<BufReader<Stdin>>`.
> **Accept:** `cargo test plug2::cancel_does_not_block_dispatch`

> **R-PLUG2-080**
> The SDK MUST be entirely blocking (GI-5). No `tokio`, no `async fn`, no `.await`.
> **Accept:** CI grep for `async fn` / `.await` in `kn9t-plugin-sdk`.

### 4.4 Documentation requirements

The SDK is the public interface for third-party plugin authors. Documentation is a
first-class deliverable, not an afterthought.

> **R-PLUG2-090**
> Every public item in `kn9t-plugin-sdk` MUST have a `///` doc comment. The crate root
> `lib.rs` MUST contain a module-level doc (`//!`) that includes:
> - a one-paragraph description of what kn9t plugins are
> - the four plugin types (tool, provider, hook, event sink) with one sentence each
> - a minimal working example (the kn9t-tools pattern from §3.3)
> - a link to the wire protocol spec (this file)
> **Accept:** `cargo doc -p kn9t-plugin-sdk --no-deps` produces zero warnings.

> **R-PLUG2-095**
> Each trait (`PluginTool`, `PluginProvider`, `PluginHook`, `PluginEventSink`) MUST have a
> doc example showing a complete minimal implementation.
> **Accept:** `cargo test --doc -p kn9t-plugin-sdk` passes.

---

## 5. Hot reload

> **R-PLUG2-100**
> When the host receives a reload signal for a plugin (via `POST /plugin/{name}/reload`
> or SIGHUP to the server), it MUST:
> 1. Send `{"t":"cancel","id":N}` for every in-flight call on that plugin.
> 2. Wait up to `before_tool_call` timeout for `done` replies.
> 3. Send `{"t":"shutdown"}` and close the write pipe.
> 4. Spawn a new process from the same `cmd` path.
> 5. Re-do the handshake; re-register tools, provider, hooks, event subscriptions.
> In-flight calls that do not complete before step 3 receive a synthetic error result.
> The agent continues; the model sees tool errors it can retry.
> **Accept:** `cargo test plug2::hot_reload_cancels_inflight`

---

## 6. External plugin: `kn9t-tools` (auto-discovered)

The default tools (`bash`, `read`, `write`, `edit`) ship as an **external** subprocess
plugin binary (`plugins/kn9t-tools`) that kn9t **discovers** at startup. This validates
the full subprocess path under realistic conditions and makes the tool set
hot-reloadable and replaceable. The repo's `plugins/` is build source;
`~/.kn9t/plugins/` is the install target (ADR-0004).

> **R-PLUG2-110** *(rewritten Phase 3.4 — was "auto-spawn sibling of exe, fail if missing"; now discovery)*
> At server startup, before accepting any session requests, kn9t MUST discover tool
> plugins by scanning the **user plugin directory** `<KN9T_HOME|~/.kn9t>/plugins/`
> (ADR-0004) — the **only** directory scanned, **never** a project-relative
> `plugins/` directory (`git clone` then `kn9t` must not be code execution;
> R-PLUG-100) — and handshaking every executable found (Unix: regular file with
> an execute bit; Windows: `.exe`), merging the resulting tools with any pinned
> `[[plugin]]` entries from the **global** config `~/.kn9t/config.toml` into one
> `ToolRegistry` (config wins on conflict). Specifically:
> - A pinned entry with `cmd = [...]` is spawned as a user plugin; a discovered
>   plugin with the **same declared `name`** or the **same binary path** is suppressed.
> - A config entry with `enabled = false` or `disabled = true` suppresses the
>   discovered plugin with that `name` (file-stem heuristic pre-handshake, declared
>   name post-handshake) and is not itself spawned.
> - A config entry with `cmd` omitted but `env` set injects those env vars into the
>   discovered spawn for the matching name.
> - Duplicate tool names across plugins are deduped (first wins, warning logged).
> - A plugin that fails to spawn or handshake is **soft-failed** with a warning;
>   startup continues with the remaining plugins. An empty or missing plugin
>   directory is a **warning, not a startup failure** (a server with zero tools is
>   degraded but still serves; bootstrap installs `kn9t-tools` on first run when a
>   build artifact is found, so the common case is never empty).
> **Accept:** `cargo test plug2::autostart_tools_plugin` (SDK publishability) +
> `cargo test -p kn9t-server tools::discovery_*` (positive, ADR-0004 negative
> `discovery_ignores_project_relative_plugins`, missing-dir, sorted order,
> `disabled`/`pinned`/`env`/`dedup` including `kn9t-tools` regression) all green;
> live `kn9t chat` starts with `total tools registered: 4` when
> `~/.kn9t/plugins/kn9t-tools` is present.

> **R-PLUG2-120**
> `kn9t-tools` MUST declare `capabilities: ["streaming", "cancelable"]`.
> `bash` MUST stream stdout lines as `chunk` messages while the child process runs.
> `bash` MUST stop the child process and send a `done` error when `cancel` fires.
> `read` and `edit` MAY use atomic `result` (they complete quickly).
> **Accept:** `cargo test plug2::bash_streams_progress`

> **R-PLUG2-130**
> Read-tracking (the `read`→`edit` conflict detection map) MUST live entirely inside
> the `kn9t-tools` process. It is NOT carried on the wire. `read` and `edit` share the
> map via an `Arc<Mutex<HashMap<PathBuf,(Sha256,SystemTime)>>>` inside the binary.
> **Accept:** `cargo test plug2::edit_detects_stale_read`

---

## 7. Stage gate

> **R-PLUG2-900**
> Stage 08b is done when:
> - `kn9t-plugin-sdk` compiles with zero workspace deps, `cargo doc` clean, all doc tests pass.
> - All new wire message types (`chunk`, `done`, `cancel`) round-trip correctly.
> - `kn9t-tools` subprocess streams bash progress and honours cancel.
> - Hot-reload cancels in-flight calls and re-registers tools.
> - `kn9t-plugin` host side handles `chunk`/`done`/`cancel` for both tool and provider calls.
> - GI-1 holds for all new and updated crates.
> - All acceptance tests named in §2–§5 pass.


