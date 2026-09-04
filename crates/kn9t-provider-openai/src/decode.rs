//! R-OAI-020/030 — decode OpenAI SSE stream into `Chunk`s.

use kn9t_provider_core::{CallId, Chunk, ModelRef, ProvErr, Quirks, StopReason, Tokens, Usage};
use serde_json::Value;

/// State accumulated while streaming, for tool-call correlation.
///
/// Crate-visible so it matches `decode_delta`'s visibility; the public surface
/// is the `DecodeState` newtype, which keeps the fields private.
#[derive(Default)]
pub(crate) struct StreamState {
    tools: Vec<ToolState>,
    has_tool_calls: bool,
}

#[derive(Default)]
struct ToolState {
    idx: u32,
    id: String,
    name: String,
}

/// Decode a single SSE `data:` JSON payload into zero or more `Chunk`s.
///
/// Crate-internal: `StreamState` is private, so this was never callable from
/// outside even while marked `pub`. `DecodeState::decode` is the public entry.
pub(crate) fn decode_delta(
    json_bytes: &[u8],
    state: &mut StreamState,
    quirks: &Quirks,
    model_ref: &ModelRef,
) -> Result<Vec<Chunk>, ProvErr> {
    let v: Value =
        serde_json::from_slice(json_bytes).map_err(|e| ProvErr::Decode(format!("json: {e}")))?;

    // Error from API.
    if let Some(err) = v.get("error") {
        return Err(ProvErr::Stream(err.to_string()));
    }

    let mut chunks = Vec::new();

    // Usage chunk (may appear at end).
    if let Some(usage) = v.get("usage") {
        let tokens = decode_usage(usage);
        chunks.push(Chunk::Usage(Usage {
            tokens,
            model: model_ref.clone(),
        }));
    }

    let choices = match v.get("choices").and_then(|c| c.as_array()) {
        Some(c) if !c.is_empty() => c,
        _ => return Ok(chunks),
    };

    let choice = &choices[0];
    let delta = match choice.get("delta") {
        Some(d) => d,
        None => return Ok(chunks),
    };

    // Text delta.
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            chunks.push(Chunk::Text {
                idx: 0,
                delta: text.to_owned(),
            });
        }
    }

    // Reasoning / thinking content.
    if quirks.thinking_style == "reasoning_content" {
        if let Some(text) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                chunks.push(Chunk::Thinking {
                    idx: 0,
                    delta: text.to_owned(),
                });
            }
        }
    }

    // Tool calls.
    if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        state.has_tool_calls = true;
        for tc in tcs {
            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

            // Ensure slot exists.
            while state.tools.len() <= idx as usize {
                state.tools.push(ToolState::default());
            }
            let slot = &mut state.tools[idx as usize];
            slot.idx = idx;

            // R-OAI-030: correlate by id when index absent.
            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                if slot.id.is_empty() {
                    slot.id = id.to_owned();
                    let name = tc
                        .pointer("/function/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_owned();
                    slot.name = name.clone();
                    chunks.push(Chunk::ToolCall {
                        idx,
                        id: CallId(id.to_owned()),
                        name,
                    });
                }
            }

            if let Some(args) = tc.pointer("/function/arguments").and_then(|a| a.as_str()) {
                if !args.is_empty() {
                    chunks.push(Chunk::ToolArgs {
                        idx,
                        delta: args.to_owned(),
                    });
                }
            }
        }
    }

    // Stop reason.
    if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
        if !reason.is_empty() && reason != "null" {
            let stop = decode_stop(reason, state.has_tool_calls, quirks);
            chunks.push(Chunk::Stop(stop));
        }
    }

    Ok(chunks)
}

pub(crate) fn decode_stop(reason: &str, has_tools: bool, quirks: &Quirks) -> StopReason {
    match reason {
        "stop" => {
            if !quirks.finish_reason && has_tools {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        }
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::Length,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::Stop,
    }
}

pub fn decode_usage(u: &Value) -> Tokens {
    let prompt_tokens = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let output = u
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // R-NBED-060: cache counters at root or under prompt_tokens_details.
    let cache_write = u
        .get("cache_creation_input_tokens")
        .or_else(|| u.pointer("/prompt_tokens_details/cache_creation_input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_read = u
        .get("cached_tokens")
        .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let reasoning = u
        .get("reasoning_tokens")
        .or_else(|| u.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // §8.4.3 partition: `input` is uncached-only. OpenAI/Bedrock report `prompt_tokens`
    // as total (includes cached), so subtract cache tokens to get uncached portion.
    // This ensures cost formula doesn't double-bill cached tokens.
    let input = prompt_tokens.saturating_sub(cache_read + cache_write);

    Tokens {
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
    }
}

/// Public stream state wrapper.
pub struct DecodeState(StreamState);

impl DecodeState {
    pub fn new() -> Self {
        DecodeState(StreamState::default())
    }
    pub fn decode(
        &mut self,
        bytes: &[u8],
        quirks: &Quirks,
        model_ref: &ModelRef,
    ) -> Result<Vec<Chunk>, ProvErr> {
        decode_delta(bytes, &mut self.0, quirks, model_ref)
    }
}
