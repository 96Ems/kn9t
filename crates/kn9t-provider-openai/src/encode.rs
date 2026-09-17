//! R-OAI-010 — build the OpenAI chat-completions request body from a `Request`.

use kn9t_provider_core::{
    Cache, CacheMode, Content, Effort, Message, Quirks, Request, Role, Thinking,
};
use serde_json::{json, Value};
use std::collections::HashSet;

/// Build the complete request body JSON.
pub fn build_request(
    req: &Request<'_>,
    quirks: &Quirks,
    cache_mode: &CacheMode,
    dump_request: bool,
) -> Value {
    let mut body = json!({});

    // Model id.
    body["model"] = Value::String(req.model.api_id.clone());

    // max_tokens / max_completion_tokens.
    if let Some(max) = req.max_tokens {
        body[&quirks.max_tokens_field] = json!(max);
    }

    // Stream.
    body["stream"] = json!(quirks.streaming);
    if quirks.streaming && quirks.usage_in_stream {
        body["stream_options"] = json!({ "include_usage": true });
    }

    // Build set of message indices that need cache_control.
    let cache_indices: HashSet<usize> = req
        .cache
        .iter()
        .filter_map(|c| {
            match c {
                Cache::AfterMessage { idx } => Some(*idx),
                Cache::System => None, // handled separately
            }
        })
        .collect();
    let cache_system = req.cache.iter().any(|c| matches!(c, Cache::System));

    // System message. Cache control is applied to the LAST TOOL (not here) to cache
    // the entire system + tools prefix together. See tools section below.
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = req.system {
        messages.push(json!({ "role": &quirks.system_role, "content": sys }));
    }

    // A Tool-role message with N results expands to N wire messages.
    for (idx, msg) in req.messages.iter().enumerate() {
        let needs_cache = !matches!(cache_mode, CacheMode::None) && cache_indices.contains(&idx);
        encode_messages(msg, quirks, needs_cache, &mut messages);
    }
    body["messages"] = json!(messages);

    // Cache the system + tools prefix by tagging the last tool (opencode "caterpillar").
    let tools_count = req.tools.len();
    let mut tools_json: Vec<Value> = req
        .tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut tool = json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema,
                }
            });
            // Apply cache_control to the LAST tool when Cache::System is requested.
            // This ensures the entire system prompt + all tools are cached together.
            if cache_system && !matches!(cache_mode, CacheMode::None) && i == tools_count - 1 {
                tool["cache_control"] = json!({ "type": "ephemeral" });
            }
            tool
        })
        .collect();

    // R-NBED-050 §2: inject placeholder tool if require_tools and no tools.
    if quirks.require_tools && tools_json.is_empty() {
        let mut placeholder = json!({
            "type": "function",
            "function": {
                "name": "_placeholder",
                "description": "Never called; satisfies gateway tool-presence requirement.",
                "parameters": { "type": "object", "properties": {} }
            }
        });
        // Cache the placeholder tool if system caching is requested.
        if cache_system && !matches!(cache_mode, CacheMode::None) {
            placeholder["cache_control"] = json!({ "type": "ephemeral" });
        }
        tools_json.push(placeholder);
        body["tool_choice"] = json!("auto");
    }

    if !tools_json.is_empty() {
        body["tools"] = json!(tools_json);
    }

    // An `Off` turn sends `reasoning_effort: "none"` — omitting the field leaves reasoning at
    // the model default (on for DeepSeek, which then returns no text). A gateway that rejects
    // `"none"` uses `quirks.reasoning = "none"` instead, which sends nothing at all.
    match quirks.reasoning.as_str() {
        "reasoning_effort" => {
            let effort = match req.thinking {
                Thinking::Off => Some("none"),
                other => effort_of(other),
            };
            if let Some(effort) = effort {
                body["reasoning_effort"] = json!(effort);
            }
        }
        "budget_tokens" => {
            if let Thinking::Budget(n) = req.thinking {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": n });
            }
        }
        "adaptive" => {
            // R-NBED-050 §1: adaptive thinking.
            if let Some(effort) = effort_of(req.thinking) {
                body["thinking"] = json!({ "type": "adaptive" });
                body["output_config"] = json!({ "effort": effort });
            }
        }
        _ => {} // "none"
    }

    // Extra body (LiteLLM passthrough).
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

/// `Thinking` → the reasoning-effort string the OpenAI family takes. `None` for `Off`,
/// which omits the field entirely.
pub(crate) fn effort_of(t: Thinking) -> Option<&'static str> {
    match t {
        Thinking::Off => None,
        Thinking::Effort(Effort::Low) => Some("low"),
        Thinking::Effort(Effort::Medium) => Some("medium"),
        Thinking::Effort(Effort::High) => Some("high"),
        Thinking::Budget(_) => Some("medium"),
    }
}

/// Expand one `Message` into ≥1 wire objects, pushing into `out`. A Tool-role message with N
/// ToolResult blocks becomes N `{ role: "tool", tool_call_id, content }` messages.
///
/// R-OAI-IMG: chat-completions rejects `image_url` inside a `role: "tool"` message, so tool-result
/// images go out as a synthetic `user` message immediately after.
pub fn encode_messages(msg: &Message, quirks: &Quirks, needs_cache: bool, out: &mut Vec<Value>) {
    if msg.role == Role::Tool {
        let results: Vec<_> = msg
            .content
            .iter()
            .filter_map(|block| {
                if let Content::ToolResult { id, content, .. } = block {
                    let inner_text: String = content
                        .iter()
                        .filter_map(|c| {
                            if let Content::Text { text } = c {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    // empty `content` 400s strict gateways; `(no output)` is the wire
                    // form only, the genuine output stays empty in the TUI.
                    let inner_text = if inner_text.trim().is_empty() {
                        "(no output)".to_string()
                    } else {
                        inner_text
                    };
                    let images: Vec<&Content> = content
                        .iter()
                        .filter(|c| matches!(c, Content::Image { .. }))
                        .collect();
                    Some((id, inner_text, images))
                } else {
                    None
                }
            })
            .collect();

        // Apply cache_control to last tool result if needed.
        let last_idx = results.len().saturating_sub(1);
        for (i, (id, inner_text, images)) in results.into_iter().enumerate() {
            if needs_cache && i == last_idx {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": id.0,
                    "content": [{
                        "type": "text",
                        "text": inner_text,
                        "cache_control": { "type": "ephemeral" }
                    }]
                }));
            } else {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": id.0,
                    "content": inner_text,
                }));
            }
            // R-OAI-IMG: tool-result images ride as a synthetic user message (see above).
            if !images.is_empty() {
                let parts: Vec<Value> = images.into_iter().map(|c| encode_content(c, quirks)).collect();
                out.push(json!({ "role": "user", "content": parts }));
            }
        }
        return;
    }

    // R-OAI-010 / DESIGN §4.2: chat has no reasoning content part — DeepSeek 400s on a
    // replayed persisted thinking block, so `strip` drops it. Responses makes the same choice.
    let content: Vec<&Content> = if quirks.thinking_replay == "strip" {
        msg.content
            .iter()
            .filter(|c| !matches!(c, Content::Thinking { .. }))
            .collect()
    } else {
        msg.content.iter().collect()
    };

    // Nothing left to send (e.g. a reasoning-only turn); `content: []` 400s strict gateways.
    if content.is_empty() {
        return;
    }

    out.push(encode_message(msg, &content, quirks, needs_cache));
}

fn encode_message(
    msg: &Message,
    content: &[&Content],
    quirks: &Quirks,
    needs_cache: bool,
) -> Value {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
        Role::System => "system",
    };

    // Simple text-only messages → string content (no cache needed).
    if content.len() == 1 {
        if let Content::Text { text } = content[0] {
            // Bedrock quirk: trim trailing whitespace from assistant messages.
            let text = if quirks.trim_trailing_whitespace && msg.role == Role::Assistant {
                text.trim_end()
            } else {
                text.as_str()
            };
            if !needs_cache {
                return json!({ "role": role, "content": text });
            }
            // With caching: use array format with cache_control on the text block.
            return json!({
                "role": role,
                "content": [{
                    "type": "text",
                    "text": text,
                    "cache_control": { "type": "ephemeral" }
                }]
            });
        }
    }

    // Tool-role messages normally go through `encode_messages`; fallback encodes the first result.
    if msg.role == Role::Tool {
        if let Some(Content::ToolResult { id, content, .. }) = content.first() {
            let inner_text: String = content
                .iter()
                .filter_map(|c| {
                    if let Content::Text { text } = c {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            // see encode_messages — empty tool content 400s strict gateways.
            let inner_text = if inner_text.trim().is_empty() {
                "(no output)".to_string()
            } else {
                inner_text
            };
            return json!({
                "role": "tool",
                "tool_call_id": id.0,
                "content": inner_text,
            });
        }
    }

    // Tool calls live in a top-level `tool_calls` array, not content parts; content is null
    // when there is no text alongside them.
    if msg.role == Role::Assistant {
        let tool_calls: Vec<Value> = content
            .iter()
            .filter_map(|c| {
                if let Content::ToolCall {
                    id,
                    name,
                    args_json,
                } = c
                {
                    Some(json!({
                        "id": id.0,
                        "type": "function",
                        "function": { "name": name, "arguments": args_json },
                    }))
                } else {
                    None
                }
            })
            .collect();

        if !tool_calls.is_empty() {
            // Collect any text parts alongside tool calls.
            let text_parts: Vec<&str> = content
                .iter()
                .filter_map(|c| {
                    if let Content::Text { text } = c {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            let content_val = if text_parts.is_empty() {
                Value::Null
            } else {
                let joined = text_parts.join("");
                // Bedrock quirk: trim trailing whitespace from assistant messages.
                let trimmed = if quirks.trim_trailing_whitespace {
                    joined.trim_end().to_string()
                } else {
                    joined
                };
                if trimmed.is_empty() {
                    Value::Null
                } else {
                    Value::String(trimmed)
                }
            };
            return json!({
                "role": "assistant",
                "content": content_val,
                "tool_calls": tool_calls,
            });
        }
    }

    // Multi-part content.
    let mut parts: Vec<Value> = content
        .iter()
        .map(|c| encode_content(c, quirks))
        .collect();

    // Apply cache_control to the last content part if needed.
    if needs_cache && !parts.is_empty() {
        if let Some(last) = parts.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
            }
        }
    }

    json!({ "role": role, "content": parts })
}

fn encode_content(c: &Content, _quirks: &Quirks) -> Value {
    match c {
        Content::Text { text } => json!({ "type": "text", "text": text }),
        Content::Image { sha256, mime } => {
            // sha256 may already be a data URI (resolved by plan_request) or a raw hash.
            let url = if sha256.starts_with("data:") {
                sha256.clone()
            } else {
                format!("data:{mime};base64,{sha256}")
            };
            // R-OAI-IMG-GUARD: a corrupt image would stay in the transcript and fail every
            // later turn, so it degrades to text here instead of being forwarded.
            match crate::image_guard::check(&url) {
                crate::image_guard::Verdict::Pass => json!({
                    "type": "image_url",
                    "image_url": { "url": url }
                }),
                crate::image_guard::Verdict::Repaired(fixed) => json!({
                    "type": "image_url",
                    "image_url": { "url": fixed }
                }),
                crate::image_guard::Verdict::Unusable(note) => {
                    json!({ "type": "text", "text": note })
                }
            }
        }
        Content::ToolCall {
            id,
            name,
            args_json,
        } => json!({
            "type": "tool_call",
            "id": id.0,
            "name": name,
            "arguments": args_json,
        }),
        Content::ToolResult {
            id,
            content,
            is_error,
        } => {
            let text = content
                .iter()
                .filter_map(|c| {
                    if let Content::Text { text } = c {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({ "type": "tool_result", "tool_use_id": id.0, "content": text, "is_error": is_error })
        }
        Content::Thinking { text, .. } => json!({ "type": "thinking", "thinking": text }),
    }
}

