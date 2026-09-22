//! kn9t-demo-policy — policy gate for screen recordings (stands in for kn9t-policy).
//!
//! Answers `before_tool_call` with the ADR-0008 contract: `ask` on fs writes
//! when DEMO_ASK is set (the TUI shows the approval overlay), `allow` otherwise.

use kn9t_plugin_sdk::{traits::PluginHook, Plugin};
use serde_json::{json, Value};

struct DemoPolicy {
    asked: std::sync::atomic::AtomicBool,
}

impl PluginHook for DemoPolicy {
    fn hooks(&self) -> Vec<&'static str> {
        vec!["before_tool_call"]
    }

    fn call(&self, _hook: &str, payload: &Value) -> Value {
        let ask_mode = std::env::var("DEMO_ASK").is_ok();
        let name = payload
            .get("name")
            .or_else(|| payload.get("tool"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Ask ONCE per run (the first fs write), then get out of the way. Notably
        // we never answer "always": that persists to config.toml, and the config
        // watcher's reload reaps provider-plugin subprocesses mid-prompt — which
        // would kill the scripted demo provider behind this recording.
        let first = !self.asked.swap(true, std::sync::atomic::Ordering::SeqCst);
        let ask = ask_mode && first && (name.is_empty() || name == "write");
        eprintln!(
            "[demo-policy] tool={:?} keys={:?} first={} -> {}",
            name,
            payload.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            first,
            if ask { "ask" } else { "allow" }
        );
        if ask {
            json!({
                "action": "ask",
                "reason": "demo policy: write touches the filesystem — approve to continue",
            })
        } else {
            json!({ "action": "allow" })
        }
    }
}

fn main() {
    Plugin::new("kn9t-demo-policy")
        .hook(DemoPolicy {
            asked: std::sync::atomic::AtomicBool::new(false),
        })
        .run();
}
