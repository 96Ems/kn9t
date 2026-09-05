//! Plugin management routes.
//!
//! - `POST /plugin/{name}/reload` — hot-reload an existing plugin (R-PLUG2-100).
//! - `POST /plugin/load` — hot-load a new plugin without server restart.
//!
//! Reload steps, per `spec/08b-plugin-redesign.md` R-PLUG2-100:
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

/// Request body for POST /plugin/load.
#[derive(serde::Deserialize)]
pub struct LoadPluginReq {
    /// Command + args to spawn the plugin. Required unless `from_config` is true.
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    /// Environment variables to inject.
    #[serde(default)]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// If true, re-read config.toml and load any new [[plugin]] entries.
    #[serde(default)]
    pub from_config: bool,
}

/// POST /plugin/load — hot-load a new plugin.
pub fn load(state: &Arc<ServerState>, body: LoadPluginReq) -> Reply {
    if body.from_config {
        // Load new plugins from config.toml.
        match state.load_plugins_from_config() {
            Ok(loaded) if loaded.is_empty() => {
                JsonResp::ok(serde_json::json!({
                    "loaded": [],
                    "message": "no new plugins found in config"
                }))
                .into()
            }
            Ok(loaded) => {
                let plugins: Vec<serde_json::Value> = loaded
                    .iter()
                    .map(|(name, tools)| {
                        serde_json::json!({
                            "name": name,
                            "tools": tools
                        })
                    })
                    .collect();
                JsonResp::ok(serde_json::json!({
                    "loaded": plugins
                }))
                .into()
            }
            Err(e) => JsonResp::error(500, "load_failed", &e).into(),
        }
    } else {
        // Load a single plugin from inline cmd.
        let cmd = match body.cmd {
            Some(c) if !c.is_empty() => c,
            _ => {
                return JsonResp::error(
                    400,
                    "bad_request",
                    "either 'cmd' or 'from_config: true' is required",
                )
                .into()
            }
        };

        let env: Vec<(String, String)> = body
            .env
            .unwrap_or_default()
            .into_iter()
            .collect();

        match state.load_plugin(cmd, env) {
            Ok((name, tools)) => JsonResp::ok(serde_json::json!({
                "loaded": name,
                "tools": tools
            }))
            .into(),
            Err(e) if e.contains("already loaded") => {
                JsonResp::error(409, "conflict", &e).into()
            }
            Err(e) => JsonResp::error(500, "load_failed", &e).into(),
        }
    }
}
