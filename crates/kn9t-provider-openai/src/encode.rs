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

    // User/assistant/tool messages.
    // A Tool-role message may carry N ToolResult blocks (one per parallel call).
    // The wire format requires one message per result, so we expand here.
    for (idx, msg) in req.messages.iter().enumerate() {
        let needs_cache = !matches!(cache_mode, CacheMode::None) && cache_indices.contains(&idx);
        encode_messages(msg, quirks, needs_cache, &mut messages);
    }
    body["messages"] = json!(messages);

    // Tools: build array and apply cache_control to the last tool if Cache::System is set.
    // This caches the entire system + tools prefix (opencode "caterpillar" strategy).
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

    // Reasoning / thinking quirk.
    match quirks.reasoning.as_str() {
        "reasoning_effort" => {
            let effort_str = match req.thinking {
                Thinking::Off => "low",
                Thinking::Effort(e) => match e {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                },
                Thinking::Budget(_) => "medium",
            };
            body["reasoning_effort"] = json!(effort_str);
        }
        "budget_tokens" => {
            if let Thinking::Budget(n) = req.thinking {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": n });
            }
        }
        "adaptive" => {
            // R-NBED-050 §1: adaptive thinking.
            let effort_str = match req.thinking {
                Thinking::Off => "low",
                Thinking::Effort(e) => match e {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                },
                Thinking::Budget(_) => "medium",
            };
            body["thinking"] = json!({ "type": "adaptive" });
            body["output_config"] = json!({ "effort": effort_str });
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

/// Expand one `Message` into ≥1 wire objects, pushing into `out`.
/// Tool-role messages with N ToolResult blocks become N separate wire messages
/// (one `{ role: "tool", tool_call_id, content }` per result).
fn encode_messages(msg: &Message, quirks: &Quirks, needs_cache: bool, out: &mut Vec<Value>) {
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
                    // 96E-19: an empty tool result must still carry non-empty content —
                    // strict gateways (opencode zen, OpenAI) reject `content: ""` with
                    // HTTP 400 "empty content". The output is genuinely empty; this is
                    // the wire form, not a masking placeholder (the TUI still shows
                    // nothing under the tool card output).
                    let inner_text = if inner_text.trim().is_empty() {
                        "(no output)".to_string()
                    } else {
                        inner_text
                    };
                    Some((id, inner_text))
                } else {
                    None
                }
            })
            .collect();

        // Apply cache_control to last tool result if needed.
        let last_idx = results.len().saturating_sub(1);
        for (i, (id, inner_text)) in results.into_iter().enumerate() {
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
        }
        return;
    }
    out.push(encode_message(msg, quirks, needs_cache));
}

fn encode_message(msg: &Message, quirks: &Quirks, needs_cache: bool) -> Value {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
        Role::System => "system",
    };

    // Simple text-only messages → string content (no cache needed).
    if msg.content.len() == 1 {
        if let Content::Text { text } = &msg.content[0] {
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

    // Tool result message — handled by encode_messages; should not reach here.
    // Fallback: encode first result only (safe, but encode_messages avoids this path).
    if msg.role == Role::Tool {
        if let Some(Content::ToolResult { id, content, .. }) = msg.content.first() {
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
            // 96E-19: see encode_messages — empty tool content 400s strict gateways.
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

    // Assistant message with tool calls — OpenAI wire format puts them in a top-level
    // `tool_calls` array, NOT in content parts. Content is null when only tool calls present.
    if msg.role == Role::Assistant {
        let tool_calls: Vec<Value> = msg
            .content
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
            let text_parts: Vec<&str> = msg
                .content
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
    let mut parts: Vec<Value> = msg
        .content
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
            json!({
                "type": "image_url",
                "image_url": { "url": url }
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use kn9t_provider_core::{CallId, MsgId};

    /// 96E-19 — a genuinely empty tool result must not 400 the gateway:
    /// `content` on the wire is non-empty while the TUI still renders no output.
    #[test]
    fn tool_result_empty_content_is_nonempty_on_wire() {
        let msg = Message {
            id: MsgId::new(),
            role: Role::Tool,
            content: vec![Content::ToolResult {
                id: CallId("call_1".into()),
                content: vec![Content::Text { text: "   ".into() }], // whitespace-only = empty
                is_error: false,
            }],
            silent: false,
        };
        let quirks = Quirks::default();
        let mut out = Vec::new();
        encode_messages(&msg, &quirks, false, &mut out);
        assert_eq!(out.len(), 1, "one wire message per tool result");
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "call_1");
        assert_eq!(
            out[0]["content"],
            serde_json::json!("(no output)"),
            "empty tool content must be replaced with a non-empty wire form"
        );
    }

    /// 96E-19 — non-empty results are untouched.
    #[test]
    fn tool_result_keeps_real_output() {
        let msg = Message {
            id: MsgId::new(),
            role: Role::Tool,
            content: vec![Content::ToolResult {
                id: CallId("call_2".into()),
                content: vec![Content::Text {
                    text: "file1\nfile2".into(),
                }],
                is_error: false,
            }],
            silent: false,
        };
        let quirks = Quirks::default();
        let mut out = Vec::new();
        encode_messages(&msg, &quirks, false, &mut out);
        assert_eq!(out[0]["content"], serde_json::json!("file1\nfile2"));
    }
}
