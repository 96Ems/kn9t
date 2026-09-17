//! Core traits: Store, Tool, Compactor, and Approver. Defined here so kn9t-react
//! depends only on the trait interface, not the implementations.

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

/// Range of events to be compacted and the replacement message.
#[derive(Clone)]
pub struct CompactSpan {
    pub replaced: SeqRange,
    pub messages: Vec<Message>,
}

/// Request assembly: system prompt, messages, tools, and optional compaction.
pub struct RequestPlan {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub cache: Vec<Cache>,
    /// `Some` => summarize before sending.
    pub compact: Option<CompactSpan>,
}

/// Current session state snapshot for plan assembly.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub head_seq: u64,
    pub ctx_tokens: u32,
    /// Session's own cost (excludes inherited); stored in micros.
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_micros: i64,
    pub model: ModelRef,
    /// Disabled tools for this session (from latest ToolsToggled). Blocking enforced at runtime.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

/// Persistent session storage: planning, appending events, and snapshots.
pub trait Store: Send + Sync {
    fn plan_request(&self, session: &SessionId) -> Result<RequestPlan, StoreErr>;
    /// Appends event, assigns seq, writes atomically, returns assigned seq.
    fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr>;
    fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, StoreErr>;
}

// Tool execution interface

/// 32-byte content hash used by the edit staleness guard.
pub type Sha256 = [u8; 32];

/// Result of tool execution: model-visible content, full details, and error flag.
pub struct ToolOutput {
    /// What the MODEL sees, truncated.
    pub content: Vec<Content>,
    /// What UI/DB see, full.
    pub details: Option<serde_json::Value>,
    pub is_error: bool,
}

/// Context passed to tool execute: cwd, file cache, event sink, and call ID.
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub read: Arc<Mutex<HashMap<PathBuf, (Sha256, SystemTime)>>>,
    pub bus: Arc<dyn crate::bus::EventSink>,
    /// The `CallId` of the call being executed. Tools use this to emit
    /// `ToolProgress` events so the TUI can stream output to the right tool line.
    pub call_id: CallId,
}

/// Tool execution interface: spec, execute, parallelism, and ownership info.
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
    /// Plugin name if this tool is plugin-backed; None for built-in tools.
    fn plugin(&self) -> Option<&str> {
        None
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
    fn kv_get(
        &self,
        plugin: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>, StoreErr>;
    /// Upsert `(plugin, scope, key)` → `value`.
    fn kv_set(
        &self,
        plugin: &str,
        scope: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), StoreErr>;
    /// Delete `(plugin, scope, key)`.  A no-op if the key does not exist.
    fn kv_del(&self, plugin: &str, scope: &str, key: &str) -> Result<(), StoreErr>;
    /// Delete all keys matching `(plugin, scope)`.
    /// Call with `scope = session_id` on compaction or session delete.
    fn kv_del_scope(&self, plugin: &str, scope: &str) -> Result<(), StoreErr>;
}

// -- pluggable compaction

/// Handoff plan: which calls to keep/summarize/drop and resume actions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPlanData {
    pub keep: Vec<CallId>,
    pub summarize: Vec<crate::event::HandoffSummary>,
    #[serde(rename = "drop")]
    pub drop_ids: Vec<CallId>,
    pub resume_actions: Vec<String>,
}

/// Result of compaction: summary message and optional structured handoff.
#[derive(Clone)]
pub struct CompactionPlan {
    pub summary: Message,
    pub handoff: Option<HandoffPlanData>,
}

/// Compaction delegate: reduces context by summarizing or dropping old messages.
/// When no compactor is installed, compaction is fail-closed: turns error on context overflow.
pub trait Compactor: Send + Sync {
    fn compact(&self, span: CompactSpan, model: &ModelRef) -> Result<CompactionPlan, String>;
}

// Tool execution approval and policy

/// Tool call at dispatch time: id, name, and accumulated args as JSON string.
#[derive(Clone)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub args_json: String,
}

/// Approval outcome: Allow, Deny, Ask, or HardDeny.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask,
    HardDeny { reason: String },
}

/// Approval mechanism: blocks a turn until the user approves or denies a tool call.
/// Reason from the policy plugin is shown to the user. Context carries session and sink.
pub trait Approver: Send + Sync {
    fn request(&self, call: &ToolCall, cwd: &Path, reason: &str, ctx: &ApprovalCtx) -> Decision;
}

/// Context for approval: session id, event sink for prompting, cancel token for ESC.
pub struct ApprovalCtx<'a> {
    pub session: &'a str,
    pub sink: &'a dyn crate::bus::EventSink,
    pub cancel: &'a crate::Cancel,
}
