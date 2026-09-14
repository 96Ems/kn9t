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
                .find_model(id)
                .ok_or_else(|| format!("model {id:?} not in registry"));
        }
        let session = self.require_session(payload.get("session").and_then(|v| v.as_str()))?;
        self.state
            .store
            .get_model_spec_for_session(session)
            .or_else(|| self.state.default_model_snapshot())
            .ok_or_else(|| "no model available".to_string())
    }

    fn sink(&self, session: &str) -> SessionSink {
        SessionSink::with_store(
            self.state.buses.bus_for(session),
            self.state.store.clone(),
            SessionId(session.to_string()),
            self.state.clone(),
        )
    }

    /// Get the working directory for a session from the database.
    /// Falls back to the server's cwd if the session is not found.
    fn session_cwd(&self, session: &str) -> std::path::PathBuf {
        self.state
            .store
            .query_one("SELECT cwd FROM sessions WHERE id=?1", &[&session], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.state.cwd.clone())
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

    /// `session_create` — create a brand new session (no parent, no fork).
    /// Use this for fully independent workers that don't need context from the caller.
    /// Optional: `"model"` (model id), `"cwd"` (working directory, defaults to caller's).
    /// Reply: `{"session":"<new-id>"}`.
    fn session_create(&self, session: Option<&str>, payload: &Value) -> Result<Value, String> {
        let new_id = SessionId::new();
        
        // Use specified model, or inherit from calling session, or use default
        let model_ref = if let Some(id) = payload.get("model").and_then(|v| v.as_str()) {
            self.state
                .find_model(id)
                .map(|m| m.r#ref.clone())
                .ok_or_else(|| format!("model {id:?} not in registry"))?
        } else if let Some(sess) = session {
            self.state
                .store
                .get_model_spec_for_session(sess)
                .or_else(|| self.state.default_model_snapshot())
                .map(|m| m.r#ref.clone())
                .ok_or_else(|| "no model available".to_string())?
        } else {
            self.state
                .default_model_snapshot()
                .map(|m| m.r#ref.clone())
                .ok_or_else(|| "no model available".to_string())?
        };

        // Use specified cwd, or inherit from calling session, or use server cwd
        let cwd = if let Some(c) = payload.get("cwd").and_then(|v| v.as_str()) {
            c.to_string()
        } else if let Some(sess) = session {
            self.session_cwd(sess).to_string_lossy().to_string()
        } else {
            self.state.cwd.to_string_lossy().to_string()
        };

        kn9t_store::create_session(&self.state.store, &new_id, &cwd, &model_ref)
            .map_err(|e| format!("session_create: {}", e.0))?;

        Ok(json!({ "session": new_id.0 }))
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
            self.state.find_model(id)
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
        // Subagent inherits the parent session's cwd, not the server's process cwd.
        let cwd = self.session_cwd(session);
        let cwd_str = cwd.to_string_lossy().to_string();
        if copy_events {
            kn9t_store::fork_session(
                &self.state.store,
                &parent,
                &child,
                parent_head,
                kn9t_core::ForkReason::Subagent,
                budget_usd,
                &cwd_str,
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
                &cwd_str,
            )
            .map_err(|e| format!("session_fork(bare): {}", e.0))?;
        }
        Ok(json!({ "session": child.0 }))
    }

    /// `session_prompt` — run one full synchronous turn on `session` with `text`
    /// (the session's own model, tool subset optional, fork budget enforced).
    /// Reply: `{"session":"...","result":"..."}`.
    ///
    /// 96E-39: passes parent session's Cancel so ESC propagates to subagent.
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
        // 96E-39: get parent session's Cancel so ESC propagates to subagent
        let parent_cancel = crate::turn::get_cancel(&self.state, session);
        let result = crate::turn::run_session_turn(
            &self.state,
            &SessionId(session.to_string()),
            text,
            tools,
            timeout,
            parent_cancel,
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
            session,
            plugin,
            &prompt_payload,
        );
        // Emit to session bus — TUI renders generically from `payload`.
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::InteractionRequest {
            id,
            plugin: plugin.to_string(),
            payload: prompt_payload,
        });
        // Block on condvar until `POST /ui-respond` resolves it.
        // 96E-39: get the session's Cancel so ESC can abort the wait.
        let cancel = crate::turn::get_cancel(&self.state, session)
            .unwrap_or_else(Cancel::new);
        match self.state.interaction_registry.wait(&handle, &cancel) {
            Some(response) => Ok(json!({ "payload": response })),
            None => Err("interaction cancelled".to_string()),
        }
    }

    /// `provider_complete` — one real provider call with the session's model.
    /// Reply: `{"content":[...],"stop":"...","usage":{"input":..,"output":..}}`.
    /// Optional: `"tools"` — array of tool specs to enable tool_use responses.
    ///
    /// 96E-39: uses session's Cancel so ESC can abort the provider call.
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
            .or_else(|| self.state.provider_snapshot())
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
        // 96E-39: use session's Cancel so ESC can abort the provider call
        let cancel = crate::turn::get_cancel(&self.state, session)
            .unwrap_or_else(Cancel::new);
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
                    price_snapshot: model.price,
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

    /// `ui_register_lua {source}` — register the plugin's TUI code.
    ///
    /// The plugin ships Lua defining `render(state)`, which returns a widget
    /// tree. Sent once (cheap to re-send on change); state updates go through
    /// `ui_set_state`, so the source is not re-transmitted per update.
    ///
    /// The server does not parse the Lua: it is opaque here and only meaningful
    /// to the TUI, which owns the widget vocabulary.
    ///
    /// Optional `placement` / `title` / `rows` / `cols` travel alongside the
    /// source. They are the plugin's *request*, not a command: a plugin says "I
    /// am a main-pane panel called Diff review wanting ~24 rows", and the TUI
    /// config decides whether to honour it. Without them a config could only
    /// place a view by hardcoding the plugin's name, which is the coupling this
    /// removes — see `plugin_ui.rs`'s "Who decides placement".
    fn ui_register_lua(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let source = payload
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "ui_register_lua requires \"source\"".to_string())?;

        // Guard against a runaway plugin shipping megabytes of source: this
        // crosses the wire and is held per plugin.
        const MAX_SOURCE: usize = 256 * 1024;
        if source.len() > MAX_SOURCE {
            return Err(format!(
                "ui_register_lua source too large: {} bytes (max {})",
                source.len(),
                MAX_SOURCE
            ));
        }

        // Unknown placements are rejected here rather than silently defaulting:
        // a typo that quietly becomes "sidebar" is harder to notice than an
        // error at registration.
        const PLACEMENTS: &[&str] = &["sidebar", "main", "status"];
        let placement = payload.get("placement").and_then(|v| v.as_str());
        if let Some(p) = placement {
            if !PLACEMENTS.contains(&p) {
                return Err(format!(
                    "ui_register_lua: unknown placement {p:?} (expected one of {PLACEMENTS:?})"
                ));
            }
        }

        let mut forward = json!({"source": source});
        for key in ["placement", "title", "rows", "cols"] {
            if let Some(v) = payload.get(key) {
                forward[key] = v.clone();
            }
        }

        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: "lua".to_string(),
            op: "register_lua".to_string(),
            payload: forward,
        });
        Ok(json!({"ok": true}))
    }

    /// `ui_set_state {state}` — push new state for the plugin's `render(state)`.
    ///
    /// Arbitrary JSON: the plugin's own Lua decides how to display it, so there
    /// is no fixed placeholder vocabulary to conform to.
    fn ui_set_state(
        &self,
        session: Option<&str>,
        payload: &Value,
        plugin: &str,
    ) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let state = payload
            .get("state")
            .cloned()
            .ok_or_else(|| "ui_set_state requires \"state\"".to_string())?;

        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: "lua".to_string(),
            op: "set_state".to_string(),
            payload: json!({"state": state}),
        });
        Ok(json!({"ok": true}))
    }

    /// `ui_clear` — drop the plugin's UI entirely.
    fn ui_clear(&self, session: Option<&str>, plugin: &str) -> Result<Value, String> {
        let session = self.require_session(session)?;
        let sink: Arc<dyn kn9t_core::EventSink> = Arc::new(self.sink(session));
        sink.emit(kn9t_core::LiveEvent::UiDirective {
            plugin: plugin.to_string(),
            target: "lua".to_string(),
            op: "clear".to_string(),
            payload: json!({}),
        });
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
        // Use the session's cwd, not the server's process cwd.
        let session_cwd = self.session_cwd(session);

        // 96E-33: the session and its sink are passed explicitly. This call runs on an API
        // worker thread, not the turn thread, so the old thread-local sink was always unset
        // here and every approval fell through to "no sink" -> Deny. Now the prompt actually
        // reaches the session's SSE stream.
        // 96E-39: use session's cancel so ESC can abort the approval wait and tool execution.
        let cancel = crate::turn::get_cancel(&self.state, session)
            .unwrap_or_else(Cancel::new);
        let approval_sink = self.sink(session);
        let ctx = kn9t_core::ApprovalCtx {
            session,
            sink: &approval_sink,
            cancel: &cancel,
        };
        match self.state.approver_snapshot().request(
            &call,
            &session_cwd,
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
        // 96E-39: reuse session's cancel (already fetched above)
        let ctx = ToolCtx {
            cwd: session_cwd,
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
            "session_create" => self.session_create(session, payload),
            "session_prompt" => self.session_prompt(session, payload),
            "tool_list" => self.tool_list(session, payload),
            "interaction_request" => self.interaction_request(session, payload, plugin),
            "ui_directive" | "ui_push" => self.ui_directive(session, payload, plugin),
            "ui_register_lua" => self.ui_register_lua(session, payload, plugin),
            "ui_set_state" => self.ui_set_state(session, payload, plugin),
            "ui_clear" => self.ui_clear(session, plugin),
            other => Err(format!("unknown host API op {other:?}")),
        }
    }
}
