pub mod abort;
pub mod assemble;
pub mod http;
pub mod pricing;
pub mod quirks;
pub mod retry;
pub mod sse;

pub use abort::CancellableReader;
pub use assemble::{assemble, AssembleResult};
pub use http::{send, send_get, AuthScheme, HttpRequest, HttpResponse};
pub use pricing::lookup_price;
pub use quirks::Quirks;
pub use retry::{is_retryable, with_retry, with_retry_with_sink, Backoff};
pub use sse::sse_lines;

// Re-export kn9t-core types used by crates that swap their dep from
// kn9t-core to kn9t-provider-core (replay, react — R-RPLY-070 / DB-02).
// Note: kn9t_core::Quirks (model quirks) is intentionally NOT re-exported here
// to avoid collision with kn9t_provider_core::Quirks (HTTP quirks). Callers
// that need kn9t_core::Quirks must use kn9t_core directly or the full path.
pub use kn9t_core::{
    cost_micros, validate_handoff, ApprovalCtx, Approver, Bus, Cache, CacheMode, CallId, Cancel,
    Chunk, CompactSpan, CompactionPlan, Compactor, Content, Decision, Effort, Event, EventSink,
    ForkReason, ForkSnapshot, HandoffPlanData, HandoffSummary, HookHost, HookName, HookVeto,
    LiveEvent, Message, ModelRef, ModelSpec, MoneyMicros, MsgId, NextTurnPatch, NoopHookHost,
    Price, ProvErr, Provider, Request, RequestPlan, Role, SeqRange, SessionId, SessionSnapshot,
    Sha256, StopReason, Store, StoreErr, Thinking, Tokens, Tool, ToolCall, ToolCtx, ToolErr,
    ToolOutput, ToolRegistry, ToolSpec, Usage, UsageKind,
};
