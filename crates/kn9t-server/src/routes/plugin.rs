//! Plugin management routes.
//!
//! - `GET  /plugin` — inventory: declared name, Running/Stopped, tools (96E-49).
//! - `POST /plugin/{name}/reload` — hot-reload an existing plugin (R-PLUG2-100).
//! - `POST /plugin/{name}/stop` — stop and leave it off, keeping the recipe (96E-47).
//! - `POST /plugin/{name}/start` — respawn a stopped plugin (96E-47).
//! - `POST /plugin/load` — hot-load a new plugin without server restart.
//! - `POST /plugin/{name}/ui_event` — forward a UI interaction to a plugin.
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

/// Request body for POST /plugin/{name}/ui_event.
#[derive(serde::Deserialize)]
pub struct UiEventReq {
    pub session_id: String,
    pub event: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

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

/// POST /plugin/{name}/stop — 96E-47: cut a plugin and leave it off.
///
/// Distinct from `reload`, which always respawns. The spawn recipe is kept, so `start`
/// can bring the same plugin back. The plugin's tools stay in the registry and stay in
/// the `tools` array sent to the model; they are refused at execution time instead
/// (`ServerState::blocked_tools`), so a stop never invalidates the level-1 cache prefix.
pub fn stop(state: &Arc<ServerState>, name: &str) -> Reply {
    match state.stop_plugin(name) {
        Ok(stopped) => JsonResp::ok(serde_json::json!({ "stopped": stopped })).into(),
        Err(e) if e.contains("not found") => JsonResp::error(404, "not_found", &e).into(),
        Err(e) if e.contains("already stopped") => JsonResp::error(409, "conflict", &e).into(),
        Err(e) => JsonResp::error(500, "stop_failed", &e).into(),
    }
}

/// POST /plugin/{name}/start — 96E-47: respawn a stopped plugin from its known recipe.
///
/// A name that was never loaded is a 404, not a silent spawn: bringing a brand new
/// command up is `POST /plugin/load`'s job.
pub fn start(state: &Arc<ServerState>, name: &str) -> Reply {
    match state.start_plugin(name) {
        Ok((started, tools)) => JsonResp::ok(serde_json::json!({
            "started": started,
            "tools": tools
        }))
        .into(),
        Err(e) if e.contains("not found") => JsonResp::error(404, "not_found", &e).into(),
        Err(e) if e.contains("already running") => JsonResp::error(409, "conflict", &e).into(),
        Err(e) => JsonResp::error(500, "start_failed", &e).into(),
    }
}

/// GET /plugin — 96E-49: the plugin inventory (name, running state, tools).
pub fn list(state: &Arc<ServerState>) -> Reply {
    let plugins: Vec<serde_json::Value> = state
        .plugin_inventory()
        .into_iter()
        .map(|(name, running, tools)| {
            serde_json::json!({
                "name": name,
                "state": if running { "running" } else { "stopped" },
                "tools": tools,
            })
        })
        .collect();
    JsonResp::ok(serde_json::json!({ "plugins": plugins })).into()
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
            Ok(loaded) if loaded.is_empty() => JsonResp::ok(serde_json::json!({
                "loaded": [],
                "message": "no new plugins found in config"
            }))
            .into(),
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

        let env: Vec<(String, String)> = body.env.unwrap_or_default().into_iter().collect();

        match state.load_plugin(cmd, env) {
            Ok((name, tools)) => JsonResp::ok(serde_json::json!({
                "loaded": name,
                "tools": tools
            }))
            .into(),
            Err(e) if e.contains("already loaded") => JsonResp::error(409, "conflict", &e).into(),
            Err(e) => JsonResp::error(500, "load_failed", &e).into(),
        }
    }
}

/// POST /plugin/{name}/ui_event — forward a UI interaction to a plugin.
/// The plugin receives this via HostMsg::Event if it subscribed to "ui_interaction".
pub fn ui_event(state: &Arc<ServerState>, plugin_name: &str, body: UiEventReq) -> Reply {
    let payload = serde_json::json!({
        "kind": "ui_interaction",
        "plugin": plugin_name,
        "session_id": body.session_id,
        "event": body.event,
        "data": body.data,
    });

    match state.send_plugin_event(plugin_name, payload) {
        Ok(()) => JsonResp::ok(serde_json::json!({ "sent": true })).into(),
        Err(e) if e.contains("not found") => JsonResp::error(404, "not_found", &e).into(),
        Err(e) => JsonResp::error(500, "send_failed", &e).into(),
    }
}
