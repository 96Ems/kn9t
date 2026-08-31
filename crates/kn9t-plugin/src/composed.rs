//! R-PLUG-070 — ComposedHookHost: implements HookHost over a `Vec<Arc<PluginHost>>`.
//!
//! Composition classes:
//! - pipeline: B sees A's output (before_request, after_tool_call, prepare_next_turn)
//! - veto: strictest wins (`Deny > Ask > Allow`), ADR-0008 (before_tool_call)
//! - collect: concat in declared order, host-queue first (get_steering, get_followup)
//! - any-says-stop: should_stop_after_turn
//! - first-non-null: get_api_key

use crate::host::PluginHost;
use kn9t_core::{Content, HookHost, HookVeto, Message, ModelRef, NextTurnPatch, StopReason, Usage};
use std::path::Path;
use std::sync::Arc;

/// Composes multiple `PluginHost` instances into a single `HookHost`.
pub struct ComposedHookHost {
    plugins: Vec<Arc<PluginHost>>,
}

impl ComposedHookHost {
    pub fn new(plugins: Vec<Arc<PluginHost>>) -> Self {
        ComposedHookHost { plugins }
    }

    pub fn plugins(&self) -> &[Arc<PluginHost>] {
        &self.plugins
    }
}

impl HookHost for ComposedHookHost {
    /// Veto composition: **strictest wins** (`Deny > Ask > Allow`), ADR-0008.
    ///
    /// Not first-deny-wins: with `Ask` in the vocabulary, short-circuiting would make the
    /// outcome depend on plugin load order (an early `Ask` would mask a later `Deny`). Every
    /// plugin is therefore consulted and the most restrictive reply wins.
    ///
    /// `Replace` is the exception that still short-circuits: it rewrites the arguments every
    /// subsequent plugin would be judging, so continuing would consult them on stale input.
    fn before_tool_call(&self, tool: &str, args: &serde_json::Value, cwd: &Path) -> HookVeto {
        let mut strictest = HookVeto::Allow;
        for plugin in &self.plugins {
            let result = plugin.before_tool_call(tool, args, cwd);
            if matches!(result, HookVeto::Replace { .. }) {
                return result;
            }
            if result.severity() > strictest.severity() {
                strictest = result;
            }
        }
        strictest
    }

    /// Pipeline composition: each plugin sees the previous plugin's output.
    fn after_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        mut result: Vec<Content>,
    ) -> Vec<Content> {
        eprintln!("[DEBUG ComposedHookHost::after_tool_call] tool={}, {} plugins", tool, self.plugins.len());
        for (i, plugin) in self.plugins.iter().enumerate() {
            eprintln!("[DEBUG ComposedHookHost::after_tool_call] plugin[{}]='{}' has_hook={}",
                i, plugin.declaration.name, plugin.has_hook(kn9t_core::HookName::AfterToolCall));
            result = plugin.after_tool_call(tool, args, result);
        }
        result
    }

    /// Pipeline composition: each plugin sees the previous plugin's output.
    fn before_request(
        &self,
        mut msgs: Vec<Message>,
        model: &ModelRef,
        system: Option<&str>,
    ) -> Vec<Message> {
        for plugin in &self.plugins {
            msgs = plugin.before_request(msgs, model, system);
        }
        msgs
    }

    /// Any-says-stop composition.
    fn should_stop_after_turn(&self, stop: StopReason, usage: &Usage, turn: u32) -> bool {
        for plugin in &self.plugins {
            if plugin.should_stop_after_turn(stop, usage, turn) {
                return true;
            }
        }
        false
    }

    /// Pipeline composition.
    fn prepare_next_turn(&self, stop: StopReason, usage: &Usage) -> NextTurnPatch {
        let mut patch = NextTurnPatch::default();
        for plugin in &self.plugins {
            let p = plugin.prepare_next_turn(stop, usage);
            // Later plugins override earlier ones (pipeline)
            if p.model.is_some() {
                patch.model = p.model;
            }
            if p.thinking.is_some() {
                patch.thinking = p.thinking;
            }
        }
        patch
    }

    /// Collect composition: concat in declared order.
    fn get_steering(&self) -> Vec<Message> {
        eprintln!("[DEBUG ComposedHookHost::get_steering] {} plugins", self.plugins.len());
        let mut out = Vec::new();
        for (i, plugin) in self.plugins.iter().enumerate() {
            eprintln!("[DEBUG ComposedHookHost::get_steering] plugin[{}] = '{}', has get_steering = {}",
                i, plugin.declaration.name, plugin.has_hook(kn9t_core::HookName::GetSteering));
            let msgs = plugin.get_steering();
            eprintln!("[DEBUG ComposedHookHost::get_steering] plugin[{}] returned {} messages", i, msgs.len());
            out.extend(msgs);
        }
        eprintln!("[DEBUG ComposedHookHost::get_steering] total = {} messages", out.len());
        out
    }

    /// Collect composition: concat in declared order.
    fn get_followup(&self) -> Vec<Message> {
        let mut out = Vec::new();
        for plugin in &self.plugins {
            out.extend(plugin.get_followup());
        }
        out
    }

    /// First non-null wins.
    fn get_api_key(&self, provider: &str) -> Option<String> {
        for plugin in &self.plugins {
            if let Some(key) = plugin.get_api_key(provider) {
                return Some(key);
            }
        }
        None
    }
}
