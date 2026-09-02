//! R-CORE-250 .. R-CORE-270 -- Store, Tool, and Policy traits (defined in core so
//! `kn9t-react` sees only `dyn Trait`, GI-1; implemented in later stages).

use crate::cache::Cache;
use crate::cancel::Cancel;
use crate::error::{StoreErr, ToolErr};
use crate::event::{Event, SeqRange};
use crate::ids::{CallId, SessionId};
use crate::message::{Content, Message};
use crate::model::ModelRef;
use crate::toolspec::ToolSpec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// -- R-CORE-250: Store --

/// R-CORE-250
#[derive(Clone)]
pub struct CompactSpan {
    pub replaced: SeqRange,
    pub messages: Vec<Message>,
}

/// R-CORE-250
pub struct RequestPlan {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub cache: Vec<Cache>,
    /// `Some` => summarize before sending.
    pub compact: Option<CompactSpan>,
}

/// R-CORE-250
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub head_seq: u64,
    pub ctx_tokens: u32,
    /// This session's own spend, excludes inherited. 96E-14: micros is truth.
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_micros: i64,
    pub model: ModelRef,
}

/// R-CORE-250 -- `plan_request` also computes cache breakpoints (it already walks
/// the messages and holds `ModelSpec`); §7.5.
pub trait Store: Send + Sync {
    fn plan_request(&self, session: &SessionId) -> Result<RequestPlan, StoreErr>;
    /// Assigns seq, writes events + projections in one txn, returns the seq (§3.1).
    fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr>;
    fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, StoreErr>;
}

// -- R-CORE-260: Tool --

/// 32-byte content hash used by the edit staleness guard.
pub type Sha256 = [u8; 32];

/// R-CORE-260
pub struct ToolOutput {
    /// What the MODEL sees, truncated.
    pub content: Vec<Content>,
    /// What UI/DB see, full.
    pub details: Option<serde_json::Value>,
    pub is_error: bool,
}

/// R-CORE-260 -- `read`'s `HashMap` is internal shared state, never serialized
/// (GI-3 concerns serialization only); the lock is held only for lookup/insert,
/// never across I/O (§11.2).
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub read: Arc<Mutex<HashMap<PathBuf, (Sha256, SystemTime)>>>,
    pub bus: Arc<dyn crate::bus::EventSink>,
    /// The `CallId` of the call being executed. Tools use this to emit
    /// `ToolProgress` events so the TUI can stream output to the right tool line.
    pub call_id: CallId,
    /// 96E-17: the session this call runs in. Built-in tools such as
    /// `spawn_session` need it to fork/locate the parent session. `None` in
    /// unit tests where no session exists.
    pub session: Option<SessionId>,
}

/// R-CORE-260
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    fn execute(
        &self,
        args: &serde_json::Value,
        ctx: &ToolCtx,
        cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr>;
    fn parallel_safe(&self) -> bool {
        false
    }
}

// -- PluginKv: persistent KV store for plugin state --

/// Trait for persistent, per-plugin key-value storage backed by the SQLite store.
///
/// Keys are scoped by `(plugin, scope, key)`:
/// - `plugin` is the plugin name set by the host — plugins cannot cross-namespace.
/// - `scope` is an arbitrary grouping string chosen by the plugin.  Use `""` for
///   global (process-lifetime) state or a `session_id` for session-scoped state.
/// - `key` is a plain string; `value` is any JSON value.
///
/// GI-4 does not apply here — this table is mutable-in-place.  It is metadata,
/// not the event log.
pub trait PluginKv: Send + Sync {
    /// Return the JSON value stored at `(plugin, scope, key)`, or `None` if absent.
    fn kv_get(&self, plugin: &str, scope: &str, key: &str) -> Result<Option<serde_json::Value>, StoreErr>;
    /// Upsert `(plugin, scope, key)` → `value`.
    fn kv_set(&self, plugin: &str, scope: &str, key: &str, value: &serde_json::Value) -> Result<(), StoreErr>;
    /// Delete `(plugin, scope, key)`.  A no-op if the key does not exist.
    fn kv_del(&self, plugin: &str, scope: &str, key: &str) -> Result<(), StoreErr>;
    /// Delete all keys matching `(plugin, scope)`.
    /// Call with `scope = session_id` on compaction or session delete.
    fn kv_del_scope(&self, plugin: &str, scope: &str) -> Result<(), StoreErr>;
}

// -- 96E-16: pluggable compaction --

/// 96E-16 — data for a structured handoff (ID-based keep/summarize/drop + resume actions).
/// This is the *data* half of `Event::Handoff` without the `seq`; `Compactor` returns it
/// and the host stamps `seq` via `Event::Handoff { seq, .. }`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPlanData {
    pub keep: Vec<CallId>,
    pub summarize: Vec<crate::event::HandoffSummary>,
    #[serde(rename = "drop")]
    pub drop_ids: Vec<CallId>,
    pub resume_actions: Vec<String>,
}

/// 96E-16 — result of a compaction delegation.
#[derive(Clone)]
pub struct CompactionPlan {
    pub summary: Message,
    pub handoff: Option<HandoffPlanData>,
}

/// 96E-16/17 — pluggable compaction delegate, analogous to `PluginProvider`.
///
/// When set, `ReactLoop::run_compaction` delegates to this trait; `validate_handoff`
/// is applied host-side before any `Handoff` is persisted (host-side is safer — cannot
/// be bypassed by a buggy/malicious compactor).
///
/// 96E-17: when no compactor is installed (`None`), compaction is **fail-closed** — the
/// turn errors with `ReactError::CompactionUnavailable` and nothing is persisted. The
/// hardcoded inline-prompt fallback was removed; without a compactor plugin a session
/// simply ends when its context window is exhausted.
pub trait Compactor: Send + Sync {
    fn compact(&self, span: CompactSpan, model: &ModelRef) -> Result<CompactionPlan, String>;
}

// -- R-CORE-270: approval (ADR-0008) --

/// R-CORE-270 -- the dispatch-time view (fully accumulated args, no `Content`
/// wrapper); distinct from `Content::ToolCall`.
#[derive(Clone)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub args_json: String,
}

/// R-CORE-270 — the outcome of an approval request. Also the wire type of
/// `POST /approve` (`{"decision":"allow"}`), which is why it keeps `Ask`/`HardDeny`
/// even though ADR-0008 removed the code that used to *derive* them.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask,
    HardDeny { reason: String },
}

/// R-CORE-270 → ADR-0008 — the **approval mechanism**, not the approval decision.
///
/// Before ADR-0008 this trait judged risk (`check(call, cwd) -> Decision`, with the
/// server combining `ToolSpec.effects` and classifying shell commands). That judgement now
/// belongs to a policy plugin via `HookVeto` on `before_tool_call`; what remains here is
/// the part a subprocess cannot own: showing the request to the user and blocking the turn
/// until an answer arrives.
///
/// The server implementation emits `Event::ApprovalRequest` on the session bus, waits on a
/// `Condvar` until `POST /approve` resolves it, and applies the `once|session|always`
/// scope. `kn9t-react` only sees `dyn Approver` (GI-1) — it cannot reach the bus, the
/// write lease, or `~/.kn9t/config.toml` itself, which is precisely why this seam exists.
///
/// `reason` is the plugin's explanation, shown to the user so the prompt says *why*.
pub trait Approver: Send + Sync {
    fn request(&self, call: &ToolCall, cwd: &Path, reason: &str) -> Decision;
}
