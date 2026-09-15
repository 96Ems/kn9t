//! Plugin hook interface: invocation surface for session lifecycle events.
//! Composition and timeout handling are applied by the react loop, not here.

use crate::message::{Content, Message};
use crate::model::{ModelRef, Thinking};
use crate::usage::{StopReason, Usage};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Before-tool-call veto reply: Allow, Ask (escalate to user), Deny, or Replace arguments.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookVeto {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
    Replace { args: serde_json::Value },
}

impl HookVeto {
    /// Severity for composing multiple plugin replies: Deny > Ask > Allow/Replace.
    pub fn severity(&self) -> u8 {
        match self {
            HookVeto::Allow | HookVeto::Replace { .. } => 0,
            HookVeto::Ask { .. } => 1,
            HookVeto::Deny { .. } => 2,
        }
    }
}

/// Patch applied before next turn's request is built: optional model and thinking changes.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NextTurnPatch {
    pub model: Option<ModelRef>,
    pub thinking: Option<Thinking>,
}

/// Hook host: invokes all lifecycle hooks. Each method is synchronous and blocking.
pub trait HookHost: Send + Sync {
    fn before_tool_call(&self, tool: &str, args: &serde_json::Value, cwd: &Path) -> HookVeto;
    fn after_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &Path,
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

/// No-op hook host: allows every call and makes no changes. Used when no plugins are configured.
pub struct NoopHookHost;

impl HookHost for NoopHookHost {
    fn before_tool_call(&self, _tool: &str, _args: &serde_json::Value, _cwd: &Path) -> HookVeto {
        HookVeto::Allow
    }
    fn after_tool_call(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _cwd: &Path,
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
