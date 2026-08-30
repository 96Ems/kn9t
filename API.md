# kn9t Server HTTP API Reference

> **Audience:** Client implementors building web apps, VS Code extensions, CLI bots, or any other client against the kn9t server.

---

## 1. Connection Basics

| Item | Detail |
|------|--------|
| **Port** | Read from `~/.kn9t/port` (plain integer, one line) |
| **Token** | Read from `~/.kn9t/token` (plain string, one line) |
| **Auth** | `Authorization: Bearer <token>` — required on every request |
| **Origin** | Requests with an `Origin` header are rejected with `403`. Browser `fetch()` is blocked by design. Use a native HTTP client. |
| **Base URL** | `http://127.0.0.1:<port>` |

---

## 2. Common Types

### Message

```json
{
  "id": "string",
  "role": "user" | "assistant" | "tool" | "system",
  "content": [Content]
}
```

### Content (union, discriminated by `type`)

| `type` | Additional fields |
|--------|-------------------|
| `"text"` | `text: string` |
| `"tool_call"` | `id: string`, `name: string`, `args_json: string` |
| `"tool_result"` | `id: string`, `content: Content[]`, `is_error: bool` |
| `"thinking"` | `text: string`, `signature: string \| null` |
| `"image"` | `sha256: string`, `mime: string` |

### Tokens

```json
{ "input": u64, "output": u64, "cache_read": u64, "cache_write": u64, "reasoning": u64 }
```

### Price (per million tokens)

```json
{ "input": f64, "output": f64, "cache_read": f64, "cache_write": f64 }
```

---

## 3. Lease System

Write operations require a **lease** — a short string token that identifies the current writer.

- Acquire with `POST /session/{id}/lease` → receive a `holder` string.
- Pass `X-Lease: <holder>` on every write request (`/prompt`, `/steer`, `/abort`, `/model`, `/approve`).
- Release with `DELETE /session/{id}/lease`.
- If another client holds the lease, `POST /session/{id}/lease` returns `409`. Pass `?takeover=1` to force-steal it.
- SSE streaming never requires a lease.

---

## 4. Routes

### 4.1 Sessions

#### `POST /session` — Create session

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | no | Initial model ID |
| `title` | string | no | Human-readable title |

**Response `200`**

```json
{ "id": "string", "model": "string", "title": "string", "created_at": "ISO8601" }
```

**Errors:** `401` bad token.

---

#### `GET /session` — List sessions

**Request body:** none

**Response `200`**

```json
[{ "id": "string", "model": "string", "title": "string", "created_at": "ISO8601" }]
```

---

#### `GET /session/{id}` — Session snapshot

**Request body:** none

**Response `200`**

```json
{
  "id": "string",
  "model": "string",
  "title": "string",
  "created_at": "ISO8601",
  "messages": [Message],
  "seq": "u64"
}
```

`seq` is the current store sequence number. Use it as the `?from=` value when opening the SSE stream.

**Errors:** `404` session not found.

---

#### `DELETE /session/{id}` — Delete session

**Request body:** none  
**Response `200`:** `{}`  
**Errors:** `404`.

---

#### `POST /session/{id}/fork` — Fork session

Creates a new session that is a copy of the given session up to its current state.

**Request body:** none

**Response `200`**

```json
{ "id": "string", "model": "string", "title": "string", "created_at": "ISO8601" }
```

**Errors:** `404`.

---

### 4.2 Lease

#### `POST /session/{id}/lease` — Acquire write lease

**Query params**

| Param | Type | Description |
|-------|------|-------------|
| `takeover` | `1` | Force-steal lease from current holder |

**Request body:** none

**Response `200`**

```json
{ "lease": "string", "session": "string" }
```

`lease` is the holder token to pass in `X-Lease` headers. `session` echoes back the session ID.

**Errors:** `404` session not found, `409` lease already held (omit `?takeover=1`).

---

#### `DELETE /session/{id}/lease` — Release write lease

**Required header:** `X-Lease: <holder>`

**Request body:** none  
**Response `200`:** `{}`  
**Errors:** `403` wrong holder, `404`.

---

### 4.3 SSE Event Stream

#### `GET /session/{id}/events?from=<seq>` — Subscribe to events

Opens a persistent `text/event-stream` connection. No lease required.

**Query params**

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `from` | u64 | yes | Replay durable events with `seq >= from`. Pass `0` for full history, or last seen `seq` on reconnect. |

**Response headers**

```
Content-Type: text/event-stream
Cache-Control: no-cache
```

**Wire format:** Standard SSE. Each frame:

```
data: <json>\n\n
```

No `event:` line — the event type is inside the JSON as `"kind"`.

**Errors:** `404` session not found.

---

#### SSE Event Catalogue

All events are JSON objects with a `"kind"` field (PascalCase). Serialized with `#[serde(tag = "kind")]`.

##### Durable events (replayed on reconnect via `?from=<seq>`)

| `kind` | Fields | Notes |
|--------|--------|-------|
| `MessageAppended` | `seq: u64`, `msg: Message` | A complete message was committed to the store |
| `UsageRecorded` | `seq: u64`, `provider: string`, `model: string`, `kind: "main"\|"compaction"\|"subagent"\|"title"`, `tokens: Tokens`, `price_snapshot: Price`, `cost_usd: f64`, `estimated: bool` | Token usage for one LLM call |
| `ModelChanged` | `seq: u64`, `provider: string`, `id: string` | Active model was switched |
| `Compacted` | `seq: u64`, `replaced: { start: u64, end: u64 }`, `summary: Message` | History range was compacted; `replaced` gives the seq range removed |

##### Non-durable events (live only, not replayed)

| `kind` | Fields | Notes |
|--------|--------|-------|
| `TurnStarted` | `turn: u32` | LLM turn began |
| `TurnEnded` | `turn: u32`, `stop: "Stop"\|"ToolUse"\|"MaxTokens"\|"Aborted"\|"Steer"` | LLM turn finished; `stop` is the stop reason |
| `TextDelta` | `seq: u64`, `call_id: null`, `delta: string` | Streaming text chunk from the assistant |
| `ThinkingDelta` | `seq: u64`, `delta: string` | Streaming thinking/reasoning chunk |
| `ToolArgsDelta` | `seq: u64`, `call_id: string`, `delta: string` | Streaming tool argument chunk |
| `ToolStarted` | `call_id: string`, `name: string` | Tool execution began |
| `ToolProgress` | `call_id: string`, `note: string` | Progress note from a running tool |
| `ToolFinished` | `call_id: string`, `is_error: bool` | Tool execution completed |
| `ApprovalRequest` | `id: u64`, `call_id: string`, `name: string`, `args_json: string` | Server is waiting for tool-call approval; respond via `POST /approve` |
| `HookFailed` | `hook: string`, `error: string` | A lifecycle hook failed (non-fatal) |
| `TitleChanged` | `title: string` | Session title was auto-generated or changed |
| `PluginNotification` | `plugin: string`, `message: string` | Plugin notification (see §8.6) |
| `Error` | `message: string` | Server-side error in the stream |

> **Reconnect pattern:** Track the highest `seq` seen from durable events. On reconnect, open `GET /session/{id}/events?from=<last_seq+1>` to resume without replaying already-processed events.

---

### 4.4 Write Operations (lease required)

All write operations require `X-Lease: <holder>` where `<holder>` is the string returned by `POST /session/{id}/lease`.

**Common errors for all write operations:** `401` bad token, `403` wrong or missing lease, `404` session not found, `409` no active turn (where applicable).

---

#### `POST /session/{id}/prompt` — Send user prompt

**Required header:** `X-Lease: <holder>`

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | yes* | Text content of the prompt |
| `blobs` | string[] | no | Array of blob SHA-256 hashes (pre-uploaded via `POST /blob`) |
| `images` | string[] | no | Array of base64 data URIs (`data:image/png;base64,...`) |

\* `text` may be empty if `blobs` or `images` is provided.

**Image handling:**
- `blobs`: References pre-uploaded images by hash
- `images`: Inline base64 data URIs (TUI clipboard paste) — server parses, stores as blobs, then references by hash

Both methods result in `Content::Image { sha256: "sha256:<hash>", mime }` stored in the message.

**Response `200`:** `{}`

---

#### `POST /session/{id}/steer` — Inject steering message

Injects a message mid-turn to redirect the assistant without ending the turn.

**Required header:** `X-Lease: <holder>`

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | yes | Steering instruction text |

**Response `200`:** `{}`

---

#### `POST /session/{id}/abort` — Abort running turn

**Required header:** `X-Lease: <holder>`

**Request body:** none  
**Response `200`:** `{}`

---

#### `POST /session/{id}/model` — Switch model

**Required header:** `X-Lease: <holder>`

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `model` | string | yes | Model ID to switch to (must be in configured models list) |

**Response `200`:** `{}`

---

### 4.5 Tool Approval

#### `POST /approve` — Approve or deny a tool call

Called in response to an `ApprovalRequest` SSE event.

**Required headers:** `X-Lease: <holder>`, `X-Lease-Session: <session_id>`

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | u64 | yes | Approval request ID from the `ApprovalRequest` event |
| `approved` | bool | yes | `true` to allow the tool call, `false` to deny |

**Response `200`:** `{}`

**Errors:** `404` no pending approval with that ID.

---

### 4.6 Blobs

#### `POST /blob` — Upload blob (image)

**Request headers**

| Header | Value |
|--------|-------|
| `Content-Type` | MIME type of the blob (e.g. `image/png`, `image/jpeg`) |

**Request body:** Raw binary blob data.

**Response `200`**

```json
{ "sha256": "string" }
```

Use the returned `sha256` in `POST /session/{id}/prompt` `images` array.

**Errors:** `400` unsupported MIME type.

---

#### `GET /blob/{sha256}` — Download blob

**Request body:** none

**Response `200`:** Raw binary data with appropriate `Content-Type` header.

**Errors:** `404` blob not found.

---

### 4.7 Models

#### `GET /models` — List configured models

**Request body:** none

**Response `200`**

```json
[{ "id": "string", "provider": "string", "display_name": "string" }]
```

---

### 4.8 Cost & Budget

#### `GET /cost?session=<id>&from=<seq>` — Usage cost query

**Query params**

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `session` | string | yes | Session ID |
| `from` | u64 | no | Only include usage records with `seq >= from` |

**Response `200`**

```json
{
  "total_usd": "f64",
  "records": [
    {
      "seq": "u64",
      "provider": "string",
      "model": "string",
      "kind": "main" | "compaction" | "subagent" | "title",
      "tokens": Tokens,
      "cost_usd": "f64",
      "estimated": "bool"
    }
  ]
}
```

**Errors:** `404` session not found.

---

#### `GET /budget` — Budget summary

**Request body:** none

**Response `200`**

```json
{
  "limit_usd": "f64 | null",
  "spent_usd": "f64",
  "remaining_usd": "f64 | null"
}
```

---

## 5. Client Workflow

```
1. Read ~/.kn9t/port  →  base URL = http://127.0.0.1:<port>
   Read ~/.kn9t/token →  Authorization: Bearer <token>

2. Create or attach to a session:
     POST /session                  →  { id }   (new session)
     GET  /session                  →  [...]    (pick existing)

3. Acquire write lease:
     POST /session/{id}/lease       →  { holder }
     On 409: enter observer mode (SSE still works, prompts blocked)

4. Open SSE stream (separate long-lived connection):
     GET /session/{id}/events?from=0

5. Send a prompt:
     POST /session/{id}/prompt
     X-Lease: <holder>
     { "text": "Hello" }

6. Handle ApprovalRequest events:
     POST /approve
     X-Lease: <holder>
     X-Lease-Session: <id>
     { "id": <approval_id>, "approved": true }

7. On SSE disconnect, reconnect with last seen durable seq:
     GET /session/{id}/events?from=<last_seq + 1>

8. Release lease when done:
     DELETE /session/{id}/lease
     X-Lease: <holder>
```

---

## 6. Error Response Format

All error responses return JSON:

```json
{ "error": "string" }
```

| Code | Meaning |
|------|---------|
| `400` | Bad request / invalid body |
| `401` | Missing or invalid Bearer token |
| `403` | Origin header present, or wrong lease holder |
| `404` | Resource not found |
| `409` | Conflict (lease held, or no active turn) |
| `500` | Internal server error |

---

## 7. Plugin Provider API

When implementing a custom plugin provider (`kind: "plugin"` in config), the server communicates via stdin/stdout JSON-RPC. This section documents the request format plugins receive and how to implement cache control.

### 7.1 Request Format

The server sends completion requests as JSON:

```json
{
  "model": {
    "ref": { "provider": "my-plugin", "id": "model-name" },
    "api_id": "model-name",
    "ctx_window": 200000,
    "max_out": 8192
  },
  "system": "You are a helpful assistant.",
  "messages": [
    { "role": "user", "content": [{ "type": "text", "text": "Hello" }] },
    { "role": "assistant", "content": [{ "type": "text", "text": "Hi!" }] }
  ],
  "tools": [
    { "name": "read", "description": "Read a file", "schema": { ... } }
  ],
  "thinking": "off",
  "max_tokens": 8192,
  "cache": [
    { "at": "system" },
    { "at": "after_message", "idx": 1 },
    { "at": "after_message", "idx": 0 }
  ]
}
```

### 7.2 Cache Control Implementation

The `cache` array specifies which messages should have cache breakpoints. This enables prompt caching on providers that support it (Anthropic, Bedrock, etc.).

#### Cache Entry Format

| `at` value | Meaning |
|------------|---------|
| `"system"` | Apply cache control to the system message |
| `"after_message"` | Apply cache control to message at index `idx` |

#### How to Apply Cache Control

For each cache entry, add `"cache_control": {"type": "ephemeral"}` to the **last content block** of the target message.

**Example transformation:**

Input message (index 1 is in cache array):
```json
{
  "role": "assistant",
  "content": [{ "type": "text", "text": "Hello!" }]
}
```

Output with cache control applied:
```json
{
  "role": "assistant",
  "content": [{
    "type": "text",
    "text": "Hello!",
    "cache_control": { "type": "ephemeral" }
  }]
}
```

#### Implementation Pseudocode

```python
def apply_cache_control(request):
    cache_indices = set()
    cache_system = False
    
    for entry in request.get("cache", []):
        if entry["at"] == "system":
            cache_system = True
        elif entry["at"] == "after_message":
            cache_indices.add(entry["idx"])
    
    # Apply to system message
    if cache_system and request.get("system"):
        # Wrap system as content block with cache_control
        system_content = [{
            "type": "text",
            "text": request["system"],
            "cache_control": {"type": "ephemeral"}
        }]
    
    # Apply to indexed messages
    for idx, msg in enumerate(request["messages"]):
        if idx in cache_indices:
            last_block = msg["content"][-1]
            last_block["cache_control"] = {"type": "ephemeral"}
```

#### Breakpoint Selection Strategy

The server uses up to 4 cache breakpoints (Anthropic's limit), selected in priority order:

1. **System prompt** — stable, cached across all turns
2. **Last user message** — caches conversation history up to user's request  
3. **Second-to-last message** — progressive tool result caching
4. **Last message** — captures the latest turn

During tool loops, breakpoints 3 and 4 "slide forward" like a caterpillar:

```
Turn 2: [Sys📑₁] [User📑₂] [Asst] [Tool📑₃📑₄] [Asst generating...]
Turn 3: [Sys📑₁] [User📑₂] [Asst] [Tool📑₃] [Asst] [Tool📑₄] [Asst...]
```

### 7.3 Response Format

Plugins stream responses as JSON chunks to stdout:

```json
{"kind": "text_delta", "idx": 0, "text": "Hello"}
{"kind": "tool_call", "idx": 1, "call_id": "abc", "name": "read"}
{"kind": "tool_args_delta", "idx": 1, "delta": "{\"path\":"}
{"kind": "usage", "input": 1500, "output": 200, "cache_read": 800, "cache_write": 100}
{"kind": "stop", "reason": "stop"}
```

#### Usage Fields for Cache

| Field | Description |
|-------|-------------|
| `cache_read` | Tokens read from cache (reduced cost) |
| `cache_write` | Tokens written to cache (first request overhead) |

The server tracks these for cost calculation:
- Cache reads cost ~10% of normal input tokens
- Cache writes cost ~25% more than normal input tokens (one-time)

---

## 8. Plugin Protocol Wire Format

All plugin communication uses newline-delimited JSON (NdJSON) over stdin/stdout.

### 8.1 JSON Convention

**All JSON uses `snake_case`** for field names and enum variants. No PascalCase, no camelCase.

### 8.2 Protocol Messages

#### 8.2.1 Message Types

**Host → Plugin:**

| Type | Fields | Description |
|------|--------|-------------|
| `hello` | `proto`, `kn9t` | Protocol handshake (host initiates) |
| `hook` | `id`, `hook`, `payload` | Hook invocation (requires response) |
| `event` | `kind`, `...` | Event notification (fire-and-forget) |
| `cancel` | `id` | Cancel an in-progress hook call |
| `shutdown` | (none) | Graceful shutdown request |

**Plugin → Host:**

| Type | Fields | Description |
|------|--------|-------------|
| `hello` | `name`, `hooks`, `tools`, `capabilities`, `events` | Plugin registration |
| `result` | `id`, `...body` | Atomic hook response |
| `chunk` | `id`, `...body` | Streaming intermediate (more coming) |
| `done` | `id`, `...body` | Streaming final (stream complete) |
| `event` | `kind`, `...` | Plugin notification (forwarded to SSE) |

#### 8.2.2 Handshake Sequence

```
Host   →  {"t": "hello", "proto": 1, "kn9t": "0.1.0"}
Plugin →  {"t": "hello", "name": "my-plugin", "hooks": [...], "tools": [...], "capabilities": [...], "events": [...]}
```

| Field | Type | Description |
|-------|------|-------------|
| `proto` | int | Protocol version (currently `1`) |
| `kn9t` | string | Server version string |
| `name` | string | Plugin name (unique identifier) |
| `hooks` | array | List of hooks to subscribe to |
| `tools` | array | List of tool declarations (see §8.4) |
| `capabilities` | array | `["streaming", "cancelable"]` |
| `events` | array | Event kinds to receive (e.g., `["MessageAppended"]`) |

#### 8.2.3 Flattened Body Fields

The kn9t protocol uses `#[serde(flatten)]` on body fields. This means body fields are at the **same level** as `t` and `id`, NOT nested inside a `"body"` wrapper.

```json
// ✅ Correct — body fields flattened:
{"t": "result", "id": 7, "messages": [...]}
{"t": "done", "id": 42, "content": [...], "is_error": false}

// ❌ Wrong — nested body:
{"t": "result", "id": 7, "body": {"messages": [...]}}
```

This applies to `result`, `chunk`, and `done` message types.

### 8.3 Tool Call (Host → Plugin)

When the server dispatches a tool to a plugin:

```json
{"t": "hook", "id": 42, "hook": "before_tool_call", "payload": {"tool": "bash", "args": {"cmd": "pwd"}}}
```

| Field | Type | Description |
|-------|------|-------------|
| `tool` | string | Tool name (matches `ToolSpec.name` from handshake) |
| `args` | object | Tool arguments as parsed JSON |

### 8.4 Tool Response (Plugin → Host)

**Atomic response:**
```json
{"t": "result", "id": 42, "content": [{"type": "text", "text": "output"}], "is_error": false}
```

**Streaming response (requires `streaming` capability):**
```json
{"t": "chunk", "id": 42, "delta": "partial output..."}
{"t": "chunk", "id": 42, "delta": "more output..."}
{"t": "done", "id": 42, "content": [...], "is_error": false}
```

| Field | Type | Description |
|-------|------|-------------|
| `content` | array | Array of `Content` blocks (see §2) |
| `is_error` | bool | `true` if tool execution failed |
| `delta` | string | Streaming text delta (chunk only) |

### 8.5 Tool Declaration (Plugin Hello)

Tools are declared in the plugin hello message:

```json
{
  "t": "hello",
  "name": "my-plugin",
  "capabilities": ["streaming", "cancelable"],
  "tools": [
    {
      "name": "my_tool",
      "description": "Does something useful",
      "schema": { "type": "object", "properties": { ... } },
      "parallel_safe": true,
      "hidden": false
    }
  ]
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | required | Unique tool name (snake_case) |
| `description` | string | required | Human-readable description shown to the model |
| `schema` | object | required | JSON Schema for tool arguments |
| `parallel_safe` | bool | `false` | If true, tool can run in parallel with others |
| `hidden` | bool | `false` | If true, tool is registered but not shown in system prompt (see §10) |

### 8.5 Lifecycle Hooks

Plugins can subscribe to lifecycle hooks by listing them in the hello message:

```json
{
  "t": "hello",
  "name": "my-plugin",
  "hooks": ["after_tool_call", "get_steering"],
  "tools": []
}
```

**Available hooks:**

| Hook | Payload | Reply | Composition | Failure Posture |
|------|---------|-------|-------------|-----------------|
| `before_tool_call` | `{session_id, tool, args}` | `{action: "allow"\|"deny"\|"replace", ...}` | First-deny-wins | **Deny (fail closed)** |
| `after_tool_call` | `{session_id, tool, args, result}` | `{action: "keep"\|"replace", content: [...]}` | Pipeline | Keep original |
| `before_request` | `{session_id, messages, model, system}` | `{action: "keep"\|"replace", messages: [...]}` | Pipeline | Use original |
| `should_stop_after_turn` | `{session_id, stop, usage, turn}` | `{action: "continue"\|"stop"}` | Any-says-stop | Continue |
| `prepare_next_turn` | `{session_id, stop, usage}` | `{model?, thinking?}` | Pipeline | No change |
| `get_steering` | `{session_id}` | `{messages: [...]}` | Concat | Empty |
| `get_followup` | `{session_id}` | `{messages: [...]}` | Concat | Empty |
| `get_api_key` | `{session_id, provider}` | `{key: string\|null}` | First non-null | Fall back to config |

> **All hooks include `session_id`.** This allows plugins to maintain per-session state
> (e.g., tracking which AGENTS.md files have been injected for each session).

**Hook invocation (Host → Plugin):**

```json
{"t": "hook", "id": 7, "hook": "get_steering", "payload": {"session_id": "01M14AEM63SM754EA9G78AS9MQ"}}
```

**Hook reply (Plugin → Host):**

```json
{"t": "result", "id": 7, "messages": [{"role": "system", "content": [...]}]}
```

> **Message `id` field is optional.** Plugins do not need to provide `id` for messages
> in `get_steering`/`get_followup` responses. The server generates a `MsgId` for any
> message without one.

> **Silent messages.** Messages can include `"silent": true` to be persisted and sent
> to the LLM but **not displayed in the TUI**. This is useful for plugins that inject
> context (e.g., AGENTS.md) and handle their own user-facing notification via events.
>
> ```json
> {"role": "user", "content": [...], "silent": true}
> ```

> **IMPORTANT: Flattened body fields.** The kn9t protocol uses `#[serde(flatten)]` on
> the body field. This means body fields must be at the **same level** as `t` and `id`,
> NOT nested inside a `"body"` wrapper.
>
> ✅ Correct: `{"t": "result", "id": 7, "messages": [...]}`
> ❌ Wrong:   `{"t": "result", "id": 7, "body": {"messages": [...]}}`
>
> The same applies to `chunk` and `done` message types.

**Composition classes:**

- **Pipeline** — each plugin sees previous plugin's output (B sees A's result)
- **First-deny-wins** — short-circuit on first deny/replace
- **Concat** — all results concatenated in declaration order
- **Any-says-stop** — first "stop" wins
- **First non-null** — first plugin returning a value wins

**Example: AGENTS.md injection via `get_steering`:**

```json
// Plugin → Host (on get_steering hook)
{"t": "result", "id": 7, "messages": [{
  "role": "user",
  "content": [{"type": "text", "text": "<system-reminder source=\"AGENTS.md: /project/AGENTS.md (project, 214 lines)\">\n...content...\n</system-reminder>"}]
}]}
```

> **Why `role: "user"` with `<system-reminder>`?** Many providers reject `role: "system"`
> except in the first message. The `<system-reminder>` tag is the standard way to inject
> instructions mid-conversation as a user message that the model treats as system guidance.

### 8.6 Plugin Event Emission (Plugin → Host)

Plugins can emit **notification events** to the host's EventBus at any time. These events are wrapped in a generic `PluginNotification` and forwarded to all SSE subscribers.

**Plugin sends:**
```json
{"t": "event", "plugin": "kn9t-agents-md", "message": "Loaded /project/AGENTS.md (project, 42 lines)"}
```

**Server broadcasts (SSE):**
```json
{"kind": "plugin_notification", "plugin": "kn9t-agents-md", "message": "Loaded /project/AGENTS.md (project, 42 lines)"}
```

**TUI displays:**
```
ℹ kn9t-agents-md: Loaded /project/AGENTS.md (project, 42 lines)
```

| Field | Type | Description |
|-------|------|-------------|
| `plugin` | string | Plugin name (for display attribution) |
| `message` | string | Human-readable notification text |

**Flow:**

```
Plugin ──{"t":"event","plugin":"...","message":"..."}──> PluginHost.reader_thread
                                                                │
                                                                ▼
                                                         ReaderMsg::Event
                                                                │
                                                                ▼
                                                    forward_plugin_event()
                                                                │
                                                                ▼
                                      Event::PluginNotification { payload }
                                                                │
                                                                ▼
                                                        SSE broadcast to all clients
```

**Notes:**
- Events are fire-and-forget (no response expected)
- Events can be emitted at any time, even during hook execution
- The payload is flattened into the SSE event (no nesting)

**Client handling:**

Any SSE client (TUI, web UI, CLI, etc.) receives `PluginNotification` events with `plugin` and `message` fields. The TUI displays them as `ℹ {plugin}: {message}`.

Example client logic:
```
on PluginNotification { plugin, message }:
  show "ℹ {plugin}: {message}"
```

### 8.7 Built-in Plugin: kn9t-agents-md

The `kn9t-agents-md` plugin auto-discovers and injects AGENTS.md files into agent context.

**Hooks used:**
- `after_tool_call` — tracks file paths from `read`, `glob`, `grep`, `bash` tools
- `get_steering` — returns pending AGENTS.md content as silent user messages

**Discovery order:**
1. **Global:** `~/.kn9t/AGENTS.md` (injected on first `get_steering`)
2. **Project:** `$CWD/AGENTS.md` (injected on first `get_steering`)
3. **Subdirectory:** ancestors of paths seen in tool calls, up to workspace root

**Per-session state:** The plugin maintains a separate cache of injected paths for each session (keyed by `session_id` from hook payloads). Each AGENTS.md is injected only once per session.

> **Note:** Compaction is not yet handled. After a compaction, AGENTS.md files are not
> re-injected. This will be fixed when event forwarding to plugins is implemented.

**Injection format:**
```
<system-reminder source="AGENTS.md: {path} ({source}, {lines} lines)">
{content}
</system-reminder>
```

**Events emitted:** Plugin notification with `message` describing the loaded file.

**Configuration:**
```toml
# ~/.kn9t/config.toml
[[plugin]]
name = "kn9t-agents-md"
cmd  = ["path/to/kn9t-agents-md.exe"]
```

---

## 9. SSE Event Sequence for Tool Calls

Understanding the SSE event order is critical for building TUI clients. Events arrive in a specific sequence that clients must handle correctly.

### 9.1 Event Flow

```
TurnStarted
    │
    ├── TextDelta (streaming assistant text)
    ├── ToolArgsDelta (streaming tool arguments) ◄── FOR LIVE DISPLAY ONLY
    │
MessageAppended (assistant message with complete ToolCalls)
    │
    ├── ToolStarted (tool execution begins)
    ├── ToolProgress (streaming output, e.g., bash lines, diff hunks)
    ├── ToolFinished (tool execution complete)
    │
MessageAppended (tool results)
    │
UsageRecorded
TurnEnded
```

### 9.2 Key Design Decisions

| Event | Purpose | Client Action |
|-------|---------|---------------|
| `TextDelta` | Stream assistant text as it arrives | Append to live display |
| `ToolArgsDelta` | Stream tool arguments | **Optional** — for live display only |
| `MessageAppended` | Commit complete message | **Create ToolCards** with full args |
| `ToolStarted` | Tool execution begins | Set tool status to "running" |
| `ToolProgress` | Stream tool output | Append to tool output display |
| `ToolFinished` | Tool execution complete | Set tool status to "done"/"error" |

### 9.3 Important: ToolArgsDelta vs MessageAppended

**Do NOT rely on `ToolArgsDelta` to build tool arguments.** It arrives during streaming before the message is committed. Use `MessageAppended` which contains complete `ToolCall` blocks with `args_json`.

```json
// MessageAppended contains complete tool calls:
{
  "kind": "message_appended",
  "msg": {
    "role": "assistant",
    "content": [
      { "type": "text", "text": "Let me read that file:" },
      { "type": "tool_call", "id": "call_123", "name": "read", "args_json": "{\"path\":\"/tmp/foo.txt\"}" }
    ]
  }
}
```

The client should:
1. On `MessageAppended` with `tool_call` content: Create tool cards with full `args_json`
2. On `ToolStarted`: Find card by `call_id`, set status to "running"
3. On `ToolProgress`: Append output to card
4. On `ToolFinished`: Set status to "done" or "error"

### 9.4 Tool Status Lifecycle

```
pending → running → done
                 └→ error
```

- **pending**: Tool card created (from MessageAppended), awaiting execution
- **running**: Execution started (ToolStarted received)
- **done**: Execution complete, success (ToolFinished with is_error=false)
- **error**: Execution complete, failure (ToolFinished with is_error=true)

---

## 10. Lazy Tool Discovery (Hidden Tools)

Plugins can declare tools as **hidden** — registered and executable, but not shown in the initial system prompt sent to the LLM. This enables lazy tool discovery patterns where a meta-tool reveals other tools on demand.

### 10.1 Use Case

When a plugin exposes many tools (e.g., 100+ MCP tools from TeamForge, Jira, Confluence), including them all in the system prompt:
- Wastes context tokens on every request
- Overwhelms the model with too many choices
- Breaks prompt caching (tools array changes per configuration)

**Solution:** Mark most tools as `hidden: true` and expose a meta-tool (e.g., `mcp_search_tools`) that returns tool specifications in its result. The agent discovers tools as needed.

### 10.2 Protocol: Hidden Flag in Tool Declaration

In the plugin hello, each tool can declare `hidden: true`:

```json
{
  "t": "hello",
  "name": "my-plugin",
  "tools": [
    {
      "name": "search_tools",
      "description": "Search and discover tools by category",
      "schema": { ... },
      "hidden": false
    },
    {
      "name": "jira_create_issue",
      "description": "Create a Jira issue",
      "schema": { ... },
      "hidden": true
    }
  ]
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `hidden` | bool | `false` | If true, tool is registered but not in initial system prompt |

### 10.3 Server Behavior

1. **All tools are registered** in `ToolRegistry` — hidden or not
2. **Only visible tools** (`hidden: false`) are sent to the LLM in the `tools` array
3. **Hidden tools can be executed** once the agent discovers them via a meta-tool
4. **Cache is stable** — hidden tools don't affect the cache prefix

```
                    ┌─────────────────────────────────────────┐
                    │           ToolRegistry                  │
                    │  ┌──────────────────────────────────┐   │
                    │  │ visible tools (hidden=false)     │──►│ sent to LLM
                    │  ├──────────────────────────────────┤   │
                    │  │ hidden tools (hidden=true)       │   │ NOT sent to LLM
                    │  └──────────────────────────────────┘   │ (but executable)
                    └─────────────────────────────────────────┘
```

### 10.4 Meta-Tool Pattern

A typical meta-tool returns tool specifications in its result:

```json
{
  "t": "done",
  "id": 1,
  "content": [{
    "type": "text",
    "text": "{\"tools\": [{\"name\": \"jira_create_issue\", \"description\": \"...\", \"parameters\": {...}}]}"
  }],
  "is_error": false
}
```

The agent sees the specs in the tool result (in context), then calls the discovered tool by name on subsequent turns.

### 10.5 Example: MCP Plugin

The `kn9t-mcp` plugin bridges to MCP servers (TeamForge, Jira, Confluence, etc.) with 100+ tools total.

**Visible tools (2):**
- `mcp_list_servers` — list available MCP servers
- `mcp_search_tools` — discover tools from a specific server

**Hidden tools (148):**
- `mcp_teamforge_get_artifact`, `mcp_jira_create_issue`, etc.

**Flow:**
```
User: "Create a Jira ticket for bug XYZ"

Agent: mcp_list_servers()
       → {"servers": [{"server": "jira", "tools": 50}, ...]}

Agent: mcp_search_tools(server="jira", query="create")
       → {"tools": [{"name": "mcp_jira_create_issue", "parameters": {...}}]}

Agent: mcp_jira_create_issue(project="PROJ", summary="Bug XYZ", ...)
       → {"key": "PROJ-1234", "url": "https://..."}
```

### 10.6 Implementation Notes

- **kn9t-core:** `ToolSpec.hidden: bool` field
- **ToolRegistry:** `visible_specs()` method filters `hidden: false`
- **ReactLoop:** uses `visible_specs()` for LLM requests
- **Execution:** `get(name)` finds ALL tools, hidden or not

For plugin authors: declare `hidden: true` for tools meant to be discovered lazily. Ensure your meta-tool returns complete specs so the agent knows how to call them.
