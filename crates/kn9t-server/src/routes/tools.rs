//! `GET /tools` — list registered tools (F9).
//!
//! Exposes the server's `ToolRegistry` (populated from auto-discovered + pinned
//! plugins via `spawn_all_plugins_with_info`). This endpoint is the source of
//! truth for the TUI sidebar and the tools-manager overlay.
//!
//! Tools-enable/disable: each entry carries its owning `plugin` (for grouping and
//! toggle-by-plugin) and, when a `?session=<id>` query is supplied, a `disabled`
//! flag reflecting that session's latest `ToolsToggled` state. Blocking is enforced
//! at execution time (see `kn9t-react` `authorize`), so the provider still receives
//! every tool spec and the level-1 cache prefix is unchanged.

use std::sync::Arc;

use crate::http_util::{query_param, JsonResp};
use crate::state::ServerState;
use kn9t_core::{SessionId, Store};

/// `GET /tools[?session=<id>]` — return `{tools: [{name, description, hidden, plugin, disabled}]}`.
pub fn list(state: &Arc<ServerState>, query: &str) -> JsonResp {
    // Per-session disabled set (empty when no session or never toggled).
    let disabled: std::collections::HashSet<String> = query_param(query, "session")
        .and_then(|sid| state.store.snapshot(&SessionId(sid)).ok())
        .map(|snap| snap.disabled_tools.into_iter().collect())
        .unwrap_or_default();

    let registry = state.tools_snapshot();
    let tools: Vec<serde_json::Value> = registry
        .iter()
        .map(|t| {
            let s = t.spec();
            serde_json::json!({
                "name": s.name,
                "description": s.description,
                "hidden": s.hidden,
                "plugin": t.plugin(),
                "disabled": disabled.contains(&s.name),
            })
        })
        .collect();
    JsonResp::ok(serde_json::json!({ "tools": tools }))
}
