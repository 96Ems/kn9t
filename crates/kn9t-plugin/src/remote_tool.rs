//! R-PLUG-050 — RemoteTool: a tool declared by a plugin, exposed as a `Tool` trait object.

use crate::host::PluginHost;
use kn9t_core::{Cancel, Content, LiveEvent, Tool, ToolCtx, ToolErr, ToolOutput, ToolSpec};
use std::sync::Arc;
use std::time::Duration;

/// A tool declared by a plugin during handshake. Forwarded to the plugin via
/// the hook wire when the ReAct loop dispatches it.
pub struct RemoteTool {
    spec: ToolSpec,
    host: Arc<PluginHost>,
    parallel_safe: bool,
    /// The owning plugin's declared name.
    ///
    /// Held by value rather than read from `host.declaration` on each call, and
    /// that is sound rather than merely convenient: `PluginMsg::Declare` carries
    /// only `capabilities`/`hooks`/`tools`/`events`, so a hot re-declare cannot
    /// rename a plugin. The name is immutable for the host's lifetime.
    ///
    /// This is also what lets `Tool::plugin` return `Option<&str>`: a `&str`
    /// cannot be borrowed out of `Arc<Mutex<PluginDeclaration>>`, and taking the
    /// lock on every call would mean contending a mutex during render for a value
    /// that never changes. Do not "fix" this into a lock read.
    plugin_name: String,
}

impl RemoteTool {
    pub fn new(spec: ToolSpec, host: Arc<PluginHost>) -> Self {
        let parallel_safe = spec
            .schema
            .get("x-parallel-safe")
            .and_then(|v: &serde_json::Value| v.as_bool())
            .unwrap_or(false);
        let plugin_name = host.name();
        RemoteTool {
            spec,
            host,
            parallel_safe,
            plugin_name,
        }
    }
}

impl Tool for RemoteTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(
        &self,
        args: &serde_json::Value,
        ctx: &ToolCtx,
        cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr> {
        let payload = serde_json::json!({
            "tool": self.spec.name,
            "args": args,
            // 96E-17: the calling session — plugin tools that spawn sessions
            // (fork/prompt) need it. Set by the server per turn via TLS.
            "session": self.host.session_id(),
        });

        // Clone what we need for the closure.
        let bus = ctx.bus.clone();
        let call_id = ctx.call_id.clone();

        // Tool calls can be long-running (bash has 120s default, but builds can take longer).
        // Use 5 minutes as the host timeout; the tool's internal timeout takes precedence.
        // Cancellable: polls `Cancel` every 10ms and sends `HostMsg::Cancel` on fire (`docs/internal/job/instant-cut.md`).
        let result = self.host.call_raw_hook_str_streaming_cancellable(
            "tool_call",
            payload,
            Duration::from_secs(300),
            cancel,
            |chunk| {
                // Plugin sends chunks like {"text": "--- a/file.rs"}
                if let Some(text) = chunk.get("text").and_then(|v| v.as_str()) {
                    bus.emit(LiveEvent::ToolProgress {
                        call_id: call_id.clone(),
                        note: text.to_string(),
                    });
                }
            },
        );

        match result {
            Ok(body) => {
                let is_error = body
                    .get("is_error")
                    .and_then(|v: &serde_json::Value| v.as_bool())
                    .unwrap_or(false);
                
                // Try to parse content array from plugin response.
                let content: Vec<Content> = if let Some(c) = body.get("content") {
                    match serde_json::from_value::<Vec<Content>>(c.clone()) {
                        Ok(parsed) => parsed,
                        Err(e) => {
                            eprintln!("[remote_tool] Failed to parse content: {} (raw: {})", e, c);
                            vec![Content::Text {
                                text: body.to_string(),
                            }]
                        }
                    }
                } else {
                    eprintln!("[remote_tool] No 'content' field in body: {}", body);
                    vec![Content::Text {
                        text: body.to_string(),
                    }]
                };
                
                Ok(ToolOutput {
                    content,
                    details: None,
                    is_error,
                })
            }
            Err(e) => Err(ToolErr(e)),
        }
    }

    fn parallel_safe(&self) -> bool {
        self.parallel_safe
    }

    fn plugin(&self) -> Option<&str> {
        Some(&self.plugin_name)
    }
}
