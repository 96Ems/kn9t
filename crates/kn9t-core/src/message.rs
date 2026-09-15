//! Messages and content blocks: text, images, tool calls, results, and thinking.

use crate::ids::{CallId, MsgId};
use serde::{Deserialize, Serialize};

/// Message role: System, User, Assistant, or Tool.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Message: id, role, content blocks, and silent flag (if not displayed in TUI).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: MsgId,
    pub role: Role,
    pub content: Vec<Content>,
    /// If true, message is persisted and sent to LLM but hidden from TUI.
    /// Used by plugins that inject context and handle their own notifications.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
}

/// Content block: flat enum covering text, image, tool call, tool result, and thinking.
/// ToolCall args_json is stored verbatim from provider; Thinking signature is opaque.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    /// Image: stored as sha256 hash reference, not inline bytes.
    Image {
        sha256: String,
        mime: String,
    },
    /// Tool call: id, name, and args_json (stored verbatim from provider).
    ToolCall {
        id: CallId,
        name: String,
        args_json: String,
    },
    ToolResult {
        id: CallId,
        content: Vec<Content>,
        is_error: bool,
    },
    /// Thinking: text and optional provider-owned signature.
    Thinking {
        text: String,
        signature: Option<String>,
    },
}
