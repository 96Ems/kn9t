//! `POST /plugin/{name}/reload` — hot-reload a plugin (R-PLUG2-100).
//!
//! Steps, per `spec/08b-plugin-redesign.md` R-PLUG2-100:
//! 1. `cancel` for every in-flight call on that plugin.
//! 2. wait up to `before_tool_call` timeout for `done` replies.
//! 3. `shutdown`, close write pipe.
//! 4. respawn from the same `cmd`.
//! 5. re-handshake; re-register tools, provider, hooks, event subscriptions.
//!
//! In-flight calls that miss step 3 get a synthetic error at the call site
//! (the pending channel is dropped → `disconnected`).

use std::sync::Arc;

use crate::http_util::{JsonResp, Reply};
use crate::state::ServerState;

/// POST /plugin/{name}/reload
pub fn reload(state: &Arc<ServerState>, name: &str) -> Reply {
    match state.reload_plugin(name) {
        Ok((declared, tools)) => JsonResp::ok(serde_json::json!({
            "reloaded": declared,
            "tools": tools
        }))
        .into(),
        Err(e) if e.contains("not found") => JsonResp::error(404, "not_found", &e).into(),
        Err(e) => {
            // respawn failure etc.
            JsonResp::error(500, "reload_failed", &e).into()
        }
    }
}
