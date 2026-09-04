//! 96E-26 reference example — SubagentSpec-based spawn tool
//!
//! Mirrors `ask_user` (96E-28) as the SDK's second reference example, proving the
//! platform's generic primitives (host_api ops + interaction primitive) are
//! sufficient without any built-in tools.
//!
//! ```bash
//! cargo run -p kn9t-plugin-sdk --example subagent_spawn
//! ```
//!
//! In production this binary would be built as `kn9t-subagent-rs` and dropped
//! into `~/.kn9t/plugins/`.

use kn9t_plugin_sdk::{subagent::SubagentTool, Plugin};

fn main() {
    Plugin::new("kn9t-subagent-rs")
        .tool(SubagentTool::new(
            "spawn_subagent",
            "Spawn a subagent to do the task; budget_usd/timeout_s/tool_subset map to host_api, visibility is plugin-consumed",
        ))
        .run();
}
