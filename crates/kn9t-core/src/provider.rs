//! Provider interface: streaming chunks from language model APIs.

use crate::cache::Cache;
use crate::cancel::Cancel;
use crate::error::ProvErr;
use crate::ids::CallId;
use crate::message::Message;
use crate::model::{ModelSpec, Thinking};
use crate::toolspec::ToolSpec;
use crate::usage::{StopReason, Usage};
use serde::{Deserialize, Serialize};

/// Provider request: model, system prompt, messages, tools, thinking, and cache breakpoints.
pub struct Request<'a> {
    pub model: &'a ModelSpec,
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    pub thinking: Thinking,
    pub max_tokens: Option<u32>,
    /// Cache breakpoints: priority order (not positional).
    pub cache: &'a [Cache],
}

/// Chunk from provider: text, thinking, tool call, tool args, usage, or stop reason.
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
    /// Tool arguments: raw JSON fragments for streaming assembly.
    ToolArgs {
        idx: u32,
        delta: String,
    },
    Usage(Usage),
    Stop(StopReason),
}

/// Language model provider: streams chunks from the model. Iterator's next() blocks on I/O.
/// Connection retries happen before first chunk; mid-stream errors are fatal to the turn.
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn stream(
        &self,
        req: &Request,
        cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>;
    /// Like stream() but emits retry events to sink for TUI progress display.
    fn stream_with_sink(
        &self,
        req: &Request,
        cancel: &Cancel,
        _sink: Option<&dyn crate::bus::EventSink>,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        self.stream(req, cancel)
    }
}
