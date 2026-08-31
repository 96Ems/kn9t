//! kn9t-test-plugin — configurable plugin for acceptance tests.
//!
//! Entirely driven by environment variables:
//!
//!   TEST_PLUGIN_HOOK      which hook to register (e.g. "after_tool_call")
//!   TEST_PLUGIN_REPLY     JSON reply body to return for that hook
//!   TEST_PLUGIN_SLEEP_MS  ms to sleep before replying (default 0)
//!
//! GI-1: one workspace dep (kn9t-plugin-sdk).
//! GI-5: no async.

use kn9t_plugin_sdk::traits::PluginHook;
use serde_json::Value;
use std::env;
use std::thread;
use std::time::Duration;

struct EnvHook {
    hook_name: &'static str,
    reply:     Value,
    sleep_ms:  u64,
}

// We need to leak the hook name string since the trait returns `Vec<&'static str>`.
// Safe: the string lives for the duration of the process.
fn leak_string(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

impl PluginHook for EnvHook {
    fn hooks(&self) -> Vec<&'static str> {
        vec![self.hook_name]
    }

    fn call(&self, _hook: &str, _payload: &Value) -> Value {
        if self.sleep_ms > 0 {
            thread::sleep(Duration::from_millis(self.sleep_ms));
        }
        self.reply.clone()
    }
}

fn main() {
    let hook_name_owned = env::var("TEST_PLUGIN_HOOK")
        .unwrap_or_else(|_| "after_tool_call".to_string());
    let reply_str = env::var("TEST_PLUGIN_REPLY")
        .unwrap_or_else(|_| r#"{"action":"keep"}"#.to_string());
    let sleep_ms: u64 = env::var("TEST_PLUGIN_SLEEP_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let reply: Value = serde_json::from_str(&reply_str)
        .unwrap_or_else(|_| serde_json::json!({"action": "keep"}));

    let hook_name = leak_string(hook_name_owned);

    kn9t_plugin_sdk::Plugin::new("kn9t-test-plugin")
        .hook(EnvHook { hook_name, reply, sleep_ms })
        .run();
}
