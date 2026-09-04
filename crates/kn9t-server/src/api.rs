//! GENERATED FILE — do not edit by hand.
//! Source of truth: `schema/http.json` (ADR-0005). Regenerate with
//! `cargo run -p xtask -- generate`.
//!
//! Typed request bodies for the JSON POST routes. `#[serde(deny_unknown_fields)]`
//! makes a mistyped or unknown field a **400** at parse time instead of the
//! silent-ignore that the old hand-poked `body.get()` routes performed (F6).
//!
//! The server implementation never duplicates these shapes: it deserializes into
//! them via `http_util::parse_json` and any drift is caught by
//! `scripts/check-schema.sh`.

use serde::Deserialize;

/// Model reference `{provider, id}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionReq {
    /// Working directory for the session
    pub cwd: Option<String>,
    pub model: Option<ModelRef>,
    /// Human title, suppresses auto-title
    pub name: Option<String>,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkReq {
    pub origin_seq: Option<u64>,
    /// Allowed values: fork | rewind | subagent | tree.
    pub reason: Option<String>,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptReq {
    pub blobs: Option<Vec<String>>,
    pub images: Option<Vec<String>>,
    pub text: Option<String>,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerReq {
    pub text: String,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetModelReq {
    pub id: String,
    pub provider: String,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetToolsReq {
    /// Complete set of tool names to disable for this session (replaces the previous set)
    pub disabled: Vec<String>,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveReq {
    /// Allowed values: allow | deny | always.
    pub decision: String,
    pub id: u64,
    /// Allowed values: once | session | always.
    pub scope: Option<String>,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameReq {
    /// New human title for the session
    pub name: String,
}

/// Request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRespondReq {
    /// Pending interaction id from interaction_request event
    pub id: u64,
    /// Opaque JSON response — forwarded verbatim to the waiting plugin
    pub payload: serde_json::Value,
}

