//! # kn9t-core
//!
//! The vocabulary crate of kn9t: every type on the wire, in the log, and across
//! every trait boundary. Contains **no I/O, no threads spawned, no provider, no
//! store** -- only data types, trait definitions, the bus, and pure functions.
//!
//! Dependencies are exactly `serde` + `serde_json` (GI-2). No async anywhere (GI-5).

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
pub use event::{Event, ForkReason, ForkSnapshot, HookName, SeqRange, UsageKind};
pub use hook::{HookHost, HookVeto, NextTurnPatch, NoopHookHost};
pub use ids::{ApprovalId, CallId, MsgId, SessionId};
pub use message::{Content, Message, Role};
pub use model::{Effort, ModelRef, ModelSpec, Price, Quirks, Thinking, ThinkingReplay};
pub use provider::{Chunk, Provider, Request};
pub use registry::ToolRegistry;
pub use toolspec::{Effect, EffectKind, ToolSpec};
pub use traits::{
    CompactSpan, Decision, Policy, PluginKv, RequestPlan, SessionSnapshot, Sha256, Store, Tool,
    ToolCall, ToolCtx, ToolOutput,
};
pub use usage::{StopReason, Tokens, Usage};
