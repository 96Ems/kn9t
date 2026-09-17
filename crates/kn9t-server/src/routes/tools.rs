//! `GET /tools` — list registered tools (F9), the source of truth for the TUI sidebar and
//! tools-manager overlay.
//!
//! Each entry carries its owning `plugin` (grouping, toggle-by-plugin) and, with `?session=<id>`,
//! a `disabled` flag from that session's latest `ToolsToggled`. Blocking is enforced at execution
//! (`kn9t-react` `authorize`), so the provider still gets every spec and the level-1 cache prefix
//! is unchanged.

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
