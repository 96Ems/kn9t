//! Wire types — serde mirrors of server API.
//!
//! R-TUI-010: No kn9t-* deps, so we duplicate the types here.
//! R-TUI-012: MUST match API.md wire format exactly.

use serde::{Deserialize, Serialize};

/// SSE frame from the server — matches kn9t-core Event enum.
/// Uses `#[serde(tag = "kind")]` per API.md §4.3.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SseFrame {
    // ── Durable events (have seq) ──
    MessageAppended {
        seq: u64,
        msg: WireMessage,
    },
    UsageRecorded {
        seq: u64,
        provider: String,
        model: String,
        usage_kind: String,
        tokens: WireTokens,
        cost_usd: f64,
        estimated: bool,
    },
    ModelChanged {
        seq: u64,
        model: WireModelRef,
    },
    Compacted {
        seq: u64,
    },

    // ── Transient events (no seq) ──
    TurnStarted {
        turn: u32,
    },
    TextDelta {
        msg_id: String,
        idx: u32,
        delta: String,
    },
    ThinkingDelta {
        msg_id: String,
        idx: u32,
        delta: String,
    },
    ToolArgsDelta {
        msg_id: String,
        idx: u32,
        delta: String,
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
        id: u64,
        tool: String,
        args: serde_json::Value,
        cwd: String,
    },
    TurnEnded {
        turn: u32,
        stop: String,
    },
    HookFailed {
        plugin: String,
        hook: String,
        reason: String,
    },
    TitleChanged {
        title: String,
    },
    Error {
        message: String,
    },
    /// Generic plugin notification — plugins can emit custom events.
    /// Payload has `plugin` (name) and `message` (display text) fields.
    PluginNotification {
        plugin: String,
        message: String,
    },
}

impl SseFrame {
    /// Get seq if this is a durable event.
    pub fn seq(&self) -> Option<u64> {
        match self {
            SseFrame::MessageAppended { seq, .. }
            | SseFrame::UsageRecorded { seq, .. }
            | SseFrame::ModelChanged { seq, .. }
            | SseFrame::Compacted { seq, .. } => Some(*seq),
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

/// Wire model reference.
#[derive(Debug, Clone, Deserialize)]
pub struct WireModelRef {
    pub provider: String,
    pub id: String,
}

/// Session list response.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,
    pub head_seq: u64,
    pub cwd: Option<String>,
    /// Timestamp for date grouping (can be integer or string from SQLite).
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    pub created_at: Option<String>,
}

/// Deserialize timestamp that may be integer (Unix epoch) or string (ISO 8601).
fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    
    struct TimestampVisitor;
    
    impl<'de> Visitor<'de> for TimestampVisitor {
        type Value = Option<String>;
        
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a timestamp as integer or string")
        }
        
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            // Convert Unix timestamp to ISO 8601 date string.
            // Simple conversion: just extract year-month-day.
            let secs = v;
            let days = secs / 86400;
            // Approximate date calculation from days since epoch.
            let years = days / 365;
            let year = 1970 + years;
            let remaining_days = days % 365;
            let month = (remaining_days / 30).min(11) + 1;
            let day = (remaining_days % 30) + 1;
            Ok(Some(format!("{:04}-{:02}-{:02}T00:00:00", year, month, day)))
        }
        
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_i64(v as i64)
        }
        
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(v.to_string()))
        }
        
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(v))
        }
    }
    
    deserializer.deserialize_any(TimestampVisitor)
}

/// Session detail response.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDetail {
    pub meta: serde_json::Value,
    pub model: serde_json::Value,
    pub cost_usd: f64,
    #[serde(default)]
    pub head_seq: u64,
    pub transcript: Vec<TranscriptMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: serde_json::Value,
    /// If true, message is persisted but not displayed in TUI.
    #[serde(default)]
    pub silent: bool,
}

/// Create session request.
#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionReq {
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Prompt request.
#[derive(Debug, Clone, Serialize)]
pub struct PromptReq {
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// Approval response — DESIGN §10 scope=once|session|always.
/// For backward compat the server still accepts `decision="always"` as
/// `allow+always`; new clients should send `decision="allow", scope="always"`.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResp {
    pub id: u64,
    pub decision: String, // "allow" | "deny" (legacy "always" still accepted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>, // "once" | "session" | "always"
}

/// Model info.
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
