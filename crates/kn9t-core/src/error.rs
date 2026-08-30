//! R-CORE-130, R-CORE-135 — error types.

use serde::{Deserialize, Serialize};
use std::fmt;

/// R-CORE-130 — load-bearing: retry (PCORE §8.1), compaction trigger (§7.5), and
/// truncation policy (RCT §8.6.6) all branch on these variants.
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

/// R-CORE-135
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreErr(pub String);

impl fmt::Display for StoreErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "store error: {}", self.0)
    }
}
impl std::error::Error for StoreErr {}

/// R-CORE-135
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolErr(pub String);

impl fmt::Display for ToolErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tool error: {}", self.0)
    }
}
impl std::error::Error for ToolErr {}
