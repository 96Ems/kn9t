//! Q26 / §13.8 — RemoteProvider: adapts a PluginHost into kn9t_core::Provider.
//!
//! The host sends `{"t":"hook","hook":"provider_complete","payload":<Request>}`.
//! The plugin streams `Chunk` messages then a `Done` with stop + usage.

use crate::codec::{HostMsg, write_host_msg};
use crate::host::PluginHost;
use kn9t_core::{
    CallId, Cancel, Chunk, ModelRef, ProvErr, Provider, Request, StopReason, Tokens, Usage,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Six-hundred second timeout for a full streaming call.
const STREAM_TIMEOUT: Duration = Duration::from_secs(600);

/// Adapts a `PluginHost` (subprocess) into the `Provider` trait (Q26).
pub struct RemoteProvider {
    pub(crate) host: Arc<PluginHost>,
    provider_id: String,
}

impl RemoteProvider {
    /// Create from a plugin host. `provider_id` comes from `ProviderDecl::id`.
    pub fn new(host: Arc<PluginHost>, provider_id: String) -> Self {
        Self { host, provider_id }
    }
}

impl Provider for RemoteProvider {
    fn name(&self) -> &str { &self.provider_id }

    fn stream(
        &self,
        req: &Request,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let payload = serialise_request(req);
        let id = self.host.next_id.fetch_add(1, Ordering::Relaxed);
        let model_ref = req.model.r#ref.clone();

        let msg = HostMsg::Hook {
            id,
            hook: "provider_complete".to_string(),
            payload,
        };
        {
            let mut w = self.host.writer.lock().unwrap();
            write_host_msg(&mut **w, &msg)
                .map_err(|e| ProvErr::Connect(format!("plugin write: {e}")))?;
        }

        // Collect all chunks synchronously from the plugin.
        let mut chunks: Vec<Result<Chunk, ProvErr>> = Vec::new();
        let mut had_usage = false;
        let mut stream_err: Option<ProvErr> = None;

        let done_result = self.host.wait_for_streaming(
            id,
            STREAM_TIMEOUT,
            |body: Value| {
                if stream_err.is_some() { return; }
                match decode_chunk_body(&body) {
                    Ok(Some(Chunk::Usage(_))) => {
                        had_usage = true;
                        // Defer until done so we have the complete usage.
                    }
                    Ok(Some(c)) => chunks.push(Ok(c)),
                    Ok(None) => {}  // unknown kind — ignored
                    Err(e) => stream_err = Some(e),
                }
            },
        );

        if let Some(e) = stream_err {
            return Err(e);
        }

        // Decode done body.
        match done_result {
            Err(e) => {
                let s = e.to_string();
                if s.contains("context deadline exceeded") || s.contains("deadline exceeded") {
                    return Err(ProvErr::Truncated);
                }
                if s.contains("prompt is too long") {
                    return Err(ProvErr::ContextOverflow);
                }
                return Err(ProvErr::Stream(s));
            }
            Ok(done) => {
                if let Some(err_str) = done.get("error").and_then(|e| e.as_str()) {
                    let e = err_str.to_string();
                    if e.contains("prompt is too long") { return Err(ProvErr::ContextOverflow); }
                    return Err(ProvErr::Stream(e));
                }

                let stop = decode_stop(&done);
                let tokens = decode_tokens(&done.get("usage").unwrap_or(&Value::Null));

                // Emit Usage chunk, then Stop.
                chunks.push(Ok(Chunk::Usage(Usage {
                    tokens,
                    model: model_ref.clone(),
                })));
                chunks.push(Ok(Chunk::Stop(stop)));
            }
        }

        Ok(Box::new(chunks.into_iter()))
    }
}

// ── request serialisation ─────────────────────────────────────────────────────

fn serialise_request(req: &Request) -> Value {
    json!({
        "model": req.model,
        "system": req.system,
        "messages": serde_json::to_value(req.messages).unwrap_or(Value::Null),
        "tools": serde_json::to_value(req.tools).unwrap_or(Value::Null),
        "thinking": req.thinking,
        "max_tokens": req.max_tokens,
        "cache": serde_json::to_value(req.cache).unwrap_or(Value::Null),
    })
}

// ── chunk decoding ────────────────────────────────────────────────────────────

fn decode_chunk_body(body: &Value) -> Result<Option<Chunk>, ProvErr> {
    let kind = body.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let idx = body.get("idx").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

    let c = match kind {
        "text_delta" => {
            let delta = body.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
            Chunk::Text { idx, delta }
        }
        "thinking_delta" => {
            let delta = body.get("thinking").and_then(|t| t.as_str()).unwrap_or("").to_string();
            Chunk::Thinking { idx, delta }
        }
        "tool_use_start" => {
            let call_id = body.get("call_id").and_then(|i| i.as_str()).unwrap_or("");
            let name = body.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            Chunk::ToolCall { idx, id: CallId(call_id.to_string()), name }
        }
        "tool_use_delta" => {
            let delta = body.get("args_json").and_then(|a| a.as_str()).unwrap_or("").to_string();
            Chunk::ToolArgs { idx, delta }
        }
        "usage" | "input_tokens" => {
            // Signal caller to defer — pass a sentinel usage with zeros.
            Chunk::Usage(Usage {
                tokens: Tokens::default(),
                model: ModelRef { provider: String::new(), id: String::new() },
            })
        }
        "" => return Err(ProvErr::Decode("chunk body missing 'kind'".into())),
        _ => return Ok(None), // unknown — silently ignored (R-CP-060 custom provider)
    };
    Ok(Some(c))
}

fn decode_stop(body: &Value) -> StopReason {
    match body.get("stop").and_then(|s| s.as_str()).unwrap_or("") {
        s if s.to_ascii_lowercase().contains("abort") => StopReason::Aborted,
        s if s.to_ascii_lowercase().contains("tool") => StopReason::ToolUse,
        s if s.to_ascii_lowercase().contains("length")
            || s.to_ascii_lowercase().contains("max") => StopReason::Length,
        _ => StopReason::Stop,
    }
}

fn decode_tokens(u: &Value) -> Tokens {
    let get = |keys: &[&str]| -> u32 {
        keys.iter()
            .find_map(|k| u.get(*k).and_then(|v| v.as_u64()))
            .unwrap_or(0) as u32
    };
    Tokens {
        input:       get(&["input", "prompt_tokens", "input_tokens", "promptTokens"]),
        output:      get(&["output", "completion_tokens", "output_tokens", "completionTokens"]),
        cache_read:  get(&["cache_read", "cache_read_input_tokens", "cacheReadTokens", "cached_tokens"]),
        cache_write: get(&["cache_write", "cache_creation_input_tokens"]),
        reasoning:   get(&["reasoning_tokens"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that decode_stop correctly handles ABORTED from custom provider cancel fix.
    #[test]
    fn decode_stop_handles_aborted() {
        // The custom provider fix returns stop="ABORTED" when cancelled mid-stream
        let body = json!({"stop": "ABORTED"});
        assert!(matches!(decode_stop(&body), StopReason::Aborted));
        
        // Also test lowercase
        let body = json!({"stop": "aborted"});
        assert!(matches!(decode_stop(&body), StopReason::Aborted));
        
        // And mixed case
        let body = json!({"stop": "Aborted"});
        assert!(matches!(decode_stop(&body), StopReason::Aborted));
        
        // And with prefix/suffix
        let body = json!({"stop": "user_aborted"});
        assert!(matches!(decode_stop(&body), StopReason::Aborted));
    }

    #[test]
    fn decode_stop_handles_tool_call() {
        let body = json!({"stop": "TOOL_CALL"});
        assert!(matches!(decode_stop(&body), StopReason::ToolUse));
        
        let body = json!({"stop": "tool_use"});
        assert!(matches!(decode_stop(&body), StopReason::ToolUse));
    }

    #[test]
    fn decode_stop_handles_length() {
        let body = json!({"stop": "LENGTH"});
        assert!(matches!(decode_stop(&body), StopReason::Length));
        
        let body = json!({"stop": "max_tokens"});
        assert!(matches!(decode_stop(&body), StopReason::Length));
    }

    #[test]
    fn decode_stop_defaults_to_stop() {
        let body = json!({"stop": "STOP"});
        assert!(matches!(decode_stop(&body), StopReason::Stop));
        
        let body = json!({"stop": "end_turn"});
        assert!(matches!(decode_stop(&body), StopReason::Stop));
        
        // Missing stop field
        let body = json!({});
        assert!(matches!(decode_stop(&body), StopReason::Stop));
    }

    /// Ensure ABORTED takes priority (checked first).
    #[test]
    fn decode_stop_aborted_priority() {
        // Edge case: what if someone sends "aborted_tool"?
        // ABORTED should match first since it's checked first
        let body = json!({"stop": "aborted_tool"});
        assert!(matches!(decode_stop(&body), StopReason::Aborted),
            "ABORTED should be checked before TOOL");
    }
}
