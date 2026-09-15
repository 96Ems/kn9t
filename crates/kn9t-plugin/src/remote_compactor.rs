//! RemoteCompactor delegates compaction to a plugin subprocess.
//! The plugin receives a span and model, runs its own agent turn if needed,
//! and returns a CompactionPlan. CallId validation stays host-side.

use crate::host::PluginHost;
use kn9t_core::{CompactSpan, CompactionPlan, Compactor, HandoffPlanData, Message, ModelRef};
use std::sync::Arc;
use std::time::Duration;

/// Plugin compaction timeout — the plugin may run several LLM turns.
const COMPACTOR_TIMEOUT: Duration = Duration::from_secs(300);

/// Delegates `compactor.compact` to the plugin subprocess.
pub struct RemoteCompactor {
    host: Arc<PluginHost>,
}

impl RemoteCompactor {
    pub fn new(host: Arc<PluginHost>) -> Self {
        RemoteCompactor { host }
    }
}

impl Compactor for RemoteCompactor {
    fn compact(&self, span: CompactSpan, model: &ModelRef) -> Result<CompactionPlan, String> {
        // Session ID included in payload so plugin can use host_api ops.
        let payload = serde_json::json!({
            "session": self.host.session_id(),
            "model": model,
            "replaced": {
                "start": span.replaced.start,
                "end": span.replaced.end,
            },
        });

        let body = self
            .host
            .call_raw_hook_str("compactor_compact", payload, COMPACTOR_TIMEOUT)
            .map_err(|e| format!("compactor plugin: {e}"))?;

        if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }

        let summary: Message = body
            .get("summary")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| "compactor reply missing valid summary message".to_string())?;

        let handoff: Option<HandoffPlanData> = match body.get("handoff").cloned() {
            Some(value) if !value.is_null() => Some(
                serde_json::from_value(value)
                    .map_err(|e| format!("compactor handoff malformed: {e}"))?,
            ),
            _ => None,
        };

        Ok(CompactionPlan { summary, handoff })
    }
}
