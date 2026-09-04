//! R-CORE-170 .. R-CORE-190 — the provider interface.

use crate::cache::Cache;
use crate::cancel::Cancel;
use crate::error::ProvErr;
use crate::ids::CallId;
use crate::message::Message;
use crate::model::{ModelSpec, Thinking};
use crate::toolspec::ToolSpec;
use crate::usage::{StopReason, Usage};
use serde::{Deserialize, Serialize};

/// R-CORE-170 — defined **once**, carrying the cache breakpoints. A borrowing view;
/// **not** `Serialize` (it is never persisted, only its constituent parts are).
/// This is the one non-payload struct in the crate.
pub struct Request<'a> {
    pub model: &'a ModelSpec,
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    pub thinking: Thinking,
    pub max_tokens: Option<u32>,
    /// Priority order, deduplicated, capped (R-CORE-200). NOT positional.
    pub cache: &'a [Cache],
}

/// R-CORE-180 — `Serialize/Deserialize` so the replay provider (02) can store
/// decoded chunks for its own unit tests, even though fixtures are raw bytes.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "chunk", rename_all = "snake_case")]
pub enum Chunk {
    Text {
        idx: u32,
        delta: String,
    },
    Thinking {
        idx: u32,
        delta: String,
    },
    ToolCall {
        idx: u32,
        id: CallId,
        name: String,
    },
    /// Raw JSON fragments.
    ToolArgs {
        idx: u32,
        delta: String,
    },
    Usage(Usage),
    Stop(StopReason),
}

/// R-CORE-190 — the returned iterator's `next()` blocks on socket I/O (this is why
/// threads, not async, §1). Connection/HTTP-status retry happens **before the first
/// chunk is yielded** (PCORE §8.1); a mid-stream error is yielded as
/// `Err(ProvErr::Stream)` and is fatal to the turn.
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn stream(
        &self,
        req: &Request,
        cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>;
    /// Same as `stream` but may emit `Event::RetryAttempt` / `Event::TurnStatus` via `sink`
    /// before each retry sleep so the TUI can show progress instead of a silent spinner.
    /// Default impl delegates to `stream` without emitting (backward compat for tests/fakes).
    fn stream_with_sink(
        &self,
        req: &Request,
        cancel: &Cancel,
        _sink: Option<&dyn crate::bus::EventSink>,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        self.stream(req, cancel)
    }
}
