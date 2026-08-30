//! R-PLUG-100 — PluginConfig type and project-local plugin guard.
//!
//! Project-local `[[plugin]]` entries in a workspace config are silently ignored
//! (R-PLUG-100). Only user-level plugin configs are loaded.

use serde::{Deserialize, Serialize};

/// Configuration for a single plugin.
#[derive(Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Plugin name (must match the handshake declaration).
    pub name: String,
    /// Path or command to spawn the plugin subprocess.
    pub command: String,
    /// Optional arguments to pass to the plugin subprocess.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether this config came from a project-local source.
    #[serde(default)]
    pub project_local: bool,
}

impl PluginConfig {
    /// R-PLUG-100: returns true if this config should be ignored.
    /// Project-local plugin configs are always ignored.
    pub fn is_ignored(&self) -> bool {
        self.project_local
    }
}

/// Filter a list of plugin configs, removing project-local entries (R-PLUG-100).
pub fn filter_configs(configs: Vec<PluginConfig>) -> Vec<PluginConfig> {
    configs.into_iter().filter(|c| !c.is_ignored()).collect()
}

/// Subagent configuration (R-PLUG-120).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SubagentConfig {
    /// Tool names available to the child agent. `None` = inherit parent set.
    pub tools: Option<Vec<String>>,
}
