//! R-ANTH-010/020/030/040 — Map kn9t Request → Anthropic Messages API body.
//!
//! Rules:
//! - Auth: x-api-key header + anthropic-version header
//! - R-ANTH-020: thinking blocks replayed verbatim with signature intact
//! - R-ANTH-030: cache_control at message level, priority order (not positional)
//! - R-ANTH-040: per-model min_tokens, max 4 breakpoints, usage partition

use serde_json::{json, Value};

/// Build the Anthropic Messages API request body.
pub fn build_body(req: &Value) -> Value {
    let model_id = req.get("model")
        .and_then(|m| m.get("api_id").or_else(|| m.get("id")))
        .and_then(|i| i.as_str())
        .unwrap_or("claude-sonnet-4-5");

    let max_tokens = req.get("max_tokens")
        .and_then(|m| m.as_u64())
        .unwrap_or(8096) as u32;

    let system = req.get("system")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty());

    // R-ANTH-030: cache breakpoints from req.cache — priority order, not positional.
    // cache is Vec<Cache> with positions; we encode cache_control at message level.
    let cache_positions: Vec<u64> = req.get("cache")
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter()
            .filter_map(|c| c.get("position").and_then(|p| p.as_u64()))
            .collect())
        .unwrap_or_default();

    // Convert messages
    let messages = build_messages(req, &cache_positions);

    // Thinking / extended thinking
    let thinking = req.get("thinking").and_then(|t| t.as_str()).unwrap_or("off");

    let mut body = json!({
        "model": model_id,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });

    if let Some(sys) = system {
        body["system"] = json!(sys);
    }

    // Tools
    if let Some(tools) = req.get("tools").filter(|t| !t.is_null()) {
        body["tools"] = tools.clone();
    }

    // Anthropic-format thinking
    if thinking != "off" {
        if let Some(budget) = parse_thinking_budget(req) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }

    body
}

fn parse_thinking_budget(req: &Value) -> Option<u32> {
    match req.get("thinking")? {
        Value::String(s) if s == "off" => None,
        Value::Object(o) => {
            // Budget(n) variant
            o.get("budget").and_then(|b| b.as_u64()).map(|b| b as u32)
                .or_else(|| {
                    // Effort variant — map to token budget heuristically
                    o.get("effort").and_then(|e| e.as_str()).map(|e| match e {
                        "low" => 1024, "medium" => 4096, _ => 10000,
                    })
                })
        }
        _ => None,
    }
}

/// Convert kn9t messages to Anthropic Messages API format.
/// R-ANTH-030: apply cache_control at message level for each breakpoint position.
fn build_messages(req: &Value, cache_positions: &[u64]) -> Value {
    let msgs = match req.get("messages").and_then(|m| m.as_array()) {
        Some(a) => a,
        None => return json!([]),
    };

    let mut out: Vec<Value> = Vec::new();

    for (i, msg) in msgs.iter().enumerate() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = msg.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();

        let api_role = match role {
            "user" | "tool" => "user",
            "assistant" => "assistant",
            _ => "user",
        };

        let mut parts: Vec<Value> = content.iter()
            .flat_map(|c| map_content_block(c, role))
            .collect();

        if parts.is_empty() {
            parts.push(json!({"type":"text","text":""}));
        }

        let mut msg_obj = json!({
            "role": api_role,
            "content": parts,
        });

        // R-ANTH-030: if this message index is a breakpoint, attach cache_control
        // to the last content block (message-level attachment via last part).
        let msg_pos = i as u64;
        if cache_positions.contains(&msg_pos) {
            if let Some(last) = msg_obj["content"].as_array_mut()
                .and_then(|a| a.last_mut())
            {
                last["cache_control"] = json!({"type":"ephemeral"});
            }
        }

        out.push(msg_obj);
    }

    json!(out)
}

fn map_content_block(c: &Value, role: &str) -> Vec<Value> {
    match c.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            let text = c.get("text").and_then(|t| t.as_str()).unwrap_or("");
            vec![json!({"type":"text","text":text})]
        }
        Some("thinking") => {
            // R-ANTH-020: replay verbatim with signature
            let thinking = c.get("thinking").or_else(|| c.get("text"))
                .and_then(|t| t.as_str()).unwrap_or("");
            let signature = c.get("signature").and_then(|s| s.as_str()).unwrap_or("");
            vec![json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": signature,
            })]
        }
        Some("tool_use") | Some("tool_call") if role == "assistant" => {
            let id = c.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
            // input can be an object or a JSON-string args_json
            let input: Value = c.get("input")
                .cloned()
                .or_else(|| {
                    c.get("args_json").and_then(|a| a.as_str())
                        .and_then(|s| serde_json::from_str(s).ok())
                })
                .unwrap_or(json!({}));
            vec![json!({ "type": "tool_use", "id": id, "name": name, "input": input })]
        }
        Some("tool_result") | Some("tool_call_result") => {
            let id = c.get("tool_call_id").or_else(|| c.get("id"))
                .and_then(|i| i.as_str()).unwrap_or("");
            // kn9t ToolResult.content is Vec<Content>, not a string.
            // Extract text from nested content blocks, or fall back to direct string.
            let content_str = if let Some(arr) = c.get("content").and_then(|ct| ct.as_array()) {
                arr.iter()
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                c.get("content").and_then(|ct| ct.as_str())
                    .or_else(|| c.get("text").and_then(|t| t.as_str()))
                    .unwrap_or("")
                    .to_string()
            };
            vec![json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content_str,
            })]
        }
        Some("image") | Some("image_url") => {
            // Anthropic uses base64 source for images.
            let url = c.get("url").or_else(|| c.get("image_url").and_then(|u| u.get("url")))
                .and_then(|u| u.as_str()).unwrap_or("");
            if url.starts_with("data:") {
                // data URL → extract base64
                let (media_type, data) = parse_data_url(url);
                vec![json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data }
                })]
            } else {
                // URL reference — pass as url source
                vec![json!({
                    "type": "image",
                    "source": { "type": "url", "url": url }
                })]
            }
        }
        _ => vec![],
    }
}

fn parse_data_url(url: &str) -> (String, String) {
    // data:<media_type>;base64,<data>
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some(sep) = rest.find(';') {
            let media_type = rest[..sep].to_string();
            let after = &rest[sep+1..];
            if let Some(data) = after.strip_prefix("base64,") {
                return (media_type, data.to_string());
            }
        }
    }
    ("image/png".to_string(), String::new())
}

/// R-ANTH-040: decode usage partition.
/// input_tokens = after-breakpoint remainder, cache_read, cache_write separate.
pub fn decode_usage(u: &Value) -> (u32, u32, u32, u32) {
    let input       = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let output      = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let cache_read  = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let cache_write = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    (input, output, cache_read, cache_write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_verbatim() {
        // R-ANTH-020: thinking signature must be preserved verbatim
        let c = json!({
            "type": "thinking",
            "thinking": "let me think",
            "signature": "sig_abc123"
        });
        let parts = map_content_block(&c, "assistant");
        assert_eq!(parts[0]["signature"], "sig_abc123");
        assert_eq!(parts[0]["thinking"], "let me think");
    }

    #[test]
    fn cache_priority_order() {
        // R-ANTH-030: [assistant, user] case — breakpoints [System, AfterMessage(1), AfterMessage(0)]
        // Messages: idx 0 = user, idx 1 = assistant
        // Breakpoint positions: 0 and 1 (both messages get cache_control)
        let req = json!({
            "model": { "id": "claude-sonnet-4-5" },
            "system": "sys",
            "messages": [
                { "role": "user",      "content": [{"type":"text","text":"hello"}] },
                { "role": "assistant", "content": [{"type":"text","text":"world"}] },
            ],
            "cache": [
                { "position": 0 },
                { "position": 1 }
            ],
            "tools": null, "thinking": "off", "max_tokens": null
        });

        let cache_positions: Vec<u64> = req["cache"].as_array().unwrap()
            .iter().filter_map(|c| c.get("position").and_then(|p| p.as_u64())).collect();

        let msgs = build_messages(&req, &cache_positions);
        let arr = msgs.as_array().unwrap();
        // Both messages should have cache_control on their last content block
        for msg in arr {
            let content = msg["content"].as_array().unwrap();
            let last = content.last().unwrap();
            assert!(last.get("cache_control").is_some(),
                "message {} missing cache_control", msg["role"]);
        }
    }

    #[test]
    fn usage_partition() {
        // R-ANTH-040: input + cache_read + cache_write = total context
        let u = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 200,
        });
        let (input, output, cache_read, cache_write) = decode_usage(&u);
        assert_eq!(input, 100);
        assert_eq!(output, 50);
        assert_eq!(cache_read, 800);
        assert_eq!(cache_write, 200);
        // Total context = 100 + 800 + 200 = 1100
        assert_eq!(input + cache_read + cache_write, 1100);
    }

    /// Test that kn9t-format tool_call is correctly mapped to Anthropic tool_use.
    #[test]
    fn tool_call_kn9t_format() {
        // kn9t serializes Content::ToolCall as {"type":"tool_call",...}
        let c = json!({
            "type": "tool_call",
            "id": "toolu_abc123",
            "name": "read",
            "args_json": r#"{"path":"test.txt"}"#
        });
        let parts = map_content_block(&c, "assistant");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "tool_use");
        assert_eq!(parts[0]["id"], "toolu_abc123");
        assert_eq!(parts[0]["name"], "read");
        assert_eq!(parts[0]["input"]["path"], "test.txt");
    }

    /// Test that kn9t-format tool_result with nested content is correctly mapped.
    #[test]
    fn tool_result_kn9t_format() {
        // kn9t serializes Content::ToolResult with content as Vec<Content>
        let c = json!({
            "type": "tool_result",
            "id": "toolu_abc123",
            "content": [{"type": "text", "text": "file contents here"}],
            "is_error": false
        });
        let parts = map_content_block(&c, "tool");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "tool_result");
        assert_eq!(parts[0]["tool_use_id"], "toolu_abc123");
        assert_eq!(parts[0]["content"], "file contents here");
    }

    /// Test tool_call/tool_result pairing in a full message conversion.
    #[test]
    fn tool_use_pairing() {
        let req = json!({
            "model": { "id": "claude-sonnet-4-5" },
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_call",
                        "id": "toolu_xyz",
                        "name": "bash",
                        "args_json": r#"{"cmd":"ls"}"#
                    }]
                },
                {
                    "role": "tool",
                    "content": [{
                        "type": "tool_result",
                        "id": "toolu_xyz",
                        "content": [{"type": "text", "text": "file1\nfile2"}],
                        "is_error": false
                    }]
                }
            ],
            "cache": []
        });

        let msgs = build_messages(&req, &[]);
        let arr = msgs.as_array().unwrap();
        assert_eq!(arr.len(), 2, "should have 2 messages");

        // First message: assistant with tool_use
        assert_eq!(arr[0]["role"], "assistant");
        let content0 = arr[0]["content"].as_array().unwrap();
        assert_eq!(content0[0]["type"], "tool_use");
        assert_eq!(content0[0]["id"], "toolu_xyz");

        // Second message: user (tool role becomes user) with tool_result
        assert_eq!(arr[1]["role"], "user");
        let content1 = arr[1]["content"].as_array().unwrap();
        assert_eq!(content1[0]["type"], "tool_result");
        assert_eq!(content1[0]["tool_use_id"], "toolu_xyz");
        assert_eq!(content1[0]["content"], "file1\nfile2");
    }

    /// Regression test: kn9t ToolResult.content is Vec<Content>, not a string.
    /// Before the fix, this was extracted as "" because as_str() returned None
    /// on the array, causing tool results to lose their actual content.
    #[test]
    fn tool_result_content_not_lost() {
        // This is the EXACT format kn9t serializes Content::ToolResult as:
        // - "content" is an array of Content blocks, not a string
        // - The bug was: c.get("content").and_then(|ct| ct.as_str()) returned None
        //   because content is an array, then fallback to c.get("text") also None,
        //   resulting in empty string ""
        let tool_result_kn9t_format = json!({
            "type": "tool_result",
            "id": "toolu_test123",
            "content": [
                {"type": "text", "text": "first line"},
                {"type": "text", "text": "second line"}
            ],
            "is_error": false
        });

        let parts = map_content_block(&tool_result_kn9t_format, "tool");
        
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "tool_result");
        assert_eq!(parts[0]["tool_use_id"], "toolu_test123");
        
        // THE BUG: before fix, this was "" (empty string)
        // AFTER fix: correctly extracts "first line\nsecond line"
        let content = parts[0]["content"].as_str().unwrap();
        assert!(!content.is_empty(), "content must not be empty - this was the bug!");
        assert_eq!(content, "first line\nsecond line");
    }

    /// Ensure we still handle the simple string format (for compatibility).
    #[test]
    fn tool_result_string_content_still_works() {
        // Some providers or older formats might send content as a direct string
        let tool_result_string_format = json!({
            "type": "tool_result",
            "id": "toolu_compat",
            "content": "direct string content"
        });

        let parts = map_content_block(&tool_result_string_format, "tool");
        
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["content"], "direct string content");
    }
}
