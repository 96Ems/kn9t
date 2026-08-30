pub mod assemble;
pub mod http;
pub mod pricing;
pub mod quirks;
pub mod retry;
pub mod sse;

pub use assemble::{assemble, AssembleResult};
pub use http::{AuthScheme, HttpRequest, HttpResponse, send};
pub use pricing::lookup_price;
pub use quirks::Quirks;
pub use retry::{Backoff, with_retry};
pub use sse::sse_lines;

// Re-export kn9t-core types used by crates that swap their dep from
// kn9t-core to kn9t-provider-core (replay, react — R-RPLY-070 / DB-02).
// Note: kn9t_core::Quirks (model quirks) is intentionally NOT re-exported here
// to avoid collision with kn9t_provider_core::Quirks (HTTP quirks). Callers
// that need kn9t_core::Quirks must use kn9t_core directly or the full path.
pub use kn9t_core::{
    Bus, Cache, CallId, Cancel, CacheMode, Chunk, CompactSpan, Content, Decision,
    Effort, Event, EventSink, ForkReason, ForkSnapshot, HookHost, HookName, HookVeto,
    MsgId, Message, ModelRef, ModelSpec, NextTurnPatch, NoopHookHost, Policy,
    Price, ProvErr, Provider, Request, RequestPlan, Role, SeqRange, SessionId,
    SessionSnapshot, Sha256, StopReason, Store, StoreErr, Thinking, Tokens,
    Tool, ToolCall, ToolCtx, ToolErr, ToolOutput, ToolRegistry, ToolSpec,
    Usage, UsageKind,
};
