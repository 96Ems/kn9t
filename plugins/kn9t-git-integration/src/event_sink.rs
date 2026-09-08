//! Event sink that receives UI interactions from the TUI via POST /plugin/{name}/ui_event.
//!
//! This replaces the temp file hack for Lua→Rust communication. The Lua UI calls
//! `kn9t.send_plugin_event(event, data)` which routes through the server to this sink.

use kn9t_plugin_sdk::traits::PluginEventSink;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::poller;

pub struct GitEventSink {
    cwd: Arc<Mutex<Option<PathBuf>>>,
}

impl GitEventSink {
    pub fn new() -> Self {
        Self {
            cwd: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        *self.cwd.lock().unwrap() = Some(cwd);
    }

    pub fn cwd_handle(&self) -> Arc<Mutex<Option<PathBuf>>> {
        Arc::clone(&self.cwd)
    }
}

impl PluginEventSink for GitEventSink {
    fn event_filter(&self) -> Vec<&'static str> {
        vec!["plugin_notification"]
    }

    fn on_event(&self, _kind: &str, event: &Value) {
        let ui_event = event.get("event").and_then(|e| e.as_str()).unwrap_or("");
        let data = event.get("data").cloned().unwrap_or(Value::Null);

        eprintln!("[git-event-sink] Received event: {} with data: {}", ui_event, data);

        match ui_event {
            "request_commit_diff" => {
                if let Some(sha) = data.get("sha").and_then(|s| s.as_str()) {
                    eprintln!("[git-event-sink] Requesting diff for commit: {}", sha);
                    if let Some(cwd) = self.cwd.lock().unwrap().as_ref() {
                        poller::request_commit_diff(cwd, sha.to_string());
                    }
                }
            }
            "clear_commit_diff" => {
                eprintln!("[git-event-sink] Clearing commit diff");
                if let Some(cwd) = self.cwd.lock().unwrap().as_ref() {
                    poller::clear_commit_diff(cwd);
                }
            }
            _ => {
                eprintln!("[git-event-sink] Unknown event: {}", ui_event);
            }
        }
    }
}
