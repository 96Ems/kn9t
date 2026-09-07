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

const LEASE_PROSE: &str =
    "Write operations require an **X-Lease** header: the holder token minted by\n\
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
    s.push_str(
        "All error responses return JSON: `{ \"error\": \"code\", \"message\": \"...\" }`.\n\n",
    );
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
    let title = route.description.unwrap_or("(no description in schema)");
    s.push_str(&format!(
        "### `{} {}` — {title}\n\n",
        route.method, route.path
    ));
    s.push_str(&format!(
        "- **Lease required:** {}\n",
        if route.lease { "yes" } else { "no" }
    ));

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
///
/// `markdown_type` already spells out enum members, and `describe` adds any
/// `default`, so a param's allowed values and fallback are both visible.
fn query_params_table(query: &Value) -> String {
    let Some(map) = query.as_object() else {
        return String::new();
    };
    let mut s = String::new();
    s.push_str("| Param | Type | Description |\n|-------|------|-------------|\n");
    for (key, prop) in map.iter() {
        let ty = markdown_type(prop);
        let desc = describe(prop);
        s.push_str(&format!("| `{key}` | {ty} | {desc} |\n"));
    }
    s.push('\n');
    s
}

/// Render an object subschema as a markdown property table.
///
/// Emits every fact the schema states about each property: type (with `$ref`s
/// resolved and enums spelled out by `markdown_type`), requiredness, default,
/// and — for a nested object or array-of-object with no named definition — an
/// inline `{a, b, c}` shape. Previously nested shapes collapsed to a bare
/// `object`, so e.g. `GET /models`'s per-model fields were entirely invisible.
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
        let desc = describe(prop);
        if is_response {
            s.push_str(&format!("| `{key}` | {ty} | {desc} |\n"));
        } else {
            let required_flag = if req.iter().any(|r| r == &key) {
                "yes"
            } else {
                "no"
            };
            s.push_str(&format!("| `{key}` | {ty} | {required_flag} | {desc} |\n"));
        }
    }
    s.push('\n');
    s
}

/// Description cell for a property: the schema's own `description`, plus any
/// `default` and any inline nested shape that would otherwise be dropped.
///
/// Kept in one place so every table (routes, definitions, messages, hooks)
/// surfaces the same facts instead of each caller remembering a subset.
fn describe(prop: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(d) = prop.get("description").and_then(|d| d.as_str()) {
        if !d.is_empty() {
            parts.push(d.to_string());
        }
    }

    // A nested object with no `$ref` has no section to link to, so name its
    // fields here or they appear nowhere in the document at all. When there *is*
    // a `$ref`, the type column already names a type with its own section, and
    // repeating the shape inline would be noise.
    if crate::schema::ref_name(prop).is_none() {
        let nested = match prop.get("type").and_then(|t| t.as_str()) {
            // `is_model_ref` shapes render as `ModelRef` in the type column via
            // `rust_type`, so they too already point at a documented section.
            Some("object") if !crate::schema::is_model_ref(prop) => {
                crate::schema::inline_shape(prop)
            }
            Some("array") => prop
                .get("items")
                .filter(|i| crate::schema::ref_name(i).is_none())
                .and_then(crate::schema::inline_shape)
                .map(|shape| format!("{shape}[]")),
            _ => None,
        };
        if let Some(shape) = nested {
            parts.push(format!("Shape: `{shape}`"));
        }
    }

    if let Some(d) = crate::schema::default_display(prop) {
        parts.push(format!("Default: {d}"));
    }

    parts.join(". ")
}

fn emit_sse(http: &Value) -> String {
    let mut s = String::new();
    s.push_str(
        "### `GET /session/{id}/events?from=<seq>&lease=<holder>` — Subscribe to events\n\n",
    );
    s.push_str(
        "Opens a persistent `text/event-stream`. No lease required to *subscribe*. Query param\n",
    );
    s.push_str(
        "`from` is the replay cursor: pass `0` for full history, or `last_seen_seq` on reconnect\n",
    );
    s.push_str("to resume without replaying already-processed events (exact gap-free dedup on the server).\n\n");
    s.push_str(
        "**Wire format:** each frame is `event: <kind>\\ndata: <json>\\n\\n`. The `kind` is\n",
    );
    s.push_str(
        "**snake_case** (AGENTS.md §12) and matches the `kind` discriminator inside `data`.\n\n",
    );
    s.push_str("**Query params**\n\n");
    s.push_str("| Param | Type | Required | Description |\n");
    s.push_str("|-------|------|----------|-------------|\n");
    s.push_str("| `from` | u64 | no | Replay cursor; durable events with `seq > from` are replayed, then the stream goes live. Default `0` (full history). |\n");
    s.push_str("| `lease` | string | no | Lease holder token from `POST /session/{id}/lease`. If given, **this stream owns the lease**: see \"Keeping a write lease alive\" below. |\n\n");
    s.push_str("**Keeping a write lease alive (client authors: read this).**\n\n");
    s.push_str(
        "The write lease has an idle timeout (default 5 min, DESIGN §12.6). Only *successful\n",
    );
    s.push_str("writes* (`prompt`/`steer`/`abort`/`model`/`compact`) refresh it. A client that holds the\n");
    s.push_str(
        "lease but only reads — i.e. sits on the event stream without sending anything — would\n",
    );
    s.push_str(
        "otherwise idle-lose its lease after the timeout, and its **next `prompt` would 409\n",
    );
    s.push_str("`session_busy`** even though the same client is still connected.\n\n");
    s.push_str(
        "To avoid this, pass your lease holder as `?lease=<holder>` when you open the stream.\n",
    );
    s.push_str("The server then treats this SSE connection as the *owner* of that lease:\n\n");
    s.push_str(
        "- **Kept warm while connected** — every heartbeat (`: keepalive`) refreshes the lease's\n",
    );
    s.push_str("  `last_active`, so the idle timer never fires for an attached reader.\n");
    s.push_str(
        "- **Released on disconnect** — when the stream ends (client close, network drop, or\n",
    );
    s.push_str("  server heartbeat write failure), the server releases that lease. On reconnect you must\n");
    s.push_str("  re-acquire it via `POST /session/{id}/lease` (and pass the new holder to the new stream).\n\n");
    s.push_str("Recommended client sequence: `POST /lease` → open `GET …/events?from=<seq>&lease=<holder>`\n");
    s.push_str("→ `POST …/prompt` with `X-Lease: <holder>`. Keep the stream open for the whole session.\n\n");
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
    s.push_str(
        "**Durable events** carry `seq` and are replayed on reconnect. **Transient events** are\n",
    );
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
    s.push_str(
        "All plugin communication is newline-delimited JSON (NdJSON) over stdin/stdout.\n\n",
    );
    s.push_str("### 5.1 JSON convention\n\n");
    s.push_str(
        "**All JSON uses `snake_case`** for field names and enum variants (AGENTS.md §12).\n\n",
    );

    s.push_str("### 5.2 Host → Plugin messages\n\n");
    s.push_str(&msg_table(plugin.get("host_to_plugin")));
    s.push_str("### 5.3 Plugin → Host messages\n\n");
    s.push_str(&msg_table(plugin.get("plugin_to_host")));

    // Numbering is threaded rather than computed per-block: the definitions loop
    // used to emit `5.4 + i` while the sections after it were hardcoded to
    // `5.5`/`5.6`/`5.7`, so with seven definitions the document contained two
    // `5.5`s, two `5.6`s and two `5.7`s, and the anchors collided.
    let mut n = 4;
    if let Some(defs) = plugin.get("definitions").and_then(|d| d.as_object()) {
        for (name, subschema) in defs.iter() {
            s.push_str(&format!("### 5.{n} `{name}`\n\n"));
            if let Some(d) = subschema.get("description").and_then(|d| d.as_str()) {
                s.push_str(&format!("{d}\n\n"));
            }
            s.push_str(&props_table(subschema, true));
            n += 1;
        }
    }

    s.push_str(&format!("### 5.{n} Handshake sequence\n\n"));
    s.push_str(HANDSHAKE_PROSE);
    n += 1;
    s.push_str(&format!("\n### 5.{n} Flattened body fields\n\n"));
    s.push_str(FLATTEN_PROSE);
    n += 1;
    s.push_str(&format!("\n### 5.{n} Lifecycle hooks\n\n"));
    s.push_str(&hooks_table(plugin));
    s
}

/// Hook table, driven by `schema/plugin.json`'s `hook_payloads`.
///
/// Iterates the **schema**, not a hardcoded list, so a hook added there always
/// appears — `tool_call` was previously absent from the document entirely
/// because it was missing from the local list. Payload fields come from the
/// schema too; only the reply/composition/failure columns are prose, since they
/// describe host behaviour the schema does not express.
///
/// A hook with no prose entry still renders (with `—` in those columns) rather
/// than vanishing, and a prose entry naming a hook the schema lacks renders as
/// `*(not in schema)*`. Both mismatches are visible instead of silent.
fn hooks_table(plugin: &Value) -> String {
    // hook -> (reply, composition, failure posture)
    const SEMANTICS: &[(&str, &str, &str, &str)] = &[
        (
            "before_tool_call",
            r#"`{action: "allow"\|"deny"\|"replace", ...}`"#,
            "First-deny-wins",
            "**Deny** (fail closed)",
        ),
        (
            "after_tool_call",
            r#"`{action: "keep"\|"replace", content}`"#,
            "Pipeline",
            "Keep original",
        ),
        (
            "before_request",
            r#"`{action: "keep"\|"replace", messages}`"#,
            "Pipeline",
            "Use original",
        ),
        (
            "should_stop_after_turn",
            r#"`{action: "continue"\|"stop"}`"#,
            "Any-says-stop",
            "Continue",
        ),
        (
            "prepare_next_turn",
            "`{model?, thinking?}`",
            "Pipeline",
            "No change",
        ),
        ("get_steering", "`{messages}`", "Concat", "Empty"),
        ("get_followup", "`{messages}`", "Concat", "Empty"),
        (
            "get_api_key",
            "`{key}`",
            "First non-null",
            "Fall back to config",
        ),
        // Not a lifecycle interceptor but a real dispatch the host sends, and it
        // is in `hook_payloads`; documenting it here beats omitting it.
        (
            "tool_call",
            "`{content, is_error}`",
            "Single handler",
            "Error result",
        ),
    ];

    let mut s = String::new();
    s.push_str(HOOKS_INTRO);
    s.push_str("\n| Hook | Payload | Reply | Composition | Failure posture |\n");
    s.push_str("|------|---------|-------|-------------|-----------------|\n");

    let hooks = plugin.get("hook_payloads").and_then(|h| h.as_object());
    let semantics_for = |name: &str| {
        SEMANTICS
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, r, c, f)| (*r, *c, *f))
    };

    // Schema order drives the rows, so nothing in the schema can be missed.
    let mut rendered: Vec<&str> = Vec::new();
    if let Some(map) = hooks {
        for (name, sub) in map.iter() {
            let fields: Vec<String> = properties(sub).into_iter().map(|(k, _)| k).collect();
            let payload = format!("`{{{}}}`", fields.join(", "));
            let (reply, composition, failure) =
                semantics_for(name).unwrap_or(("—", "—", "—"));
            s.push_str(&format!(
                "| `{name}` | {payload} | {reply} | {composition} | {failure} |\n"
            ));
            rendered.push(name);
        }
    }

    // Anything described in prose but absent from the schema: surface the gap.
    for (name, reply, composition, failure) in SEMANTICS {
        if !rendered.contains(name) {
            s.push_str(&format!(
                "| `{name}` | *(not in schema)* | {reply} | {composition} | {failure} |\n"
            ));
        }
    }

    s.push_str(HOOKS_OUTRO);

    // Field-level detail per hook. The summary table above lists only field
    // names, which loses types, requiredness and — critically — enum members:
    // `should_stop_after_turn.stop` has four allowed values that appeared
    // nowhere in this document before.
    if let Some(map) = hooks {
        s.push_str("\n#### Hook payload fields\n\n");
        for (name, sub) in map.iter() {
            if properties(sub).is_empty() {
                continue;
            }
            s.push_str(&format!("**`{name}`**\n\n"));
            s.push_str(&props_table(sub, false));
        }
    }

    s
}

/// One-row-per-message summary of a `host_to_plugin` / `plugin_to_host` map.
///
/// Required fields are marked, so a client author can tell `id` (always present)
/// from an optional payload without cross-referencing the schema by hand.
fn msg_table(obj: Option<&Value>) -> String {
    let Some(map) = obj.and_then(|o| o.as_object()) else {
        return String::new();
    };
    let mut s = String::new();
    s.push_str("| `t` | Fields | Notes |\n|-----|--------|-------|\n");
    for (name, subschema) in map {
        let req = required(subschema);
        let props = properties(subschema);
        let names: Vec<String> = props
            .iter()
            .filter(|(k, _)| k != "t")
            .map(|(k, p)| {
                // `?` marks optional, matching the `{model?, thinking?}` notation
                // already used for hook replies elsewhere in this document.
                let opt = if req.iter().any(|r| r == k) { "" } else { "?" };
                format!("`{k}{opt}: {}`", markdown_type(p))
            })
            .collect();
        let notes = subschema
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        s.push_str(&format!(
            "| `{}` | {} | {notes} |\n",
            name.to_lowercase(),
            names.join(", ")
        ));
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

const HOOKS_INTRO: &str = r#"Plugins subscribe to lifecycle hooks in the hello message. Each hook invocation is
`{"t": "hook", "id": <int>, "hook": "<name>", "payload": {...}}`; the plugin answers with
`{"t": "result", "id": <same>, ...flattened reply fields}`.
"#;

const HOOKS_OUTRO: &str = r#"
**All hooks include `session_id`**, so plugins can keep per-session state.

Hooks whose payload includes `cwd` also carry the session's working directory.
This is what lets a plugin bootstrap per-session background work (a poller, a
file watcher) from a real lifecycle signal - `get_steering` fires every turn -
instead of shipping a dummy agent-callable tool purely to obtain a host channel.
In the Rust SDK, override `PluginHook::call_with_ctx` to receive a
`HookCtx { host, session_id, cwd }`; plain `call` remains available for hooks
that only transform their payload.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas() -> (Value, Value) {
        // `env!` resolves at compile time, so this works regardless of the cwd
        // the test binary happens to run in.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives one level below the workspace root")
            .to_path_buf();
        let http = serde_json::from_str(
            &std::fs::read_to_string(root.join("schema/http.json")).expect("read http.json"),
        )
        .expect("parse http.json");
        let plugin = serde_json::from_str(
            &std::fs::read_to_string(root.join("schema/plugin.json")).expect("parse plugin.json"),
        )
        .expect("parse plugin.json");
        (http, plugin)
    }

    fn rendered() -> String {
        let (http, plugin) = schemas();
        generate(&http, &plugin).expect("generator must succeed")
    }

    /// Walk every property name the schemas define, at any depth, and require it
    /// to appear somewhere in API.md.
    ///
    /// This is the guard that makes "the document cannot omit anything" true
    /// rather than aspirational. Before it existed, `GET /models`'s per-model
    /// fields (`ctx_window`, `max_out`, `price`) and the whole `tool_call` hook
    /// were silently absent — the generator dropped nested shapes and iterated a
    /// hardcoded hook list.
    #[test]
    fn every_schema_property_appears_in_the_document() {
        let doc = rendered();
        let (http, plugin) = schemas();

        fn collect(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    if let Some(Value::Object(props)) = map.get("properties") {
                        for (k, sub) in props {
                            out.push(k.clone());
                            collect(sub, out);
                        }
                    }
                    // Recurse through everything else too (items, definitions,
                    // routes, ...) so nesting depth is irrelevant.
                    for (k, sub) in map {
                        if k != "properties" {
                            collect(sub, out);
                        }
                    }
                }
                Value::Array(items) => {
                    for it in items {
                        collect(it, out);
                    }
                }
                _ => {}
            }
        }

        let mut names = Vec::new();
        collect(&http, &mut names);
        collect(&plugin, &mut names);
        names.sort();
        names.dedup();
        assert!(!names.is_empty(), "sanity: schemas define properties");

        let missing: Vec<&String> = names.iter().filter(|n| !doc.contains(n.as_str())).collect();
        assert!(
            missing.is_empty(),
            "these schema properties never appear in API.md: {missing:?}"
        );
    }

    /// Every route, SSE event, definition and hook must have its own entry.
    #[test]
    fn every_schema_entity_appears_in_the_document() {
        let doc = rendered();
        let (http, plugin) = schemas();

        for route in routes(&http) {
            assert!(
                doc.contains(route.path),
                "route {} {} is undocumented",
                route.method,
                route.path
            );
        }
        for ev in sse_events(&http) {
            assert!(
                doc.contains(&format!("`{}`", ev.kind)),
                "SSE event '{}' is undocumented",
                ev.kind
            );
        }
        for group in ["definitions", "hook_payloads", "host_to_plugin", "plugin_to_host"] {
            let Some(map) = plugin.get(group).and_then(|g| g.as_object()) else {
                panic!("schema/plugin.json is missing '{group}'");
            };
            for name in map.keys() {
                // Messages are titled by their lowercased `t` value.
                let lowered = name.to_lowercase();
                assert!(
                    doc.contains(&format!("`{name}`")) || doc.contains(&format!("`{lowered}`")),
                    "{group} entry '{name}' is undocumented"
                );
            }
        }
    }

    /// Enum members are the allowed values of a field; omitting them makes a
    /// request unwritable without reading the schema.
    #[test]
    fn enum_members_are_spelled_out() {
        let doc = rendered();
        let (http, plugin) = schemas();

        fn collect_enums(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    if let Some(Value::Array(members)) = map.get("enum") {
                        for m in members.iter().filter_map(|m| m.as_str()) {
                            out.push(m.to_string());
                        }
                    }
                    for sub in map.values() {
                        collect_enums(sub, out);
                    }
                }
                Value::Array(items) => {
                    for it in items {
                        collect_enums(it, out);
                    }
                }
                _ => {}
            }
        }

        let mut members = Vec::new();
        collect_enums(&http, &mut members);
        collect_enums(&plugin, &mut members);
        members.sort();
        members.dedup();
        assert!(!members.is_empty(), "sanity: schemas define enums");

        let missing: Vec<&String> = members
            .iter()
            .filter(|m| !doc.contains(m.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "these enum values never appear in API.md: {missing:?}"
        );
    }

    /// Section numbers are threaded through the plugin chapter; a duplicate
    /// means two headings collide and in-page anchors break.
    #[test]
    fn section_numbers_are_unique_and_sequential() {
        let doc = rendered();
        let mut nums: Vec<u32> = doc
            .lines()
            .filter_map(|l| l.strip_prefix("### 5."))
            .filter_map(|rest| {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse().ok()
            })
            .collect();

        assert!(nums.len() > 3, "expected several 5.x sections, got {nums:?}");
        let before = nums.clone();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(
            before.len(),
            nums.len(),
            "duplicate 5.x section numbers in {before:?}"
        );
        assert_eq!(before, nums, "5.x sections must be emitted in order");
    }

    /// Byte-identical across runs, so `xtask -- check` cannot report phantom drift.
    #[test]
    fn generation_is_deterministic() {
        assert_eq!(rendered(), rendered());
    }
}
