//! Generator for `crates/kn9t-tui/src/wire.rs` — the TUI's serde mirrors of the
//! server API.
//!
//! **GI-6 (R-TUI-010) must survive generation:** the emitted file depends only on
//! `serde` / `serde_json` and never on any `kn9t-*` crate. `scripts/check-schema.sh`
//! re-asserts the manifest stays clean after every regeneration.

use std::path::Path;

use serde_json::Value;

use crate::schema::{
    prop_type, properties, req_name_for_path, required, routes, rust_type, sse_events, to_camel,
};

const HEADER: &str = "//! Wire types — serde mirrors of the server API.
//!
//! GENERATED FILE — do not edit by hand. Regenerate with `cargo run -p xtask -- generate`.
//! Source of truth: `schema/http.json` + `schema/plugin.json` (ADR-0005).
//!
//! R-TUI-010 / GI-6: no `kn9t-*` dependency — standalone serde-only file,
//! verifiable by `crates/kn9t-tui/tests/acceptance.rs::tui_no_kn9t_deps`.
//! R-TUI-012: matches the schema wire format exactly; the server is authoritative.

";

pub fn write(root: &Path, http: &Value) -> Result<(), String> {
    let out = generate(http)?;
    let path = root.join("crates/kn9t-tui/src/wire.rs");
    std::fs::write(&path, out.as_bytes()).map_err(|e| format!("write wire.rs: {e}"))?;
    Ok(())
}

pub fn generate(http: &Value) -> Result<String, String> {
    let mut s = String::new();
    s.push_str(HEADER);
    s.push_str("use serde::{Deserialize, Serialize};\n\n");

    s.push_str(&emit_sse_frames(http));
    s.push('\n');
    s.push_str(PINNED_MESSAGE_TYPES);
    s.push('\n');
    s.push_str(&emit_http_responses(http));
    s.push('\n');
    s.push_str(&emit_http_requests(http));
    s.push('\n');
    s.push_str(&model_types_struct(http));
    Ok(s)
}

// ── SSE frames ─────────────────────────────────────────────────────────────────

/// Map the schema's compact SSE field-type strings to Rust types.
fn sse_type(t: &str) -> &'static str {
    match t {
        "u64" => "u64",
        "u32" => "u32",
        "string" => "String",
        "string[]" => "Vec<String>",
        "number" => "f64",
        "bool" => "bool",
        "Message" => "WireMessage",
        "ModelRef" => "WireModelRef",
        "SeqRange" => "WireSeqRange",
        "Tokens" => "WireTokens",
        "Value" => "serde_json::Value",
        _ => "serde_json::Value",
    }
}

fn is_durable(fields: &[(String, String)]) -> bool {
    fields.iter().any(|(k, _)| k == "seq")
}

fn emit_sse_frames(http: &Value) -> String {
    let events = sse_events(http);
    let mut s = String::new();
    s.push_str(
        "/// SSE frame from the server — `#[serde(tag = \"kind\", rename_all = \"snake_case\")]`\n",
    );
    s.push_str("/// per AGENTS.md §12. Durable events carry `seq`; transient events do not.\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("#[serde(tag = \"kind\", rename_all = \"snake_case\")]\n");
    s.push_str("pub enum SseFrame {\n");

    for (i, ev) in events.iter().enumerate() {
        let durable = is_durable(&ev.fields);
        let prev = i.checked_sub(1).map(|j| is_durable(&events[j].fields));
        if i == 0 && durable {
            s.push_str("    // ── Durable events (have seq) ──\n");
        } else if durable && prev == Some(false) {
            s.push_str("    // ── Durable events (have seq) ──\n");
        } else if !durable && prev == Some(true) {
            s.push_str("\n    // ── Transient events (no seq) ──\n");
        }
        let variant = to_camel(ev.kind);
        s.push_str(&format!("    {variant} {{\n"));
        for (field, ty) in &ev.fields {
            s.push_str(&format!("        {field}: {},\n", sse_type(ty)));
        }
        s.push_str("    },\n");
    }
    s.push_str("}\n\n");

    // The seq() helper for durable frames (client reconnect cursor).
    s.push_str("impl SseFrame {\n");
    s.push_str("    /// Get seq if this is a durable event.\n");
    s.push_str("    pub fn seq(&self) -> Option<u64> {\n");
    s.push_str("        match self {\n");
    for ev in &events {
        if is_durable(&ev.fields) {
            let variant = to_camel(ev.kind);
            s.push_str(&format!(
                "            SseFrame::{variant} {{ seq, .. }} => Some(*seq),\n"
            ));
        }
    }
    s.push_str("            _ => None,\n");
    s.push_str("        }\n");
    s.push_str("    }\n");
    s.push_str("}\n");
    s
}

// ── Pinned shared vocabulary ───────────────────────────────────────────────────

/// Message / content / tokens / seq-range types. These are the shared "Content"
/// vocabulary referenced opaquely by the schema (`Message`, `Tokens`, `SeqRange`).
/// Their shapes are pinned here to match kn9t-core; the schema cites them by name.
const PINNED_MESSAGE_TYPES: &str = r#"/// Wire message — matches kn9t-core Message.
#[derive(Debug, Clone, Deserialize)]
pub struct WireMessage {
    pub id: String,
    pub role: String,
    pub content: Vec<WireContent>,
    /// If true, message is persisted but not displayed in TUI.
    #[serde(default)]
    pub silent: bool,
}

/// Wire content block — matches kn9t-core Content.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireContent {
    Text { text: String },
    ToolCall { id: String, name: String, args_json: String },
    ToolResult { id: String, content: Vec<WireContent>, is_error: bool },
    Thinking { text: String },
    Image { sha256: String, mime: String },
}

/// Wire tokens.
#[derive(Debug, Clone, Deserialize)]
pub struct WireTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

/// Seq range (`compacted.replaced`).
#[derive(Debug, Clone, Deserialize)]
pub struct WireSeqRange {
    pub start: u64,
    pub end: u64,
}

/// Wire model reference — Serialize (request payloads) + Deserialize (SSE).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireModelRef {
    pub provider: String,
    pub id: String,
}
"#;

/// Model-info types (GET /models), derived from the schema.
///
/// This used to be a hardcoded `PINNED_MODEL_TYPES` string that listed only
/// `provider`/`id`/`api_id`/`is_default`. The schema has always also defined
/// `ctx_window`, `max_out` and `price`, so every `xtask generate` run *deleted*
/// those fields from `wire.rs` and broke `model_selector.rs`, which reads them
/// (TRACKING B3). Reading the schema here is what stops that from recurring.
///
/// `#[serde(default)]` on every optional field so a server that omits one still
/// deserializes — the TUI must tolerate an older server.
fn model_types_struct(http: &Value) -> String {
    let item = routes(http)
        .into_iter()
        .find(|r| r.method == "GET" && r.path == "/models")
        .and_then(|r| r.response_object())
        .and_then(|resp| resp.get("properties"))
        .and_then(|p| p.get("models"))
        .and_then(|a| a.get("items"));

    let mut s = String::new();
    s.push_str("/// Model info (GET /models).\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("pub struct ModelInfo {\n");
    if let Some(item) = item {
        let req = required(item);
        for (key, prop) in properties(item) {
            let ty = model_field_type(prop, &req, &key);
            if !req.iter().any(|r| r == &key) {
                s.push_str("    #[serde(default)]\n");
            }
            s.push_str(&format!("    pub {key}: {ty},\n"));
        }
    }
    s.push_str("}\n\n");

    s.push_str("/// Models list response.\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("pub struct ModelsList {\n");
    s.push_str("    pub models: Vec<ModelInfo>,\n");
    s.push_str("}\n");
    s
}

/// Rust type for a `ModelInfo` field.
///
/// Two deliberate departures from the generic `prop_type`:
///
/// - token counts are `usize`, not `u64`: they are compared against and divided
///   by other `usize` lengths in the context gauge, and `u64` would only add
///   casts at every use site;
/// - `price` stays `serde_json::Value`: nothing in the TUI reads it, so naming a
///   struct for it would be dead code that still has to be kept in sync.
fn model_field_type(prop: &Value, req: &[String], key: &str) -> String {
    let base = match prop.get("type").and_then(|t| t.as_str()) {
        Some("integer") => "usize".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("string") => "String".to_string(),
        _ => "serde_json::Value".to_string(),
    };
    // Left bare when the schema marks it required, and also for `bool` and
    // opaque `Value`: `#[serde(default)]` already covers an omitted field
    // (`false` / `Null`), so wrapping those in `Option` would add unwrapping at
    // every use site for no extra information.
    let bare = req.iter().any(|r| r == key) || base == "bool" || base == "serde_json::Value";
    if bare {
        base
    } else {
        format!("Option<{base}>")
    }
}

// ── HTTP responses (Deserialize) ───────────────────────────────────────────────

/// Responses the TUI consumes:
///  - GET /session     → `SessionList` + `SessionInfo`
///  - GET /session/{id} → `SessionDetail` + `TranscriptMessage`
fn emit_http_responses(http: &Value) -> String {
    let mut s = String::new();
    let mut list_emitted = false;
    let mut detail_emitted = false;
    for route in routes(http) {
        if route.method != "GET" {
            continue;
        }
        match route.path {
            "/session" => {
                s.push_str(&session_list_struct(route.response_object()));
                list_emitted = true;
            }
            "/session/{id}" => detail_emitted = true,
            _ => {}
        }
    }
    if detail_emitted {
        s.push_str(SESSION_DETAIL);
        s.push('\n');
        s.push_str(TRANSCRIPT_MESSAGE);
        s.push('\n');
    }
    let _ = list_emitted;
    s
}

/// `SessionList` + `SessionInfo` derived from the GET /session response schema.
/// `created_at` is a plain `Option<String>` (ISO8601) — **no** dual-format
/// timestamp visitor (F5): the server normalizes millis → ISO8601 at the boundary.
fn session_list_struct(resp: Option<&Value>) -> String {
    let item = resp
        .and_then(|r| r.get("properties"))
        .and_then(|p| p.get("sessions"))
        .and_then(|a| a.get("items"));

    let mut s = String::new();
    s.push_str("/// Session list response.\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("pub struct SessionList {\n");
    s.push_str("    pub sessions: Vec<SessionInfo>,\n");
    s.push_str("}\n\n");

    s.push_str("/// One session row — `created_at` is a plain ISO8601 string\n");
    s.push_str("/// (`YYYY-MM-DDTHH:MM:SSZ`); the server normalizes store millis at the\n");
    s.push_str("/// boundary, so no dual-format visitor is needed (F5).\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("pub struct SessionInfo {\n");
    if let Some(item) = item {
        let req = required(item);
        for (key, prop) in properties(item) {
            let ty = prop_type(prop, &req, &key, "WireModelRef");
            s.push_str(&format!("    pub {key}: {ty},\n"));
        }
    }
    s.push_str("}\n\n");
    s
}

/// GET /session/{id} response — meta is an opaque object; transcript is the
/// message projection.
const SESSION_DETAIL: &str = r#"/// Session detail response (GET /session/{id}).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDetail {
    pub meta: serde_json::Value,
    pub model: serde_json::Value,
    pub cost_usd: f64,
    #[serde(default)]
    pub head_seq: u64,
    pub transcript: Vec<TranscriptMessage>,
}
"#;

const TRANSCRIPT_MESSAGE: &str = r#"/// One transcript row (snapshot).
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: serde_json::Value,
    /// If true, message is persisted but not displayed in TUI.
    #[serde(default)]
    pub silent: bool,
}
"#;

// ── HTTP requests (Serialize) ──────────────────────────────────────────────────

/// The request structs the TUI *sends*. Types derive from the schema's POST
/// request bodies; `model` maps to `WireModelRef` (F7).
fn emit_http_requests(http: &Value) -> String {
    let mut s = String::new();
    for route in routes(http) {
        if route.method != "POST" {
            continue;
        }
        let Some(req_obj) = route.request_object() else {
            continue;
        };
        let Some(name) = req_name_for_path(route.path) else {
            continue;
        };
        let wire_name = match name {
            "CreateSessionReq" => "CreateSessionReq",
            "PromptReq" => "PromptReq",
            "SteerReq" => "SteerReq",
            "ApproveReq" => "ApprovalResp",
            "UiRespondReq" => "UiRespondReq",
            _ => continue,
        };
        s.push_str(&emit_serialize_struct(wire_name, req_obj));
        s.push('\n');
    }
    s
}

fn emit_serialize_struct(name: &str, obj: &Value) -> String {
    let props = properties(obj);
    let req = required(obj);
    let mut s = String::new();
    s.push_str(&format!("/// `{name}` — request body (schema-derived).\n"));
    s.push_str("#[derive(Debug, Clone, Serialize)]\n");
    s.push_str(&format!("pub struct {name} {{\n"));
    for (key, prop) in props {
        let base = rust_type(prop, "WireModelRef");
        let required_field = req.iter().any(|r| r == &key);
        if required_field {
            s.push_str(&format!("    pub {key}: {base},\n"));
        } else {
            // Optional: strip exactly one schema-level Option<…> wrapper, then wrap.
            let inner = if base.starts_with("Option<") {
                base[7..base.len().saturating_sub(1)].to_string()
            } else {
                base
            };
            s.push_str("    #[serde(skip_serializing_if = \"Option::is_none\")]\n");
            s.push_str(&format!("    pub {key}: Option<{inner}>,\n"));
        }
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_schema() -> Value {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives one level below the workspace root");
        serde_json::from_str(
            &std::fs::read_to_string(root.join("schema/http.json")).expect("read http.json"),
        )
        .expect("parse http.json")
    }

    /// Every field the schema defines for a model must appear in the generated
    /// `ModelInfo`.
    ///
    /// This is the TRACKING B3 regression guard: `ModelInfo` used to be a
    /// hardcoded string listing four fields, so each `xtask generate` silently
    /// deleted `ctx_window`/`max_out`/`price` from `wire.rs` and broke
    /// `model_selector.rs`, which reads the first two.
    #[test]
    fn model_info_keeps_every_schema_field() {
        let http = http_schema();
        let out = generate(&http).expect("generator must succeed");

        let item = routes(&http)
            .into_iter()
            .find(|r| r.method == "GET" && r.path == "/models")
            .and_then(|r| r.response_object())
            .and_then(|resp| resp.get("properties"))
            .and_then(|p| p.get("models"))
            .and_then(|a| a.get("items"))
            .expect("schema must define GET /models item shape");

        let fields: Vec<String> = properties(item).into_iter().map(|(k, _)| k).collect();
        assert!(fields.len() >= 4, "sanity: expected several fields");

        for f in &fields {
            assert!(
                out.contains(&format!("pub {f}:")),
                "ModelInfo is missing schema field '{f}' - generated:\n{out}"
            );
        }
        // The two the TUI actually reads, named explicitly so a future edit that
        // drops them fails with an obvious message.
        assert!(out.contains("pub ctx_window:"));
        assert!(out.contains("pub max_out:"));
    }

    /// Token counts must stay `usize`: `model_selector.rs` and the context gauge
    /// compare them against other `usize` values.
    #[test]
    fn model_token_counts_are_optional_usize() {
        let out = generate(&http_schema()).unwrap();
        assert!(out.contains("pub ctx_window: Option<usize>"), "got:\n{out}");
        assert!(out.contains("pub max_out: Option<usize>"), "got:\n{out}");
    }

    /// Optional fields need `#[serde(default)]` or an older server that omits
    /// one would fail to deserialize entirely.
    #[test]
    fn optional_model_fields_have_serde_default() {
        let out = generate(&http_schema()).unwrap();
        let idx = out.find("pub struct ModelInfo").expect("ModelInfo present");
        let body = &out[idx..];
        let end = body.find("\n}").expect("struct is closed");
        let body = &body[..end];

        for line in body.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("pub ") else {
                continue;
            };
            // `required` fields may be bare; optional ones must be defaulted.
            if rest.contains("Option<") {
                let name = rest.split(':').next().unwrap_or("?");
                assert!(
                    body.contains(&format!("#[serde(default)]\n    pub {name}:"))
                        || body.contains(&format!("#[serde(default)]\r\n    pub {name}:")),
                    "optional field '{name}' lacks #[serde(default)]"
                );
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let http = http_schema();
        assert_eq!(generate(&http).unwrap(), generate(&http).unwrap());
    }
}
