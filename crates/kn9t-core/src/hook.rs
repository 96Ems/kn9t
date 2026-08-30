//! R-RCT-100 — the plugin hook surface, defined in `kn9t-core` so both `kn9t-react`
//! (the loop that invokes hooks) and `kn9t-plugin` (the subprocess host that answers
//! them) depend only on this crate (GI-1). The subprocess implementation lives in
//! PLUG/08; this crate defines the trait and its data types, and provides a no-op
//! implementation so the loop runs with zero plugins configured.
//!
//! `HookName` (the durable-event tag, R-CORE-155) lives in `event.rs`; this module adds
//! the invocation surface. Composition and failure posture are DESIGN §13.3–13.5, applied
//! by the loop (RCT R-RCT-110/120), not here.

use crate::message::{Content, Message};
use crate::model::{ModelRef, Thinking};
use crate::usage::{StopReason, Usage};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// R-RCT-100 — reply of the `before_tool_call` veto hook (DESIGN §13.3).
/// `Replace` swaps the tool arguments before dispatch.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookVeto {
    Allow,
    Deny { reason: String },
    Replace { args: serde_json::Value },
}

/// R-RCT-100 — reply of `prepare_next_turn`: an optional model / thinking patch applied
/// before the next turn's request is built (DESIGN §13.3).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NextTurnPatch {
    pub model: Option<ModelRef>,
    pub thinking: Option<Thinking>,
}

/// R-RCT-100 — the eight-hook surface (DESIGN §13.3). Every method is synchronous and
/// blocking (GI-5); a real host applies per-hook timeouts and the failure posture of
/// §13.5. `on_event` is a bus subscription, not a hook, and is absent here.
pub trait HookHost: Send + Sync {
    fn before_tool_call(&self, tool: &str, args: &serde_json::Value, cwd: &Path) -> HookVeto;
    fn after_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        result: Vec<Content>,
    ) -> Vec<Content>;
    fn before_request(
        &self,
        msgs: Vec<Message>,
        model: &ModelRef,
        system: Option<&str>,
    ) -> Vec<Message>;
    fn should_stop_after_turn(&self, stop: StopReason, usage: &Usage, turn: u32) -> bool;
    fn prepare_next_turn(&self, stop: StopReason, usage: &Usage) -> NextTurnPatch;
    fn get_steering(&self) -> Vec<Message>;
    fn get_followup(&self) -> Vec<Message>;
    fn get_api_key(&self, provider: &str) -> Option<String>;
}

/// R-RCT-100 — the do-nothing host: allow every call, change nothing, queue nothing,
/// never stop early. Lets the loop run with no plugins configured. Every default here is
/// the same value the failure posture (§13.5) falls back to, by design.
pub struct NoopHookHost;

impl HookHost for NoopHookHost {
    fn before_tool_call(&self, _tool: &str, _args: &serde_json::Value, _cwd: &Path) -> HookVeto {
        HookVeto::Allow
    }
    fn after_tool_call(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        result: Vec<Content>,
    ) -> Vec<Content> {
        result
    }
    fn before_request(
        &self,
        msgs: Vec<Message>,
        _model: &ModelRef,
        _system: Option<&str>,
    ) -> Vec<Message> {
        msgs
    }
    fn should_stop_after_turn(&self, _stop: StopReason, _usage: &Usage, _turn: u32) -> bool {
        false
    }
    fn prepare_next_turn(&self, _stop: StopReason, _usage: &Usage) -> NextTurnPatch {
        NextTurnPatch::default()
    }
    fn get_steering(&self) -> Vec<Message> {
        Vec::new()
    }
    fn get_followup(&self) -> Vec<Message> {
        Vec::new()
    }
    fn get_api_key(&self, _provider: &str) -> Option<String> {
        None
    }
}
