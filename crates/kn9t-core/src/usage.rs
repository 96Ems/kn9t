//! R-CORE-100, R-CORE-110 — usage and stop reasons.

use crate::model::ModelRef;
use serde::{Deserialize, Serialize};

/// R-CORE-100 — a **partition, not an overlap**: `input` counts only tokens after
/// the last cache breakpoint. Total context is `input + cache_read + cache_write`
/// (§8.4.3). Providers that do not report a counter leave it `0`, which costs
/// correctly by construction.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub reasoning: u32,
}

/// R-CORE-100
#[derive(Clone, Serialize, Deserialize)]
pub struct Usage {
    pub tokens: Tokens,
    pub model: ModelRef,
}

/// R-CORE-110
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolUse,
    Length,
    Aborted,
    Refusal,
}
