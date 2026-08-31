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
    pub inherited_cost_usd: f64,
    pub inherited_tokens_in: u64,
    pub inherited_tokens_out: u64,
    pub inherited_cache_read: u64,
    pub inherited_messages: u32,
    pub inherited_ctx_tokens: u32,
    pub budget_remaining_usd: Option<f64>,
    pub model_at_fork: ModelRef,
    pub thinking_at_fork: Thinking,
    pub cwd_at_fork: PathBuf,
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
}

impl Event {
    /// R-CORE-145 — `Some` iff the variant is durable (carries `seq`).
    pub fn seq(&self) -> Option<u64> {
        match self {
            Event::SessionForked { seq, .. }
            | Event::MessageAppended { seq, .. }
            | Event::ModelChanged { seq, .. }
            | Event::Compacted { seq, .. }
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
            | Event::UsageRecorded { seq, .. } => *seq = new_seq,
            _ => {}
        }
        self
    }
}
