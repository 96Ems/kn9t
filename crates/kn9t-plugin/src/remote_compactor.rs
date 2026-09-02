//! 96E-16/17 — RemoteCompactor: a `kn9t_core::Compactor` backed by a plugin
//! that declared the `compactor` capability.
//!
//! The host delegates the `CompactSpan → CompactionPlan` step to the plugin
//! over the standard hook wire (`hook: "compactor_compact"`). The plugin is
//! free to run an agent turn of its own (using the host_api ops:
//! `session_read` + `provider_complete`) and returns the plan as a plain
//! `result`. CallId validation stays host-side (`validate_handoff`) — it can
//! never be bypassed by a buggy or malicious compactor.
//!
//! If no plugin declares the capability, the ReactLoop stays fail-closed
//! (96E-17): no compactor = no compaction = session ends on context overflow.

use crate::host::PluginHost;
use kn9t_core::{CompactionPlan, Compactor, CompactSpan, Content, HandoffPlanData, HandoffSummary, Message, ModelRef, MsgId, Role};
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
        // Session id rides in the payload (TLS session lives on the turn thread,
        // but the plugin needs it to issue session_read / provider_complete ops).
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

// ── test helpers (host-side plan fabrication; used by react acceptance tests) ──

/// Build a `CompactionPlan` from a summary text (kept/verbatim content optional).
#[allow(dead_code)]
pub fn plan_from_text(text: &str) -> CompactionPlan {
    CompactionPlan {
        summary: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: vec![Content::Text { text: text.to_string() }],
            silent: false,
        },
        handoff: None,
    }
}

/// Build a `HandoffPlanData` value from explicit lists (unit-test friendly).
#[allow(dead_code)]
pub fn handoff_from_lists(
    keep: Vec<String>,
    summarize: Vec<(String, String)>,
    drop: Vec<String>,
    resume_actions: Vec<String>,
) -> HandoffPlanData {
    HandoffPlanData {
        keep: keep.into_iter().map(kn9t_core::CallId).collect(),
        summarize: summarize
            .into_iter()
            .map(|(id, summary)| HandoffSummary {
                id: kn9t_core::CallId(id),
                summary,
            })
            .collect(),
        drop_ids: drop.into_iter().map(kn9t_core::CallId).collect(),
        resume_actions,
    }
}