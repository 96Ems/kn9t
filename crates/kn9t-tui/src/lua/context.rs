//! Context info exposed to Lua — for status bar customization.
//!
//! Exposes session/transcript stats to Lua so users can build custom status bars.

use mlua::{Lua, Result as LuaResult, Table};

/// Context stats to expose to Lua.
#[derive(Debug, Clone, Default)]
pub struct ContextStats {
    /// Number of system messages
    pub system_count: usize,
    /// Number of user messages
    pub user_count: usize,
    /// Number of assistant messages
    pub assistant_count: usize,
    /// Number of tool results
    pub tool_count: usize,
    /// Total input tokens
    pub tokens_in: usize,
    /// Total output tokens
    pub tokens_out: usize,
    /// Cache read tokens
    pub cache_read: usize,
    /// Cache write tokens  
    pub cache_write: usize,
    /// Cost in USD
    pub cost: f64,
    /// Current model name
    pub model: String,
    /// Current turn phase
    pub phase: String,
    /// Session title
    pub title: String,
    /// Wrapped height the input box currently needs, in rows.
    pub input_height: u16,
    /// Whether a turn is currently streaming.
    pub streaming: bool,
    /// Usable context window of the active model, in tokens.
    ///
    /// `None` when the server did not report one. Published so a status bar can
    /// show a real percentage; the built-in config had to assume 200k, which was
    /// wrong on every other model (TRACKING B3).
    pub ctx_window: Option<usize>,
    /// Maximum output tokens of the active model.
    pub max_out: Option<usize>,
    /// Whether server plugins are fully loaded.
    ///
    /// `false` during startup while plugins load in background. The status bar
    /// can show a loading indicator when this is false.
    pub plugins_ready: bool,
    /// Number of messages in the steering buffer.
    pub steering_count: usize,
    /// Number of messages in the queue buffer.
    pub queue_count: usize,
}

/// Update the kn9t.context table with current stats.
pub fn update_context(lua: &Lua, stats: &ContextStats) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = globals
        .get("kn9t")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    let ctx = lua.create_table()?;

    // Message counts
    ctx.set("system_count", stats.system_count)?;
    ctx.set("user_count", stats.user_count)?;
    ctx.set("assistant_count", stats.assistant_count)?;
    ctx.set("tool_count", stats.tool_count)?;

    // Token stats
    ctx.set("tokens_in", stats.tokens_in)?;
    ctx.set("tokens_out", stats.tokens_out)?;
    ctx.set("cache_read", stats.cache_read)?;
    ctx.set("cache_write", stats.cache_write)?;
    ctx.set("cost", stats.cost)?;

    // Session info
    ctx.set("model", stats.model.as_str())?;
    ctx.set("phase", stats.phase.as_str())?;
    ctx.set("title", stats.title.as_str())?;
    ctx.set("input_height", stats.input_height)?;
    ctx.set("streaming", stats.streaming)?;
    ctx.set("plugins_ready", stats.plugins_ready)?;
    // Pending message counts for steering/queue buffers.
    ctx.set("steering_count", stats.steering_count)?;
    ctx.set("queue_count", stats.queue_count)?;
    // Left nil when unknown, so Lua can tell "no data" from "zero" and fall
    // back to its own estimate rather than dividing by zero.
    if let Some(w) = stats.ctx_window {
        ctx.set("ctx_window", w)?;
    }
    if let Some(m) = stats.max_out {
        ctx.set("max_out", m)?;
    }

    kn9t.set("context", ctx)?;
    globals.set("kn9t", kn9t)?;

    Ok(())
}

/// Collect context stats from App state.
#[allow(clippy::too_many_arguments)]
pub fn collect_stats(
    messages: &[crate::message_handler::Message],
    tokens: &crate::token_tracker::TokenTracker,
    model: &str,
    phase: &str,
    title: &str,
    input_height: u16,
    streaming: bool,
    // Active model's limits, when the server reported them. Passed as the entry
    // rather than two more scalars: this call already takes seven positional
    // arguments, and adding same-typed ones invites silent transposition.
    model_limits: Option<&crate::model_selector::ModelEntry>,
    plugins_ready: bool,
    steering_count: usize,
    queue_count: usize,
) -> ContextStats {
    let mut stats = ContextStats::default();

    for msg in messages {
        match msg.role.as_str() {
            "system" => stats.system_count += 1,
            "user" => stats.user_count += 1,
            "assistant" => stats.assistant_count += 1,
            _ => {}
        }
        // Count tool results in the message
        stats.tool_count += msg.tools.len();
    }

    stats.tokens_in = tokens.tokens_in();
    stats.tokens_out = tokens.tokens_out();
    stats.cache_read = tokens.cache_read();
    stats.cache_write = tokens.cache_write();
    stats.cost = tokens.cost;
    stats.model = model.to_string();
    stats.phase = phase.to_string();
    stats.title = title.to_string();
    stats.input_height = input_height;
    stats.ctx_window = model_limits.and_then(|m| m.ctx_window);
    stats.max_out = model_limits.and_then(|m| m.max_out);
    stats.streaming = streaming;
    stats.plugins_ready = plugins_ready;
    stats.steering_count = steering_count;
    stats.queue_count = queue_count;

    stats
}

