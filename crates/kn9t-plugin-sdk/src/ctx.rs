//! Context types passed to plugin handlers (spec §4.2).
//!
//! Plugin authors receive these through their trait method signatures.
//! The SDK constructs them; authors only call their methods.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::io::Write;

// ── CancelToken ───────────────────────────────────────────────────────────────

/// Delivers cancellation to an in-flight call (spec §2.1 `"cancelable"`).
///
/// The SDK sets the flag when the host sends `{"t":"cancel","id":N}`.
/// Check [`is_cancelled`](CancelToken::is_cancelled) before starting any
/// expensive operation, and again at natural checkpoints (between loop
/// iterations, before spawning subprocesses, etc.).
///
/// # Example
/// ```no_run
/// # use kn9t_plugin_sdk::ctx::CancelToken;
/// fn long_work(cancel: &CancelToken) -> String {
///     for i in 0..1000 {
///         if cancel.is_cancelled() { return "cancelled".into(); }
///         // ... do chunk of work ...
///     }
///     "done".into()
/// }
/// ```
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Create a new unfired token. Called by the SDK internally.
    pub fn new() -> Self { Self(Arc::new(AtomicBool::new(false))) }
    /// Fire cancellation. Called by the SDK cancel-listener thread.
    pub fn cancel(&self) { self.0.store(true, Ordering::Release); }
    /// Returns `true` if the host has requested cancellation of this call.
    pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Acquire) }
}

// ── ProgressSender ────────────────────────────────────────────────────────────

/// Sends progress text chunks to the host during a streaming tool call.
///
/// Each call to [`send`](ProgressSender::send) emits a
/// `{"t":"chunk","id":N,"text":"..."}` line to the host, which the TUI
/// displays as live tool output.
///
/// # Example
/// ```no_run
/// # use kn9t_plugin_sdk::ctx::ProgressSender;
/// fn run_task(progress: &ProgressSender) {
///     progress.send("starting...");
///     // ... do work ...
///     progress.send("step 1 done");
/// }
/// ```
/// A cloneable handle for sending progress from background threads.
/// A cloneable handle for sending progress from background threads.
#[derive(Clone)]
pub struct ProgressSender {
    /// Call id this sender is associated with.
    pub id: u64,
    /// Shared writer (Arc so it can be cloned across threads).
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl ProgressSender {
    /// Emit a progress text line. Silently drops on write error (the call
    /// can still complete normally; the final `done` is what matters).
    pub fn send(&self, text: impl Into<String>) {
        let body = serde_json::json!({ "text": text.into() });
        let msg = crate::wire::PluginMsg::Chunk { id: self.id, body };
        if let Ok(mut w) = self.writer.lock() {
            let _ = crate::wire::write_plugin(&mut **w, &msg);
        }
    }
}

// ── ChunkSender ───────────────────────────────────────────────────────────────

/// Streams token deltas to the host during a provider call (spec §2.4).
///
/// Methods correspond to the `kind` values in the chunk kind table.
/// Call them in the order your model produces output; the host assembles
/// them into a complete message.
///
/// # Parallel Tool Calls
///
/// When a model emits multiple tool calls in a single response (parallel tool
/// calls), the SDK automatically assigns a stable `idx` to each tool call based
/// on its `call_id`. This ensures the host can correctly group tool call
/// fragments even when they arrive interleaved or out of order.
///
/// The `idx` is assigned on the first `tool_use_start` for a given `call_id`
/// and reused for all subsequent `tool_use_delta` calls with the same `call_id`.
///
/// # Example
/// ```no_run
/// # use kn9t_plugin_sdk::ctx::ChunkSender;
/// fn stream_response(chunks: &ChunkSender) {
///     chunks.text_delta("Hello");
///     chunks.text_delta(" world");
/// }
/// ```
///
/// # Example: Parallel Tool Calls
/// ```no_run
/// # use kn9t_plugin_sdk::ctx::ChunkSender;
/// fn stream_parallel_tools(chunks: &ChunkSender) {
///     // Two tool calls emitted by the model
///     chunks.tool_use_start("call_1", "read", "");      // gets idx=0
///     chunks.tool_use_start("call_2", "bash", "");      // gets idx=1
///     chunks.tool_use_delta("call_1", r#"{"path":"#);   // uses idx=0
///     chunks.tool_use_delta("call_2", r#"{"cmd":"#);    // uses idx=1
///     chunks.tool_use_delta("call_1", r#"a.txt"}"#);    // uses idx=0
///     chunks.tool_use_delta("call_2", r#"ls"}"#);       // uses idx=1
/// }
/// ```
pub struct ChunkSender {
    pub(crate) id: u64,
    pub(crate) writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Maps call_id → stable idx for parallel tool call support.
    /// Assigned on first `tool_use_start`, reused for `tool_use_delta`.
    call_id_to_idx: Mutex<HashMap<String, u32>>,
    /// Next idx to assign for a new tool call.
    next_tool_idx: AtomicU32,
}

impl ChunkSender {
    /// Create a new ChunkSender.
    ///
    /// This is primarily used internally by the SDK. Plugin authors receive
    /// a `ChunkSender` via [`ProviderCallCtx`] and don't need to construct one.
    ///
    /// Made public for testing parallel tool call behavior.
    pub fn new(id: u64, writer: Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        Self {
            id,
            writer,
            call_id_to_idx: Mutex::new(HashMap::new()),
            next_tool_idx: AtomicU32::new(0),
        }
    }

    fn send_chunk(&self, body: serde_json::Value) {
        let msg = crate::wire::PluginMsg::Chunk { id: self.id, body };
        if let Ok(mut w) = self.writer.lock() {
            let _ = crate::wire::write_plugin(&mut **w, &msg);
        }
    }

    /// Emit an incremental text token.
    pub fn text_delta(&self, text: &str) {
        self.send_chunk(serde_json::json!({ "kind": "text_delta", "text": text }));
    }

    /// Emit an incremental thinking block.
    /// `signature` is provider-opaque — pass it through unchanged.
    pub fn thinking_delta(&self, thinking: &str, signature: &str) {
        self.send_chunk(serde_json::json!({
            "kind": "thinking_delta",
            "thinking": thinking,
            "signature": signature,
        }));
    }

    /// Signal the start of a tool call.
    ///
    /// Automatically assigns a stable `idx` to this tool call based on `call_id`.
    /// The same `idx` is reused for all subsequent `tool_use_delta` calls with
    /// the same `call_id`, enabling correct parallel tool call handling.
    ///
    /// `args_json` may be `""` initially if arguments stream in via `tool_use_delta`.
    pub fn tool_use_start(&self, call_id: &str, name: &str, args_json: &str) {
        let idx = self.get_or_assign_idx(call_id);
        self.send_chunk(serde_json::json!({
            "kind": "tool_use_start",
            "idx": idx,
            "call_id": call_id,
            "name": name,
            "args_json": args_json,
        }));
    }

    /// Append more argument JSON for a tool call.
    ///
    /// Uses the same `idx` assigned during `tool_use_start` for this `call_id`.
    pub fn tool_use_delta(&self, call_id: &str, args_json: &str) {
        let idx = self.get_or_assign_idx(call_id);
        self.send_chunk(serde_json::json!({
            "kind": "tool_use_delta",
            "idx": idx,
            "call_id": call_id,
            "args_json": args_json,
        }));
    }

    /// Get or assign a stable idx for a call_id.
    fn get_or_assign_idx(&self, call_id: &str) -> u32 {
        let mut map = self.call_id_to_idx.lock().unwrap();
        if let Some(&idx) = map.get(call_id) {
            return idx;
        }
        let idx = self.next_tool_idx.fetch_add(1, Ordering::Relaxed);
        map.insert(call_id.to_string(), idx);
        idx
    }

    /// Emit the pre-generation input token count (optional).
    pub fn input_tokens(&self, count: u64) {
        self.send_chunk(serde_json::json!({ "kind": "input_tokens", "count": count }));
    }

    /// Send a raw chunk body. The `kind` field must be set by the caller.
    /// Prefer the typed helpers above; use this only for forward-compatibility shims.
    pub fn send_raw(&self, body: serde_json::Value) {
        self.send_chunk(body);
    }
}

// ── KvClient ─────────────────────────────────────────────────────────────────

/// Synchronous client for the host's persistent plugin KV store.
///
/// Keys are scoped by `(plugin_name, scope, key)`.  The `plugin_name` is set
/// by the host — plugins cannot write into another plugin's namespace.  Use
/// `scope = ""` for global (process-lifetime) state or a `session_id` for
/// session-scoped state.
///
/// All calls are **blocking** — they send a request to the host and wait for
/// `KvResult`.  Timeout is 5 seconds; on timeout or error the call returns
/// `Err(String)`.
///
/// # Example
/// ```no_run
/// # use kn9t_plugin_sdk::ctx::{KvClient, ToolCallCtx};
/// fn my_hook(kv: &KvClient) {
///     let prev = kv.get("", "counter").unwrap_or(None);
///     let n: u64 = prev.and_then(|v| v.as_u64()).unwrap_or(0);
///     let _ = kv.set("", "counter", &serde_json::json!(n + 1));
/// }
/// ```
pub struct KvClient {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::SyncSender<KvReply>>>>,
    next_id: Arc<AtomicU64>,
}

/// Internal reply type for KV operations.
pub(crate) struct KvReply {
    pub value: Option<serde_json::Value>,
    pub ok: bool,
    pub error: Option<String>,
}

impl KvClient {
    /// Create a new KvClient.  Called by the SDK internally.
    pub(crate) fn new(
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        pending: Arc<Mutex<HashMap<u64, mpsc::SyncSender<KvReply>>>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        Self { writer, pending, next_id }
    }

    /// A `KvClient` that writes to a sink and never receives a reply, for use in
    /// plugin unit tests.
    ///
    /// [`ToolCallCtx`] and [`ProviderCallCtx`] have `pub` fields so plugin authors can
    /// build them in tests, but a real `KvClient` is only obtainable from the SDK's own
    /// dispatch loop. Without this constructor those context structs are publicly
    /// declared yet impossible to instantiate outside this crate, which breaks every
    /// external plugin's test suite.
    ///
    /// `get` returns `Err` after the 5 s timeout and `set` discards its write, so use
    /// this only for tests that do not exercise KV behaviour.
    pub fn for_test() -> Self {
        Self {
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Retrieve the value at `(scope, key)`.  Returns `None` if absent.
    pub fn get(&self, scope: &str, key: &str) -> Result<Option<serde_json::Value>, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let msg = crate::wire::PluginMsg::KvGet {
            id,
            scope: scope.to_string(),
            key: key.to_string(),
        };
        {
            let mut w = self.writer.lock().map_err(|e| format!("writer lock: {e}"))?;
            crate::wire::write_plugin(&mut **w, &msg).map_err(|e| format!("kv_get write: {e}"))?;
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => {
                self.pending.lock().unwrap().remove(&id);
                if r.ok { Ok(r.value) } else { Err(r.error.unwrap_or_default()) }
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("kv_get timeout".to_string())
            }
        }
    }

    /// Upsert `(scope, key)` → `value`.
    pub fn set(&self, scope: &str, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let msg = crate::wire::PluginMsg::KvSet {
            id,
            scope: scope.to_string(),
            key: key.to_string(),
            value: value.clone(),
        };
        {
            let mut w = self.writer.lock().map_err(|e| format!("writer lock: {e}"))?;
            crate::wire::write_plugin(&mut **w, &msg).map_err(|e| format!("kv_set write: {e}"))?;
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => {
                self.pending.lock().unwrap().remove(&id);
                if r.ok { Ok(()) } else { Err(r.error.unwrap_or_default()) }
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("kv_set timeout".to_string())
            }
        }
    }

    /// Delete `(scope, key)`.  A no-op if the key does not exist.
    pub fn del(&self, scope: &str, key: &str) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let msg = crate::wire::PluginMsg::KvDel {
            id,
            scope: scope.to_string(),
            key: key.to_string(),
        };
        {
            let mut w = self.writer.lock().map_err(|e| format!("writer lock: {e}"))?;
            crate::wire::write_plugin(&mut **w, &msg).map_err(|e| format!("kv_del write: {e}"))?;
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => {
                self.pending.lock().unwrap().remove(&id);
                if r.ok { Ok(()) } else { Err(r.error.unwrap_or_default()) }
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("kv_del timeout".to_string())
            }
        }
    }

    /// Delete all keys for `scope`.  Use this to clean up session-scoped state
    /// when a session is compacted or when you receive a `"compacted"` event.
    pub fn del_scope(&self, scope: &str) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending.lock().unwrap().insert(id, tx);
        let msg = crate::wire::PluginMsg::KvDelScope {
            id,
            scope: scope.to_string(),
        };
        {
            let mut w = self.writer.lock().map_err(|e| format!("writer lock: {e}"))?;
            crate::wire::write_plugin(&mut **w, &msg).map_err(|e| format!("kv_del_scope write: {e}"))?;
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => {
                self.pending.lock().unwrap().remove(&id);
                if r.ok { Ok(()) } else { Err(r.error.unwrap_or_default()) }
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("kv_del_scope timeout".to_string())
            }
        }
    }
}

// ── ToolCallCtx ──────────────────────────────────────────────────────────────

/// Context passed to [`PluginTool::execute`](crate::traits::PluginTool::execute).
pub struct ToolCallCtx {
    // NOTE: fields are pub so plugin authors can access them directly.
    /// Fires when the host sends `{"t":"cancel","id":N}`.
    pub cancel: CancelToken,
    /// Send progress text chunks to the host for live TUI display.
    pub progress: ProgressSender,
    /// Persistent KV store backed by the host's SQLite database.
    pub kv: KvClient,
}

// ── ProviderCallCtx ──────────────────────────────────────────────────────────

/// Context passed to [`PluginProvider::complete`](crate::traits::PluginProvider::complete).
pub struct ProviderCallCtx {
    /// Fires when the host sends `{"t":"cancel","id":N}`.
    pub cancel: CancelToken,
    /// Send token delta chunks to the host.
    pub chunk: ChunkSender,
    /// Persistent KV store backed by the host's SQLite database.
    pub kv: KvClient,
}
