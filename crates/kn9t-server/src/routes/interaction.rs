//! 96E-28 — `POST /ui-respond` — resolve a pending generic interaction.
//!
//! Validates that `id` is actually pending before forwarding — same principle as
//! rejecting undeclared placeholder writes.

use std::sync::Arc;

use crate::api;
use crate::http_util::JsonResp;
use crate::state::ServerState;

/// `POST /ui-respond` — `{id, payload}`. `payload` is opaque JSON the host
/// does not interpret; it is forwarded to the plugin waiting on `id` via the
/// `InteractionRegistry` condvar. Unknown `id` → 400 (not silent ignore).
pub fn respond(state: &Arc<ServerState>, req: api::UiRespondReq) -> JsonResp {
    let payload = req.payload;
    if state.interaction_registry.resolve(req.id, payload.clone()) {
        JsonResp::ok(serde_json::json!({ "responded": req.id, "payload": payload }))
    } else {
        JsonResp::error(400, "unknown_interaction", &format!("no pending interaction with id {}", req.id))
    }
}
