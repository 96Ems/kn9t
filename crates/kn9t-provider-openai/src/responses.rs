//! R-OAI-060/070 — the OpenAI Responses API (`/responses`).
//!
//! Same family as chat-completions, different schema: `instructions` instead of a
//! system message, `input` items instead of `messages`, flat tools, and a different
//! SSE event stream. Selected per model by the `api` quirk, because one gateway
//! routinely fronts both and a given model answers on only one of them.
//!
//! Two invariants that are easy to get wrong:
//! - `store` defaults to **true** upstream. kn9t owns the transcript, so it always
//!   sends `store: false`; otherwise every turn is retained for 30 days and the
//!   stateless replay below is not actually stateless.
//! - reasoning items are **not** replayed. With `store: false` a reasoning item only
//!   round-trips with `include: ["reasoning.encrypted_content"]`, and we do not ask
//!   for that. Dropping them is the same choice as `thinking_replay = "strip"`.

use kn9t_provider_core::{
    CallId, Chunk, Content, Message, ModelRef, ProvErr, Quirks, Request, Role, StopReason, Tokens,
    Usage,
};
use serde_json::{json, Value};

/// Pick the request path for a quirks set (the provider appends it to `base_url`).
pub fn path(quirks: &Quirks) -> &'static str {
    if quirks.api == "responses" {
        "/responses"
    } else {
        "/chat/completions"
    }
}

// ── encode ────────────────────────────────────────────────────────────────────

pub fn build_body(req: &Request<'_>, quirks: &Quirks, dump_request: bool) -> Value {
    let mut body = json!({});
    body["model"] = Value::String(req.model.api_id.clone());

    // `instructions` replaces the chat `role: "system"` message.
    if let Some(sys) = req.system {
        body["instructions"] = json!(sys);
    }
    if let Some(max) = req.max_tokens {
        body["max_output_tokens"] = json!(max);
    }
    body["stream"] = json!(quirks.streaming);
    body["store"] = json!(false);

    let mut input: Vec<Value> = Vec::new();
    for msg in req.messages {
        encode_message(msg, &mut input);
    }
    body["input"] = json!(input);

    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.schema,
                "strict": false,
            })
        })
        .collect();
    let mut tools = tools;

    // R-NBED-050 §2: adaptive thinking demands a non-empty tools array.
    if quirks.require_tools && tools.is_empty() {
        tools.push(json!({
            "type": "function",
            "name": "_placeholder",
            "description": "Never called; satisfies gateway tool-presence requirement.",
            "parameters": { "type": "object", "properties": {} },
            "strict": false,
        }));
        body["tool_choice"] = json!("auto");
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }

    // Responses nests reasoning configuration. `budget_tokens` and `adaptive` are
    // chat-completions shapes with no equivalent here, so they are not translated.
    // `Off` omits the field, as on the chat path.
    if quirks.reasoning == "reasoning_effort" {
        if let Some(effort) = crate::encode::effort_of(req.thinking) {
            body["reasoning"] = json!({ "effort": effort });
        }
    }

    if let Value::Object(extra) = quirks.extra_body.clone() {
        if let Some(obj) = body.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
    }

    if dump_request {
        eprintln!(
            "[kn9t dump-request] {}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    }

    body
}

/// Expand one `Message` into its `input` items.
///
/// A tool-role message with N results becomes N `function_call_output` items, and an
/// assistant message with tool calls becomes one `function_call` item per call plus a
/// text item when there is prose alongside (the chat encoder makes the same split --
/// content parts are never a valid carrier for a call here either).
fn encode_message(msg: &Message, out: &mut Vec<Value>) {
    if msg.role == Role::Tool {
        for block in &msg.content {
            if let Content::ToolResult { id, content, .. } = block {
                let text = join_text(content);
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": id.0,
                    "output": if text.trim().is_empty() { "(no output)".to_string() } else { text },
                }));
            }
        }
        return;
    }

    if msg.role == Role::Assistant {
        // Emit in content order: the model's prose and its calls are separate items,
        // and reordering them misrepresents what it produced. Consecutive text/image
        // blocks stay in one message item; a call flushes them first.
        let mut parts: Vec<Value> = Vec::new();
        for block in &msg.content {
            match block {
                Content::Text { text } if !text.is_empty() => {
                    parts.push(json!({ "type": "input_text", "text": text }));
                }
                Content::Image { sha256, mime } => parts.push(input_image(sha256, mime)),
                Content::ToolCall {
                    id,
                    name,
                    args_json,
                } => {
                    flush_parts(&mut parts, msg.role, out);
                    out.push(json!({
                        "type": "function_call",
                        "call_id": id.0,
                        "name": name,
                        "arguments": args_json,
                    }));
                }
                _ => {} // Thinking is dropped -- see the module docs.
            }
        }
        flush_parts(&mut parts, msg.role, out);
        return;
    }

    // Text (and images for user turns). Thinking is dropped -- see the module docs.
    let parts: Vec<Value> = msg
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } if !text.is_empty() => {
                Some(json!({ "type": "input_text", "text": text }))
            }
            Content::Image { sha256, mime } => Some(input_image(sha256, mime)),
            _ => None,
        })
        .collect();

    if !parts.is_empty() {
        out.push(json!({ "role": role_str(msg.role), "content": parts }));
    }
}

fn flush_parts(parts: &mut Vec<Value>, role: Role, out: &mut Vec<Value>) {
    if !parts.is_empty() {
        out.push(json!({ "role": role_str(role), "content": std::mem::take(parts) }));
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "user", // unreachable: handled above
    }
}

fn input_image(sha256: &str, mime: &str) -> Value {
    let url = if sha256.starts_with("data:") {
        sha256.to_string()
    } else {
        format!("data:{mime};base64,{sha256}")
    };
    // Same funnel as the chat path: a corrupt image degrades to text rather than
    // being forwarded and failing every subsequent turn (R-OAI-IMG-GUARD).
    let url = match crate::image_guard::check(&url) {
        crate::image_guard::Verdict::Pass => url,
        crate::image_guard::Verdict::Repaired(fixed) => fixed,
        crate::image_guard::Verdict::Unusable(note) => {
            return json!({ "type": "input_text", "text": note })
        }
    };
    json!({ "type": "input_image", "image_url": url })
}

fn join_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── decode ────────────────────────────────────────────────────────────────────

/// Tool-call slots seen so far, for correlating argument deltas.
#[derive(Default)]
pub struct ResponsesState {
    has_tool_calls: bool,
}

impl ResponsesState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Decode one SSE `data:` payload.
///
/// Unknown event types (including the gateway's trailing `ping`) yield nothing: the
/// stream carries dozens of event kinds we do not act on, and erroring on an
/// unrecognised one turns a cosmetic upstream addition into an outage.
pub fn decode_event(
    bytes: &[u8],
    state: &mut ResponsesState,
    _quirks: &Quirks,
    model_ref: &ModelRef,
) -> Result<Vec<Chunk>, ProvErr> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| ProvErr::Decode(format!("json: {e}")))?;
    let mut chunks = Vec::new();

    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "response.output_text.delta" => {
            if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                if !d.is_empty() {
                    chunks.push(Chunk::Text {
                        idx: 0,
                        delta: d.to_owned(),
                    });
                }
            }
        }
        // Models that expose a reasoning summary stream it here; `reasoning_text`
        // is the verbatim variant.
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                if !d.is_empty() {
                    chunks.push(Chunk::Thinking {
                        idx: 0,
                        delta: d.to_owned(),
                    });
                }
            }
        }
        "response.output_item.added" => {
            if let Some(item) = v.get("item") {
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    state.has_tool_calls = true;
                    let call_id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                    chunks.push(Chunk::ToolCall {
                        idx: output_index(&v),
                        id: CallId(call_id.to_owned()),
                        name: name.to_owned(),
                    });
                }
            }
        }
        // Argument fragments carry no call id -- they are correlated by output index,
        // which the `ToolCall` above already published.
        "response.function_call_arguments.delta" => {
            if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                if !d.is_empty() {
                    chunks.push(Chunk::ToolArgs {
                        idx: output_index(&v),
                        delta: d.to_owned(),
                    });
                }
            }
        }
        "response.completed" => {
            let resp = v.get("response");
            if let Some(usage) = resp.and_then(|r| r.get("usage")) {
                chunks.push(Chunk::Usage(Usage {
                    tokens: decode_usage(usage),
                    model: model_ref.clone(),
                }));
            }
            chunks.push(Chunk::Stop(if state.has_tool_calls {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }));
        }
        "response.incomplete" => {
            if let Some(usage) = v
                .get("response")
                .and_then(|r| r.get("usage"))
            {
                chunks.push(Chunk::Usage(Usage {
                    tokens: decode_usage(usage),
                    model: model_ref.clone(),
                }));
            }
            let reason = v
                .pointer("/response/incomplete_details/reason")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            chunks.push(Chunk::Stop(match reason {
                "max_output_tokens" => StopReason::Length,
                "content_filter" => StopReason::Refusal,
                _ => StopReason::Stop,
            }));
        }
        // Terminal failures. An overflow can arrive in-band on a 200 stream, so it is
        // classified exactly like the pre-stream path (R-RCT-080).
        "response.failed" | "response.error" => {
            let text = v
                .pointer("/response/error")
                .or_else(|| v.get("error"))
                .map(|e| e.to_string())
                .unwrap_or_else(|| v.to_string());
            if kn9t_provider_core::is_context_overflow(400, &text) {
                return Err(ProvErr::ContextOverflow);
            }
            return Err(ProvErr::Stream(text));
        }
        _ => {}
    }

    Ok(chunks)
}

fn output_index(v: &Value) -> u32 {
    v.get("output_index")
        .and_then(|i| i.as_u64())
        .unwrap_or(0) as u32
}

pub fn decode_usage(u: &Value) -> Tokens {
    let input_total = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let output = u
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_read = u
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_write = u
        .pointer("/input_tokens_details/cache_write_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let reasoning = u
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // §8.4.3 partition: `input` is the uncached remainder, so the cost formula does
    // not bill cached tokens twice. Responses reports `input_tokens` as the total.
    Tokens {
        input: input_total.saturating_sub(cache_read + cache_write),
        output,
        cache_read,
        cache_write,
        reasoning,
    }
}

/// R-NBED-050 §3: synthesise the chunk sequence from a complete (non-streaming)
/// response, so the `Iterator` contract downstream is unchanged.
pub fn synthesize_chunks(
    v: &Value,
    _quirks: &Quirks,
    model_ref: &ModelRef,
) -> Result<Vec<Chunk>, ProvErr> {
    let mut state = ResponsesState::new();
    let mut chunks = Vec::new();

    let output = v
        .get("output")
        .and_then(|o| o.as_array())
        .ok_or_else(|| ProvErr::Decode("no output".into()))?;

    for (idx, item) in output.iter().enumerate() {
        match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "message" => {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for part in parts {
                        if part.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                if !t.is_empty() {
                                    chunks.push(Chunk::Text {
                                        idx: 0,
                                        delta: t.to_owned(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            "function_call" => {
                state.has_tool_calls = true;
                let call_id = item
                    .get("call_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_owned();
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_owned();
                chunks.push(Chunk::ToolCall {
                    idx: idx as u32,
                    id: CallId(call_id),
                    name,
                });
                if let Some(args) = item.get("arguments").and_then(|a| a.as_str()) {
                    if !args.is_empty() {
                        chunks.push(Chunk::ToolArgs {
                            idx: idx as u32,
                            delta: args.to_owned(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(usage) = v.get("usage") {
        chunks.push(Chunk::Usage(Usage {
            tokens: decode_usage(usage),
            model: model_ref.clone(),
        }));
    }
    let reason = v
        .pointer("/incomplete_details/reason")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    chunks.push(Chunk::Stop(match reason {
        "max_output_tokens" => StopReason::Length,
        "content_filter" => StopReason::Refusal,
        _ if state.has_tool_calls => StopReason::ToolUse,
        _ => StopReason::Stop,
    }));

    Ok(chunks)
}
