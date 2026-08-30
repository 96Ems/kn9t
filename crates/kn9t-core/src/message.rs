//! R-CORE-050 .. R-CORE-064 — messages and content blocks.

use crate::ids::{CallId, MsgId};
use serde::{Deserialize, Serialize};

/// R-CORE-050
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// R-CORE-050
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MsgId,
    pub role: Role,
    pub content: Vec<Content>,
    /// If true, this message is persisted and sent to LLM but not displayed in TUI.
    /// Used by plugins that inject context (e.g., AGENTS.md) and handle their own
    /// user-facing notification via events.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
}

/// R-CORE-060 — one flat enum covering every provider's block types.
///
/// R-CORE-062: `ToolCall::args_json` holds the provider's exact bytes, stored
/// verbatim; no code path parses and re-serializes it back into `args_json`.
///
/// R-CORE-064: `Thinking::signature` is opaque and provider-owned; the stored
/// form is always verbatim (including `None`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    /// Never inline bytes: a `sha256:<hex>` ref into `blobs` (STOR / §12.4).
    Image {
        sha256: String,
        mime: String,
    },
    /// `args_json` holds the provider's exact bytes; see R-CORE-062.
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
    /// `signature` is opaque and provider-owned; see R-CORE-064.
    Thinking {
        text: String,
        signature: Option<String>,
    },
}
