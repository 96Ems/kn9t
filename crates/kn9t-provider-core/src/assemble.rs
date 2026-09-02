//! R-PCORE-050 — assemble chunks into final (Message, Usage, StopReason).
//! DB-02: this is the canonical implementation; kn9t-react delegates here.

use kn9t_core::{
    CallId, Chunk, Content, EventSink, LiveEvent, Message, ModelRef, MsgId, ProvErr, Role,
    StopReason, Tokens, Usage,
};

struct ToolAccum {
    idx:       u32,
    id:        CallId,
    name:      String,
    args_json: String,
}

/// Result of assembling a chunk stream. `usage_reported` distinguishes a
/// provider-sent Usage chunk from the zeroed default (used by the ReAct abort
/// path to decide whether to estimate, R-RCT-050).
pub struct AssembleResult {
    pub message:        Message,
    pub usage:          Usage,
    pub stop:           StopReason,
    pub usage_reported: bool,
}

/// R-PCORE-050
pub fn assemble(
    chunks: impl Iterator<Item = Result<Chunk, ProvErr>>,
    sink: &dyn EventSink,
) -> Result<AssembleResult, ProvErr> {
    let msg_id = MsgId::new();
    let mut text_parts:  Vec<(u32, String)> = Vec::new();
    let mut think_parts: Vec<(u32, String)> = Vec::new();
    let mut tools: Vec<ToolAccum> = Vec::new();
    let mut usage_opt: Option<Usage> = None;
    let mut stop = StopReason::Stop;

    for chunk_res in chunks {
        let chunk = chunk_res?;
        match chunk {
            Chunk::Text { idx, delta } => {
                sink.emit(LiveEvent::TextDelta { msg_id: msg_id.clone(), idx, delta: delta.clone() });
                match text_parts.iter_mut().find(|(i, _)| *i == idx) {
                    Some(e) => e.1.push_str(&delta),
                    None    => text_parts.push((idx, delta)),
                }
            }
            Chunk::Thinking { idx, delta } => {
                sink.emit(LiveEvent::ThinkingDelta { msg_id: msg_id.clone(), idx, delta: delta.clone() });
                match think_parts.iter_mut().find(|(i, _)| *i == idx) {
                    Some(e) => e.1.push_str(&delta),
                    None    => think_parts.push((idx, delta)),
                }
            }
            Chunk::ToolCall { idx, id, name } => {
                // ToolStarted is emitted by the exec layer after the message is appended,
                // not here — assemble only emits streaming deltas (TextDelta, ThinkingDelta,
                // ToolArgsDelta). The exec layer knows call order and emits ToolStarted
                // per R-RCT-060 sequence.
                tools.push(ToolAccum { idx, id, name, args_json: String::new() });
            }
            Chunk::ToolArgs { idx, delta } => {
                sink.emit(LiveEvent::ToolArgsDelta { msg_id: msg_id.clone(), idx, delta: delta.clone() });
                if let Some(t) = tools.iter_mut().find(|t| t.idx == idx) {
                    t.args_json.push_str(&delta);
                }
            }
            Chunk::Usage(u) => {
                usage_opt = Some(u);
            }
            Chunk::Stop(s) => {
                stop = s;
            }
        }
    }

    // Build content list.
    let mut content: Vec<Content> = Vec::new();
    text_parts.sort_by_key(|(i, _)| *i);
    for (_, text) in text_parts {
        if !text.is_empty() {
            content.push(Content::Text { text });
        }
    }
    think_parts.sort_by_key(|(i, _)| *i);
    for (_, text) in think_parts {
        if !text.is_empty() {
            content.push(Content::Thinking { text, signature: None });
        }
    }
    tools.sort_by_key(|t| t.idx);
    for t in tools {
        // If no arguments were streamed, use empty JSON object.
        // Some providers (custom provider) reject empty string as invalid JSON.
        let args = if t.args_json.is_empty() { "{}".to_string() } else { t.args_json };
        // R-PCORE-050 — parse the accumulated args once, here, as the gate that decides
        // whether this message may exist at all. A stream cut mid-`ToolArgs` yields a
        // syntactically incomplete concat; persisting it bricks the session for good,
        // because `events` is append-only (GI-4) and every later `plan_request` replays
        // the same unparseable bytes to the provider. `ProvErr::Truncated` is exactly
        // this condition, and the loop already knows how to retry it (R-RCT-070).
        // The parse result is discarded: `args_json` stays the verbatim concat (R-CORE-062).
        if serde_json::from_str::<serde_json::Value>(&args).is_err() {
            return Err(ProvErr::Truncated);
        }
        content.push(Content::ToolCall {
            id:        t.id,
            name:      t.name,
            args_json: args, // raw concat, never re-serialized (R-CORE-062)
        });
    }

    let msg = Message {
        id:      msg_id,
        role:    Role::Assistant,
        content,
        silent:  false,
    };

    let usage_reported = usage_opt.is_some();
    let usage = usage_opt.unwrap_or(Usage {
        tokens: Tokens::default(),
        model:  ModelRef { provider: String::new(), id: String::new() },
    });

    Ok(AssembleResult { message: msg, usage, stop, usage_reported })
}
