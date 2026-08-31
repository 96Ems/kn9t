//! Policy info routes — ADR-0008: policy decisions moved to plugin.
//!
//! The server no longer manages policy rules. The `kn9t-policy` plugin reads
//! `~/.kn9t/policy.py` directly. These routes are informational only.
//!
//! Routes:
//! - `GET /policy` — current policy state (informational)

use crate::config;
use crate::http_util::JsonResp;
use crate::state::ServerState;
use std::sync::Arc;

/// GET /policy — current policy state (informational only, plugin decides)
pub fn get_state(_state: &Arc<ServerState>) -> JsonResp {
    match config::get_policy_state() {
        Ok(ps) => JsonResp::ok(serde_json::json!({
            "mode": ps.mode,
            "approvals": ps.approvals,
            "note": "ADR-0008: policy decisions are made by the kn9t-policy plugin. Edit ~/.kn9t/policy.py to customize.",
        })),
        Err(e) => JsonResp::error(500, "config_error", &e),
    }
}
