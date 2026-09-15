//! Error types for provider, store, and tool execution.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Provider error: determines retry behavior, compaction triggers, and truncation policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProvErr {
    /// Pre-stream; retried inside `stream()`.
    Connect(String),
    /// Pre-stream; retried on 429/5xx.
    Http { status: u16, body: String },
    /// Mid-stream error frame; fatal to the turn.
    Stream(String),
    /// Prompt too long → triggers compaction.
    ContextOverflow,
    /// Stream ended with unfinished tool calls.
    Truncated,
    /// Unparseable wire bytes.
    Decode(String),
}

impl fmt::Display for ProvErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProvErr::Connect(s) => write!(f, "connect error: {s}"),
            ProvErr::Http { status, body } => write!(f, "http {status}: {body}"),
            ProvErr::Stream(s) => write!(f, "stream error: {s}"),
            ProvErr::ContextOverflow => write!(f, "context overflow"),
            ProvErr::Truncated => write!(f, "stream truncated with unfinished tool calls"),
            ProvErr::Decode(s) => write!(f, "decode error: {s}"),
        }
    }
}

impl std::error::Error for ProvErr {}

/// Store error: session, event, or persistence failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreErr(pub String);

impl fmt::Display for StoreErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store error: {}", self.0)
    }
}
impl std::error::Error for StoreErr {}

/// Tool execution error: returned by tools on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolErr(pub String);

impl fmt::Display for ToolErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tool error: {}", self.0)
    }
}
impl std::error::Error for ToolErr {}
