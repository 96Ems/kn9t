//! Config management routes.
//!
//! - `POST /config/reload` — re-read `config.toml`, swap providers + models
//!   (R-SRV-CFG-100).
//!
//! An action endpoint, not a PATCH (AGENTS.md §11): the operation is a full
//! replacement of the resolved config, and there are no merge semantics to define.
//!
//! The same operation runs automatically from `crate::watch` on file change; this
//! endpoint is the explicit trigger for clients and for tests.

use std::sync::Arc;

use crate::http_util::{JsonResp, Reply};
use crate::state::ServerState;

/// POST /config/reload
pub fn reload(state: &Arc<ServerState>) -> Reply {
    match state.reload_config() {
        Ok((providers, models)) => {
            let default_model = state
                .default_model_snapshot()
                .map(|m| m.r#ref.id)
                .unwrap_or_default();
            JsonResp::ok(serde_json::json!({
                "reloaded": true,
                "providers": providers,
                "models": models,
                "default_model": default_model,
            }))
            .into()
        }
        // A malformed TOML or a config that resolves to no providers is a client
        // error in the file, not a server fault, and the old config is still live.
        Err(e) => JsonResp::error(400, "reload_failed", &e).into(),
    }
}
