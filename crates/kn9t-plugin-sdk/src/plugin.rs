//! The [`Plugin`] container and `Plugin::run()` main loop (spec §4.3).
#![allow(missing_docs)] // Internal runner types are implementation details.
//!
//! A complete plugin binary needs only:
//! ```no_run
//! use kn9t_plugin_sdk::Plugin;
//! # struct MyTool;
//! # impl kn9t_plugin_sdk::traits::PluginTool for MyTool {
//! #   fn spec(&self) -> kn9t_plugin_sdk::wire::ToolSpec { todo!() }
//! #   fn execute(&self, _: &serde_json::Value, _: &kn9t_plugin_sdk::ctx::ToolCallCtx)
//! #     -> kn9t_plugin_sdk::traits::ToolOutput { todo!() }
//! # }
//! fn main() {
//!     Plugin::new("my-plugin")
//!         .tool(MyTool)
//!         .run();
//! }
//! ```

use crate::ctx::{CancelToken, ChunkSender, KvClient, KvReply, ProgressSender, ProviderCallCtx, ToolCallCtx};
use crate::traits::{
    PluginEventSink, PluginHook, PluginProvider, PluginTool, ProviderResult,
    ToolOutput,
};
use crate::wire::{
    read_host, write_plugin, HostMsg, PluginMsg, ProviderDecl, ToolSpec,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

// ── Plugin ────────────────────────────────────────────────────────────────────

/// Container that assembles a plugin from traits and runs the main loop.
///
/// Build it with the builder methods, then call [`run`](Plugin::run).
pub struct Plugin {
    name: String,
    tools: Vec<Box<dyn PluginTool>>,
    provider: Option<Box<dyn PluginProvider>>,
    hooks: Vec<Box<dyn PluginHook>>,
    sinks: Vec<Box<dyn PluginEventSink>>,
}

impl Plugin {
    /// Create a new plugin with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Plugin { name: name.into(), tools: vec![], provider: None, hooks: vec![], sinks: vec![] }
    }

    /// Register a tool.
    pub fn tool(mut self, t: impl PluginTool + 'static) -> Self {
        self.tools.push(Box::new(t)); self
    }

    /// Register a provider. A plugin may have at most one.
    pub fn provider(mut self, p: impl PluginProvider + 'static) -> Self {
        self.provider = Some(Box::new(p)); self
    }

    /// Register a hook handler.
    pub fn hook(mut self, h: impl PluginHook + 'static) -> Self {
        self.hooks.push(Box::new(h)); self
    }

    /// Register an event sink.
    pub fn event_sink(mut self, s: impl PluginEventSink + 'static) -> Self {
        self.sinks.push(Box::new(s)); self
    }

    /// Perform the handshake, then block dispatching messages forever.
    ///
    /// Returns only on `{"t":"shutdown"}` or stdin EOF.
    /// This is the only entry point for production use.
    pub fn run(self) {
        Runner::new(self).run_loop();
    }
}

// ── Runner (internal) ─────────────────────────────────────────────────────────

/// Shared writer — all threads write through this.
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Per-call cancel token map (call id → token).
type CancelMap = Arc<Mutex<HashMap<u64, CancelToken>>>;

/// Per-KV-call reply map (kv request id → channel).
type KvPending = Arc<Mutex<HashMap<u64, mpsc::SyncSender<KvReply>>>>;
type ApiPending = Arc<Mutex<HashMap<u64, mpsc::SyncSender<crate::ctx::ApiReply>>>>;

struct Runner {
    name:       String,
    tools:      Arc<Vec<Box<dyn PluginTool>>>,
    provider:   Arc<Option<Box<dyn PluginProvider>>>,
    hooks:      Arc<Vec<Box<dyn PluginHook>>>,
    sinks:      Arc<Vec<Box<dyn PluginEventSink>>>,
    writer:     SharedWriter,
    cancels:    CancelMap,
    kv_pending: KvPending,
    kv_next_id: Arc<AtomicU64>,
    api_pending: ApiPending,
    api_next_id: Arc<AtomicU64>,
}

impl Runner {
    fn new(p: Plugin) -> Self {
        let stdout: Box<dyn Write + Send> = Box::new(io::stdout());
        Runner {
            name:       p.name,
            tools:      Arc::new(p.tools),
            provider:   Arc::new(p.provider),
            hooks:      Arc::new(p.hooks),
            sinks:      Arc::new(p.sinks),
            writer:     Arc::new(Mutex::new(stdout)),
            cancels:    Arc::new(Mutex::new(HashMap::new())),
            kv_pending: Arc::new(Mutex::new(HashMap::new())),
            // Use a high base to avoid ID collision with hook call IDs (which start at 1).
            kv_next_id: Arc::new(AtomicU64::new(1_000_000)),
            api_pending: Arc::new(Mutex::new(HashMap::new())),
            api_next_id: Arc::new(AtomicU64::new(2_000_000)),
        }
    }

    fn has_streaming(&self) -> bool {
        !self.tools.is_empty() || self.provider.is_some()
    }

    fn has_cancelable(&self) -> bool {
        !self.tools.is_empty() || self.provider.is_some()
    }

    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    fn event_filters(&self) -> Vec<String> {
        let mut out: Vec<String> = self.sinks.iter()
            .flat_map(|s| s.event_filter().into_iter().map(|s| s.to_string()))
            .collect();
        out.dedup();
        out
    }

    fn hook_names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.hooks.iter()
            .flat_map(|h| h.hooks().into_iter().map(|s| s.to_string()))
            .collect();
        out.dedup();
        out
    }

    fn capabilities(&self) -> Vec<String> {
        let mut caps = vec![];
        if self.has_streaming()  { caps.push("streaming".to_string()); }
        if self.has_cancelable() { caps.push("cancelable".to_string()); }
        caps
    }

    fn provider_decl(&self) -> Option<ProviderDecl> {
        self.provider.as_ref().as_ref().map(|p| ProviderDecl {
            id: p.id().to_string(),
            models: p.models(),
        })
    }

    fn write_msg(&self, msg: &PluginMsg) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = write_plugin(&mut **w, msg);
        }
    }

    fn make_kv_client(&self) -> KvClient {
        KvClient::new(
            Arc::clone(&self.writer),
            Arc::clone(&self.kv_pending),
            Arc::clone(&self.kv_next_id),
        )
    }

    fn make_host_client(&self, session: Option<String>) -> crate::ctx::HostApiClient {
        crate::ctx::HostApiClient::new(
            Arc::clone(&self.writer),
            Arc::clone(&self.api_pending),
            Arc::clone(&self.api_next_id),
            session,
        )
    }

    // ── main loop ─────────────────────────────────────────────────────────────

    fn run_loop(self) {
        let mut stdin = BufReader::new(io::stdin());

        // 1. Read host hello
        let host_hello = match read_host(&mut stdin) {
            Ok(HostMsg::Hello { proto, .. }) if proto == 1 => {}
            Ok(HostMsg::Hello { proto, .. }) => {
                eprintln!("[kn9t-plugin-sdk] unsupported proto {proto}");
                return;
            }
            _ => { eprintln!("[kn9t-plugin-sdk] expected hello"); return; }
        };
        let _ = host_hello; // consumed above via pattern

        // 2. Send plugin hello
        self.write_msg(&PluginMsg::Hello {
            name:         self.name.clone(),
            capabilities: self.capabilities(),
            hooks:        self.hook_names(),
            tools:        self.tool_specs(),
            provider:     self.provider_decl(),
            events:       self.event_filters(),
        });

        // 3. Wrap self in Arc for sharing across dispatch threads
        let runner = Arc::new(self);

        // 4. Main dispatch loop
        loop {
            let msg = match read_host(&mut stdin) {
                Ok(m) => m,
                Err(_) => break,
            };

            match msg {
                HostMsg::Shutdown => break,

                HostMsg::Cancel { id } => {
                    // Deliver cancellation to the matching in-flight call.
                    if let Some(tok) = runner.cancels.lock().unwrap().get(&id) {
                        tok.cancel();
                    }
                }

                HostMsg::Event { payload } => {
                    let kind = payload.get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let sinks = Arc::clone(&runner.sinks);
                    let payload_clone = payload.clone();
                    std::thread::spawn(move || {
                        for sink in sinks.iter() {
                            let filters = sink.event_filter();
                            if filters.iter().any(|f| *f == "*" || *f == kind.as_str()) {
                                sink.on_event(&kind, &payload_clone);
                            }
                        }
                    });
                }

                // KvResult: route reply to the waiting KvClient channel.
                HostMsg::KvResult { id, value, ok, error } => {
                    let reply = KvReply { value, ok, error };
                    if let Some(tx) = runner.kv_pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(reply);
                    }
                }

                HostMsg::ApiResult { id, result, ok, error } => {
                    let reply = crate::ctx::ApiReply { ok, result, error };
                    if let Some(tx) = runner.api_pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(reply);
                    }
                }

                HostMsg::Hook { id, hook, payload } => {
                    let r = Arc::clone(&runner);
                    std::thread::spawn(move || {
                        r.dispatch_hook(id, hook, payload);
                    });
                }

                HostMsg::Hello { .. } => {} // ignore duplicate hellos
            }
        }
    }

    // ── hook/tool/provider dispatch ───────────────────────────────────────────

    fn dispatch_hook(self: &Arc<Self>, id: u64, hook: String, payload: Value) {
        match hook.as_str() {
            "tool_call" => self.dispatch_tool(id, payload),
            "provider_complete" => self.dispatch_provider(id, payload),
            _ => self.dispatch_hook_handler(id, &hook, payload),
        }
    }

    fn dispatch_tool(self: &Arc<Self>, id: u64, payload: Value) {
        // Plugin protocol: {"tool": "<name>", "args": {...}, "session_id": "..."}
        let name = payload.get("tool").and_then(|n| n.as_str()).unwrap_or("");
        let args = payload.get("args").cloned().unwrap_or(Value::Null);
        let session = payload.get("session_id").or_else(|| payload.get("session")).and_then(|v| v.as_str()).map(|s| s.to_string());

        let tool = self.tools.iter().find(|t| t.spec().name == name);
        let tool = match tool {
            Some(t) => t,
            None => {
                let body = serde_json::json!({
                    "content": [{"type":"text","text":format!("unknown tool: {name}")}],
                    "is_error": true,
                });
                self.write_msg(&PluginMsg::Done { id, body });
                return;
            }
        };

        let cancel = CancelToken::new();
        self.cancels.lock().unwrap().insert(id, cancel.clone());

        let writer = Arc::clone(&self.writer);
        let ctx = ToolCallCtx {
            cancel: cancel.clone(),
            progress: ProgressSender { id, writer },
            kv: self.make_kv_client(),
            host: self.make_host_client(session),
        };

        let output = tool.execute(&args, &ctx);
        self.cancels.lock().unwrap().remove(&id);
        self.send_tool_done(id, output);
    }

    fn send_tool_done(&self, id: u64, output: ToolOutput) {
        let content: Vec<Value> = output.content.iter()
            .map(|b| serde_json::to_value(b).unwrap_or(Value::Null))
            .collect();
        let body = serde_json::json!({
            "content": content,
            "is_error": output.is_error,
        });
        self.write_msg(&PluginMsg::Done { id, body });
    }

    fn dispatch_provider(self: &Arc<Self>, id: u64, payload: Value) {
        let provider = match self.provider.as_ref() {
            Some(p) => p,
            None => {
                let body = serde_json::json!({
                    "stop": "end_turn",
                    "usage": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 },
                });
                self.write_msg(&PluginMsg::Done { id, body });
                return;
            }
        };

        let cancel = CancelToken::new();
        self.cancels.lock().unwrap().insert(id, cancel.clone());

        let ctx = ProviderCallCtx {
            cancel: cancel.clone(),
            chunk: ChunkSender::new(id, Arc::clone(&self.writer)),
            kv: self.make_kv_client(),
            host: self.make_host_client(None),
        };

        let result = provider.complete(&payload, &ctx);
        self.cancels.lock().unwrap().remove(&id);
        self.send_provider_done(id, result);
    }

    fn send_provider_done(&self, id: u64, r: ProviderResult) {
        let mut body = serde_json::json!({
            "stop": r.stop,
            "usage": {
                "input":       r.usage.input,
                "output":      r.usage.output,
                "cache_read":  r.usage.cache_read,
                "cache_write": r.usage.cache_write,
            },
        });
        if let Some(cost) = r.cost_usd {
            body["cost_usd"] = cost.into();
        }
        if let Some(err) = r.error {
            body["error"] = err.into();
        }
        self.write_msg(&PluginMsg::Done { id, body });
    }

    fn dispatch_hook_handler(&self, id: u64, hook: &str, payload: Value) {
        // Find first hook handler that declares this hook name.
        for h in self.hooks.iter() {
            if h.hooks().iter().any(|n| *n == hook) {
                let reply_body = h.call(hook, &payload);
                self.write_msg(&PluginMsg::Result { id, body: reply_body });
                return;
            }
        }
        // No handler — return a default pass-through reply.
        let default = default_hook_reply(hook);
        self.write_msg(&PluginMsg::Result { id, body: default });
    }
}

// ── default hook replies (pass-through / no-op) ───────────────────────────────

fn default_hook_reply(hook: &str) -> Value {
    match hook {
        "before_tool_call"       => serde_json::json!({ "action": "allow" }),
        "after_tool_call"        => serde_json::json!({ "action": "keep" }),
        "before_request"         => serde_json::json!({ "action": "keep" }),
        "should_stop_after_turn" => serde_json::json!({ "action": "continue" }),
        "prepare_next_turn"      => serde_json::json!({ "action": "keep" }),
        "get_steering"           => serde_json::json!({ "messages": [] }),
        "get_followup"           => serde_json::json!({ "messages": [] }),
        "get_api_key"            => serde_json::json!({ "key": null }),
        _                        => serde_json::json!({}),
    }
}
