//! 96E-17 — the plugin → host API surface (host_api capability).
//!
//! kn9t does NOT embed sub-agents. Instead it opens this host API so external
//! plugins can run their own agent loops: the plugin sends a `request` message
//! and the host executes the operation with its own providers/store/policy.
//!
//! Ops handled by the server (`kn9t-server` `ServerHostApi`):
//! - `provider_complete` — run the session's provider (usage recorded as
//!   `UsageKind::Subagent`), giving the plugin real LLM turns with the session's
//!   model, credentials and cache.
//! - `session_read` — read projected messages by seq range (ID → content
//!   resolution for tool results).
//! - `tool_execute` — execute a registry tool through the normal policy path.
//!
//! The trait lives here so `kn9t-plugin` stays GI-1 clean (it only names
//! `Value`); the concrete implementation is the server's business.

use std::sync::Arc;

/// One host-side operation handler, registered on each `PluginHost` by the
/// server. Must be fast to *dispatch*: the host spawns a worker thread per
/// request so a slow op can never block the plugin reader (96E-9).
pub trait HostApi: Send + Sync {
    /// Handle one plugin request. `session` is the plugin's current session
    /// (set via `PluginHost::set_session` per turn; `None` outside a turn).
    fn handle(
        &self,
        plugin: &str,
        session: Option<&str>,
        op: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

/// Registry of per-host API handlers (None until the server installs one).
pub type ApiHandler = Option<Arc<dyn HostApi>>;