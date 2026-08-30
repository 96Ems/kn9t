//! Generator for `API.md` — the human-readable contract.
//!
//! Derived entirely from `schema/http.json` + `schema/plugin.json` (ADR-0005).
//! API.md must **never** be hand-edited again; `scripts/check-schema.sh` fails on
//! drift. The server is authoritative for behavior (R-TUI-012); this document is
//! the schema, rendered.

use std::path::Path;

use serde_json::Value;

use crate::schema::{markdown_type, properties, required, routes, sse_events};

const HEADER: &str = "# kn9t Server HTTP API Reference

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

";

const ERRORS_TABLE: &str = "| Code | Meaning |\n\
    |------|---------|\n\
    | `400` | Bad request / invalid body (malformed JSON, unknown field, bad enum value) |\n\
    | `401` | Missing or invalid Bearer token |\n\
    | `403` | Origin header present, or wrong lease holder |\n\
    | `404` | Resource not found |\n\
    | `409` | Conflict (lease held by another client, or turn already running) |\n\
    | `500` | Internal server error |\n";

const LEASE_PROSE: &str = "Write operations require an **X-Lease** header: the holder token minted by\n\
    `POST /session/{id}/lease`. Routes marked **lease required** below return `409` without it.\n\
    `POST /approve` additionally needs `X-Lease-Session: <session_id>`.\n";

pub fn write(root: &Path, http: &Value, plugin: &Value) -> Result<(), String> {
    let out = generate(http, plugin)?;
    let path = root.join("API.md");
    std::fs::write(&path, out.as_bytes()).map_err(|e| format!("write API.md: {e}"))?;
    Ok(())
}

pub fn generate(http: &Value, plugin: &Value) -> Result<String, String> {
    let mut s = String::new();
    s.push_str(HEADER);
    s.push_str(LEASE_PROSE);
    s.push('\n');

    // ── routes ──
    let mut last_method = "";
    let mut first = true;
    for route in routes(http) {
        if route.method != last_method {
            if !first {
                s.push_str("\n---\n");
            }
            last_method = route.method;
        }
        first = false;
        s.push_str(&emit_route(&route));
        s.push('\n');
    }

    // ── sse ──
    s.push_str("---\n\n## 3. SSE Event Stream\n\n");
    s.push_str(&emit_sse(http));
    s.push('\n');

    // ── errors ──
    s.push_str("---\n\n## 4. Error Response Format\n\n");
    s.push_str("All error responses return JSON: `{ \"error\": \"code\", \"message\": \"...\" }`.\n\n");
    s.push_str(ERRORS_TABLE);
    s.push('\n');

    // ── plugin protocol ──
    s.push_str("---\n\n## 5. Plugin Protocol Wire Format (NdJSON)\n\n");
    s.push_str(&emit_plugin(plugin));

    // ── common types ──
    s.push_str("---\n\n## 6. Common Types\n\n");
    s.push_str(COMMON_TYPES);

    Ok(s)
}

fn emit_route(route: &crate::schema::Route<'_>) -> String {
    let mut s = String::new();
    let title = route
        .description
        .unwrap_or("(no description in schema)");
    s.push_str(&format!("### `{} {}` — {title}\n\n", route.method, route.path));
    s.push_str(&format!("- **Lease required:** {}\n", if route.lease { "yes" } else { "no" }));

    if let Some(q) = route.query {
        if !q.is_null() {
            s.push_str("\n**Query params**\n\n");
            s.push_str(&query_params_table(q));
        }
    }

    match route.request_object() {
        Some(body) => {
            s.push_str("\n**Request body**\n\n");
            s.push_str(&props_table(body, false));
        }
        None => match route.request {
            Some(r) if r.get("type").and_then(|t| t.as_str()) == Some("string") => {
                s.push_str("\n**Request body:** raw bytes (see `contentEncoding`).\n");
            }
            _ => {
                s.push_str("\n**Request body:** none\n");
            }
        },
    }

    match route.response_object() {
        Some(body) => {
            s.push_str("\n**Response `200`**\n\n");
            s.push_str(&props_table(body, true));
        }
        None => match route.response {
            Some(r) if r.get("type").and_then(|t| t.as_str()) == Some("string") => {
                s.push_str("\n**Response `200`:** raw bytes.\n");
            }
            _ => {}
        },
    }
    s
}

/// Render a route-level `query` map (`{since: {...}, group_by: {...}}`) as a table.
fn query_params_table(query: &Value) -> String {
    let Some(map) = query.as_object() else {
        return String::new();
    };
    let mut s = String::new();
    s.push_str("| Param | Type | Description |\n|-------|------|-------------|\n");
    for (key, prop) in map.iter() {
        let ty = markdown_type(prop);
        let desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        if let Some(en) = prop.get("enum").and_then(|e| e.as_array()) {
            let vals: Vec<&str> = en.iter().filter_map(|v| v.as_str()).collect();
            s.push_str(&format!(
                "| `{key}` | {ty} | {desc} (values: {}) |\n",
                vals.join(" \\| ")
            ));
        } else {
            s.push_str(&format!("| `{key}` | {ty} | {desc} |\n"));
        }
    }
    s.push('\n');
    s
}

/// Render an object subschema as a markdown property table.
fn props_table(body: &Value, is_response: bool) -> String {
    let req = required(body);
    let props = properties(body);
    if props.is_empty() {
        return "Opaque object (`type: object`, no schema properties).\n".to_string();
    }
    let mut s = String::new();
    if is_response {
        s.push_str("| Field | Type | Description |\n|-------|------|-------------|\n");
    } else {
        s.push_str("| Field | Type | Required | Description |\n|-------|------|----------|-------------|\n");
    }
    for (key, prop) in props {
        let ty = markdown_type(prop);
        let desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        if is_response {
            s.push_str(&format!("| `{key}` | {ty} | {desc} |\n"));
        } else {
            let required_flag = if req.iter().any(|r| r == &key) { "yes" } else { "no" };
            s.push_str(&format!("| `{key}` | {ty} | {required_flag} | {desc} |\n"));
        }
    }
    s.push('\n');
    s
}

fn emit_sse(http: &Value) -> String {
    let mut s = String::new();
    s.push_str("### `GET /session/{id}/events?from=<seq>` — Subscribe to events\n\n");
    s.push_str("Opens a persistent `text/event-stream`. No lease required. Query param `from` is the\n");
    s.push_str("replay cursor: pass `0` for full history, or `last_seen_seq` on reconnect to resume\n");
    s.push_str("without replaying already-processed events (exact gap-free dedup on the server).\n\n");
    s.push_str("**Wire format:** each frame is `event: <kind>\\ndata: <json>\\n\\n`. The `kind` is\n");
    s.push_str("**snake_case** (AGENTS.md §12) and matches the `kind` discriminator inside `data`.\n\n");
    s.push_str("**Errors:** `404` session not found.\n\n");

    let events = sse_events(http);
    s.push_str("#### Event catalogue\n\n");
    s.push_str("| kind | Fields | Durable |\n|------|--------|---------|\n");
    for ev in &events {
        let durable = ev.fields.iter().any(|(k, _)| k == "seq");
        let fields: Vec<String> = ev
            .fields
            .iter()
            .map(|(k, t)| format!("`{k}: {}`", sse_md_type(t)))
            .collect();
        s.push_str(&format!(
            "| `{}` | {} | {} |\n",
            ev.kind,
            fields.join(", "),
            if durable { "yes" } else { "no" }
        ));
    }
    s.push('\n');
    s.push_str("**Durable events** carry `seq` and are replayed on reconnect. **Transient events** are\n");
    s.push_str("live only. Clients track the highest durable `seq` seen and reconnect with `from=last+1`.\n");
    s
}

/// Display type for an SSE field (compact schema strings).
fn sse_md_type(t: &str) -> String {
    match t {
        "u64" | "u32" => t.to_string(),
        "string" => "string".to_string(),
        "number" => "number".to_string(),
        "bool" => "bool".to_string(),
        "Message" => "Message".to_string(),
        "ModelRef" => "ModelRef".to_string(),
        "SeqRange" => "SeqRange".to_string(),
        "Tokens" => "Tokens".to_string(),
        "Value" => "object".to_string(),
        other => other.to_string(),
    }
}

fn emit_plugin(plugin: &Value) -> String {
    let mut s = String::new();
    s.push_str("All plugin communication is newline-delimited JSON (NdJSON) over stdin/stdout.\n\n");
    s.push_str("### 5.1 JSON convention\n\n");
    s.push_str("**All JSON uses `snake_case`** for field names and enum variants (AGENTS.md §12).\n\n");

    s.push_str("### 5.2 Host → Plugin messages\n\n");
    s.push_str(&msg_table(plugin.get("host_to_plugin")));
    s.push_str("### 5.3 Plugin → Host messages\n\n");
    s.push_str(&msg_table(plugin.get("plugin_to_host")));

    if let Some(defs) = plugin.get("definitions").and_then(|d| d.as_object()) {
        for (i, (name, subschema)) in defs.iter().enumerate() {
            s.push_str(&format!("### 5.{} `{name}`\n\n", 4 + i));
            s.push_str(&props_table(subschema, true));
        }
    }

    s.push_str("### 5.5 Handshake sequence\n\n");
    s.push_str(HANDSHAKE_PROSE);
    s.push_str("\n### 5.6 Flattened body fields\n\n");
    s.push_str(FLATTEN_PROSE);
    s.push_str("\n### 5.7 Lifecycle hooks\n\n");
    s.push_str(HOOKS_PROSE);
    s
}

fn msg_table(obj: Option<&Value>) -> String {
    let Some(map) = obj.and_then(|o| o.as_object()) else {
        return String::new();
    };
    let mut s = String::new();
    s.push_str("| `t` | Fields | Notes |\n|-----|--------|-------|\n");
    for (name, subschema) in map {
        let props = properties(subschema);
        let names: Vec<String> = props
            .iter()
            .filter(|(k, _)| k != "t")
            .map(|(k, p)| format!("`{k}: {}`", markdown_type(p)))
            .collect();
        let notes = subschema
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        s.push_str(&format!("| `{}` | {} | {notes} |\n", name.to_lowercase(), names.join(", ")));
    }
    s.push('\n');
    s
}

const HANDSHAKE_PROSE: &str = r#"```
Host   →  {"t": "hello", "proto": 1, "kn9t": "0.1.0"}
Plugin →  {"t": "hello", "name": "my-plugin", "hooks": [...], "tools": [...], "capabilities": [...], "events": [...]}
```

`proto` is the protocol version (currently `1`); `kn9t` is the server version string.
A plugin declares itself with `name` (unique id), `hooks`, `tools` (see `ToolSpec`),
`capabilities` (e.g. `["streaming", "cancelable"]`), and the `events` it wants.
"#;

const FLATTEN_PROSE: &str = r#"The protocol wraps hook **bodies** with `#[serde(flatten)]` — body fields sit at the
**same level** as `t` and `id`, NOT nested under a `"body"` key:

```json
// ✅ correct — body fields flattened:
{"t": "result", "id": 7, "messages": [...]}
{"t": "done",  "id": 42, "content": [...], "is_error": false}

// ❌ wrong — nested body:
{"t": "result", "id": 7, "body": {"messages": [...]}}
```

This applies to `result`, `chunk`, and `done` plugin→host messages.
"#;

const HOOKS_PROSE: &str = r#"Plugins subscribe to lifecycle hooks in the hello message. Each hook invocation is
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
"#;

const COMMON_TYPES: &str = r#"### Message

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
"#;