//! kn9t-plugin — Stage 08: stdio plugin host, 8 hooks, subagent spawn.
//!
//! GI-1: depends only on `kn9t-core` (plus `serde_json` which is not a workspace member).
//! GI-5: no tokio, no async fn, no .await.

pub mod codec;
pub mod composed;
pub mod config;
pub mod host;
pub mod host_api;
pub mod remote_compactor;
pub mod remote_provider;
pub mod remote_tool;
pub mod spawn_tool;

pub use codec::PluginDeclaration;
pub use composed::ComposedHookHost;
pub use config::{filter_configs, PluginConfig, SubagentConfig};
pub use host::PluginHost;
pub use host_api::HostApi;
pub use remote_compactor::RemoteCompactor;
pub use remote_provider::RemoteProvider;
pub use remote_tool::RemoteTool;
pub use spawn_tool::SpawnTool;

// ── NoOpPluginKv ──────────────────────────────────────────────────────────────

use kn9t_core::{PluginKv, StoreErr};

/// A no-op `PluginKv` implementation — all reads return `None`, all writes succeed silently.
///
/// Use in tests and anywhere a real `SqliteStore` is not available.
pub struct NoOpPluginKv;

impl PluginKv for NoOpPluginKv {
    fn kv_get(&self, _plugin: &str, _scope: &str, _key: &str) -> Result<Option<serde_json::Value>, StoreErr> {
        Ok(None)
    }
    fn kv_set(&self, _plugin: &str, _scope: &str, _key: &str, _value: &serde_json::Value) -> Result<(), StoreErr> {
        Ok(())
    }
    fn kv_del(&self, _plugin: &str, _scope: &str, _key: &str) -> Result<(), StoreErr> {
        Ok(())
    }
    fn kv_del_scope(&self, _plugin: &str, _scope: &str) -> Result<(), StoreErr> {
        Ok(())
    }
}
