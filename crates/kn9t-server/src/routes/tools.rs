//! `GET /tools` — list registered tools (F9).
//!
//! Exposes the server's `ToolRegistry` (populated from auto-discovered + pinned
//! plugins via `spawn_all_plugins_with_info`). The TUI previously hardcoded
//! `bash/read/write/edit` (`app.rs:188-192`) and a dead `enabled` toggle
//! (`app.rs:1782`) that nothing read. This endpoint is the source of truth for
//! the sidebar, fixing the correctness bug where plugins could register tools the
//! TUI could never display.

use std::sync::Arc;

use crate::http_util::JsonResp;
use crate::state::ServerState;

/// `GET /tools` — return `{tools: [{name, description, hidden}]}`.
pub fn list(state: &Arc<ServerState>) -> JsonResp {
    let registry = state.tools_snapshot();
    let specs = registry.specs();
    let tools: Vec<serde_json::Value> = specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "description": s.description,
                "hidden": s.hidden,
            })
        })
        .collect();
    JsonResp::ok(serde_json::json!({ "tools": tools }))
}
