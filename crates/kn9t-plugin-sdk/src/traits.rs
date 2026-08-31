//! The four plugin traits (spec §3 SDK contract, §4.1).
//!
//! Implement the trait(s) that match what your plugin does, then register them
//! with [`Plugin`](crate::Plugin). The SDK handles all wire ceremony.

use crate::ctx::{ProviderCallCtx, ToolCallCtx};
use crate::wire::{ToolSpec, Usage};
use serde_json::Value;

// ── PluginTool ────────────────────────────────────────────────────────────────

/// A tool exposed by the plugin to the agent's ReAct loop.
///
/// The agent calls the tool by name; the SDK routes the call to
/// [`execute`](PluginTool::execute). If the plugin declared `"streaming"`,
/// use `ctx.progress` to emit incremental output before returning; the SDK
/// sends the final `done` automatically.
///
/// # Example
/// ```
/// use kn9t_plugin_sdk::traits::{PluginTool, ToolOutput};
/// use kn9t_plugin_sdk::ctx::ToolCallCtx;
/// use kn9t_plugin_sdk::wire::ToolSpec;
/// use serde_json::{json, Value};
///
/// struct Echo;
///
/// impl PluginTool for Echo {
///     fn spec(&self) -> ToolSpec {
///         ToolSpec {
///             name: "echo".into(),
///             description: "Returns its input unchanged.".into(),
///             schema: json!({"type":"object","properties":{"msg":{"type":"string"}},"required":["msg"]}),
///             parallel_safe: true,
///             hidden: false,
///             effects: vec![],
///         }
///     }
///     fn execute(&self, args: &Value, _ctx: &ToolCallCtx) -> ToolOutput {
///         let msg = args["msg"].as_str().unwrap_or("").to_string();
///         ToolOutput::text(msg)
///     }
/// }
/// ```
pub trait PluginTool: Send + Sync {
    /// Returns the tool's name, description, and JSON Schema.
    fn spec(&self) -> ToolSpec;
    /// Execute the tool.
    ///
    /// - Check `ctx.cancel.is_cancelled()` at natural checkpoints.
    /// - Call `ctx.progress.send(text)` for live streaming output.
    /// - Return the final authoritative content.
    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput;
}

/// Result of a tool execution.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Content blocks shown to the model (text, image references, etc.).
    pub content: Vec<ContentBlock>,
    /// Whether the execution ended in an error.
    pub is_error: bool,
}

impl ToolOutput {
    /// Convenience: a single text block, not an error.
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::Text { text: s.into() }], is_error: false }
    }
    /// Convenience: a single text block marking an error.
    pub fn error(s: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::Text { text: s.into() }], is_error: true }
    }
}

/// A single content block in a tool result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
    },
    /// An image referenced by its SHA-256 content hash (uploaded via the blob store).
    Image {
        /// Hex SHA-256 of the blob.
        sha256: String,
        /// MIME type, e.g. `"image/png"`.
        media_type: String,
    },
}

// ── PluginProvider ────────────────────────────────────────────────────────────

/// A language model provider implemented as a plugin.
///
/// The host sends the full request; the plugin streams token deltas via
/// `ctx.chunk` and returns usage accounting when complete.
///
/// Provider plugins MUST declare `"streaming"` in their capabilities.
///
/// # Example (sketch)
/// ```no_run
/// use kn9t_plugin_sdk::traits::{PluginProvider, ProviderResult};
/// use kn9t_plugin_sdk::ctx::ProviderCallCtx;
/// use kn9t_plugin_sdk::wire::{ModelDecl, Usage};
/// use serde_json::Value;
///
/// struct MyLlm;
///
/// impl PluginProvider for MyLlm {
///     fn id(&self) -> &str { "my-llm" }
///     fn models(&self) -> Vec<ModelDecl> {
///         vec![ModelDecl { id: "my-model-7b".into(), ctx_window: 32768, price: None }]
///     }
///     fn complete(&self, _request: &Value, ctx: &ProviderCallCtx) -> ProviderResult {
///         ctx.chunk.text_delta("Hello from my model!");
///         ProviderResult {
///             stop: "end_turn".into(),
///             usage: Usage { input: 5, output: 6, ..Default::default() },
///             cost_usd: None,
///             error: None,
///         }
///     }
/// }
/// ```
pub trait PluginProvider: Send + Sync {
    /// Short identifier for this provider, e.g. `"my-llm"`.
    fn id(&self) -> &str;
    /// List of models this provider offers.
    fn models(&self) -> Vec<crate::wire::ModelDecl>;
    /// Execute a completion request, streaming deltas via `ctx.chunk`.
    ///
    /// - Check `ctx.cancel.is_cancelled()` between streaming steps.
    /// - Return usage counts when streaming is complete.
    fn complete(&self, request: &Value, ctx: &ProviderCallCtx) -> ProviderResult;
}

/// Accounting returned when a provider call finishes.
#[derive(Debug, Clone)]
pub struct ProviderResult {
    /// Stop reason: `"STOP"`, `"TOOL_CALL"`, `"LENGTH"`, `"error"`.
    pub stop: String,
    /// Token counts for this call.
    pub usage: Usage,
    /// Provider-computed cost in USD. Omit (`None`) if unknown.
    pub cost_usd: Option<f64>,
    /// If set, the host treats this as a `ProvErr::Stream`. Leave `None` for success.
    pub error: Option<String>,
}

impl ProviderResult {
    /// Construct a zero-accounting error result. The host will return `ProvErr::Stream(msg)`.
    pub fn error(msg: impl Into<String>) -> Self {
        ProviderResult {
            stop: "error".to_string(),
            usage: Usage { input: 0, output: 0, cache_read: 0, cache_write: 0 },
            cost_usd: None,
            error: Some(msg.into()),
        }
    }
}

// ── PluginHook ────────────────────────────────────────────────────────────────

/// An interceptor for agent lifecycle hooks.
///
/// Implement this to add approval gates, redaction, cost guards,
/// or model-switching behaviour.
///
/// # Example
/// ```
/// use kn9t_plugin_sdk::traits::PluginHook;
/// use serde_json::{json, Value};
///
/// struct AllowAll;
///
/// impl PluginHook for AllowAll {
///     fn hooks(&self) -> Vec<&'static str> { vec!["before_tool_call"] }
///     fn call(&self, _hook: &str, _payload: &Value) -> Value {
///         json!({ "action": "allow" })
///     }
/// }
/// ```
pub trait PluginHook: Send + Sync {
    /// Hook names this handler wants to intercept (e.g. `"before_tool_call"`).
    fn hooks(&self) -> Vec<&'static str>;
    /// Handle a hook invocation. Return a JSON reply matching the hook's reply
    /// schema (spec §2.6). Called synchronously; return quickly.
    fn call(&self, hook: &str, payload: &Value) -> Value;
}

// ── PluginEventSink ───────────────────────────────────────────────────────────

/// An observer for bus events (logging, metrics, audit trails).
///
/// Event delivery is fire-and-forget. The SDK calls `on_event` on a dedicated
/// thread; it must never block the main dispatch loop.
///
/// # Example
/// ```
/// use kn9t_plugin_sdk::traits::PluginEventSink;
/// use serde_json::Value;
///
/// struct Logger;
///
/// impl PluginEventSink for Logger {
///     fn event_filter(&self) -> Vec<&'static str> { vec!["*"] }
///     fn on_event(&self, kind: &str, event: &Value) {
///         eprintln!("[event] {kind}: {event}");
///     }
/// }
/// ```
pub trait PluginEventSink: Send + Sync {
    /// Event kind strings to subscribe to. Use `"*"` for all events.
    fn event_filter(&self) -> Vec<&'static str>;
    /// Called on a background thread for each matching event. Must not block.
    fn on_event(&self, kind: &str, event: &Value);
}
