//! Wire types — serde mirrors of the server API.
//!
//! GENERATED FILE — do not edit by hand. Regenerate with `cargo run -p xtask -- generate`.
//! Source of truth: `schema/http.json` + `schema/plugin.json` (ADR-0005).
//!
//! R-TUI-010 / GI-6: no `kn9t-*` dependency — standalone serde-only file,
//! verifiable by `crates/kn9t-tui/tests/acceptance.rs::tui_no_kn9t_deps`.
//! R-TUI-012: matches the schema wire format exactly; the server is authoritative.

use serde::{Deserialize, Serialize};

/// SSE frame from the server — `#[serde(tag = "kind", rename_all = "snake_case")]`
/// per AGENTS.md §12. Durable events carry `seq`; transient events do not.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SseFrame {
    // ── Durable events (have seq) ──
    MessageAppended {
        msg: WireMessage,
        seq: u64,
    },
    UsageRecorded {
        cost_usd: f64,
        estimated: bool,
        model: String,
        provider: String,
        seq: u64,
        tokens: WireTokens,
        usage_kind: String,
    },
    ModelChanged {
        model: WireModelRef,
        seq: u64,
    },
    ToolsToggled {
        disabled: Vec<String>,
        seq: u64,
    },
    Compacted {
        replaced: WireSeqRange,
        seq: u64,
        summary: WireMessage,
    },

    // ── Transient events (no seq) ──
    TurnStarted {
        turn: u32,
    },
    TextDelta {
        delta: String,
        idx: u32,
        msg_id: String,
    },
    ThinkingDelta {
        delta: String,
        idx: u32,
        msg_id: String,
    },
    ToolArgsDelta {
        delta: String,
        idx: u32,
        msg_id: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
    },
    ToolProgress {
        call_id: String,
        note: String,
    },
    ToolFinished {
        call_id: String,
        is_error: bool,
    },
    ApprovalRequest {
        args: serde_json::Value,
        cwd: String,
        id: u64,
        tool: String,
    },
    TurnEnded {
        stop: String,
        turn: u32,
    },
    HookFailed {
        hook: String,
        plugin: String,
        reason: String,
    },
    TitleChanged {
        title: String,
    },
    Error {
        message: String,
    },
    RetryAttempt {
        attempt: u32,
        delay_ms: u64,
        error: String,
        max: u32,
        retry_kind: String,
    },
    TurnStatus {
        message: String,
        phase: String,
    },
    PluginNotification {
        message: String,
        plugin: String,
    },
    PluginDeclared {
        plugin: String,
        tools_added: Vec<String>,
        tools_removed: Vec<String>,
    },
    InteractionRequest {
        id: u64,
        payload: serde_json::Value,
        plugin: String,
    },
    UiDirective {
        op: String,
        payload: serde_json::Value,
        plugin: String,
        target: String,
    },
}

impl SseFrame {
    /// Get seq if this is a durable event.
    pub fn seq(&self) -> Option<u64> {
        match self {
            SseFrame::MessageAppended { seq, .. } => Some(*seq),
            SseFrame::UsageRecorded { seq, .. } => Some(*seq),
            SseFrame::ModelChanged { seq, .. } => Some(*seq),
            SseFrame::ToolsToggled { seq, .. } => Some(*seq),
            SseFrame::Compacted { seq, .. } => Some(*seq),
            _ => None,
        }
    }
}

/// Wire message — matches kn9t-core Message.
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

/// Session list response.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
}

/// One session row — `created_at` is a plain ISO8601 string
/// (`YYYY-MM-DDTHH:MM:SSZ`); the server normalizes store millis at the
/// boundary, so no dual-format visitor is needed (F5).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub created_at: Option<String>,
    pub cwd: Option<String>,
    pub head_seq: u64,
    pub id: String,
    pub name: Option<String>,
}

/// Session detail response (GET /session/{id}).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDetail {
    pub meta: serde_json::Value,
    pub model: serde_json::Value,
    pub cost_usd: f64,
    #[serde(default)]
    pub head_seq: u64,
    pub transcript: Vec<TranscriptMessage>,
}

/// One transcript row (snapshot).
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: serde_json::Value,
    /// If true, message is persisted but not displayed in TUI.
    #[serde(default)]
    pub silent: bool,
}


/// `CreateSessionReq` — request body (schema-derived).
#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<WireModelRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `PromptReq` — request body (schema-derived).
#[derive(Debug, Clone, Serialize)]
pub struct PromptReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `SteerReq` — request body (schema-derived).
#[derive(Debug, Clone, Serialize)]
pub struct SteerReq {
    pub text: String,
}

/// `ApprovalResp` — request body (schema-derived).
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResp {
    pub decision: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// `UiRespondReq` — request body (schema-derived).
#[derive(Debug, Clone, Serialize)]
pub struct UiRespondReq {
    pub id: u64,
    pub payload: serde_json::Value,
}


/// Model info (GET /models).
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
