//! 96E-17 — server-side plugin → host API (host_api capability).
//!
//! kn9t does not embed sub-agents: this is the open API that lets external
//! plugins run their own agent loops with the session's own infrastructure.
//!
//! Ops v1:
//! - `provider_complete` — run the session's model through the real provider
//!   (same credentials/cache; usage recorded as `UsageKind::Subagent`).
//! - `session_read` — read projected messages by seq range (ID → content
//!   resolution for tool results / spans).
//! - `tool_execute` — run a registry tool through the normal approval path.
//!
//! Session id travels INSIDE each payload (`"session"`) — the host reader's
//! thread-local session belongs to the turn thread, not the API worker (96E-5).

use std::sync::Arc;

use kn9t_core::{
    cost_micros, Cancel, Decision, Event, Message, ModelSpec, Request, SessionId, Store,
    Thinking, ToolCall, ToolCtx, UsageKind,
};
use kn9t_plugin::HostApi;
use serde_json::{json, Value};

use crate::bus::SessionSink;
use crate::state::ServerState;

/// The host-side API implementation installed on every plugin host.
pub struct ServerHostApi {
    pub state: Arc<ServerState>,
}

impl ServerHostApi {
    fn require_session<'a>(&self, session: Option<&'a str>) -> Result<&'a str, String> {
        session.ok_or_else(|| "op requires a session id in payload (\"session\")".to_string())
    }

    fn resolve_model(&self, payload: &Value) -> Result<ModelSpec, String> {
        if let Some(id) = payload.get("model").and_then(|v| v.as_str()) {
            return self
                .state
                .model_registry
                .iter()
                .find(|m| m.r#ref.id == id)
                .cloned()
                .ok_or_else(|| format!("model {id:?} not in registry"));
        }
        let session = self.require_session(payload.get("session").and_then(|v| v.as_str()))?;
        self.state
            .store
            .get_model_spec_for_session(session)
            .or_else(|| self.state.default_model.clone())
            .ok_or_else(|| "no model available".to_string())
    }

    fn sink(&self, session: &str) -> SessionSink {
        SessionSink::with_store(
            self.state.buses.bus_for(session),
            self.state.store.clone(),
            SessionId(session.to_string()),
        )
    }

    /// `session_read` — projected messages in `[start, end]` (default: whole
    /// transcript). Reply: `{"messages":[{"seq":..,"role":..,"content":[...]}]}`.
    fn session_read(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let start = payload.get("start").and_then(|v| v.as_u64()).unwrap_or(0);
        let end = payload.get("end").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);

        let rows = self
            .state
            .store
            .query_rows(
                "SELECT seq, role, content FROM messages WHERE session_id=?1 AND seq>=?2 AND seq<=?3 ORDER BY seq",
                &[&session, &(start as i64), &(end as i64)],
                |r| {
                    let seq: i64 = r.get(0)?;
                    let role: String = r.get(1)?;
                    let content: String = r.get(2)?;
                    let content: Value = serde_json::from_str(&content)
                        .unwrap_or(Value::Array(vec![]));
                    Ok(json!({ "seq": seq, "role": role, "content": content }))
                },
            )
            .map_err(|e| format!("session_read: {}", e.0))?;
        Ok(json!({ "messages": rows }))
    }

    /// `provider_complete` — one real provider call with the session's model.
    /// Reply: `{"content":[...],"stop":"...","usage":{"input":..,"output":..}}`.
    fn provider_complete(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let model = self.resolve_model(payload)?;

        let messages: Vec<Message> = payload
            .get("messages")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| "provider_complete requires \"messages\" (list of messages)".to_string())?;
        let system = payload.get("system").and_then(|v| v.as_str());
        let max_tokens = payload.get("max_tokens").and_then(|v| v.as_u64()).map(|t| t as u32);

        let provider = self
            .state
            .get_provider(&model.r#ref.provider)
            .or_else(|| self.state.provider.clone())
            .ok_or_else(|| format!("no provider for {}", model.r#ref.provider))?;

        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        let req = Request {
            model: &model,
            system,
            messages: &messages,
            tools: &[],
            thinking: Thinking::Off,
            max_tokens,
            cache: &[],
        };
        let cancel = Cancel::new();
        let chunks = provider
            .stream_with_sink(&req, &cancel, Some(sink.as_ref()))
            .map_err(|e| format!("provider stream: {e:?}"))?;
        let assembled = kn9t_provider_core::assemble(chunks, sink.as_ref())
            .map_err(|e| format!("provider assemble: {e:?}"))?;

        // Record usage in the session (kind = Subagent) so budgets are honest.
        let micros = cost_micros(&assembled.usage.tokens, &model.price);
        self.state
            .store
            .append(
                &SessionId(session.to_string()),
                Event::UsageRecorded {
                    seq: 0,
                    provider: model.r#ref.provider.clone(),
                    model: model.r#ref.id.clone(),
                    kind: UsageKind::Subagent,
                    tokens: assembled.usage.tokens,
                    price_snapshot: model.price.clone(),
                    cost_micros: micros,
                    cost_usd: micros as f64 / 1_000_000.0,
                    estimated: !assembled.usage_reported,
                },
            )
            .map_err(|e| format!("record usage: {}", e.0))?;

        let stop = match assembled.stop {
            kn9t_core::StopReason::Stop => "stop",
            kn9t_core::StopReason::ToolUse => "tool_use",
            kn9t_core::StopReason::Length => "length",
            kn9t_core::StopReason::Aborted => "aborted",
            kn9t_core::StopReason::Refusal => "refusal",
        };
        Ok(json!({
            "content": assembled.message.content,
            "stop": stop,
            "usage": {
                "input": assembled.usage.tokens.input,
                "output": assembled.usage.tokens.output,
                "cache_read": assembled.usage.tokens.cache_read,
                "cache_write": assembled.usage.tokens.cache_write,
            },
        }))
    }

    /// `tool_execute` — run a registry tool through the normal approval path.
    /// Reply: `{"content":[...],"is_error":bool}`.
    fn tool_execute(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "tool_execute requires \"name\"".to_string())?;
        let args = payload.get("args").cloned().unwrap_or(Value::Object(Default::default()));

        let tool = self
            .state
            .tools_snapshot()
            .get(name)
            .ok_or_else(|| format!("unknown tool {name:?}"))?
            .clone();

        // Normal approval path: the approver shows/answers the request.
        let call = ToolCall {
            id: kn9t_core::CallId(format!("plugin-{name}")),
            name: name.to_string(),
            args_json: serde_json::to_string(&args).unwrap_or_default(),
        };
        match self.state.approver_snapshot().request(&call, &self.state.cwd, "plugin tool_execute") {
            Decision::Allow => {}
            decision => {
                let reason = match decision {
                    Decision::Deny { reason } | Decision::HardDeny { reason } => reason,
                    _ => "not approved".to_string(),
                };
                return Err(format!("tool {name:?} not approved: {reason}"));
            }
        }

        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        let cancel = Cancel::new();
        let ctx = ToolCtx {
            cwd: self.state.cwd.clone(),
            read: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            bus: sink,
            call_id: call.id.clone(),
        };
        let out = tool
            .execute(&args, &ctx, &cancel)
            .map_err(|e| format!("tool {name:?} failed: {e}"))?;
        Ok(json!({ "content": out.content, "is_error": out.is_error }))
    }
}

impl HostApi for ServerHostApi {
    fn handle(
        &self,
        _plugin: &str,
        session: Option<&str>,
        op: &str,
        payload: &Value,
    ) -> Result<Value, String> {
        match op {
            "session_read" => self.session_read(session, payload),
            "provider_complete" => self.provider_complete(session, payload),
            "tool_execute" => self.tool_execute(session, payload),
            other => Err(format!("unknown host API op {other:?}")),
        }
    }
}