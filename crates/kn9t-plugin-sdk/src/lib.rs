//! # kn9t Plugin SDK
//!
//! Write a kn9t plugin in Rust with a single dependency.
//!
//! A plugin is any executable that speaks the kn9t plugin wire protocol
//! (newline-delimited JSON over stdin/stdout). Plugins can provide tools,
//! implement a language model provider, intercept agent lifecycle hooks, or
//! observe bus events. This crate handles all wire ceremony — you implement
//! traits.
//!
//! ## Plugin types
//!
//! | Trait | What it does |
//! |---|---|
//! | [`PluginTool`] | Expose tools to the agent's ReAct loop |
//! | [`PluginProvider`] | Implement a language model provider (streaming) |
//! | [`PluginHook`] | Intercept agent lifecycle hooks (approval, redaction, …) |
//! | [`PluginEventSink`] | Observe bus events (logging, metrics, audit) |
//!
//! ## Quick start
//!
//! ```no_run
//! use kn9t_plugin_sdk::{Plugin, traits::{PluginTool, ToolOutput}, ctx::ToolCallCtx, wire::ToolSpec};
//! use serde_json::{json, Value};
//!
//! struct Echo;
//!
//! impl PluginTool for Echo {
//!     fn spec(&self) -> ToolSpec {
//!         ToolSpec {
//!             name: "echo".into(),
//!             description: "Returns its input unchanged.".into(),
//!             schema: json!({"type":"object","properties":{"msg":{"type":"string"}},"required":["msg"]}),
//!             parallel_safe: true,
//!             hidden: false,
//!             effects: vec![],
//!             policy: Default::default(),
//!         }
//!     }
//!     fn execute(&self, args: &Value, _ctx: &ToolCallCtx) -> ToolOutput {
//!         ToolOutput::text(args["msg"].as_str().unwrap_or(""))
//!     }
//! }
//!
//! fn main() {
//!     Plugin::new("echo-plugin")
//!         .tool(Echo)
//!         .run();
//! }
//! ```
//!
//! ## Wire protocol
//!
//! The protocol is documented in `spec/08b-plugin-redesign.md` in the kn9t
//! repository. It is language-neutral — Python, Node, and Go SDKs may be
//! written to the same spec.
//!
//! ## GI-5 compliance
//!
//! This crate is entirely synchronous and blocking. There is no tokio, no
//! `async fn`, and no `.await`. Cancel delivery uses an atomic flag set by
//! a dedicated reader thread; tool/provider handlers block their own OS thread.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ctx;
pub mod plugin;
pub mod sse;
pub mod subagent;
pub mod traits;
pub mod wire;

pub use plugin::Plugin;
pub use sse::{SseEvent, SseReader};
pub use traits::{
    ContentBlock, PluginEventSink, PluginHook, PluginProvider, PluginTool, ProviderResult,
    ToolOutput,
};
pub use ctx::KvClient;
