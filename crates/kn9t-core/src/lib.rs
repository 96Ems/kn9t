//! kn9t-core: vocabulary crate with all wire types, traits, and core abstractions.
//! No I/O, no threads, no async. Only data types, traits, bus, and pure functions.

mod bus;
mod cache;
mod cancel;
mod error;
mod event;
mod hook;
mod ids;
mod message;
mod model;
mod provider;
mod registry;
mod toolspec;
mod traits;
mod usage;

pub use bus::{Bus, EventSink, Subscription};
pub use cache::{breakpoints, Cache, CacheMode};
pub use cancel::Cancel;
pub use error::{ProvErr, StoreErr, ToolErr};
pub use event::{
    validate_handoff, Event, ForkReason, ForkSnapshot, HandoffSummary, HookName, LiveEvent,
    SeqRange, UsageKind,
};
pub use hook::{HookHost, HookVeto, NextTurnPatch, NoopHookHost};
pub use ids::{ApprovalId, CallId, MsgId, SessionId};
pub use message::{Content, Message, Role};
pub use model::{
    cost_micros, Effort, ModelRef, ModelSpec, MoneyMicros, Price, Quirks, Thinking, ThinkingReplay,
};
pub use provider::{Chunk, Provider, Request};
pub use registry::ToolRegistry;
pub use toolspec::{
    value_to_pattern, wildcard_match, DefaultPolicy, Effect, EffectKind, ToolPolicy, ToolSpec,
};
pub use traits::{
    ApprovalCtx, Approver, CompactSpan, CompactionPlan, Compactor, Decision, HandoffPlanData,
    PluginKv, RequestPlan, SessionSnapshot, Sha256, Store, Tool, ToolCall, ToolCtx, ToolOutput,
};
pub use usage::{StopReason, Tokens, Usage};

// Re-export macros so other crates don't need a direct kn9t-macros dependency (GI-1).
pub use kn9t_macros::{safe_expect, safe_unwrap};
