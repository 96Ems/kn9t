//! Preferences API — get/set user preferences stored in meta table.

use std::sync::Arc;
use crate::http_util::JsonResp;
use crate::state::ServerState;

/// GET /pref/{key} — get a preference value.
pub fn get(state: &Arc<ServerState>, key: &str) -> JsonResp {
    match state.store.get_pref(key) {
        Some(value) => JsonResp::ok(serde_json::json!({ "key": key, "value": value })),
        None => JsonResp::error(404, "not_found", &format!("preference '{key}' not set")),
    }
}

/// PUT /pref/{key} — set a preference value.
pub fn set(state: &Arc<ServerState>, key: &str, value: &str) -> JsonResp {
    match state.store.set_pref(key, value.trim()) {
        Ok(()) => JsonResp::ok(serde_json::json!({ "key": key, "value": value.trim() })),
        Err(e) => JsonResp::error(500, "store_error", &e.0),
    }
}
