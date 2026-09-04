# kn9t Server HTTP API Reference

> **GENERATED FILE — do not edit by hand.** Regenerate with
> `cargo run -p xtask -- generate`. Source of truth: `schema/http.json` +
> `schema/plugin.json` (ADR-0005). The server binary is the authoritative
> implementation (R-TUI-012); this document is derived from the schema and cannot
> drift from it. Any mismatch is a bug in the schema or the server, not in this file.

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

## 2. HTTP Routes

Write operations require an **X-Lease** header: the holder token minted by
`POST /session/{id}/lease`. Routes marked **lease required** below return `409` without it.
`POST /approve` additionally needs `X-Lease-Session: <session_id>`.

### `POST /session` — Create a session

- **Lease required:** no

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `cwd` | string | no | Working directory for the session |
| `model` | ModelRef | no |  |
| `name` | string | no | Human title, suppresses auto-title |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `cwd` | string |  |
| `id` | string |  |
| `model` | ModelRef |  |
| `name` | String \| null |  |



---
### `GET /session` — List sessions (newest first)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `sessions` | object[] |  |


### `GET /session/{id}` — Session snapshot: meta, model, head_seq, transcript

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `cost_usd` | number |  |
| `ctx_tokens` | u64 |  |
| `head_seq` | u64 |  |
| `meta` | object |  |
| `model` | ModelRef |  |
| `transcript` | object[] |  |



---
### `POST /session/{id}/fork` — Fork a session at an origin seq

- **Lease required:** no

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `origin_seq` | u64 | no |  |
| `reason` | string | no |  |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `id` | string |  |



---
### `DELETE /session/{id}` — Delete a session

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `deleted` | string |  |



---
### `POST /session/{id}/lease` — Acquire the write lease (?takeover=1 force-steals)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `lease` | string |  |
| `session` | string |  |



---
### `DELETE /session/{id}/lease` — Release the write lease (holder only)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `released` | string |  |



---
### `POST /session/{id}/prompt` — Send a user prompt and run a turn

- **Lease required:** yes

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `blobs` | string[] | no |  |
| `images` | string[] | no |  |
| `text` | string | no |  |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `accepted` | bool |  |
| `seq` | u64 |  |


### `POST /session/{id}/steer` — Inject a steering message that the running turn folds in

- **Lease required:** yes

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | yes |  |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `seq` | u64 |  |
| `steered` | bool |  |


### `POST /session/{id}/abort` — Cancel the running turn

- **Lease required:** yes

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `aborted` | string |  |


### `POST /session/{id}/model` — Switch the session's active model

- **Lease required:** yes

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | yes |  |
| `provider` | string | yes |  |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `model_set` | bool |  |
| `seq` | u64 |  |


### `POST /approve` — Approve or deny a pending tool call (decision allow|deny|always, scope once|session|always)

- **Lease required:** yes

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `decision` | string | yes |  |
| `id` | u64 | yes |  |
| `scope` | string | no |  |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `approved` | u64 |  |
| `decision` | string |  |
| `scope` | string |  |


### `POST /blob` — Upload a blob (image) — raw bytes, MIME via Content-Type

- **Lease required:** no

**Request body:** raw bytes (see `contentEncoding`).

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `hash` | string |  |
| `mime` | string |  |



---
### `GET /blob/{hash}` — Download a blob by content hash (ETag + immutable cache)

- **Lease required:** no

**Request body:** none

**Response `200`:** raw bytes.

### `GET /models` — List configured models (registry + auth status)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `auth` | object |  |
| `models` | object[] |  |


### `GET /cost` — Usage cost analytics (?since=&group_by=model|kind|session)

- **Lease required:** no

**Query params**

| Param | Type | Description |
|-------|------|-------------|
| `group_by` | string | Aggregation dimension (default model) (values: model \| kind \| session) |
| `since` | u64 | Only usage rows with ts >= since (ms epoch). 0 = all. |


**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `group_by` | string |  |
| `groups` | object[] |  |
| `since` | u64 |  |
| `total_cost_usd` | number |  |


### `GET /budget` — Budget: local estimate + provider-reported spend

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `local_estimate` | number | Cost summed from local usage rows |
| `provider_reported` | number | Provider-reported spend; omitted when unavailable |


### `GET /session/{id}/events` — SSE: ?from={seq} replay durable >from then live, snake_case kind. Optional ?lease={holder}: the stream owns that lease and keeps it warm while connected (DESIGN §12.6)

- **Lease required:** no

**Request body:** none

### `GET /attach` — Global attach heartbeat

- **Lease required:** no

**Request body:** none

### `GET /pref/{key}` — Read a user preference

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `key` | string |  |
| `value` | string |  |



---
### `PUT /pref/{key}` — Write a user preference (raw text body)

- **Lease required:** no

**Request body:** raw bytes (see `contentEncoding`).

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `key` | string |  |
| `value` | string |  |



---
### `POST /stop` — Request graceful server shutdown

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `ok` | bool |  |



---
### `GET /health` — Server health + attach/turn counters

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `attached_clients` | u64 |  |
| `idle_secs` | u64 |  |
| `ok` | bool |  |
| `running_turns` | u64 |  |


### `GET /tools` — List registered tools (from discovered + pinned plugins; reflects GET /tools for TUI sidebar, F9)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `tools` | object[] |  |



---
### `POST /session/{id}/rename` — Rename a session (action endpoint, no PATCH; auto-title must not clobber a manual rename)

- **Lease required:** no

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | New human title for the session |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `id` | string |  |
| `name` | string |  |


### `POST /session/{id}/compact` — Manually trigger compaction (engine already exists at kn9t-react exec.rs:139 run_compaction; previously unreachable via TUI /compact)

- **Lease required:** yes

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `compacted` | bool |  |
| `message` | string |  |
| `seq` | u64 |  |



---
### `GET /session/{id}/export` — Export a session transcript (JSON, all messages + meta; replaces TUI /export placeholder)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `events` | object[] |  |
| `id` | string |  |
| `meta` | object |  |
| `transcript` | object[] |  |



---
### `POST /plugin/{name}/reload` — Hot-reload a plugin by name: cancel in-flight, shutdown, respawn, re-handshake, re-register tools (R-PLUG2-100)

- **Lease required:** no

**Request body:** none

**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `reloaded` | string |  |
| `tools` | u64 | total tools after reload |


### `POST /ui-respond` — 96E-28: respond to a pending generic plugin→client interaction (opaque payload, unknown id rejected)

- **Lease required:** no

**Request body**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | u64 | yes | Pending interaction id from interaction_request event |
| `payload` | object | yes | Opaque JSON response — forwarded verbatim to the waiting plugin |


**Response `200`**

| Field | Type | Description |
|-------|------|-------------|
| `payload` | object |  |
| `responded` | u64 |  |


---

## 3. SSE Event Stream

### `GET /session/{id}/events?from=<seq>&lease=<holder>` — Subscribe to events

Opens a persistent `text/event-stream`. No lease required to *subscribe*. Query param
`from` is the replay cursor: pass `0` for full history, or `last_seen_seq` on reconnect
to resume without replaying already-processed events (exact gap-free dedup on the server).

**Wire format:** each frame is `event: <kind>\ndata: <json>\n\n`. The `kind` is
**snake_case** (AGENTS.md §12) and matches the `kind` discriminator inside `data`.

**Query params**

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `from` | u64 | no | Replay cursor; durable events with `seq > from` are replayed, then the stream goes live. Default `0` (full history). |
| `lease` | string | no | Lease holder token from `POST /session/{id}/lease`. If given, **this stream owns the lease**: see "Keeping a write lease alive" below. |

**Keeping a write lease alive (client authors: read this).**

The write lease has an idle timeout (default 5 min, DESIGN §12.6). Only *successful
writes* (`prompt`/`steer`/`abort`/`model`/`compact`) refresh it. A client that holds the
lease but only reads — i.e. sits on the event stream without sending anything — would
otherwise idle-lose its lease after the timeout, and its **next `prompt` would 409
`session_busy`** even though the same client is still connected.

To avoid this, pass your lease holder as `?lease=<holder>` when you open the stream.
The server then treats this SSE connection as the *owner* of that lease:

- **Kept warm while connected** — every heartbeat (`: keepalive`) refreshes the lease's
  `last_active`, so the idle timer never fires for an attached reader.
- **Released on disconnect** — when the stream ends (client close, network drop, or
  server heartbeat write failure), the server releases that lease. On reconnect you must
  re-acquire it via `POST /session/{id}/lease` (and pass the new holder to the new stream).

Recommended client sequence: `POST /lease` → open `GET …/events?from=<seq>&lease=<holder>`
→ `POST …/prompt` with `X-Lease: <holder>`. Keep the stream open for the whole session.

**Errors:** `404` session not found.

#### Event catalogue

| kind | Fields | Durable |
|------|--------|---------|
| `message_appended` | `msg: Message`, `seq: u64` | yes |
| `usage_recorded` | `cost_usd: number`, `estimated: bool`, `model: string`, `provider: string`, `seq: u64`, `tokens: Tokens`, `usage_kind: string` | yes |
| `model_changed` | `model: ModelRef`, `seq: u64` | yes |
| `compacted` | `replaced: SeqRange`, `seq: u64`, `summary: Message` | yes |
| `turn_started` | `turn: u32` | no |
| `text_delta` | `delta: string`, `idx: u32`, `msg_id: string` | no |
| `thinking_delta` | `delta: string`, `idx: u32`, `msg_id: string` | no |
| `tool_args_delta` | `delta: string`, `idx: u32`, `msg_id: string` | no |
| `tool_started` | `call_id: string`, `name: string` | no |
| `tool_progress` | `call_id: string`, `note: string` | no |
| `tool_finished` | `call_id: string`, `is_error: bool` | no |
| `approval_request` | `args: object`, `cwd: string`, `id: u64`, `tool: string` | no |
| `turn_ended` | `stop: string`, `turn: u32` | no |
| `hook_failed` | `hook: string`, `plugin: string`, `reason: string` | no |
| `title_changed` | `title: string` | no |
| `error` | `message: string` | no |
| `retry_attempt` | `attempt: u32`, `delay_ms: u64`, `error: string`, `max: u32`, `retry_kind: string` | no |
| `turn_status` | `message: string`, `phase: string` | no |
| `plugin_notification` | `message: string`, `plugin: string` | no |
| `interaction_request` | `id: u64`, `payload: object`, `plugin: string` | no |
| `ui_directive` | `op: string`, `payload: object`, `plugin: string`, `target: string` | no |

**Durable events** carry `seq` and are replayed on reconnect. **Transient events** are
live only. Clients track the highest durable `seq` seen and reconnect with `from=last+1`.

---

## 4. Error Response Format

All error responses return JSON: `{ "error": "code", "message": "..." }`.

| Code | Meaning |
|------|---------|
| `400` | Bad request / invalid body (malformed JSON, unknown field, bad enum value) |
| `401` | Missing or invalid Bearer token |
| `403` | Origin header present, or wrong lease holder |
| `404` | Resource not found |
| `409` | Conflict (lease held by another client, or turn already running) |
| `500` | Internal server error |

---

## 5. Plugin Protocol Wire Format (NdJSON)

All plugin communication is newline-delimited JSON (NdJSON) over stdin/stdout.

### 5.1 JSON convention

**All JSON uses `snake_case`** for field names and enum variants (AGENTS.md §12).

### 5.2 Host → Plugin messages

| `t` | Fields | Notes |
|-----|--------|-------|
| `apiresult` | `error: string`, `id: u64`, `ok: bool`, `result: object` |  |
| `cancel` | `id: u64` |  |
| `hello` | `kn9t: string`, `proto: u64` |  |
| `hook` | `hook: string`, `id: u64`, `payload: object` |  |
| `shutdown` |  |  |

### 5.3 Plugin → Host messages

| `t` | Fields | Notes |
|-----|--------|-------|
| `chunk` | `id: u64` |  |
| `done` | `id: u64` |  |
| `hello` | `capabilities: string[]`, `events: string[]`, `hooks: string[]`, `name: string`, `provider: object`, `tools: object[]` |  |
| `request` | `id: u64`, `op: string`, `payload: object` | Plugin → host API request (host_api capability). Ops: provider_complete, session_read, tool_execute, session_fork, session_prompt, tool_list. |
| `result` | `id: u64` |  |

### 5.4 `ModelDecl`

| Field | Type | Description |
|-------|------|-------------|
| `ctx_window` | u64 |  |
| `id` | string |  |
| `price` | object |  |

### 5.5 `ProviderDecl`

| Field | Type | Description |
|-------|------|-------------|
| `id` | string |  |
| `models` | object[] |  |

### 5.6 `ToolSpec`

| Field | Type | Description |
|-------|------|-------------|
| `description` | string |  |
| `effects` | object[] |  |
| `hidden` | bool |  |
| `name` | string |  |
| `parallel_safe` | bool |  |
| `schema` | object |  |

### 5.5 Handshake sequence

```
Host   →  {"t": "hello", "proto": 1, "kn9t": "0.1.0"}
Plugin →  {"t": "hello", "name": "my-plugin", "hooks": [...], "tools": [...], "capabilities": [...], "events": [...]}
```

`proto` is the protocol version (currently `1`); `kn9t` is the server version string.
A plugin declares itself with `name` (unique id), `hooks`, `tools` (see `ToolSpec`),
`capabilities` (e.g. `["streaming", "cancelable"]`), and the `events` it wants.

### 5.6 Flattened body fields

The protocol wraps hook **bodies** with `#[serde(flatten)]` — body fields sit at the
**same level** as `t` and `id`, NOT nested under a `"body"` key:

```json
// ✅ correct — body fields flattened:
{"t": "result", "id": 7, "messages": [...]}
{"t": "done",  "id": 42, "content": [...], "is_error": false}

// ❌ wrong — nested body:
{"t": "result", "id": 7, "body": {"messages": [...]}}
```

This applies to `result`, `chunk`, and `done` plugin→host messages.

### 5.7 Lifecycle hooks

Plugins subscribe to lifecycle hooks in the hello message. Each hook invocation is
`{"t": "hook", "id": <int>, "hook": "<name>", "payload": {...}}`; the plugin answers with
`{"t": "result", "id": <same>, ...flattened reply fields}`.

| Hook | Payload | Reply | Composition | Failure posture |
|------|---------|-------|-------------|-----------------|
| `before_tool_call` | `{session_id, tool, args}` | `{action: "allow"\|"deny"\|"replace", ...}` | First-deny-wins | **Deny** (fail closed) |
| `after_tool_call` | `{session_id, tool, args, result}` | `{action: "keep"\|"replace", content}` | Pipeline | Keep original |
| `before_request` | `{session_id, messages, model, system}` | `{action: "keep"\|"replace", messages}` | Pipeline | Use original |
| `should_stop_after_turn` | `{session_id, stop, usage, turn}` | `{action: "continue"\|"stop"}` | Any-says-stop | Continue |
| `prepare_next_turn` | `{session_id, stop, usage}` | `{model?, thinking?}` | Pipeline | No change |
| `get_steering` | `{session_id}` | `{messages}` | Concat | Empty |
| `get_followup` | `{session_id}` | `{messages}` | Concat | Empty |
| `get_api_key` | `{session_id, provider}` | `{key}` | First non-null | Fall back to config |

**All hooks include `session_id`**, so plugins can keep per-session state.
---

## 6. Common Types

### Message

```json
{ "id": "string", "role": "user" | "assistant" | "tool" | "system", "content": [Content], "silent": bool? }
```

`silent: true` messages are persisted and sent to the LLM but **not displayed** in clients.

### Content (union, discriminated by `type`)

| `type` | Additional fields |
|--------|-------------------|
| `"text"` | `text: string` |
| `"tool_call"` | `id: string`, `name: string`, `args_json: string` |
| `"tool_result"` | `id: string`, `content: Content[]`, `is_error: bool` |
| `"thinking"` | `text: string` |
| `"image"` | `sha256: string`, `mime: string` |

### Tokens

```json
{ "input": u64, "output": u64, "cache_read": u64, "cache_write": u64, "reasoning": u64 }
```

### ModelRef

```json
{ "provider": "string", "id": "string" }
```
