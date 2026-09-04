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
    s.push_str(PINNED_MODEL_TYPES);
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
    s.push_str("/// SSE frame from the server — `#[serde(tag = \"kind\", rename_all = \"snake_case\")]`\n");
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
            s.push_str(&format!("            SseFrame::{variant} {{ seq, .. }} => Some(*seq),\n"));
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

/// Model-info types (GET /models) — pinned to match the server's registry output.
const PINNED_MODEL_TYPES: &str = r#"/// Model info (GET /models).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub api_id: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

/// Models list response.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsList {
    pub models: Vec<ModelInfo>,
}
"#;

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