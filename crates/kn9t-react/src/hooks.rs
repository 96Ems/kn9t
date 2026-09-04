//! Store append + usage recording + hook invocation with per-hook failure posture
//! (R-RCT-095, R-RCT-110, R-RCT-120).
//!
//! Hook calls are wrapped in [`std::panic::catch_unwind`] so a panicking hook (the test
//! stub, and in production a crashed subprocess surfaced as a panic by the host) triggers
//! the documented fallback and a `HookFailed` event, rather than killing the loop.

use std::panic::{catch_unwind, AssertUnwindSafe};

use kn9t_provider_core::{
    Event, HookName, HookVeto, LiveEvent, Message, ModelRef, NextTurnPatch, StopReason, Usage,
    UsageKind,
};

use crate::loop_::{ReactError, ReactLoop, RunParams};

impl ReactLoop {
    /// R-RCT-020: durable append via the store (assigns seq, commits, returns seq).
    /// 96E-12: durable events must go through the store only; the live bus (`EventSink`)
    /// is transient-only (`LiveEvent`), so we do NOT republish the durable event via
    /// `self.bus.emit`. Durable observers read from the store (or the server's
    /// `SessionBuses::publish` for SSE echo after store commit). Cancellation is never
    /// checked inside an append (R-RCT-040).
    pub(crate) fn append(&self, params: &RunParams, event: Event) -> Result<u64, ReactError> {
        let seq = self
            .store
            .append(&params.session, event)
            .map_err(|e| ReactError::Store(e.0))?;
        Ok(seq)
    }

    /// R-RCT-095: record usage attributed to `kind`. The loop is the ONLY emitter of
    /// `UsageRecorded` (DESIGN sec.3). `estimated` is set when usage was inferred after an
    /// abort cut the stream before real usage arrived (R-CORE-142, R-RCT-050).
    pub(crate) fn record_usage(
        &self,
        params: &RunParams,
        usage: &Usage,
        kind: UsageKind,
        estimated: bool,
    ) -> Result<(), ReactError> {
        let price = params.model.price;
        let cost_micros = kn9t_provider_core::cost_micros(&usage.tokens, &price);
        let cost_usd = cost_micros as f64 / 1_000_000.0;
        self.append(
            params,
            Event::UsageRecorded {
                seq: 0,
                provider: params.model.r#ref.provider.clone(),
                model: params.model.r#ref.id.clone(),
                kind,
                tokens: usage.tokens,
                price_snapshot: price,
                cost_micros,
                cost_usd,
                estimated,
            },
        )?;
        Ok(())
    }

    // ---- hook wrappers: apply the per-hook failure posture (R-RCT-110) ----

    pub(crate) fn hook_before_request(
        &self,
        msgs: Vec<Message>,
        model: &ModelRef,
        system: Option<&str>,
    ) -> Vec<Message> {
        let hooks = self.hooks.clone();
        let orig = msgs.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            hooks.before_request(msgs, model, system)
        })) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::BeforeRequest, "hook panicked");
                orig // fail open: use original
            }
        }
    }

    pub(crate) fn hook_before_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &std::path::Path,
    ) -> HookVeto {
        let hooks = self.hooks.clone();
        match catch_unwind(AssertUnwindSafe(|| hooks.before_tool_call(tool, args, cwd))) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::BeforeToolCall, "hook panicked");
                // fail CLOSED: deny.
                HookVeto::Deny {
                    reason: "before_tool_call hook failed; denied (fail closed)".to_string(),
                }
            }
        }
    }

    pub(crate) fn hook_after_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        result: Vec<kn9t_provider_core::Content>,
    ) -> Vec<kn9t_provider_core::Content> {
        let hooks = self.hooks.clone();
        let orig = result.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            hooks.after_tool_call(tool, args, result)
        })) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::AfterToolCall, "hook panicked");
                orig // keep original
            }
        }
    }

    pub(crate) fn hook_should_stop(&self, stop: StopReason, usage: &Usage, turn: u32) -> bool {
        let hooks = self.hooks.clone();
        match catch_unwind(AssertUnwindSafe(|| {
            hooks.should_stop_after_turn(stop, usage, turn)
        })) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::ShouldStopAfterTurn, "hook panicked");
                false // continue
            }
        }
    }

    pub(crate) fn apply_prepare_next_turn(
        &self,
        params: &mut RunParams,
        stop: StopReason,
        usage: &Usage,
    ) {
        let hooks = self.hooks.clone();
        let patch = match catch_unwind(AssertUnwindSafe(|| hooks.prepare_next_turn(stop, usage))) {
            Ok(p) => p,
            Err(_) => {
                self.hook_failed(HookName::PrepareNextTurn, "hook panicked");
                NextTurnPatch::default() // no change
            }
        };
        if let Some(t) = patch.thinking {
            params.thinking = t;
        }
        if let Some(m) = patch.model {
            // Only the ref changes here; the full ModelSpec swap is the server's job (06).
            params.model.r#ref = m;
        }
    }

    pub(crate) fn collect_steering(&self) -> Vec<Message> {
        let hooks = self.hooks.clone();
        match catch_unwind(AssertUnwindSafe(|| hooks.get_steering())) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::GetSteering, "hook panicked");
                Vec::new() // empty
            }
        }
    }

    pub(crate) fn collect_followup(&self) -> Vec<Message> {
        let hooks = self.hooks.clone();
        match catch_unwind(AssertUnwindSafe(|| hooks.get_followup())) {
            Ok(v) => v,
            Err(_) => {
                self.hook_failed(HookName::GetFollowup, "hook panicked");
                Vec::new() // empty
            }
        }
    }

    fn hook_failed(&self, hook: HookName, reason: &str) {
        self.bus.emit(LiveEvent::HookFailed {
            plugin: "host".to_string(),
            hook,
            reason: reason.to_string(),
        });
    }
}
