//! Events are the durable log: session state is reconstructed by folding events in seq order.
//! Each variant is marked as durable (carries `seq`) or transient (live only).

use crate::ids::{ApprovalId, CallId, MsgId, SessionId};
use crate::message::Message;
use crate::model::{ModelRef, Price, Thinking};
use crate::usage::{StopReason, Tokens};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Serde-friendly range for compaction events: holds start/end seq numbers.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SeqRange {
    pub start: u64,
    pub end: u64,
}

/// Categorizes the type of token usage tracked in a UsageRecorded event.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageKind {
    Main,
    Compaction,
    Subagent,
    Title,
}

/// Hooks that plugins can implement in the session lifecycle.
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

/// Reason a session was forked from another: fork, rewind, subagent, or tree.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkReason {
    Fork,
    Rewind,
    Subagent,
    Tree,
}

/// Snapshot captured at session fork time: preserves origin, cost, and state needed for session resumption.
#[derive(Clone, Serialize, Deserialize)]
pub struct ForkSnapshot {
    pub origin_session: SessionId,
    pub origin_seq: u64,
    pub reason: ForkReason,
    #[serde(default)]
    pub inherited_cost_usd: f64,
    /// Integer microseconds: the authoritative unit for budget comparison.
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

/// Summary of a tool call during a structured handoff between sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffSummary {
    pub id: CallId,
    pub summary: String,
}

/// Validates that all CallIds in a Handoff event exist in the known set.
/// Prevents a compactor from citing non-existent IDs.
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

/// Main event enum. Durable variants carry `seq: u64` and fold in order to reconstruct session state.
/// Transient variants are live-only and not persisted. Wire format uses snake_case.
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
    /// Structured handoff to resume a session from a fork: keep/summarize/drop tool calls
    /// with resume actions. Not projected into messages; resumer reads it explicitly.
    Handoff {
        seq: u64,
        keep: Vec<CallId>,
        summarize: Vec<HandoffSummary>,
        #[serde(rename = "drop")]
        drop_ids: Vec<CallId>,
        resume_actions: Vec<String>,
    },
    /// Disabled tools for this session: full list (not a diff) so replays are idempotent.
    /// Blocking enforced at execution time; provider still sees all specs for cache integrity.
    ToolsToggled {
        seq: u64,
        disabled: Vec<String>,
    },
    UsageRecorded {
        seq: u64,
        provider: String,
        model: String,
        // Serde field renamed to "usage_kind" to avoid collision with enum tag.
        #[serde(rename = "usage_kind")]
        kind: UsageKind,
        tokens: Tokens,
        price_snapshot: Price,
        /// Cost in integer microseconds (source of truth; cost_usd kept for wire compat).
        #[serde(default)]
        cost_micros: i64,
        #[serde(default)]
        cost_usd: f64,
        /// True when usage is inferred after stream abort; false when provider-reported.
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
        /// Policy plugin's explanation shown in prompt. Default for backward compat.
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
    /// Retry attempt progress: emitted before backoff so TUI can show countdown.
    RetryAttempt {
        attempt: u32,
        max: u32,
        error: String,
        delay_ms: u64,
        retry_kind: String,
    },
    /// Turn phase sync: single source of truth for TUI status bar and spinner.
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
    /// generic client→host interaction request (transient).
    InteractionRequest {
        id: u64,
        plugin: String,
        payload: serde_json::Value,
    },
    /// Structured UI directive from plugin: session-scoped, not broadcast. Payload forwarded verbatim.
    UiDirective {
        plugin: String,
        target: String,
        op: String,
        payload: serde_json::Value,
    },
    /// Plugin hot-update notification: tool/hook/capability changes broadcast to TUI clients.
    PluginDeclared {
        plugin: String,
        tools_added: Vec<String>,
        tools_removed: Vec<String>,
    },
    /// Plugin lifecycle state changed: stopped/started/reloaded/crashed.
    /// Delivered to clients and subscribed plugins so they can react independently.
    /// Error field non-empty only on involuntary transitions.
    PluginState {
        plugin: String,
        state: String,
        error: String,
    },
}

impl Event {
    /// Returns seq if the event is durable; None for transient events.
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

    /// True if the event carries a seq number (is durable).
    pub fn is_durable(&self) -> bool {
        self.seq().is_some()
    }

    /// Sets the authoritative sequence number on a durable event. No-op for transient variants.
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

/// Type-safe wrapper for transient events only. Durable events go through Store::append.
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
    /// Internal server coordination: emitted before TurnEnded to clear turn state before clients see the end.
    TurnFinishing {
        turn: u32,
        stop: StopReason,
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
    /// Generic plugin→client interaction request. Host passes payload verbatim; client responds via /ui-respond.
    InteractionRequest {
        id: u64,
        plugin: String,
        payload: serde_json::Value,
    },
    /// Structured UI directive: session-scoped, no broadcast, same dispatch as InteractionRequest.
    UiDirective {
        plugin: String,
        target: String,
        op: String,
        payload: serde_json::Value,
    },
}

impl LiveEvent {
    /// True for internal events that coordinate the host with itself and must not reach clients.
    /// Only TurnFinishing is internal: it clears turn state before TurnEnded broadcasts.
    pub fn is_internal(&self) -> bool {
        matches!(self, LiveEvent::TurnFinishing { .. })
    }

    /// Converts to the durable Event form, or None if internal. Use on publish paths.
    pub fn to_observable_event(self) -> Option<Event> {
        if self.is_internal() {
            return None;
        }
        Some(Event::from(self))
    }
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
            // Internal event: degrade to client-visible TurnEnded to avoid panic.
            // Publish paths should use to_observable_event() which returns None for internal events.
            LiveEvent::TurnFinishing { turn, stop } => Event::TurnEnded { turn, stop },
        }
    }
}
