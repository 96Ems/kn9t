//! R-PLUG-040/060/080/090 / R-PLUG2-040/050 — PluginHost: manages one plugin subprocess.
//!
//! Protocol v2: reader thread forwards Chunk, Done, and Result messages.
//! Host can send Cancel for in-flight calls on cancelable plugins.
//! Accepts `Box<dyn Read+Send>` + `Box<dyn Write+Send>` so tests wire in-process pipes.

use crate::codec::{hook_name_str, write_host_msg, HostMsg, PluginDeclaration, PluginMsg};
use crate::host_api::HostApi;

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
    Cancel, Content, Event, EventSink, HookName, HookVeto, LiveEvent, Message, ModelRef, MsgId,
    NextTurnPatch, PluginKv, Role, StopReason, Usage,
};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

thread_local! {
    static TL_SESSION: RefCell<Option<String>> = RefCell::new(None);
    static TL_BUS: RefCell<Option<Arc<dyn EventSink>>> = RefCell::new(None);
}

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
    /// 96E-5 fix: per-session bus map for async event routing; hook calls use
    /// thread-local TL_SESSION/TL_BUS for isolation under concurrency.
    session_buses: Arc<Mutex<HashMap<String, Arc<dyn EventSink>>>>,
    /// Persistent KV store — namespaced by this plugin's name in the host.
    ///
    /// Never read through `self`: KV requests are served by the reader thread,
    /// which owns its own `Arc` clone (`kv_for_reader`). This field keeps the
    /// store alive for as long as the host lives.
    #[allow(dead_code)]
    kv: Arc<dyn PluginKv>,
    /// 96E-10: protocol health — once a malformed message is seen the connection
    /// is poisoned; new calls fail fast and the reader terminates.
    unhealthy: Arc<std::sync::atomic::AtomicBool>,
    poison_reason: Arc<Mutex<Option<String>>>,
    /// 96E-17: plugin → host API handler (host_api capability). Requests are
    /// dispatched to a worker thread so a slow op never blocks the reader (96E-9).
    api_handler: Arc<Mutex<Option<Arc<dyn HostApi>>>>,
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

        // 96E-10: health flag shared with reader thread
        let unhealthy: Arc<std::sync::atomic::AtomicBool> = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poison_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let api_handler: Arc<Mutex<Option<Arc<dyn HostApi>>>> = Arc::new(Mutex::new(None));
        let unhealthy_for_reader = Arc::clone(&unhealthy);
        let poison_for_reader = Arc::clone(&poison_reason);

        // Spawn reader thread — dispatches to per-call channels by ID.
        // KV requests (KvGet/KvSet/KvDel/KvDelScope) are handled inline here;
        // they never touch the pending_calls map.
        let api_for_reader = Arc::clone(&api_handler);
        let name_for_reader = plugin_name.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let trimmed = line.trim_end();
                // Empty lines are not protocol; ignore (codec ensures one JSON per line)
                if trimmed.is_empty() {
                    continue;
                }
                let msg = match serde_json::from_str::<PluginMsg>(trimmed) {
                    Ok(m) => m,
                    Err(e) => {
                        // 96E-10 fix: malformed output is a connection-level protocol
                        // violation. Fail all pending calls, mark unhealthy, stop
                        // accepting new calls, and terminate the reader (poisoned).
                        let reason = format!("protocol violation: malformed message: {e}");
                        *poison_for_reader.lock().unwrap() = Some(reason.clone());
                        unhealthy_for_reader.store(true, Ordering::SeqCst);
                        let pending = pending_for_reader.lock().unwrap();
                        for tx in pending.values() {
                            let _ = tx.send(ReaderMsg::Err { reason: reason.clone() });
                        }
                        break;
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
                        // 96E-9 fix: transient plugin events must not block the reader.
                        // The same reader demultiplexes RPC responses; a bounded blocking
                        // send would stall unrelated hook calls when a noisy plugin floods
                        // events. Drop under pressure — transient, safe to lose.
                        let _ = event_tx.try_send(event);
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
                    // ── 96E-17: plugin → host API request ──────────────────
                    // Dispatched to a worker thread: a slow op (provider_complete)
                    // must never block the reader (96E-9). The session id travels
                    // INSIDE the payload (TLS session is turn-thread-local, 96E-5).
                    PluginMsg::Request { id, op, payload } => {
                        let handler = api_for_reader.lock().unwrap().clone();
                        let writer = Arc::clone(&writer_clone);
                        let name = name_for_reader.clone();
                        let payload = payload.clone();
                        match handler {
                            None => {
                                let reply = HostMsg::ApiResult {
                                    id,
                                    ok: false,
                                    result: None,
                                    error: Some("host API not enabled (no handler registered)".into()),
                                };
                                if let Ok(mut w) = writer.lock() {
                                    let _ = write_host_msg(&mut **w, &reply);
                                }
                            }
                            Some(h) => {
                                std::thread::spawn(move || {
                                    let session = payload
                                        .get("session")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    let outcome = h.handle(&name, session.as_deref(), &op, &payload);
                                    let reply = match outcome {
                                        Ok(result) => HostMsg::ApiResult {
                                            id,
                                            ok: true,
                                            result: Some(result),
                                            error: None,
                                        },
                                        Err(e) => HostMsg::ApiResult {
                                            id,
                                            ok: false,
                                            result: None,
                                            error: Some(e),
                                        },
                                    };
                                    if let Ok(mut w) = writer.lock() {
                                        let _ = write_host_msg(&mut **w, &reply);
                                    }
                                });
                            }
                        }
                    }
                }
            }
        });

        // 96E-21 fix: per-session event routing is explicit — a plugin event
        // reaches a session's client IFF its `session_id` matches a registered
        // bus. Missing or unknown `session_id` is DROPPED (diagnostic, no
        // broadcast). The old "broadcast to all when untagged" leaked subagent
        // events to the master (AGENTS.md leak). If a plugin genuinely needs a
        // global event, it must use a dedicated diagnostic channel, not this.
        let session_buses: Arc<Mutex<HashMap<String, Arc<dyn EventSink>>>> = Arc::new(Mutex::new(HashMap::new()));
        let session_buses_for_thread = Arc::clone(&session_buses);
        std::thread::spawn(move || {
            while let Ok(event) = event_rx.recv() {
                let sid_opt = event.get("session_id").and_then(|v| v.as_str()).map(|s| s.to_string())
                    .or_else(|| event.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string()));
                if let Some(sid) = sid_opt {
                    if let Some(bus) = session_buses_for_thread.lock().unwrap().get(&sid) {
                        bus.emit(LiveEvent::PluginNotification { payload: event });
                    }
                    // unknown sid -> drop (no broadcast)
                } else {
                    // No session_id: drop — never broadcast to all (96E-21)
                    // Could log to stderr for diagnostics, but must not fan out to every session.
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
            session_buses,
            kv,
            unhealthy,
            poison_reason,
            api_handler,
        }
    }

    /// Whether the host is still healthy (no protocol violation seen).
    pub fn is_healthy(&self) -> bool {
        !self.unhealthy.load(Ordering::SeqCst)
    }

    /// Reason for poisoning, if unhealthy.
    pub fn poison_reason(&self) -> Option<String> {
        self.poison_reason.lock().unwrap().clone()
    }

    fn check_healthy(&self) -> Result<(), String> {
        if self.unhealthy.load(Ordering::SeqCst) {
            let r = self.poison_reason.lock().unwrap().clone().unwrap_or_else(|| "protocol violation".to_string());
            return Err(format!("plugin '{}' unhealthy: {r}", self.declaration.name));
        }
        Ok(())
    }

    /// Set the event bus for forwarding plugin events.
    /// Called by the server before each turn. Uses thread-local for isolation
    /// under concurrent sessions (96E-5 fix).
    pub fn set_bus(&self, bus: Arc<dyn EventSink>) {
        TL_BUS.with(|c| *c.borrow_mut() = Some(bus.clone()));
        // If session already set on this thread, register in the per-session map
        TL_SESSION.with(|c| {
            if let Some(sid) = c.borrow().as_ref() {
                self.session_buses.lock().unwrap().insert(sid.clone(), bus);
            }
        });
    }

    /// Set the current session ID. Called by the server before each turn.
    /// All hook payloads will include this session_id. Thread-local for isolation.
    pub fn set_session(&self, session_id: &str) {
        TL_SESSION.with(|c| *c.borrow_mut() = Some(session_id.to_string()));
        // If bus already set on this thread, register in the per-session map
        TL_BUS.with(|c| {
            if let Some(bus) = c.borrow().as_ref() {
                self.session_buses.lock().unwrap().insert(session_id.to_string(), bus.clone());
            }
        });
    }

    /// Get the current session ID (if set) — thread-local.
    pub fn session_id(&self) -> Option<String> {
        TL_SESSION.with(|c| c.borrow().clone())
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
        TL_BUS.with(|c| *c.borrow_mut() = Some(bus.clone()));
        TL_SESSION.with(|c| {
            if let Some(sid) = c.borrow().as_ref() {
                self.session_buses.lock().unwrap().insert(sid.clone(), bus);
            }
        });
        self
    }

    /// Whether this plugin subscribes to a given hook.
    pub fn has_hook(&self, hook: HookName) -> bool {
        self.declaration.hooks.contains(&hook)
    }

    /// 96E-17: whether the plugin declared a given capability in its hello.
    pub fn has_capability(&self, cap: &str) -> bool {
        self.declaration.capabilities.iter().any(|c| c == cap)
    }

    /// 96E-17: install the plugin → host API handler (server-side ops).
    pub fn set_api_handler(&self, api: Arc<dyn HostApi>) {
        *self.api_handler.lock().unwrap() = Some(api);
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
        self.check_healthy()?;
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
        self.check_healthy()?;
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
        self.check_healthy()?;
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

    /// Cancellable streaming hook — polls `Cancel` every 10ms and sends `HostMsg::Cancel` on fire.
    /// `job/instant-cut.md` step 5.
    pub fn call_raw_hook_str_streaming_cancellable(
        &self,
        hook_str: &str,
        payload: Value,
        timeout: Duration,
        cancel: &Cancel,
        on_chunk: impl FnMut(serde_json::Value),
    ) -> Result<Value, String> {
        self.check_healthy()?;
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
        self.wait_for_streaming_cancellable(id, cancel, timeout, on_chunk)
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

    /// Cancellable streaming wait — polls `Cancel` every 10ms, sends `HostMsg::Cancel` on fire.
    /// `job/instant-cut.md` step 4.
    pub fn wait_for_streaming_cancellable(
        &self,
        expected_id: u64,
        cancel: &Cancel,
        timeout: Duration,
        mut on_chunk: impl FnMut(serde_json::Value),
    ) -> Result<Value, String> {
        let rx = self.register_call(expected_id);
        let deadline = std::time::Instant::now() + timeout;
        let result = loop {
            if cancel.cancelled() {
                self.cancel_call(expected_id);
                break Err("cancelled".to_string());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break Err(format!("plugin '{}' timed out (call {})", self.declaration.name, expected_id));
            }
            let poll = remaining.min(Duration::from_millis(10));
            match rx.recv_timeout(poll) {
                Ok(ReaderMsg::Final { body }) => break Ok(body),
                Ok(ReaderMsg::Chunk { body }) => on_chunk(body),
                Ok(ReaderMsg::Err { reason }) => break Err(reason),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(format!("plugin '{}' disconnected (call {})", self.declaration.name, expected_id))
                }
            }
        };
        self.unregister_call(expected_id);
        result
    }

    /// Send a cancel message for an in-flight call (R-PLUG2-050).
    /// Only sent to plugins that declared `cancelable` capability.
    pub fn cancel_call(&self, id: u64) {
        if !self.declaration.is_cancelable() { return; }
        let mut w = self.writer.lock().unwrap();
        let _ = write_host_msg(&mut **w, &HostMsg::Cancel { id });
    }

    fn emit_hook_failed(&self, hook: HookName, reason: &str) {
        TL_BUS.with(|c| {
            if let Some(bus) = c.borrow().as_ref() {
                bus.emit(LiveEvent::HookFailed {
                    plugin: self.declaration.name.clone(),
                    hook,
                    reason: reason.to_string(),
                });
            }
        });
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
        if !self.has_hook(HookName::GetSteering) {
            return Vec::new();
        }
        let payload = serde_json::json!({ "session_id": self.session_id() });
        let timeout = default_timeout(HookName::GetSteering);
        match self.call_hook_raw(HookName::GetSteering, payload, timeout) {
            Ok(body) => parse_messages_list(&body),
            Err(e) => {
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
        // ADR-0008 — a policy plugin escalates to the user with `ask`. Before that ADR the
        // hook had no way to say this and had to answer `allow`, letting the (now retired)
        // server-side policy prompt as a side effect.
        Some("ask") => {
            let reason = body
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("approval required")
                .to_string();
            HookVeto::Ask { reason }
        }
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

/// RAII guard that restores the plugin thread-locals (`TL_SESSION`/`TL_BUS`) on
/// drop. A spawned session runs its own turn synchronously on the CALLER's
/// thread (`run_session_turn`), and `compose_loop` overwrites the caller's
/// `TL_SESSION` with the child id. Without a restore, every later hook on that
/// thread (e.g. `get_steering`) mis-attributes itself to the CHILD session —
/// AGENTS.md injected into the parent's transcript (plugin leak). Wrap the
/// child turn with this guard so the parent thread-local survives it.
pub struct SessionScope {
    session: Option<String>,
    bus: Option<Arc<dyn EventSink>>,
}

impl SessionScope {
    /// Capture the current thread-local state. `Drop` restores it.
    pub fn capture() -> Self {
        let session = TL_SESSION.with(|c| c.borrow().clone());
        let bus = TL_BUS.with(|c| c.borrow().clone());
        SessionScope { session, bus }
    }
}

impl Drop for SessionScope {
    fn drop(&mut self) {
        TL_SESSION.with(|c| *c.borrow_mut() = self.session.clone());
        TL_BUS.with(|c| *c.borrow_mut() = self.bus.clone());
    }
}

#[cfg(test)]
mod session_scope_tests {
    use super::*;

    struct NoopSink;
    impl kn9t_core::EventSink for NoopSink {
        fn emit(&self, _e: kn9t_core::LiveEvent) {}
    }

    /// Regression test for the AGENTS.md plugin leak: a spawned session runs
    /// synchronously on the parent turn's thread and `compose_loop` overwrites
    /// the thread-local session id with the child. `SessionScope` must restore
    /// the parent's id (and bus) so later hooks (get_steering) on the SAME
    /// thread still attribute to the parent — not the child.
    #[test]
    fn scope_restores_parent_session_after_nested_child_turn() {
        TL_SESSION.with(|c| *c.borrow_mut() = None);
        TL_BUS.with(|c| *c.borrow_mut() = None);

        // Simulate the parent turn: compose_loop sets the parent session + bus.
        TL_SESSION.with(|c| *c.borrow_mut() = Some("parent-001".to_string()));
        TL_BUS.with(|c| *c.borrow_mut() = Some(Arc::new(NoopSink) as Arc<dyn EventSink>));

        // run_session_turn(child) begins: capture the caller's thread-local.
        let scope = SessionScope::capture();
        assert_eq!(TL_SESSION.with(|c| c.borrow().clone()).as_deref(), Some("parent-001"));
        assert!(TL_BUS.with(|c| c.borrow().is_some()));

        // Inside the child turn, compose_loop overwrites the thread-local.
        TL_SESSION.with(|c| *c.borrow_mut() = Some("child-900".to_string()));
        TL_BUS.with(|c| *c.borrow_mut() = Some(Arc::new(NoopSink) as Arc<dyn EventSink>));

        // The child turn finishes; the scope drops and must restore.
        drop(scope);

        assert_eq!(
            TL_SESSION.with(|c| c.borrow().clone()).as_deref(),
            Some("parent-001"),
            "parent session id must be restored after the nested child turn"
        );
        assert!(
            TL_BUS.with(|c| c.borrow().is_some()),
            "parent bus must be restored after the nested child turn"
        );
    }
}
