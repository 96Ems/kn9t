//! R-PLUG-040/060/080/090 / R-PLUG2-040/050 — PluginHost: manages one plugin subprocess.
//!
//! Protocol v2: reader thread forwards Chunk, Done, and Result messages.
//! Host can send Cancel for in-flight calls on cancelable plugins.
//! Accepts `Box<dyn Read+Send>` + `Box<dyn Write+Send>` so tests wire in-process pipes.

use crate::codec::{hook_name_str, write_host_msg, HostMsg, PluginDeclaration, PluginMsg};

/// Internal channel message — what the reader thread delivers per-call.
#[derive(Debug)]
enum ReaderMsg {
    /// A complete (atomic or final streaming) body for this call id.
    Final { body: serde_json::Value },
    /// A streaming chunk for this call id.
    Chunk { body: serde_json::Value },
    /// Parse or I/O error.
    Err { reason: String },
}

use kn9t_core::{
    Content, Event, EventSink, HookName, HookVeto, Message, ModelRef, MsgId, NextTurnPatch,
    PluginKv, Role, StopReason, Usage,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// Per-call channel registration. Shared between main thread and reader thread.
type PendingCalls = Arc<Mutex<HashMap<u64, mpsc::SyncSender<ReaderMsg>>>>;

// ── per-hook default timeouts (ms) ────────────────────────────────────────────

pub fn default_timeout(hook: HookName) -> Duration {
    let ms = match hook {
        HookName::BeforeToolCall => 30_000,
        HookName::AfterToolCall => 2_000,
        HookName::BeforeRequest => 2_000,
        HookName::ShouldStopAfterTurn => 1_000,
        HookName::PrepareNextTurn => 1_000,
        HookName::GetSteering => 500,
        HookName::GetFollowup => 500,
        HookName::GetApiKey => 5_000,
    };
    Duration::from_millis(ms)
}

// ── PluginHost ─────────────────────────────────────────────────────────────────

/// Tracks consecutive on_event failures for unsubscribe logic (R-PLUG-090).
struct EventState {
    consecutive_failures: u32,
    unsubscribed: bool,
}

/// Shared bus reference for event forwarding thread.
type SharedBus = Arc<Mutex<Option<Arc<dyn EventSink>>>>;

/// Shared writer — used by hook callers, KV reader thread, and RemoteProvider.
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// One connected plugin. Thread-safe: the writer is behind an Arc<Mutex>.
/// The reader runs in a background thread and dispatches responses to per-call channels.
///
/// **Concurrency model:** Each call gets its own channel. The reader thread demuxes by
/// call ID and forwards to the appropriate channel. This allows multiple concurrent calls
/// from different sessions without blocking each other.
pub struct PluginHost {
    pub declaration: PluginDeclaration,
    /// Shared writer — also used by `RemoteProvider` and the KV reader thread.
    pub(crate) writer: SharedWriter,
    /// Per-call response channels. Reader thread dispatches here; callers wait on their own channel.
    pending_calls: PendingCalls,
    /// Monotonic call-id counter — also used by `RemoteProvider`.
    pub(crate) next_id: AtomicU64,
    event_state: Mutex<EventState>,
    /// Bus sink for emitting HookFailed and plugin events. Shared with event forwarder thread.
    bus: SharedBus,
    /// Current session ID — set before each turn, included in hook payloads.
    current_session: Mutex<Option<String>>,
    /// Persistent KV store — namespaced by this plugin's name in the host.
    kv: Arc<dyn PluginKv>,
}

impl PluginHost {
    /// Construct directly from I/O streams (skips handshake). Used by tests.
    pub fn from_io(
        read: Box<dyn Read + Send>,
        write: Box<dyn Write + Send>,
        declaration: PluginDeclaration,
        kv: Arc<dyn PluginKv>,
    ) -> Self {
        let pending_calls: PendingCalls = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = Arc::clone(&pending_calls);
        // Channel for events only (fire-and-forget, not keyed by call ID).
        let (event_tx, event_rx) = mpsc::sync_channel::<serde_json::Value>(64);

        // Shared writer for KV replies from the reader thread.
        let writer_for_kv: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(write));
        let writer_clone = Arc::clone(&writer_for_kv);

        let plugin_name = declaration.name.clone();
        let kv_for_reader = Arc::clone(&kv);

        // Spawn reader thread — dispatches to per-call channels by ID.
        // KV requests (KvGet/KvSet/KvDel/KvDelScope) are handled inline here;
        // they never touch the pending_calls map.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let trimmed = line.trim_end();
                let msg = match serde_json::from_str::<PluginMsg>(trimmed) {
                    Ok(m) => m,
                    Err(e) => {
                        // Broadcast parse error to all pending calls.
                        let pending = pending_for_reader.lock().unwrap();
                        for tx in pending.values() {
                            let _ = tx.send(ReaderMsg::Err { reason: format!("parse: {e}") });
                        }
                        continue;
                    }
                };
                match msg {
                    PluginMsg::Result { id, body } | PluginMsg::Done { id, body } => {
                        let pending = pending_for_reader.lock().unwrap();
                        if let Some(tx) = pending.get(&id) {
                            let _ = tx.send(ReaderMsg::Final { body });
                        }
                    }
                    PluginMsg::Chunk { id, body } => {
                        let pending = pending_for_reader.lock().unwrap();
                        if let Some(tx) = pending.get(&id) {
                            let _ = tx.send(ReaderMsg::Chunk { body });
                        }
                    }
                    PluginMsg::Event { event } => {
                        let _ = event_tx.send(event);
                    }
                    PluginMsg::Hello { .. } => continue, // ignore late hellos
                    // ── KV requests: handle inline, reply immediately ──────────
                    PluginMsg::KvGet { id, scope, key } => {
                        let reply = match kv_for_reader.kv_get(&plugin_name, &scope, &key) {
                            Ok(val) => HostMsg::KvResult { id, value: val, ok: true, error: None },
                            Err(e) => HostMsg::KvResult { id, value: None, ok: false, error: Some(e.0) },
                        };
                        if let Ok(mut w) = writer_clone.lock() {
                            let _ = write_host_msg(&mut **w, &reply);
                        }
                    }
                    PluginMsg::KvSet { id, scope, key, value } => {
                        let reply = match kv_for_reader.kv_set(&plugin_name, &scope, &key, &value) {
                            Ok(()) => HostMsg::KvResult { id, value: None, ok: true, error: None },
                            Err(e) => HostMsg::KvResult { id, value: None, ok: false, error: Some(e.0) },
                        };
                        if let Ok(mut w) = writer_clone.lock() {
                            let _ = write_host_msg(&mut **w, &reply);
                        }
                    }
                    PluginMsg::KvDel { id, scope, key } => {
                        let reply = match kv_for_reader.kv_del(&plugin_name, &scope, &key) {
                            Ok(()) => HostMsg::KvResult { id, value: None, ok: true, error: None },
                            Err(e) => HostMsg::KvResult { id, value: None, ok: false, error: Some(e.0) },
                        };
                        if let Ok(mut w) = writer_clone.lock() {
                            let _ = write_host_msg(&mut **w, &reply);
                        }
                    }
                    PluginMsg::KvDelScope { id, scope } => {
                        let reply = match kv_for_reader.kv_del_scope(&plugin_name, &scope) {
                            Ok(()) => HostMsg::KvResult { id, value: None, ok: true, error: None },
                            Err(e) => HostMsg::KvResult { id, value: None, ok: false, error: Some(e.0) },
                        };
                        if let Ok(mut w) = writer_clone.lock() {
                            let _ = write_host_msg(&mut **w, &reply);
                        }
                    }
                }
            }
        });

        // Spawn event forwarder thread — forwards plugin events to the bus.
        // This runs separately so events don't block on any specific call.
        let bus_for_events: SharedBus = Arc::new(Mutex::new(None));
        let bus_for_thread = Arc::clone(&bus_for_events);
        std::thread::spawn(move || {
            while let Ok(event) = event_rx.recv() {
                if let Some(bus) = bus_for_thread.lock().unwrap().as_ref() {
                    bus.emit(Event::PluginNotification { payload: event });
                }
            }
        });

        PluginHost {
            declaration,
            writer: writer_for_kv,
            pending_calls,
            next_id: AtomicU64::new(1),
            event_state: Mutex::new(EventState {
                consecutive_failures: 0,
                unsubscribed: false,
            }),
            bus: bus_for_events,
            current_session: Mutex::new(None),
            kv,
        }
    }

    /// Set the event bus for forwarding plugin events.
    /// Called by the server before each turn.
    pub fn set_bus(&self, bus: Arc<dyn EventSink>) {
        *self.bus.lock().unwrap() = Some(bus);
    }

    /// Set the current session ID. Called by the server before each turn.
    /// All hook payloads will include this session_id.
    pub fn set_session(&self, session_id: &str) {
        *self.current_session.lock().unwrap() = Some(session_id.to_string());
    }

    /// Get the current session ID (if set).
    fn session_id(&self) -> Option<String> {
        self.current_session.lock().unwrap().clone()
    }

    /// Spawn a plugin subprocess at `binary`, perform the hello/hello handshake,
    /// and return a live `PluginHost`. This is the production entry point (R-PLUG-040).
    ///
    /// `env_vars` are added to the child's environment (use for api keys, endpoints).
    /// `kv` is the persistent KV store; pass `Arc::new(NoOpPluginKv)` for tests.
    pub fn spawn(
        binary: impl Into<PathBuf>,
        env_vars: &[(&str, &str)],
        kv: Arc<dyn PluginKv>,
    ) -> Result<Self, String> {
        let binary = binary.into();
        let mut cmd = Command::new(&binary);
        cmd.stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::inherit()); // plugin stderr → host terminal for debug
        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()
            .map_err(|e| format!("spawn '{}': {e}", binary.display()))?;

        let stdin  = child.stdin.take()
            .ok_or("plugin stdin not captured")?;
        let stdout = child.stdout.take()
            .ok_or("plugin stdout not captured")?;

        // Perform handshake over the real pipes.
        let mut reader = BufReader::new(stdout);
        let mut writer: Box<dyn Write + Send> = Box::new(stdin);

        // 1. Send host hello.
        write_host_msg(&mut *writer, &HostMsg::Hello {
            proto: 1,
            kn9t: env!("CARGO_PKG_VERSION").to_string(),
        }).map_err(|e| format!("hello write: {e}"))?;

        // 2. Read plugin hello (blocking; no timeout here — use OS pipe buffering).
        let mut line = String::new();
        reader.read_line(&mut line)
            .map_err(|e| format!("hello read: {e}"))?;
        let plugin_hello: PluginMsg = serde_json::from_str(line.trim_end())
            .map_err(|e| format!("hello parse: {e}"))?;

        // 3. Extract declaration from hello.
        let declaration = match plugin_hello {
            PluginMsg::Hello { name, capabilities, hooks, tools, provider, events } => {
                use crate::codec::parse_hook_name;
                PluginDeclaration {
                    name,
                    capabilities,
                    hooks: hooks.iter().filter_map(|h| parse_hook_name(h)).collect(),
                    tools,
                    subscribed_events: events,
                    provider,
                }
            }
            _ => return Err("expected hello from plugin".to_string()),
        };

        // 4. Detach the child so it runs independently (we hold the pipes).
        // We intentionally do NOT call child.wait() here — the child lives until
        // we send Shutdown or drop the writer. Store the child handle so we can
        // reap it on Drop (avoids zombie processes).
        std::thread::spawn(move || { let _ = child.wait(); });

        // 5. Build PluginHost from the handshaked I/O.
        let read: Box<dyn Read + Send> = Box::new(reader.into_inner());
        Ok(Self::from_io(read, writer, declaration, kv))
    }

    /// Attach a bus sink for HookFailed events (builder pattern).
    pub fn with_bus(self, bus: Arc<dyn EventSink>) -> Self {
        *self.bus.lock().unwrap() = Some(bus);
        self
    }

    /// Whether this plugin subscribes to a given hook.
    pub fn has_hook(&self, hook: HookName) -> bool {
        self.declaration.hooks.contains(&hook)
    }

    /// Whether this plugin subscribes to a given event kind string.
    pub fn has_event(&self, kind: &str) -> bool {
        self.declaration.subscribed_events.iter().any(|e| e == kind)
    }

    /// Whether on_event has been unsubscribed due to repeated failures.
    pub fn is_event_unsubscribed(&self) -> bool {
        self.event_state.lock().unwrap().unsubscribed
    }

    // ── internal: send a hook request and read the response ──────────────────

    pub(crate) fn call_hook_raw(
        &self,
        hook: HookName,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = HostMsg::Hook {
            id,
            hook: hook_name_str(hook).to_string(),
            payload,
        };

        // Send request
        {
            let mut w = self.writer.lock().unwrap();
            write_host_msg(&mut **w, &msg).map_err(|e| format!("write error: {e}"))?;
        }

        // Wait for response with timeout
        self.wait_for_response(id, timeout)
    }

    /// Send a raw hook message with a custom hook name string (for tool_call).
    pub(crate) fn call_raw_hook_str(
        &self,
        hook_str: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = HostMsg::Hook {
            id,
            hook: hook_str.to_string(),
            payload,
        };

        {
            let mut w = self.writer.lock().unwrap();
            write_host_msg(&mut **w, &msg).map_err(|e| format!("write error: {e}"))?;
        }

        self.wait_for_response(id, timeout)
    }

    /// Send a raw hook message with streaming support (for tool_call with progress).
    /// Calls `on_chunk` for each intermediate chunk; returns the final body.
    pub fn call_raw_hook_str_streaming(
        &self,
        hook_str: &str,
        payload: Value,
        timeout: Duration,
        on_chunk: impl FnMut(serde_json::Value),
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = HostMsg::Hook {
            id,
            hook: hook_str.to_string(),
            payload,
        };

        {
            let mut w = self.writer.lock().unwrap();
            write_host_msg(&mut **w, &msg).map_err(|e| format!("write error: {e}"))?;
        }

        self.wait_for_streaming(id, timeout, on_chunk)
    }

    /// Register a per-call channel and return the receiver.
    /// Caller is responsible for unregistering after use.
    fn register_call(&self, id: u64) -> mpsc::Receiver<ReaderMsg> {
        let (tx, rx) = mpsc::sync_channel::<ReaderMsg>(32);
        self.pending_calls.lock().unwrap().insert(id, tx);
        rx
    }

    /// Unregister a per-call channel (cleanup after call completes or times out).
    fn unregister_call(&self, id: u64) {
        self.pending_calls.lock().unwrap().remove(&id);
    }

    /// Wait for the final body of call `expected_id`, discarding chunks (atomic path).
    fn wait_for_response(&self, expected_id: u64, timeout: Duration) -> Result<Value, String> {
        let rx = self.register_call(expected_id);
        let result = self.wait_on_channel(&rx, expected_id, timeout, |_| {});
        self.unregister_call(expected_id);
        result
    }

    /// Stream all chunks then the final body for `expected_id`.
    /// Calls `on_chunk` for each intermediate chunk; returns the final body.
    pub fn wait_for_streaming(
        &self,
        expected_id: u64,
        timeout: Duration,
        on_chunk: impl FnMut(serde_json::Value),
    ) -> Result<Value, String> {
        let rx = self.register_call(expected_id);
        let result = self.wait_on_channel(&rx, expected_id, timeout, on_chunk);
        self.unregister_call(expected_id);
        result
    }

    /// Common wait logic for both atomic and streaming paths.
    fn wait_on_channel(
        &self,
        rx: &mpsc::Receiver<ReaderMsg>,
        call_id: u64,
        timeout: Duration,
        mut on_chunk: impl FnMut(serde_json::Value),
    ) -> Result<Value, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!("plugin '{}' timed out (call {})", self.declaration.name, call_id));
            }
            match rx.recv_timeout(remaining) {
                Ok(ReaderMsg::Final { body }) => return Ok(body),
                Ok(ReaderMsg::Chunk { body }) => on_chunk(body),
                Ok(ReaderMsg::Err { reason }) => return Err(reason),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("plugin '{}' timed out (call {})", self.declaration.name, call_id));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("plugin '{}' disconnected (call {})", self.declaration.name, call_id));
                }
            }
        }
    }

    /// Send a cancel message for an in-flight call (R-PLUG2-050).
    /// Only sent to plugins that declared `cancelable` capability.
    pub fn cancel_call(&self, id: u64) {
        if !self.declaration.is_cancelable() { return; }
        let mut w = self.writer.lock().unwrap();
        let _ = write_host_msg(&mut **w, &HostMsg::Cancel { id });
    }

    fn emit_hook_failed(&self, hook: HookName, reason: &str) {
        if let Some(bus) = self.bus.lock().unwrap().as_ref() {
            bus.emit(Event::HookFailed {
                plugin: self.declaration.name.clone(),
                hook,
                reason: reason.to_string(),
            });
        }
    }

    // ── public hook methods ───────────────────────────────────────────────────

    pub fn before_tool_call(&self, tool: &str, args: &Value, _cwd: &Path) -> HookVeto {
        if !self.has_hook(HookName::BeforeToolCall) {
            return HookVeto::Allow;
        }
        let payload = serde_json::json!({ "session_id": self.session_id(), "tool": tool, "args": args });
        let timeout = default_timeout(HookName::BeforeToolCall);
        match self.call_hook_raw(HookName::BeforeToolCall, payload, timeout) {
            Ok(body) => parse_veto(&body),
            Err(e) => {
                self.emit_hook_failed(HookName::BeforeToolCall, &e);
                HookVeto::Deny { reason: e }
            }
        }
    }

    pub fn after_tool_call(
        &self,
        tool: &str,
        args: &Value,
        result: Vec<Content>,
    ) -> Vec<Content> {
        if !self.has_hook(HookName::AfterToolCall) {
            return result;
        }
        let result_val = serde_json::to_value(&result).unwrap_or(Value::Null);
        let payload = serde_json::json!({
            "session_id": self.session_id(),
            "tool": tool,
            "args": args,
            "result": result_val
        });
        let timeout = default_timeout(HookName::AfterToolCall);
        match self.call_hook_raw(HookName::AfterToolCall, payload, timeout) {
            Ok(body) => parse_content_replace(&body, result),
            Err(e) => {
                self.emit_hook_failed(HookName::AfterToolCall, &e);
                result
            }
        }
    }

    pub fn before_request(
        &self,
        msgs: Vec<Message>,
        model: &ModelRef,
        system: Option<&str>,
    ) -> Vec<Message> {
        if !self.has_hook(HookName::BeforeRequest) {
            return msgs;
        }
        let msgs_val = serde_json::to_value(&msgs).unwrap_or(Value::Null);
        let payload = serde_json::json!({
            "session_id": self.session_id(),
            "messages": msgs_val,
            "model": model,
            "system": system,
        });
        let timeout = default_timeout(HookName::BeforeRequest);
        match self.call_hook_raw(HookName::BeforeRequest, payload, timeout) {
            Ok(body) => parse_messages_replace(&body, msgs),
            Err(e) => {
                self.emit_hook_failed(HookName::BeforeRequest, &e);
                msgs
            }
        }
    }

    pub fn should_stop_after_turn(&self, stop: StopReason, usage: &Usage, turn: u32) -> bool {
        if !self.has_hook(HookName::ShouldStopAfterTurn) {
            return false;
        }
        let payload = serde_json::json!({
            "session_id": self.session_id(),
            "stop": stop,
            "usage": usage,
            "turn": turn,
        });
        let timeout = default_timeout(HookName::ShouldStopAfterTurn);
        match self.call_hook_raw(HookName::ShouldStopAfterTurn, payload, timeout) {
            Ok(body) => body
                .get("action")
                .and_then(|a| a.as_str())
                .map(|a| a == "stop")
                .unwrap_or(false),
            Err(e) => {
                self.emit_hook_failed(HookName::ShouldStopAfterTurn, &e);
                false
            }
        }
    }

    pub fn prepare_next_turn(&self, stop: StopReason, usage: &Usage) -> NextTurnPatch {
        if !self.has_hook(HookName::PrepareNextTurn) {
            return NextTurnPatch::default();
        }
        let payload = serde_json::json!({ "session_id": self.session_id(), "stop": stop, "usage": usage });
        let timeout = default_timeout(HookName::PrepareNextTurn);
        match self.call_hook_raw(HookName::PrepareNextTurn, payload, timeout) {
            Ok(body) => parse_next_turn_patch(&body),
            Err(e) => {
                self.emit_hook_failed(HookName::PrepareNextTurn, &e);
                NextTurnPatch::default()
            }
        }
    }

    pub fn get_steering(&self) -> Vec<Message> {
        eprintln!("[DEBUG PluginHost::get_steering] plugin='{}' has_hook={}", 
            self.declaration.name, self.has_hook(HookName::GetSteering));
        if !self.has_hook(HookName::GetSteering) {
            return Vec::new();
        }
        let payload = serde_json::json!({ "session_id": self.session_id() });
        let timeout = default_timeout(HookName::GetSteering);
        eprintln!("[DEBUG PluginHost::get_steering] calling call_hook_raw with timeout={:?}", timeout);
        match self.call_hook_raw(HookName::GetSteering, payload, timeout) {
            Ok(body) => {
                eprintln!("[DEBUG PluginHost::get_steering] got body: {}", 
                    serde_json::to_string(&body).unwrap_or_default());
                let msgs = parse_messages_list(&body);
                eprintln!("[DEBUG PluginHost::get_steering] parsed {} messages", msgs.len());
                msgs
            }
            Err(e) => {
                eprintln!("[DEBUG PluginHost::get_steering] ERROR: {}", e);
                self.emit_hook_failed(HookName::GetSteering, &e);
                Vec::new()
            }
        }
    }

    pub fn get_followup(&self) -> Vec<Message> {
        if !self.has_hook(HookName::GetFollowup) {
            return Vec::new();
        }
        let payload = serde_json::json!({ "session_id": self.session_id() });
        let timeout = default_timeout(HookName::GetFollowup);
        match self.call_hook_raw(HookName::GetFollowup, payload, timeout) {
            Ok(body) => parse_messages_list(&body),
            Err(e) => {
                self.emit_hook_failed(HookName::GetFollowup, &e);
                Vec::new()
            }
        }
    }

    pub fn get_api_key(&self, provider: &str) -> Option<String> {
        if !self.has_hook(HookName::GetApiKey) {
            return None;
        }
        let payload = serde_json::json!({ "session_id": self.session_id(), "provider": provider });
        let timeout = default_timeout(HookName::GetApiKey);
        match self.call_hook_raw(HookName::GetApiKey, payload, timeout) {
            Ok(body) => body
                .get("key")
                .and_then(|k| k.as_str())
                .map(|s| s.to_string()),
            Err(e) => {
                self.emit_hook_failed(HookName::GetApiKey, &e);
                None
            }
        }
    }

    /// Fire-and-forget event notification (R-PLUG-060 on_event).
    /// Returns false if unsubscribed (3 consecutive failures).
    pub fn send_event(&self, event: &Event) -> bool {
        {
            let state = self.event_state.lock().unwrap();
            if state.unsubscribed {
                return false;
            }
        }

        let payload = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(_) => {
                self.record_event_failure();
                return true;
            }
        };

        let msg = HostMsg::Event { payload };
        let mut w = self.writer.lock().unwrap();
        match write_host_msg(&mut **w, &msg) {
            Ok(_) => {
                drop(w);
                let mut state = self.event_state.lock().unwrap();
                state.consecutive_failures = 0;
                true
            }
            Err(_) => {
                drop(w);
                self.record_event_failure();
                true
            }
        }
    }

    fn record_event_failure(&self) {
        let mut state = self.event_state.lock().unwrap();
        state.consecutive_failures += 1;
        if state.consecutive_failures >= 3 {
            state.unsubscribed = true;
        }
    }

    /// Send shutdown message.
    pub fn shutdown(&self) {
        let mut w = self.writer.lock().unwrap();
        let _ = write_host_msg(&mut **w, &HostMsg::Shutdown);
    }

    /// Number of in-flight calls (pending responses) for this plugin.
    pub fn pending_count(&self) -> usize {
        self.pending_calls.lock().unwrap().len()
    }

    /// Snapshot of in-flight call ids, for cancel during reload (R-PLUG2-100 step 1).
    pub fn pending_ids(&self) -> Vec<u64> {
        self.pending_calls.lock().unwrap().keys().cloned().collect()
    }
}

// ── response parsers ──────────────────────────────────────────────────────────

fn parse_veto(body: &Value) -> HookVeto {
    match body.get("action").and_then(|a| a.as_str()) {
        Some("allow") => HookVeto::Allow,
        Some("deny") => {
            let reason = body
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("denied")
                .to_string();
            HookVeto::Deny { reason }
        }
        Some("replace") => {
            let args = body.get("args").cloned().unwrap_or(Value::Null);
            HookVeto::Replace { args }
        }
        _ => HookVeto::Allow,
    }
}

fn parse_content_replace(body: &Value, original: Vec<Content>) -> Vec<Content> {
    match body.get("action").and_then(|a| a.as_str()) {
        Some("replace") => {
            if let Some(content_val) = body.get("content") {
                serde_json::from_value(content_val.clone()).unwrap_or(original)
            } else {
                original
            }
        }
        _ => original,
    }
}

fn parse_messages_replace(body: &Value, original: Vec<Message>) -> Vec<Message> {
    match body.get("action").and_then(|a| a.as_str()) {
        Some("replace") => {
            if let Some(msgs_val) = body.get("messages") {
                serde_json::from_value(msgs_val.clone()).unwrap_or(original)
            } else {
                original
            }
        }
        _ => original,
    }
}

/// Parse messages from hook response, generating MsgId for messages without one.
/// Plugins are not required to provide `id` — we generate it server-side.
fn parse_messages_list(body: &Value) -> Vec<Message> {
    let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    
    msgs.iter().filter_map(|v| {
        // Try direct parse first (plugin provided id)
        if let Ok(msg) = serde_json::from_value::<Message>(v.clone()) {
            return Some(msg);
        }
        
        // Parse without id, then add one
        let role: Role = serde_json::from_value(v.get("role")?.clone()).ok()?;
        let content: Vec<Content> = serde_json::from_value(v.get("content")?.clone()).ok()?;
        let silent = v.get("silent").and_then(|s| s.as_bool()).unwrap_or(false);
        
        Some(Message {
            id: MsgId::new(),
            role,
            content,
            silent,
        })
    }).collect()
}

fn parse_next_turn_patch(body: &Value) -> NextTurnPatch {
    match body.get("action").and_then(|a| a.as_str()) {
        Some("patch") => {
            let model = body
                .get("model")
                .and_then(|m| serde_json::from_value(m.clone()).ok());
            let thinking = body
                .get("thinking")
                .and_then(|t| serde_json::from_value(t.clone()).ok());
            NextTurnPatch { model, thinking }
        }
        _ => NextTurnPatch::default(),
    }
}
