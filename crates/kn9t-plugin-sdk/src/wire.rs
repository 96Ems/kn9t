//! Wire protocol types — self-contained, language-neutral shapes from spec §2.
#![allow(missing_docs)] // Wire types are internal codec details; public API is in traits.rs.
//!
//! These types mirror the JSON framing exactly. Plugin authors never touch them
//! directly; the [`Plugin`](crate::Plugin) main loop handles all encoding and decoding.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};

// ── Host → Plugin ─────────────────────────────────────────────────────────────

/// Every message the host can send to the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    /// First message on every connection. Validate `proto == 1`.
    Hello { proto: u32, kn9t: String },
    /// Hook or tool/provider invocation. Reply with [`PluginMsg::Result`],
    /// or [`PluginMsg::Chunk`] + [`PluginMsg::Done`] if streaming.
    Hook {
        id: u64,
        hook: String,
        payload: serde_json::Value,
    },
    /// Bus event delivered to a subscribed plugin. No reply.
    Event {
        #[serde(flatten)]
        payload: serde_json::Value,
    },
    /// Abort call `id`. Only sent to plugins that declared `"cancelable"`.
    Cancel { id: u64 },
    /// Graceful shutdown. Flush stdout and exit.
    Shutdown,
    /// Reply to a `KvGet`, `KvSet`, `KvDel`, or `KvDelScope` request.
    /// For `KvGet`: `value` is `Some(json)` if found, `None` if absent.
    /// For write/delete operations: `value` is always `None`.
    /// `ok = false` means the operation failed; `error` carries the reason.
    KvResult {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// 96E-17 — reply to a plugin → host API `Request`.
    ApiResult {
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

// ── Plugin → Host ─────────────────────────────────────────────────────────────

/// Everything the plugin can send to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PluginMsg {
    /// Handshake reply. Sent once, immediately after the host hello.
    Hello {
        name: String,
        #[serde(default)]
        capabilities: Vec<String>,
        #[serde(default)]
        hooks: Vec<String>,
        #[serde(default)]
        tools: Vec<ToolSpec>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderDecl>,
        #[serde(default)]
        events: Vec<String>,
    },
    /// Atomic (non-streaming) reply to a hook or tool call.
    Result {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Partial streaming output for call `id`. More chunks or a `Done` follow.
    Chunk {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Final streaming reply for call `id`. No more messages for this id.
    Done {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Read a value from the host's persistent KV store.
    /// `scope` partitions keys — use `""` for global or a `session_id` for
    /// session-scoped state.  Host replies with [`HostMsg::KvResult`].
    KvGet { id: u64, scope: String, key: String },
    /// Upsert a value in the host's persistent KV store.
    /// Host replies with [`HostMsg::KvResult`].
    KvSet {
        id: u64,
        scope: String,
        key: String,
        value: serde_json::Value,
    },
    /// Delete a single key from the host's persistent KV store.
    /// Host replies with [`HostMsg::KvResult`].
    KvDel { id: u64, scope: String, key: String },
    /// Delete all keys in `scope` from the host's persistent KV store.
    /// Host replies with [`HostMsg::KvResult`].
    KvDelScope { id: u64, scope: String },
    /// 96E-17 — plugin → host API request (host_api capability). Host replies with `HostMsg::ApiResult`.
    Request {
        id: u64,
        op: String,
        payload: serde_json::Value,
    },
}

// ── Shared data types ─────────────────────────────────────────────────────────

/// Effect kind for ADR-0002 (server decides risk from declared effects).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Shell,
    FsRead,
    FsWrite,
    Network,
}

/// One declared side-effect of a tool argument.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Effect {
    /// JSON field name or pointer (e.g. `"cmd"` or `"/command"`).
    pub field: String,
    pub kind: EffectKind,
}

/// Default policy when no user config exists for this tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    /// Safe tool, auto-allow without prompting (e.g. "read", "glob", "grep").
    Allow,
    /// Needs user approval by default (most tools).
    #[default]
    Ask,
    /// Blocked unless explicitly allowed in user config.
    Deny,
}

/// Policy declaration for a tool. Plugins declare this to control approval behavior
/// without hardcoding tool names in the server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolPolicy {
    /// Field to extract from args for pattern matching.
    /// e.g. "cmd" for bash, "path" for read/write, "url" for web_fetch.
    /// If None, the tool doesn't support pattern matching.
    #[serde(default)]
    pub pattern_field: Option<String>,

    /// Default policy when no user config exists.
    #[serde(default)]
    pub default_policy: DefaultPolicy,

    /// Built-in allow patterns declared by the tool author.
    /// These are checked AFTER user deny patterns but BEFORE user allow patterns,
    /// so users can override them. Example: `["git log *", "git status *"]` for a git tool.
    #[serde(default)]
    pub builtin_allow: Vec<String>,

    /// Built-in deny patterns (always deny, even if user allows).
    /// Example: `["rm -rf /", "sudo *"]` for bash.
    /// These are "hard deny" — no approval prompt, just rejected.
    #[serde(default)]
    pub builtin_deny: Vec<String>,
}

/// Tool declaration sent in the plugin hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name (snake_case). Must be stable across plugin versions.
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema object describing the tool's arguments.
    pub schema: serde_json::Value,
    /// Whether calls to this tool may run in parallel with other tools.
    /// Defaults to `false` — safe tools should declare `true`.
    #[serde(default)]
    pub parallel_safe: bool,
    /// If true, tool is registered but not shown in the initial system prompt.
    /// Used for lazy tool discovery: hidden tools can still be executed once
    /// the agent discovers them via a meta-tool (e.g., `mcp_search_tools`).
    #[serde(default)]
    pub hidden: bool,
    /// Declared effects (ADR-0002). Empty → strictest policy default.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Policy declaration — controls approval behavior.
    /// If absent, uses `DefaultPolicy::Ask` with no pattern matching.
    #[serde(default)]
    pub policy: ToolPolicy,
}

/// Provider declaration sent in the plugin hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDecl {
    /// Short provider identifier, e.g. `"my-llm"`.
    pub id: String,
    /// Models this provider offers.
    pub models: Vec<ModelDecl>,
}

/// One model offered by a provider plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDecl {
    /// Model identifier, e.g. `"my-model-7b"`.
    pub id: String,
    /// Maximum context window in tokens.
    pub ctx_window: u32,
    /// Price per million tokens in USD. Omit if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceDecl>,
}

/// Token prices in USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PriceDecl {
    /// Input (prompt) token price per million.
    #[serde(default)]
    pub input: f64,
    /// Output (completion) token price per million.
    #[serde(default)]
    pub output: f64,
    /// Cache read token price per million.
    #[serde(default)]
    pub cache_read: f64,
    /// Cache write token price per million.
    #[serde(default)]
    pub cache_write: f64,
}

/// Usage counters returned in a provider `done` message.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    /// Input (prompt) tokens consumed.
    #[serde(default)]
    pub input: u64,
    /// Output (completion) tokens generated.
    #[serde(default)]
    pub output: u64,
    /// Cache-read tokens (counted at a lower rate).
    #[serde(default)]
    pub cache_read: u64,
    /// Cache-write tokens (counted at a higher rate).
    #[serde(default)]
    pub cache_write: u64,
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Write one [`HostMsg`] as a newline-terminated JSON line.
pub fn write_host(w: &mut dyn Write, msg: &HostMsg) -> io::Result<()> {
    let s =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Write one [`PluginMsg`] as a newline-terminated JSON line.
pub fn write_plugin(w: &mut dyn Write, msg: &PluginMsg) -> io::Result<()> {
    let s =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(s.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Read one [`HostMsg`] from a buffered reader (blocks until `\n`).
pub fn read_host<R: Read>(r: &mut BufReader<R>) -> io::Result<HostMsg> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "host closed stdin",
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read one [`PluginMsg`] from a buffered reader (blocks until `\n`).
pub fn read_plugin<R: Read>(r: &mut BufReader<R>) -> io::Result<PluginMsg> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "plugin closed stdout",
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
