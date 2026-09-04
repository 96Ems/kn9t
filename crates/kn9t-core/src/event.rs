//! R-CORE-140 .. R-CORE-160 — events (the wire, the log, the truth) and the fork
//! snapshot.

use crate::ids::{ApprovalId, CallId, MsgId, SessionId};
use crate::message::Message;
use crate::model::{ModelRef, Price, Thinking};
use crate::usage::{StopReason, Tokens};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// R-CORE-250 — serde-friendly range (not `std::ops::Range`, which serializes
/// awkwardly and is not `Copy`). Defined here because `Event::Compacted` needs it.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SeqRange {
    pub start: u64,
    pub end: u64,
}

/// R-CORE-150
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageKind {
    Main,
    Compaction,
    Subagent,
    Title,
}

/// R-CORE-155 — one variant per hook in the plugin surface (PLUG §13.3); `on_event`
/// is a subscription, not a hook, and is absent.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookName {
    BeforeToolCall,
    AfterToolCall,
    BeforeRequest,
    ShouldStopAfterTurn,
    PrepareNextTurn,
    GetSteering,
    GetFollowup,
    GetApiKey,
}

/// R-CORE-160 — reason a session was derived from another.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkReason {
    Fork,
    Rewind,
    Subagent,
    Tree,
}

/// R-CORE-160 — `SessionForked` (seq 0 of every derived session) carries a snapshot
/// captured **at copy time**, never recomputed.
#[derive(Clone, Serialize, Deserialize)]
pub struct ForkSnapshot {
    pub origin_session: SessionId,
    pub origin_seq: u64,
    pub reason: ForkReason,
    #[serde(default)]
    pub inherited_cost_usd: f64,
    /// 96E-14: integer micros, source of truth.
    #[serde(default)]
    pub inherited_cost_micros: i64,
    pub inherited_tokens_in: u64,
    pub inherited_tokens_out: u64,
    pub inherited_cache_read: u64,
    pub inherited_messages: u32,
    pub inherited_ctx_tokens: u32,
    #[serde(default)]
    pub budget_remaining_usd: Option<f64>,
    #[serde(default)]
    pub budget_remaining_micros: Option<i64>,
    pub model_at_fork: ModelRef,
    pub thinking_at_fork: Thinking,
    pub cwd_at_fork: PathBuf,
}

/// 96E-16 — one entry of a structured handoff summarization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffSummary {
    pub id: CallId,
    pub summary: String,
}

/// Validate that every `CallId` cited in a `Handoff` exists in `known`.
/// Host-side validation (96E-16 § gaps → open question): prevents a buggy/malicious
/// compactor plugin from citing hallucinated IDs. Called by the store/host before
/// persisting a `Handoff` produced by a compactor.
pub fn validate_handoff(event: &Event, known: &[CallId]) -> Result<(), String> {
    if let Event::Handoff {
        keep,
        summarize,
        drop_ids,
        ..
    } = event
    {
        let known_set: std::collections::HashSet<&CallId> = known.iter().collect();
        for id in keep.iter().chain(drop_ids.iter()) {
            if !known_set.contains(id) {
                return Err(format!(
                    "Handoff cites unknown CallId in keep/drop: {}",
                    id.0
                ));
            }
        }
        for s in summarize {
            if !known_set.contains(&s.id) {
                return Err(format!(
                    "Handoff cites unknown CallId in summarize: {}",
                    s.id.0
                ));
            }
        }
    }
    Ok(())
}

/// R-CORE-140 — the one `Event` enum. A variant is **durable** iff it carries a
/// `seq: u64` field; **transient** otherwise. Durable variants folded in `seq`
/// order reconstruct a session exactly (§5).
///
/// Wire format: `{"kind": "snake_case_variant", ...}` — all JSON uses snake_case
/// per project convention (AGENTS.md §JSON).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    // ── durable ──
    SessionForked {
        seq: u64,
        fork: ForkSnapshot,
    },
    MessageAppended {
        seq: u64,
        msg: Message,
    },
    ModelChanged {
        seq: u64,
        model: ModelRef,
    },
    Compacted {
        seq: u64,
        replaced: SeqRange,
        summary: Message,
    },
    /// 96E-16 — structured handoff between sessions (ID-based keep/summarize/drop
    /// plus resume actions). Durable, append-only, but not projected into
    /// `messages` (like `ModelChanged`); the resumed session reconstructs from it
    /// explicitly. Distinct from `Compacted` which is in-session reduction.
    Handoff {
        seq: u64,
        keep: Vec<CallId>,
        summarize: Vec<HandoffSummary>,
        #[serde(rename = "drop")]
        drop_ids: Vec<CallId>,
        resume_actions: Vec<String>,
    },
    /// The set of tools currently DISABLED for this session, carried as the full
    /// list (not a diff) so a replay is idempotent: the last `ToolsToggled` wins,
    /// exactly like `ModelChanged`. Durable but not projected into any row (§ like
    /// `ModelChanged`); reconstructed by reading the latest event. The tools array
    /// sent to the provider is left byte-identical — blocking happens at execution
    /// time in the loop, so the level-1 cache prefix is never disturbed.
    ToolsToggled {
        seq: u64,
        disabled: Vec<String>,
    },
    UsageRecorded {
        seq: u64,
        provider: String,
        model: String,
        // Spec bug DB-01: the Rust field is `kind` per R-CORE-140, but the enum's
        // internal tag is also "kind"; serde forbids the collision. The field name
        // stays `kind` (specs 03/06 use it); only its wire key is disambiguated.
        #[serde(rename = "usage_kind")]
        kind: UsageKind,
        tokens: Tokens,
        price_snapshot: Price,
        /// 96E-14: deterministic integer micros (1_000_000 micros = 1 USD).
        /// New writes populate `cost_micros`; `cost_usd` is kept for reading old rows
        /// (migration) and written as well for wire compat, but `cost_micros` is the
        /// source of truth for budget/comparison.
        #[serde(default)]
        cost_micros: i64,
        #[serde(default)]
        cost_usd: f64,
        /// R-CORE-142 — `true` when inferred after an abort cut the stream before
        /// usage arrived (§9.1), `false` when provider-reported.
        estimated: bool,
    },

    // ── transient ──
    TurnStarted {
        turn: u32,
    },
    TextDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ThinkingDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ToolArgsDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ToolStarted {
        call_id: CallId,
        name: String,
    },
    ToolProgress {
        call_id: CallId,
        note: String,
    },
    ToolFinished {
        call_id: CallId,
        is_error: bool,
    },
    ApprovalRequest {
        id: ApprovalId,
        tool: String,
        args: serde_json::Value,
        cwd: PathBuf,
        /// ADR-0008 — the policy plugin's explanation, shown in the prompt so the user sees
        /// *why* approval is being asked. `#[serde(default)]` keeps events written before
        /// ADR-0008 replayable (GI-4: the log is append-only, old rows are never rewritten).
        #[serde(default)]
        reason: String,
    },
    TurnEnded {
        turn: u32,
        stop: StopReason,
    },
    HookFailed {
        plugin: String,
        hook: HookName,
        reason: String,
    },
    TitleChanged {
        title: String,
    },
    Error {
        message: String,
    },
    /// R-PCORE-060 retry — transient progress while the provider pre-stream retries
    /// (429/5xx/connect). Emitted before the backoff sleep so the TUI can show
    /// "retry 1/3 in 500ms (429)" instead of a silent spinner.
    RetryAttempt {
        attempt: u32,
        max: u32,
        error: String,
        delay_ms: u64,
        retry_kind: String,
    },
    /// Phase sync — explicit server-driven turn phase so TUI spinner can't lie.
    /// `phase` is one of `thinking|streaming|tool|retrying|failed|idle`.
    /// Emitted alongside existing `TurnStarted`/`TextDelta`/`ThinkingDelta`/`ToolStarted`/`TurnEnded`
    /// to give the TUI a single source of truth for status-bar and spinner text.
    TurnStatus {
        phase: String,
        #[serde(default)]
        message: String,
    },
    /// Generic plugin notification — forwarded as-is to SSE clients.
    /// Payload must include `plugin` (name) and `message` (display text).
    PluginNotification {
        /// Arbitrary JSON payload from the plugin (must have `plugin` and `message` fields).
        #[serde(flatten)]
        payload: serde_json::Value,
    },
    /// 96E-28 — generic client→host interaction request (transient).
    InteractionRequest {
        id: u64,
        plugin: String,
        payload: serde_json::Value,
    },
    /// 96E-23 — structured plugin→TUI UI directive (transient, session-scoped).
    /// Distinct from `PluginNotification` (free-text) — carries a non-text
    /// structured payload routed via the same session-scoped dispatch fixed in
    /// 96E-21 (must NOT broadcast). `target`/`op` are host-validated; `payload`
    /// is opaque and forwarded verbatim to the TUI.
    UiDirective {
        plugin: String,
        target: String,
        op: String,
        payload: serde_json::Value,
    },
    /// R-PLUG2-110 — a plugin sent `declare` to hot-update its tools/hooks/capabilities.
    /// Transient, broadcast via SSE so TUI clients can refresh their tool lists.
    /// `tools_added`/`tools_removed` are the tool names that changed.
    PluginDeclared {
        plugin: String,
        tools_added: Vec<String>,
        tools_removed: Vec<String>,
    },
}

impl Event {
    /// R-CORE-145 — `Some` iff the variant is durable (carries `seq`).
    pub fn seq(&self) -> Option<u64> {
        match self {
            Event::SessionForked { seq, .. }
            | Event::MessageAppended { seq, .. }
            | Event::ModelChanged { seq, .. }
            | Event::Compacted { seq, .. }
            | Event::Handoff { seq, .. }
            | Event::ToolsToggled { seq, .. }
            | Event::UsageRecorded { seq, .. } => Some(*seq),
            _ => None,
        }
    }

    /// R-CORE-145
    pub fn is_durable(&self) -> bool {
        self.seq().is_some()
    }

    /// Stamp the authoritative sequence number onto a durable variant, consuming
    /// and returning the event. The store assigns `seq` at append time; callers
    /// construct durable events with a placeholder `seq` (conventionally `0`) and
    /// the store overwrites it via this setter before persisting and projecting,
    /// so `events.payload` and every projection row carry the true, gapless seq
    /// (§3.1, §5). A no-op on transient variants.
    pub fn with_seq(mut self, new_seq: u64) -> Self {
        match &mut self {
            Event::SessionForked { seq, .. }
            | Event::MessageAppended { seq, .. }
            | Event::ModelChanged { seq, .. }
            | Event::Compacted { seq, .. }
            | Event::Handoff { seq, .. }
            | Event::ToolsToggled { seq, .. }
            | Event::UsageRecorded { seq, .. } => *seq = new_seq,
            _ => {}
        }
        self
    }
}

/// 96E-12 — distinct type for transient (live) events only.
/// `EventSink::emit` accepts only this type, so durable variants cannot be
/// emitted through the live path at compile time. Durable events must go
/// through `Store::append(Event::...)` (R-CORE-225).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveEvent {
    TurnStarted {
        turn: u32,
    },
    TextDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ThinkingDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ToolArgsDelta {
        msg_id: MsgId,
        idx: u32,
        delta: String,
    },
    ToolStarted {
        call_id: CallId,
        name: String,
    },
    ToolProgress {
        call_id: CallId,
        note: String,
    },
    ToolFinished {
        call_id: CallId,
        is_error: bool,
    },
    ApprovalRequest {
        id: ApprovalId,
        tool: String,
        args: serde_json::Value,
        cwd: PathBuf,
        #[serde(default)]
        reason: String,
    },
    TurnEnded {
        turn: u32,
        stop: StopReason,
    },
    HookFailed {
        plugin: String,
        hook: HookName,
        reason: String,
    },
    TitleChanged {
        title: String,
    },
    Error {
        message: String,
    },
    RetryAttempt {
        attempt: u32,
        max: u32,
        error: String,
        delay_ms: u64,
        retry_kind: String,
    },
    TurnStatus {
        phase: String,
        #[serde(default)]
        message: String,
    },
    PluginNotification {
        #[serde(flatten)]
        payload: serde_json::Value,
    },
    /// 96E-28 — generic client→host interaction request (transient). The host does
    /// not interpret `payload`; it is the plugin's own shape (question/choices,
    /// form schema, etc.) forwarded verbatim to the client. The client responds
    /// via `POST /ui-respond {id, payload}`.
    InteractionRequest {
        id: u64,
        plugin: String,
        payload: serde_json::Value,
    },
    /// 96E-23 — structured UI directive (transient, session-scoped), same dispatch
    /// guarantees as InteractionRequest (no broadcast, plugin in payload).
    UiDirective {
        plugin: String,
        target: String,
        op: String,
        payload: serde_json::Value,
    },
}

impl From<LiveEvent> for Event {
    fn from(live: LiveEvent) -> Self {
        match live {
            LiveEvent::TurnStarted { turn } => Event::TurnStarted { turn },
            LiveEvent::TextDelta { msg_id, idx, delta } => Event::TextDelta { msg_id, idx, delta },
            LiveEvent::ThinkingDelta { msg_id, idx, delta } => {
                Event::ThinkingDelta { msg_id, idx, delta }
            }
            LiveEvent::ToolArgsDelta { msg_id, idx, delta } => {
                Event::ToolArgsDelta { msg_id, idx, delta }
            }
            LiveEvent::ToolStarted { call_id, name } => Event::ToolStarted { call_id, name },
            LiveEvent::ToolProgress { call_id, note } => Event::ToolProgress { call_id, note },
            LiveEvent::ToolFinished { call_id, is_error } => {
                Event::ToolFinished { call_id, is_error }
            }
            LiveEvent::ApprovalRequest {
                id,
                tool,
                args,
                cwd,
                reason,
            } => Event::ApprovalRequest {
                id,
                tool,
                args,
                cwd,
                reason,
            },
            LiveEvent::TurnEnded { turn, stop } => Event::TurnEnded { turn, stop },
            LiveEvent::HookFailed {
                plugin,
                hook,
                reason,
            } => Event::HookFailed {
                plugin,
                hook,
                reason,
            },
            LiveEvent::TitleChanged { title } => Event::TitleChanged { title },
            LiveEvent::Error { message } => Event::Error { message },
            LiveEvent::RetryAttempt {
                attempt,
                max,
                error,
                delay_ms,
                retry_kind,
            } => Event::RetryAttempt {
                attempt,
                max,
                error,
                delay_ms,
                retry_kind,
            },
            LiveEvent::TurnStatus { phase, message } => Event::TurnStatus { phase, message },
            LiveEvent::PluginNotification { payload } => Event::PluginNotification { payload },
            LiveEvent::InteractionRequest {
                id,
                plugin,
                payload,
            } => Event::InteractionRequest {
                id,
                plugin,
                payload,
            },
            LiveEvent::UiDirective {
                plugin,
                target,
                op,
                payload,
            } => Event::UiDirective {
                plugin,
                target,
                op,
                payload,
            },
        }
    }
}
