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
    cost_micros, Cancel, Decision, Event, Message, ModelSpec, Request, SessionId, Store, Thinking,
    ToolCall, ToolCtx, UsageKind,
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
        let end = payload
            .get("end")
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX);

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

    /// `session_fork` — spawn a new session from `session` (fork_reason=subagent).
    /// `copy_events: true` (default) inherits the parent transcript; `false`
    /// creates a bare child (task-only — the compactor use case). The fork
    /// captures the budget in the ForkSnapshot (R-PLUG-130).
    /// Reply: `{"session":"<new-id>"}`. A spawned session running a turn IS a
    /// sub-agent — there is no separate sub-agent concept in kn9t.
    fn session_fork(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let copy_events = payload
            .get("copy_events")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let budget_usd = payload.get("budget_usd").and_then(|v| v.as_f64());
        let model_id = payload.get("model").and_then(|v| v.as_str());
        let child = SessionId::new();
        let parent = SessionId(session.to_string());

        let model: Option<ModelSpec> = if let Some(id) = model_id {
            self.state
                .model_registry
                .iter()
                .find(|m| m.r#ref.id == id)
                .cloned()
        } else {
            self.state.store.get_model_spec_for_session(session)
        };
        if let Some(m) = &model {
            self.state.store.register_model_spec(m.clone());
        }

        let parent_head: u64 = self
            .state
            .store
            .query_one(
                "SELECT head_seq FROM sessions WHERE id=?1",
                &[&session],
                |r| r.get::<_, i64>(0),
            )
            .map(|h| h.max(0) as u64)
            .unwrap_or(0);
        let cwd = self.state.cwd.to_string_lossy().to_string();
        if copy_events {
            kn9t_store::fork_session(
                &self.state.store,
                &parent,
                &child,
                parent_head,
                kn9t_core::ForkReason::Subagent,
                budget_usd,
                &cwd,
            )
            .map_err(|e| format!("session_fork: {}", e.0))?;
        } else {
            kn9t_store::fork_session_empty(
                &self.state.store,
                &parent,
                &child,
                parent_head,
                kn9t_core::ForkReason::Subagent,
                budget_usd,
                &cwd,
            )
            .map_err(|e| format!("session_fork(bare): {}", e.0))?;
        }
        Ok(json!({ "session": child.0 }))
    }

    /// `session_prompt` — run one full synchronous turn on `session` with `text`
    /// (the session's own model, tool subset optional, fork budget enforced).
    /// Reply: `{"session":"...","result":"..."}`.
    fn session_prompt(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "session_prompt requires \"text\"".to_string())?;
        let tools = payload.get("tools").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        });
        let timeout = payload
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(600);
        let result = crate::turn::run_session_turn(
            &self.state,
            &SessionId(session.to_string()),
            text,
            tools,
            timeout,
        )?;
        Ok(json!({ "session": session, "result": result }))
    }

    /// `tool_list` — 96E-17: registry tool names (for composing child toolsets).
    /// Reply: `{"tools":["bash","read",...]}`.
    fn tool_list(&self, _session: Option<&str>, _payload: &Value) -> Result<Value, String> {
        let names: Vec<String> = self
            .state
            .tools_snapshot()
            .specs()
            .into_iter()
            .map(|s| s.name)
            .collect();
        Ok(json!({ "tools": names }))
    }

    /// `interaction_request` — 96E-28 generic primitive: register a pending
    /// interaction with `payload` (plugin's own opaque shape) and block until
    /// the client responds via `POST /ui-respond {id, payload}`.
    /// Emits `LiveEvent::InteractionRequest {id, plugin, payload}` to the
    /// session bus so the TUI (or any SSE client) can render it generically.
    /// Reply: `{"payload": <client response>}` — the client's opaque answer.
    fn interaction_request(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        // The plugin's prompt payload is `payload.payload` if wrapped, else the
        // whole payload. Accept both for SDK convenience — but require something.
        let prompt_payload = payload
            .get("payload")
            .cloned()
            .unwrap_or_else(|| payload.clone());
        // Create pending slot
        let (id, handle) = self.state.interaction_registry.create(
            session.to_string(),
            plugin.to_string(),
            prompt_payload.clone(),
        );
        // Emit to session bus — TUI renders generically from `payload`.
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::InteractionRequest {
            id,
            plugin: plugin.to_string(),
            payload: prompt_payload,
        });
        // Block on condvar until `POST /ui-respond` resolves it.
        let response = self.state.interaction_registry.wait(&handle);
        Ok(json!({ "payload": response }))
    }

    /// `provider_complete` — one real provider call with the session's model.
    /// Reply: `{"content":[...],"stop":"...","usage":{"input":..,"output":..}}`.
    /// Optional: `"tools"` — array of tool specs to enable tool_use responses.
    fn provider_complete(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let model = self.resolve_model(payload)?;

        let messages: Vec<Message> = payload
            .get("messages")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| {
                "provider_complete requires \"messages\" (list of messages)".to_string()
            })?;
        let system = payload.get("system").and_then(|v| v.as_str());
        let max_tokens = payload
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|t| t as u32);

        // Optional tools: either inline specs or names to look up from registry
        let tools: Vec<kn9t_core::ToolSpec> = if let Some(tools_val) = payload.get("tools") {
            if let Some(arr) = tools_val.as_array() {
                // If array of strings -> look up from registry
                // If array of objects -> parse as ToolSpec
                if arr.first().map(|v| v.is_string()).unwrap_or(false) {
                    let names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                    let registry = self.state.tools_snapshot();
                    names
                        .iter()
                        .filter_map(|n| registry.get(n).map(|t| t.spec().clone()))
                        .collect()
                } else {
                    serde_json::from_value(tools_val.clone()).unwrap_or_default()
                }
            } else {
                vec![]
            }
        } else {
            vec![]
        };
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
            tools: &tools,
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

    /// `ui_directive` / `ui_push` — 96E-23 structured plugin→TUI directive (session-scoped).
    /// Required: `"target"` (string, non-empty) + `"op"` (string, non-empty).
    /// Optional: `"payload"` (any JSON, defaults to null) — forwarded verbatim (opaque).
    /// Emits `LiveEvent::UiDirective {plugin, target, op, payload}` to the session's bus,
    /// reusing 96E-21's session-scoped routing (no broadcast fallback).
    /// Reply: `{"ok":true}`.
    fn ui_directive(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let target = payload
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_directive requires \"target\" (string)".to_string())?;
        let op = payload
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_directive requires \"op\" (string)".to_string())?;
        if target.is_empty() {
            return Err("ui_directive: target must be non-empty".into());
        }
        if op.is_empty() {
            return Err("ui_directive: op must be non-empty".into());
        }
        let inner = payload.get("payload").cloned().unwrap_or(Value::Null);
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: target.to_string(),
            op: op.to_string(),
            payload: inner,
        });
        Ok(json!({"ok": true}))
    }

    /// 96E-24 — `ui_declare_page {page_id, layout}` — declare a templated page,
    /// then `ui_write_placeholder` cheaply and `ui_clear_page` teardown.
    fn ui_declare_page(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let page_id = payload
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_declare_page requires \"page_id\"".to_string())?;
        let layout = payload
            .get("layout")
            .ok_or_else(|| "ui_declare_page requires \"layout\"".to_string())?;
        self.state
            .ui_pages
            .declare(plugin, session, page_id, layout)?;
        // Forward to TUI as a structured UiDirective (same bus, session-scoped)
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: page_id.to_string(),
            op: "declare_page".to_string(),
            payload: json!({"page_id": page_id, "layout": layout}),
        });
        Ok(json!({"ok": true, "page_id": page_id}))
    }

    fn ui_write_placeholder(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let page_id = payload
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_write_placeholder requires \"page_id\"".to_string())?;
        let placeholder_id = payload
            .get("placeholder_id")
            .or_else(|| payload.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_write_placeholder requires \"placeholder_id\"".to_string())?;
        let value = payload
            .get("value")
            .cloned()
            .ok_or_else(|| "ui_write_placeholder requires \"value\"".to_string())?;
        self.state
            .ui_pages
            .write(plugin, session, page_id, placeholder_id, value.clone())?;
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: page_id.to_string(),
            op: "write_placeholder".to_string(),
            payload: json!({"page_id": page_id, "placeholder_id": placeholder_id, "value": value}),
        });
        Ok(json!({"ok": true}))
    }

    fn ui_clear_page(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let page_id = payload
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_clear_page requires \"page_id\"".to_string())?;
        self.state.ui_pages.clear(plugin, session, page_id)?;
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: page_id.to_string(),
            op: "clear_page".to_string(),
            payload: json!({"page_id": page_id}),
        });
        // Also host-side teardown already done via clear(); TUI will drop its rendering.
        Ok(json!({"ok": true}))
    }

    /// `tool_execute` — run a registry tool through the normal approval path.
    /// Reply: `{"content":[...],"is_error":bool}`.
    /// 96E-22 fix: CallId must be unique per invocation, not `plugin-{name}` (which
    /// collides on repeated same-tool calls and silently overwrites live_tool_calls via
    /// INSERT OR REPLACE).
    fn tool_execute(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "tool_execute requires \"name\"".to_string())?;
        let args = payload
            .get("args")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let tool = self
            .state
            .tools_snapshot()
            .get(name)
            .ok_or_else(|| format!("unknown tool {name:?}"))?
            .clone();

        // Normal approval path: the approver shows/answers the request.
        // 96E-22: unique per invocation — static atomic counter avoids colliding on repeated same-tool calls.
        static TOOL_EXEC_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let call = ToolCall {
            id: kn9t_core::CallId(format!(
                "plugin-{name}-{}",
                TOOL_EXEC_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )),
            name: name.to_string(),
            args_json: serde_json::to_string(&args).unwrap_or_default(),
        };
        // 96E-33: the session and its sink are passed explicitly. This call runs on an API
        // worker thread, not the turn thread, so the old thread-local sink was always unset
        // here and every approval fell through to "no sink" -> Deny. Now the prompt actually
        // reaches the session's SSE stream.
        let approval_sink = self.sink(session);
        let ctx = kn9t_core::ApprovalCtx {
            session,
            sink: &approval_sink,
        };
        match self.state.approver_snapshot().request(
            &call,
            &self.state.cwd,
            "plugin tool_execute",
            &ctx,
        ) {
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
        plugin: &str,
        session: Option<&str>,
        op: &str,
        payload: &Value,
    ) -> Result<Value, String> {
        match op {
            "session_read" => self.session_read(session, payload),
            "provider_complete" => self.provider_complete(session, payload),
            "tool_execute" => self.tool_execute(session, payload),
            "session_fork" => self.session_fork(session, payload),
            "session_prompt" => self.session_prompt(session, payload),
            "tool_list" => self.tool_list(session, payload),
            "interaction_request" => self.interaction_request(session, payload, plugin),
            "ui_directive" | "ui_push" => self.ui_directive(session, payload, plugin),
            "ui_declare_page" => self.ui_declare_page(session, payload, plugin),
            "ui_write_placeholder" => self.ui_write_placeholder(session, payload, plugin),
            "ui_clear_page" => self.ui_clear_page(session, payload, plugin),
            other => Err(format!("unknown host API op {other:?}")),
        }
    }
}
