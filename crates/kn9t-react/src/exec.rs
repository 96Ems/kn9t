//! Provider attempt + tool batch + store/hook helpers for [`ReactLoop`] (R-RCT-020..130).

use std::thread;

use kn9t_provider_core::{
    Cancel, Content, Decision, Event, HookVeto, Message, ModelRef, MsgId, ProvErr, Request, Role,
    StopReason, Tokens, ToolCall, ToolCtx, Usage, UsageKind,
};

use crate::assembler::{assemble, Assembled};
use crate::loop_::{ReactError, ReactLoop, RunParams};
use crate::turn::Attempt;

impl ReactLoop {
    /// One provider attempt: plan (compaction decided here), before_request hook, stream,
    /// assemble. Classifies the outcome for the turn's attempt loop.
    pub(crate) fn one_attempt(
        &self,
        params: &mut RunParams,
        cancel: &Cancel,
        reminders: &[Message],
        replans: &mut u32,
    ) -> Result<Attempt, ReactError> {
        // R-RCT-020 step 2: plan_request (compaction decided by the store).
        let mut plan = self
            .store
            .plan_request(&params.session)
            .map_err(|e| ReactError::Store(e.0))?;

        // R-RCT-020 step 3 / R-RCT-090: run the compaction sub-turn then re-plan once.
        if plan.compact.is_some() {
            *replans += 1;
            // Emit compaction retry so TUI spinner shows honest phase (fix 4.2: emit after increment)
            self.bus.emit(Event::RetryAttempt { attempt: *replans, max: params.config.max_context_replans, error: "context_overflow".into(), delay_ms: 0, retry_kind: "compaction".into() });
            self.bus.emit(Event::TurnStatus { phase: "retrying".into(), message: format!("context overflow â€” compaction replan {}/{}", *replans, params.config.max_context_replans) });
            if *replans > params.config.max_context_replans {
                return Err(ReactError::CompactionLoop);
            }
            self.run_compaction(params, cancel, plan.compact.take().unwrap())?;
            plan = self
                .store
                .plan_request(&params.session)
                .map_err(|e| ReactError::Store(e.0))?;
            if plan.compact.is_some() {
                // A second compact demand is fatal -- never loop (R-RCT-090).
                return Err(ReactError::CompactionLoop);
            }
        }

        // R-RCT-020 step 1: before_request hook (pipeline, fail open).
        // System prompt comes from RunParams (server-provided), overriding plan.system.
        let system = params.system.as_deref().or(plan.system.as_deref());
        let mut messages = self.hook_before_request(plan.messages, &params.model.r#ref, system);
        // Truncation reminders (R-RCT-070) ride along as extra system messages.
        messages.extend_from_slice(reminders);

        // Use visible_specs() to exclude hidden tools from the system prompt.
        // Hidden tools can still be executed once discovered via meta-tools.
        let tool_specs = self.tools.visible_specs();
        let req = Request {
            model: &params.model,
            system,
            messages: &messages,
            tools: &tool_specs,
            thinking: params.thinking,
            max_tokens: params.max_tokens,
            cache: &plan.cache,
        };

        // R-RCT-020 step 4: stream + assemble.
        // Sync TUI status machine: thinking while provider connects
        self.bus.emit(Event::TurnStatus { phase: "thinking".into(), message: String::new() });
        let stream = match self.provider.stream_with_sink(&req, cancel, Some(self.bus.as_ref())) {
            Ok(s) => {
                // Provider connected, now streaming deltas
                self.bus.emit(Event::TurnStatus { phase: "streaming".into(), message: String::new() });
                s
            },
            Err(ProvErr::ContextOverflow) => return Ok(Attempt::ContextOverflow),
            Err(ProvErr::Truncated) => return Ok(Attempt::Truncated),
            Err(e) => {
                if cancel.cancelled() {
                    self.bus.emit(Event::TurnStatus { phase: "aborted".into(), message: String::new() });
                    return Ok(Attempt::AbortedInStream(estimated_assembled(&params.model.r#ref)));
                }
                self.bus.emit(Event::TurnStatus { phase: "failed".into(), message: format!("{e:?}") });
                self.bus.emit(Event::Error { message: format!("provider failed: {e:?}") });
                return Err(ReactError::Provider(e.to_string()));
            }
        };

        match assemble(stream, self.bus.as_ref()) {
            Ok(mut a) => {
                a.usage.model = params.model.r#ref.clone();
                if cancel.cancelled() {
                    self.bus.emit(Event::TurnStatus { phase: "aborted".into(), message: String::new() });
                    Ok(Attempt::AbortedInStream(a))
                } else {
                    // assemble emitted TextDelta/ThinkingDelta; phase inferred but emit idle briefly before Complete
                    Ok(Attempt::Completed(a))
                }
            }
            Err(ProvErr::ContextOverflow) => Ok(Attempt::ContextOverflow),
            Err(ProvErr::Truncated) => Ok(Attempt::Truncated),
            Err(e) => {
                if cancel.cancelled() {
                    self.bus.emit(Event::TurnStatus { phase: "aborted".into(), message: String::new() });
                    let est = estimated_assembled(&params.model.r#ref);
                    Ok(Attempt::AbortedInStream(est))
                } else {
                    self.bus.emit(Event::TurnStatus { phase: "failed".into(), message: format!("provider stream failed mid-stream: {e:?}") });
                    self.bus.emit(Event::Error { message: format!("provider stream failed: {e:?}") });
                    Err(ReactError::Provider(e.to_string()))
                }
            }
        }
    }

    /// R-RCT-090 / R-RCT-095: the compaction summarize sub-turn. Uses `UsageKind::Compaction`,
    /// never `Main`. The loop is the only component that calls a provider or emits usage.
    fn run_compaction(
        &self,
        params: &RunParams,
        cancel: &Cancel,
        span: kn9t_provider_core::CompactSpan,
    ) -> Result<(), ReactError> {
        // Interim compaction prompt (SPEC-OPEN sec.18.1). Wording not frozen.
        let mut msgs = span.messages.clone();
        msgs.push(Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text {
                text: "Summarize the conversation so far, preserving decisions, file paths, \
                       and open tasks, so it can replace the older messages."
                    .to_string(),
            }],
            silent: false,
        });
        let no_tools = Vec::new();
        let no_cache = Vec::new();
        let req = Request {
            model: &params.model,
            system: None,
            messages: &msgs,
            tools: &no_tools,
            thinking: params.thinking,
            max_tokens: params.max_tokens,
            cache: &no_cache,
        };
        self.bus.emit(Event::TurnStatus { phase: "thinking".into(), message: String::new() });
        let stream = self
            .provider
            .stream_with_sink(&req, cancel, Some(self.bus.as_ref()))
            .map_err(|e| ReactError::Provider(e.to_string()))?;
        let mut a = assemble(stream, self.bus.as_ref())
            .map_err(|e| ReactError::Provider(e.to_string()))?;
        a.usage.model = params.model.r#ref.clone();

        // Step 2: UsageRecorded { kind: Compaction }.
        self.record_usage(params, &a.usage, UsageKind::Compaction, !a.usage_reported)?;
        // Step 3: Compacted { replaced, summary }.
        self.append(
            params,
            Event::Compacted {
                seq: 0,
                replaced: span.replaced,
                summary: a.message,
            },
        )?;
        Ok(())
    }

    /// R-RCT-130 / DESIGN sec.11.2: run one tool batch. `parallel_safe` tools may run on OS
    /// threads; unsafe tools run sequentially. Results are returned in the model's call
    /// order regardless of completion order. Each call passes before_tool_call (fail
    /// closed) which decides allow/ask/deny, then executes, then after_tool_call.
    pub(crate) fn run_tool_batch(
        &self,
        params: &RunParams,
        calls: &[ToolCall],
        cancel: &Cancel,
    ) -> Vec<Content> {
        // Decide each call up front (hooks, ADR-0008) preserving order; then execute.
        let mut plans: Vec<CallPlan> = Vec::with_capacity(calls.len());
        for call in calls {
            plans.push(self.authorize(params, call));
        }

        // Execute: split into parallel-safe (run concurrently) and the rest (sequential),
        // but always collect results back into call order.
        let mut results: Vec<Option<Content>> = vec![None; calls.len()];

        // Launch parallel-safe authorized calls on threads.
        // Fix 96E-6: parallel path now returns raw inner content + is_error;
        // after_tool_call is applied after join in sequential order, so both
        // paths share the identical before/execute/after lifecycle.
        let mut handles: Vec<(
            usize,
            String,
            serde_json::Value,
            kn9t_provider_core::CallId,
            thread::JoinHandle<(Vec<Content>, bool)>,
        )> = Vec::new();
        for (i, plan) in plans.iter().enumerate() {
            if let CallPlan::Execute { args } = plan {
                let call = &calls[i];
                if let Some(tool) = self.tools.get(&call.name) {
                    if tool.parallel_safe() {
                        let tool = tool.clone();
                        let ctx = ToolCtx {
                            cwd: params.cwd.clone(),
                            read: params.read_map.clone(),
                            bus: self.bus.clone(),
                            call_id: call.id.clone(),
                        };
                        let cancel = cancel.clone();
                        let args = args.clone();
                        let name = call.name.clone();
                        let id = call.id.clone();
                        let bus = self.bus.clone();
                        handles.push((
                            i,
                            name.clone(),
                            args.clone(),
                            id.clone(),
                            thread::spawn(move || {
                                bus.emit(Event::ToolStarted { call_id: id.clone(), name: name.clone() });
                                let out = tool.execute(&args, &ctx, &cancel);
                                let (inner, is_error) = match out {
                                    Ok(o) => (o.content, o.is_error),
                                    Err(e) => (vec![Content::Text { text: e.0 }], true),
                                };
                                bus.emit(Event::ToolFinished { call_id: id.clone(), is_error });
                                (inner, is_error)
                            }),
                        ));
                    }
                }
            }
        }

        // Sequential pass for everything not launched on a thread.
        let launched: std::collections::HashSet<usize> = handles.iter().map(|(i, _, _, _, _)| *i).collect();
        for (i, call) in calls.iter().enumerate() {
            if launched.contains(&i) {
                continue;
            }
            results[i] = Some(self.execute_one(params, call, &plans[i], cancel));
        }

        // Join parallel handles in call order and apply after_tool_call sequentially.
        for (i, name, args, id, h) in handles {
            let content = match h.join() {
                Ok((inner, is_error)) => {
                    let patched = self.hook_after_tool_call(&name, &args, inner);
                    Content::ToolResult { id, content: patched, is_error }
                }
                Err(_) => synth_error(&id, "tool thread panicked"),
            };
            results[i] = Some(content);
        }

        results.into_iter().map(|r| r.expect("every slot filled")).collect()
    }

    /// Sequential execution of one call given its authorization plan.
    fn execute_one(
        &self,
        params: &RunParams,
        call: &ToolCall,
        plan: &CallPlan,
        cancel: &Cancel,
    ) -> Content {
        match plan {
            CallPlan::Deny(reason) => synth_error(&call.id, reason),
            CallPlan::Execute { args } => {
                if cancel.cancelled() {
                    // R-RCT-060: a call that never ran gets a synthesized aborted result.
                    return synth_error(&call.id, "aborted by user");
                }
                let Some(tool) = self.tools.get(&call.name) else {
                    return synth_error(&call.id, &format!("unknown tool `{}`", call.name));
                };
                self.bus.emit(Event::ToolStarted {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                });
                let ctx = ToolCtx {
                    cwd: params.cwd.clone(),
                    read: params.read_map.clone(),
                    bus: self.bus.clone(),
                    call_id: call.id.clone(),
                };
                let out = tool.execute(args, &ctx, cancel);
                let (inner, is_error) = match out {
                    Ok(o) => (o.content, o.is_error),
                    Err(e) => (vec![Content::Text { text: e.0 }], true),
                };
                self.bus.emit(Event::ToolFinished {
                    call_id: call.id.clone(),
                    is_error,
                });
                // after_tool_call (pipeline, keep original on failure).
                let patched = self.hook_after_tool_call(&call.name, args, inner);
                Content::ToolResult {
                    id: call.id.clone(),
                    content: patched,
                    is_error,
                }
            }
        }
    }

    /// ADR-0008 â€” the policy plugin decides, this routes. `before_tool_call` yields
    /// `Allow`/`Ask`/`Deny`/`Replace` (strictest-wins across plugins, `composed.rs`) and
    /// kn9t no longer re-derives a verdict of its own: there is no classifier and no
    /// effects combiner left. `Ask` is handed to the `Approver`, which owns the prompt.
    ///
    /// Failure posture (DESIGN Â§13.5) is unchanged: a hook that errors or times out yields
    /// `Deny` â€” a policy that cannot answer is not permission. That is distinct from *no
    /// policy installed*, which yields `Allow` (ADR-0008 decision 5).
    fn authorize(&self, params: &RunParams, call: &ToolCall) -> CallPlan {
        // Â§4.1 treats `args_json` as cache-critical verbatim provider bytes, so a parse
        // failure here is a real defect (provider sent malformed JSON, or the bytes were
        // corrupted in transit). Surface it instead of silently substituting Null, which
        // would present the tool with empty args and produce a confusing downstream error.
        let args: serde_json::Value = match serde_json::from_str(&call.args_json) {
            Ok(v) => v,
            Err(e) => {
                self.bus.emit(Event::Error {
                    message: format!(
                        "tool '{}' (call {}): malformed args_json: {e}",
                        call.name, call.id.0
                    ),
                });
                serde_json::Value::Null
            }
        };
        match self.hook_before_tool_call(&call.name, &args, &params.cwd) {
            HookVeto::Allow => CallPlan::Execute { args },
            HookVeto::Deny { reason } => CallPlan::Deny(reason),
            HookVeto::Ask { reason } => self.request_approval(params, call, args, &reason),
            // `Replace` permits the call with rewritten arguments. The plugin that rewrote
            // them has already judged them, so this does not re-ask.
            HookVeto::Replace { args: new_args } => CallPlan::Execute { args: new_args },
        }
    }

    /// ADR-0008 â€” hand an `Ask` to the approval mechanism and translate the user's answer.
    ///
    /// The `Approver` blocks this thread until `POST /approve` arrives (or the scope cache
    /// answers immediately), so no polling and no extra state machine here.
    fn request_approval(
        &self,
        params: &RunParams,
        call: &ToolCall,
        args: serde_json::Value,
        reason: &str,
    ) -> CallPlan {
        // The approver echoes the call back to the user, so give it the arguments actually
        // being dispatched (a `Replace` may have rewritten them). Local to this request and
        // never persisted, so re-serializing here cannot disturb the cached prefix
        // (R-CORE-062 concerns the durable `args_json`, not this view).
        let dispatch = ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            args_json: args.to_string(),
        };
        match self.approver.request(&dispatch, &params.cwd, reason) {
            Decision::Allow => CallPlan::Execute { args },
            Decision::Deny { reason } => CallPlan::Deny(reason),
            Decision::HardDeny { reason } => CallPlan::Deny(reason),
            // The approver resolves `Ask` internally; seeing it here means no answer could
            // be obtained (no sink, non-interactive run). Fail closed.
            Decision::Ask => CallPlan::Deny("approval required".to_string()),
        }
    }
}

/// Authorization outcome for one call.
enum CallPlan {
    Execute { args: serde_json::Value },
    Deny(String),
}

/// Turn a tool result into `Content` (for parallel path: returns the raw result content and
/// error flag; after_tool_call is applied on the sequential path by the caller).
#[allow(dead_code)]
fn tool_result_content(
    id: &kn9t_provider_core::CallId,
    out: Result<kn9t_provider_core::ToolOutput, kn9t_provider_core::ToolErr>,
) -> (Content, bool) {
    match out {
        Ok(o) => {
            let is_error = o.is_error;
            (
                Content::ToolResult {
                    id: id.clone(),
                    content: o.content,
                    is_error,
                },
                is_error,
            )
        }
        Err(e) => (synth_error(id, &e.0), true),
    }
}

/// A synthesized `is_error` tool result so no `ToolCall` is left without its `ToolResult`
/// (DESIGN sec.7.5 invariant; R-RCT-060).
fn synth_error(id: &kn9t_provider_core::CallId, msg: &str) -> Content {
    Content::ToolResult {
        id: id.clone(),
        content: vec![Content::Text {
            text: msg.to_string(),
        }],
        is_error: true,
    }
}

/// A zeroed, estimated `Assembled` used when the stream was cut before any usage arrived
/// and no message survives (R-RCT-050).
fn estimated_assembled(model: &ModelRef) -> Assembled {
    Assembled {
        message: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: Vec::new(),
            silent: false,
        },
        usage: Usage {
            tokens: Tokens::default(),
            model: model.clone(),
        },
        stop: StopReason::Aborted,
        usage_reported: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synth_error_creates_tool_result() {
        let call_id = kn9t_provider_core::CallId("call-123".into());
        let result = synth_error(&call_id, "something failed");

        match result {
            Content::ToolResult { id, content, is_error } => {
                assert_eq!(id.0, "call-123");
                assert!(is_error);
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Content::Text { text } => assert_eq!(text, "something failed"),
                    _ => panic!("expected Text content"),
                }
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn test_estimated_assembled_has_aborted_stop() {
        let model = ModelRef {
            provider: "test".into(),
            id: "test-model".into(),
        };

        let assembled = estimated_assembled(&model);

        // StopReason doesn't implement Debug, use matches! instead
        assert!(matches!(assembled.stop, StopReason::Aborted));
        assert!(!assembled.usage_reported);
        assert!(assembled.message.content.is_empty());
        // Role doesn't implement Debug, use matches! instead
        assert!(matches!(assembled.message.role, Role::Assistant));
    }

    #[test]
    fn test_estimated_assembled_copies_model() {
        let model = ModelRef {
            provider: "anthropic".into(),
            id: "claude-3".into(),
        };

        let assembled = estimated_assembled(&model);

        assert_eq!(assembled.usage.model.provider, "anthropic");
        assert_eq!(assembled.usage.model.id, "claude-3");
    }
}
