//! R-PLUG-040 / R-PLUG2-040 — newline-delimited JSON codec (protocol v2).
//!
//! Host sends `HostMsg`; plugin sends `PluginMsg`.
//! Every message is exactly one JSON object on one `\n`-terminated line.
//! V2 adds: `HostMsg::Cancel`, `PluginMsg::Chunk`, `PluginMsg::Done`.
//! V1 plugins (no `streaming`/`cancelable` capability) use only `Hook`/`Result`.

use kn9t_core::{HookName, ToolSpec};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};

// ── wire types ────────────────────────────────────────────────────────────────

/// Messages the host sends to the plugin.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    /// Initial handshake — always the first message.
    Hello { proto: u32, kn9t: String },
    /// Hook or tool/provider invocation.
    Hook {
        id: u64,
        hook: String,
        payload: serde_json::Value,
    },
    /// Fire-and-forget bus event (no reply expected).
    Event {
        #[serde(flatten)]
        payload: serde_json::Value,
    },
    /// Abort a specific in-flight call (sent only to `cancelable` plugins).
    Cancel { id: u64 },
    /// Graceful shutdown — plugin must flush stdout and exit.
    Shutdown,
    /// Reply to a `KvGet`, `KvSet`, `KvDel`, or `KvDelScope` request.
    /// For `KvGet`: `value` is `Some(json)` if found, `None` if absent.
    /// For `KvSet`, `KvDel`, `KvDelScope`: `value` is always `None`.
    /// `ok = false` means the operation failed; `error` contains the reason.
    KvResult {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// 96E-17 — reply to a plugin → host API `Request` (host_api capability).
    /// `ok = true` → `result`; `ok = false` → `error`.
    ApiResult {
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Messages the plugin sends to the host.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PluginMsg {
    /// Handshake reply — declares everything the plugin provides.
    Hello {
        name: String,
        #[serde(default)]
        capabilities: Vec<String>,
        #[serde(default)]
        hooks: Vec<String>,
        #[serde(default)]
        tools: Vec<ToolSpec>,
        #[serde(default)]
        events: Vec<String>,
        /// Optional provider declaration.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderDecl>,
    },
    /// Atomic (non-streaming) reply.
    Result {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Partial streaming output — more coming before `Done`.
    Chunk {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Final streaming reply — replaces `Result` for streaming calls.
    Done {
        id: u64,
        #[serde(flatten)]
        body: serde_json::Value,
    },
    /// Fire-and-forget event emission (no reply expected).
    /// The host forwards this to the EventBus for SSE broadcast.
    Event {
        #[serde(flatten)]
        event: serde_json::Value,
    },
    /// Plugin KV request — read a value. Host replies with `HostMsg::KvResult`.
    KvGet { id: u64, scope: String, key: String },
    /// Plugin KV request — upsert a value. Host replies with `HostMsg::KvResult`.
    KvSet {
        id: u64,
        scope: String,
        key: String,
        value: serde_json::Value,
    },
    /// Plugin KV request — delete a value. Host replies with `HostMsg::KvResult`.
    KvDel { id: u64, scope: String, key: String },
    /// Plugin KV request — delete all keys in a scope. Host replies with `HostMsg::KvResult`.
    KvDelScope { id: u64, scope: String },
    /// 96E-17 — plugin → host API request (host_api capability). The host runs
    /// the named operation (e.g. `provider_complete`, `session_read`) and replies
    /// with `HostMsg::ApiResult`. Ops are executed on a worker thread — the host
    /// reader never blocks on a slow op (96E-9).
    Request {
        id: u64,
        op: String,
        payload: serde_json::Value,
    },
    /// Hot re-declaration — plugin updates its tools/hooks/capabilities at runtime.
    /// Fields are optional; only present fields are updated (partial merge).
    /// The host rebuilds the registry and emits `Event::PluginDeclared`.
    Declare {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capabilities: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hooks: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<ToolSpec>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        events: Option<Vec<String>>,
    },
}

/// Provider declaration inside the plugin hello.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderDecl {
    pub id: String,
    pub models: Vec<ProviderModelDecl>,
}

/// One model entry in a provider declaration.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderModelDecl {
    pub id: String,
    pub ctx_window: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<serde_json::Value>,
}

/// Declaration received from a plugin during handshake.
#[derive(Clone, Default)]
pub struct PluginDeclaration {
    pub name: String,
    /// Raw capability strings: `"streaming"`, `"cancelable"`, future flags.
    pub capabilities: Vec<String>,
    pub hooks: Vec<HookName>,
    pub tools: Vec<ToolSpec>,
    pub subscribed_events: Vec<String>,
    pub provider: Option<ProviderDecl>,
}

impl PluginDeclaration {
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
    pub fn is_streaming(&self) -> bool {
        self.has_capability("streaming")
    }
    pub fn is_cancelable(&self) -> bool {
        self.has_capability("cancelable")
    }
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Write one `HostMsg` as a newline-terminated JSON line.
pub fn write_host_msg(w: &mut dyn Write, msg: &HostMsg) -> io::Result<()> {
    let line =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Write one `PluginMsg` as a newline-terminated JSON line.
pub fn write_plugin_msg(w: &mut dyn Write, msg: &PluginMsg) -> io::Result<()> {
    let line =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Read one `PluginMsg` from a buffered reader (blocks until `\n`).
pub fn read_plugin_msg<R: Read>(reader: &mut BufReader<R>) -> io::Result<PluginMsg> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "plugin closed",
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read one `HostMsg` from a buffered reader (blocks until `\n`).
pub fn read_host_msg<R: Read>(reader: &mut BufReader<R>) -> io::Result<HostMsg> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "host closed"));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Parse a hook name string into `HookName`.
pub fn parse_hook_name(s: &str) -> Option<HookName> {
    match s {
        "before_tool_call" => Some(HookName::BeforeToolCall),
        "after_tool_call" => Some(HookName::AfterToolCall),
        "before_request" => Some(HookName::BeforeRequest),
        "should_stop_after_turn" => Some(HookName::ShouldStopAfterTurn),
        "prepare_next_turn" => Some(HookName::PrepareNextTurn),
        "get_steering" => Some(HookName::GetSteering),
        "get_followup" => Some(HookName::GetFollowup),
        "get_api_key" => Some(HookName::GetApiKey),
        _ => None,
    }
}

/// Serialize a `HookName` to its wire string.
pub fn hook_name_str(h: HookName) -> &'static str {
    match h {
        HookName::BeforeToolCall => "before_tool_call",
        HookName::AfterToolCall => "after_tool_call",
        HookName::BeforeRequest => "before_request",
        HookName::ShouldStopAfterTurn => "should_stop_after_turn",
        HookName::PrepareNextTurn => "prepare_next_turn",
        HookName::GetSteering => "get_steering",
        HookName::GetFollowup => "get_followup",
        HookName::GetApiKey => "get_api_key",
    }
}
