//! Token usage tracking and turn stop reasons.

use crate::model::ModelRef;
use serde::{Deserialize, Serialize};

/// Token breakdown: input (after last cache breakpoint), output, cache_read, cache_write, reasoning.
/// Input is a partition, not overlap: total context = input + cache_read + cache_write.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub reasoning: u32,
}

/// Usage event: token counts and model reference.
#[derive(Clone, Serialize, Deserialize)]
pub struct Usage {
    pub tokens: Tokens,
    pub model: ModelRef,
}

/// Reason a turn ended: normal stop, tool use requested, context length, aborted, or refused.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    ToolUse,
    Length,
    Aborted,
    Refusal,
}
