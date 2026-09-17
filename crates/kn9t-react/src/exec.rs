//! Provider attempt and tool execution: streaming, batching, and result assembly.

use std::thread;

use kn9t_provider_core::{
    CallId, Cancel, Content, Decision, Event, HookVeto, LiveEvent, Message, ModelRef, MsgId,
    ProvErr, Request, Role, StopReason, Tokens, ToolCall, ToolCtx, ToolRegistry, Usage,
};

use crate::assembler::{assemble, Assembled};
use crate::loop_::{ReactError, ReactLoop, RunParams};
use crate::turn::Attempt;

/// Handle for parallel tool execution: index, name, args, call_id, and result channel.
type ParallelToolHandle = (
    usize,
    String,
    serde_json::Value,
    CallId,
    std::sync::mpsc::Receiver<(Vec<Content>, bool)>,
);

/// Poll interval while collecting a parallel result.
const PARALLEL_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Reason a parallel tool produced no value.
enum ParallelFailure {
    /// The worker thread died (panic) — the channel closed without a send.
    Panicked,
    /// Cancelled, and the tool did not return within [`CANCEL_ABANDON_GRACE`].
    Abandoned,
}

/// Wait for one parallel tool's result.
///
/// Blocks indefinitely while the batch is live: a long-running tool is legitimate and must
/// not be cut short. Once `cancel` fires, the tool is given `grace` to observe it and return;
/// past that the wait gives up. Without this bound a tool that ignores `Cancel` froze
/// `run_tool_batch` for the life of the process (B8).
fn recv_parallel_result(
    rx: &std::sync::mpsc::Receiver<(Vec<Content>, bool)>,
    cancel: &Cancel,
    grace: std::time::Duration,
) -> Result<(Vec<Content>, bool), ParallelFailure> {
    let mut give_up_at: Option<std::time::Instant> = None;
    loop {
        match rx.recv_timeout(PARALLEL_POLL) {
            Ok(v) => return Ok(v),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ParallelFailure::Panicked)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.cancelled() {
                    let deadline =
                        *give_up_at.get_or_insert_with(|| std::time::Instant::now() + grace);
                    if std::time::Instant::now() >= deadline {
                        return Err(ParallelFailure::Abandoned);
                    }
                }
            }
        }
    }
}

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

        // Did the store offer a span to compact this round? If it did not, nothing about the
        // request will differ on the next iteration of the attempt loop — which is what makes
        // a provider-reported overflow unrecoverable (see the guard after `provider_attempt`).
        let store_offered_compaction = plan.compact.is_some();

        // R-RCT-020 step 3 / R-RCT-090: run the compaction sub-turn then re-plan once.
        // compaction reuses the provider_attempt abstraction so cancellation,
        // truncated (malformed-incomplete), and failed outcomes are classified identically
        // to normal provider execution.
        if plan.compact.is_some() {
            *replans += 1;
            // Emit compaction retry so TUI spinner shows honest phase (fix 4.2: emit after increment)
            self.bus.emit(LiveEvent::RetryAttempt {
                attempt: *replans,
                max: params.config.max_context_replans,
                error: "context_overflow".into(),
                delay_ms: 0,
                retry_kind: "compaction".into(),
            });
            self.bus.emit(LiveEvent::TurnStatus {
                phase: "retrying".into(),
                message: format!(
                    "context overflow — compaction replan {}/{}",
                    *replans, params.config.max_context_replans
                ),
            });
            if *replans > params.config.max_context_replans {
                return Err(ReactError::CompactionLoop);
            }
            // Safe: checked is_some() above at line 43
            match self.run_compaction(params, cancel, plan.compact.take().expect("checked is_some"))? {
                Attempt::Completed(_) => {
                    // compaction committed; re-plan once
                }
                Attempt::AbortedInStream(a) => {
                    // cancelled during compaction — already recorded Compaction usage
                    // (estimated if needed) inside run_compaction, never appended Compacted.
                    // Propagate as turn abort deterministically.
                    return Ok(Attempt::AbortedInStream(a));
                }
                Attempt::Truncated => {
                    // malformed-incomplete: explicitly distinguished from cancelled/failed
                    self.bus.emit(LiveEvent::Error {
                        message: "compaction truncated: malformed-incomplete".into(),
                    });
                    return Err(ReactError::Provider(
                        "compaction truncated: malformed-incomplete".into(),
                    ));
                }
                Attempt::ContextOverflow => {
                    self.bus.emit(LiveEvent::Error {
                        message: "compaction context overflow".into(),
                    });
                    return Err(ReactError::Provider("compaction context overflow".into()));
                }
            }
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
        // snapshot per model call, not per turn — a plugin stop/start/reload
        // landing between two iterations of the ReAct loop is visible immediately.
        let tool_specs = self.tools.snapshot().visible_specs();
        let req = Request {
            model: &params.model,
            system,
            messages: &messages,
            tools: &tool_specs,
            thinking: params.thinking,
            max_tokens: params.max_tokens,
            cache: &plan.cache,
            session: Some(params.session.0.as_str()),
        };

        // R-RCT-020 step 4: stream + assemble via reusable abstraction.
        let attempt = self.provider_attempt(&req, cancel, &params.model.r#ref)?;

        // R-RCT-080 bound. `turn.rs` answers `Attempt::ContextOverflow` with a bare
        // `continue`, trusting that the next `plan_request` will offer a compaction span and
        // that the branch above will charge `replans`. That trust only holds when the store
        // agrees the context is full. It computes that locally, from its own token count
        // against `0.80 * ctx_window`, so a stale or wrong `ctx_window` (a config edit, a
        // model re-registered with a different window — see `ServerState::reload_config`)
        // makes the provider say "too long" while the store says "nothing to compact".
        //
        // In that state the request is byte-identical every iteration: same messages, same
        // tools, same cache prefix. The loop would re-issue it forever, billing a provider
        // call each time, and `cancel` is only read after the stream returns so ESC never
        // lands. Charge the same `replans` budget the compaction path uses and fail once it
        // is spent — a turn that cannot make progress must end, not spin.
        if matches!(attempt, Attempt::ContextOverflow) && !store_offered_compaction {
            *replans += 1;
            if *replans > params.config.max_context_replans {
                self.bus.emit(LiveEvent::Error {
                    message: "context overflow reported by the provider, but the store has \
                              nothing left to compact (check the model's ctx_window); ending \
                              the turn instead of retrying an identical request"
                        .into(),
                });
                return Err(ReactError::Provider(
                    "provider reported context overflow with no compaction available".into(),
                ));
            }
            self.bus.emit(LiveEvent::TurnStatus {
                phase: "retrying".into(),
                message: format!(
                    "provider reported context overflow — re-plan {}/{}",
                    *replans, params.config.max_context_replans
                ),
            });
        }

        Ok(attempt)
    }

    /// reusable provider-attempt/cancellation abstraction.
    /// Explicitly distinguishes completed, cancelled, failed, and malformed-incomplete
    /// (Truncated/ContextOverflow) outcomes with deterministic cancellation semantics.
    fn provider_attempt(
        &self,
        req: &Request,
        cancel: &Cancel,
        model: &ModelRef,
    ) -> Result<Attempt, ReactError> {
        self.bus.emit(LiveEvent::TurnStatus {
            phase: "thinking".into(),
            message: String::new(),
        });
        let stream = match self
            .provider
            .stream_with_sink(req, cancel, Some(self.bus.as_ref()))
        {
            Ok(s) => {
                self.bus.emit(LiveEvent::TurnStatus {
                    phase: "streaming".into(),
                    message: String::new(),
                });
                s
            }
            Err(ProvErr::ContextOverflow) => return Ok(Attempt::ContextOverflow),
            Err(ProvErr::Truncated) => return Ok(Attempt::Truncated),
            Err(e) => {
                if cancel.cancelled() {
                    self.bus.emit(LiveEvent::TurnStatus {
                        phase: "aborted".into(),
                        message: String::new(),
                    });
                    return Ok(Attempt::AbortedInStream(estimated_assembled(model)));
                }
                self.bus.emit(LiveEvent::TurnStatus {
                    phase: "failed".into(),
                    message: format!("{e:?}"),
                });
                self.bus.emit(LiveEvent::Error {
                    message: format!("provider failed: {e:?}"),
                });
                return Err(ReactError::Provider(e.to_string()));
            }
        };
        match assemble(stream, self.bus.as_ref()) {
            Ok(mut a) => {
                a.usage.model = model.clone();
                if cancel.cancelled() {
                    self.bus.emit(LiveEvent::TurnStatus {
                        phase: "aborted".into(),
                        message: String::new(),
                    });
                    Ok(Attempt::AbortedInStream(a))
                } else {
                    Ok(Attempt::Completed(a))
                }
            }
            Err(ProvErr::ContextOverflow) => Ok(Attempt::ContextOverflow),
            Err(ProvErr::Truncated) => Ok(Attempt::Truncated),
            Err(e) => {
                if cancel.cancelled() {
                    self.bus.emit(LiveEvent::TurnStatus {
                        phase: "aborted".into(),
                        message: String::new(),
                    });
                    let est = estimated_assembled(model);
                    Ok(Attempt::AbortedInStream(est))
                } else {
                    self.bus.emit(LiveEvent::TurnStatus {
                        phase: "failed".into(),
                        message: format!("provider stream failed mid-stream: {e:?}"),
                    });
                    self.bus.emit(LiveEvent::Error {
                        message: format!("provider stream failed: {e:?}"),
                    });
                    Err(ReactError::Provider(e.to_string()))
                }
            }
        }
    }

    /// R-RCT-090 / R-RCT-095: the compaction summarize sub-turn. Uses `UsageKind::Compaction`,
    /// never `Main`. The loop is the only component that calls a provider or emits usage.
    /// reuses `provider_attempt` so cancellation, truncated (malformed-incomplete),
    /// failed, and completed are distinguished identically to normal provider execution.
    /// Cancelled compaction records Compaction usage (estimated if needed) but never commits
    /// `Compacted` (partial state must not be treated as successful).
    fn run_compaction(
        &self,
        params: &RunParams,
        cancel: &Cancel,
        span: kn9t_provider_core::CompactSpan,
    ) -> Result<Attempt, ReactError> {
        // /17: pluggable delegation — if a compactor is set, use it. If none is
        // installed, compaction is fail-closed: the turn ends (CompactionUnavailable),
        // the provider is never called, and nothing is persisted. The hardcoded
        // inline-prompt fallback was removed.
        // Cancel is checked first: an ESC during a context-full turn is a clean abort,
        // unrelated to compactor availability.
        if cancel.cancelled() {
            self.bus.emit(LiveEvent::TurnStatus {
                phase: "aborted".into(),
                message: String::new(),
            });
            return Ok(Attempt::AbortedInStream(estimated_assembled(
                &params.model.r#ref,
            )));
        }
        let Some(compactor) = &self.compactor else {
            self.bus.emit(LiveEvent::Error { message: "context overflow — compaction required but no compactor plugin is installed; session cannot continue".into() });
            return Err(ReactError::CompactionUnavailable);
        };
        match compactor.compact(span.clone(), &params.model.r#ref) {
            Ok(plan) => {
                if let Some(handoff) = &plan.handoff {
                    let known: Vec<kn9t_provider_core::CallId> = span
                        .messages
                        .iter()
                        .flat_map(|m| &m.content)
                        .filter_map(|c| match c {
                            Content::ToolCall { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect();
                    let ev = Event::Handoff {
                        seq: 0,
                        keep: handoff.keep.clone(),
                        summarize: handoff.summarize.clone(),
                        drop_ids: handoff.drop_ids.clone(),
                        resume_actions: handoff.resume_actions.clone(),
                    };
                    if let Err(e) = kn9t_provider_core::validate_handoff(&ev, &known) {
                        self.bus.emit(LiveEvent::Error {
                            message: format!("compactor handoff validation failed: {e}"),
                        });
                        return Err(ReactError::Provider(format!(
                            "compactor handoff validation failed: {e}"
                        )));
                    }
                }
                self.append(
                    params,
                    Event::Compacted {
                        seq: 0,
                        replaced: span.replaced,
                        summary: plan.summary.clone(),
                    },
                )?;
                if let Some(handoff) = plan.handoff {
                    self.append(
                        params,
                        Event::Handoff {
                            seq: 0,
                            keep: handoff.keep,
                            summarize: handoff.summarize,
                            drop_ids: handoff.drop_ids,
                            resume_actions: handoff.resume_actions,
                        },
                    )?;
                }
                let assembled = Assembled {
                    message: plan.summary,
                    usage: kn9t_provider_core::Usage {
                        tokens: Tokens::default(),
                        model: params.model.r#ref.clone(),
                    },
                    stop: StopReason::Stop,
                    usage_reported: false,
                };
                Ok(Attempt::Completed(assembled))
            }
            Err(e) => {
                self.bus.emit(LiveEvent::Error {
                    message: format!("compactor failed: {e}"),
                });
                Err(ReactError::Provider(format!("compactor failed: {e}")))
            }
        }
    }

    /// R-RCT-130 / DESIGN sec.11.2: run one tool batch. `parallel_safe` tools may run on OS
    /// threads; unsafe tools run sequentially. Results are returned in the model's call
    /// order regardless of completion order. Each call passes before_tool_call (fail
    /// closed) which decides allow/ask/deny, then executes, then after_tool_call.
    #[doc(hidden)]
    pub fn run_tool_batch(
        &self,
        params: &RunParams,
        calls: &[ToolCall],
        cancel: &Cancel,
    ) -> Vec<Content> {
        // Decide each call up front (hooks, ADR-0008) preserving order; then execute.
        // pass cancel so approval waits can be aborted.
        let mut plans: Vec<CallPlan> = Vec::with_capacity(calls.len());
        for call in calls {
            plans.push(self.authorize(params, call, cancel));
        }

        // one snapshot for the whole batch — the batch must dispatch against a
        // single coherent registry (a plugin stopped between two calls of the same batch
        // would otherwise make results depend on scheduling). The next model call gets a
        // fresh snapshot.
        let registry = self.tools.snapshot();

        // Execute: split into parallel-safe (run concurrently) and the rest (sequential),
        // but always collect results back into call order.
        let mut results: Vec<Option<Content>> = vec![None; calls.len()];

        // Launch parallel-safe authorized calls on threads.
        // Fix parallel path now returns raw inner content + is_error;
        // after_tool_call is applied after join in sequential order, so both
        // paths share the identical before/execute/after lifecycle.
        let mut handles: Vec<ParallelToolHandle> = Vec::new();
        for (i, plan) in plans.iter().enumerate() {
            if let CallPlan::Execute { args } = plan {
                let call = &calls[i];
                if let Some(tool) = registry.get(&call.name) {
                    if tool.parallel_safe() {
                        let tool = tool.clone();
                        let ctx = ToolCtx {
                            cwd: params.cwd.clone(),
                            read: params.read_map.clone(),
                            bus: self.bus.clone(),
                            call_id: call.id.clone(),
                        };
                        let cancel = cancel.clone();
                        // The worker takes ownership of these; the handle keeps its own
                        // copies so `after_tool_call` can be applied at collection time.
                        let worker_args = args.clone();
                        let name = call.name.clone();
                        let id = call.id.clone();
                        let bus = self.bus.clone();
                        // B8: cancel is checked before dispatch here too, matching the
                        // sequential path (R-RCT-060). A batch cancelled before its threads
                        // were spawned used to launch them anyway.
                        if cancel.cancelled() {
                            continue;
                        }
                        let (tx, rx) = std::sync::mpsc::channel();
                        thread::spawn(move || {
                            bus.emit(LiveEvent::ToolStarted {
                                call_id: id.clone(),
                                name: name.clone(),
                            });
                            let out = tool.execute(&worker_args, &ctx, &cancel);
                            let (inner, is_error) = match out {
                                Ok(o) => (o.content, o.is_error),
                                Err(e) => (vec![Content::Text { text: e.0 }], true),
                            };
                            bus.emit(LiveEvent::ToolFinished {
                                call_id: id.clone(),
                                is_error,
                            });
                            // The receiver is gone if the batch abandoned this call; the
                            // send then fails and the thread simply exits.
                            let _ = tx.send((inner, is_error));
                        });
                        handles.push((
                            i,
                            call.name.clone(),
                            args.clone(),
                            call.id.clone(),
                            rx,
                        ));
                    }
                }
            }
        }

        // Sequential pass for everything not launched on a thread.
        let launched: std::collections::HashSet<usize> =
            handles.iter().map(|(i, _, _, _, _)| *i).collect();
        for (i, call) in calls.iter().enumerate() {
            if launched.contains(&i) {
                continue;
            }
            results[i] = Some(self.execute_one(params, &registry, call, &plans[i], cancel));
        }

        // Collect parallel results in call order and apply after_tool_call sequentially.
        // B8: bounded — see `recv_parallel_result`. A tool that ignores `Cancel` is
        // abandoned rather than waited on, so the batch always returns and the turn can end.
        for (i, name, args, id, rx) in handles {
            let content = match recv_parallel_result(&rx, cancel, params.config.tool_cancel_grace) {
                Ok((inner, is_error)) => {
                    let patched = self.hook_after_tool_call(&name, &args, &params.cwd, inner);
                    Content::ToolResult {
                        id,
                        content: ensure_nonempty_content(patched),
                        is_error,
                    }
                }
                Err(ParallelFailure::Panicked) => synth_error(&id, "tool thread panicked"),
                Err(ParallelFailure::Abandoned) => {
                    // R-RCT-060: the call never answered, so synthesize its result rather
                    // than leaving the ToolCall open (DESIGN §7.5). The orphaned thread is
                    // left to exit on its own; its `send` will fail harmlessly.
                    self.bus.emit(LiveEvent::Error {
                        message: format!(
                            "tool '{}' (call {}) did not stop after cancellation; abandoning it",
                            name, id.0
                        ),
                    });
                    self.bus.emit(LiveEvent::ToolFinished {
                        call_id: id.clone(),
                        is_error: true,
                    });
                    synth_error(&id, "aborted by user (tool did not stop; abandoned)")
                }
            };
            results[i] = Some(content);
        }

        // B8: a call skipped at dispatch (cancelled before its thread was spawned) still
        // owes a result. R-RCT-060 again: no ToolCall may be left without a ToolResult.
        for (i, call) in calls.iter().enumerate() {
            if results[i].is_none() {
                results[i] = Some(synth_error(&call.id, "aborted by user"));
            }
        }

        results
            .into_iter()
            .map(|r| r.expect("every slot filled"))
            .collect()
    }

    /// Sequential execution of one call given its authorization plan.
    /// `registry` is the batch's single snapshot.
    fn execute_one(
        &self,
        params: &RunParams,
        registry: &ToolRegistry,
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
                let Some(tool) = registry.get(&call.name) else {
                    return synth_error(&call.id, &format!("unknown tool `{}`", call.name));
                };
                self.bus.emit(LiveEvent::ToolStarted {
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
                self.bus.emit(LiveEvent::ToolFinished {
                    call_id: call.id.clone(),
                    is_error,
                });
                // after_tool_call (pipeline, keep original on failure).
                let patched = self.hook_after_tool_call(&call.name, args, &params.cwd, inner);
                Content::ToolResult {
                    id: call.id.clone(),
                    content: ensure_nonempty_content(patched),
                    is_error,
                }
            }
        }
    }

    /// ADR-0008 — the policy plugin decides, this routes. `before_tool_call` yields
    /// `Allow`/`Ask`/`Deny`/`Replace` (strictest-wins across plugins, `composed.rs`) and
    /// kn9t no longer re-derives a verdict of its own: there is no classifier and no
    /// effects combiner left. `Ask` is handed to the `Approver`, which owns the prompt.
    ///
    /// Failure posture (DESIGN §13.5) is unchanged: a hook that errors or times out yields
    /// `Deny` — a policy that cannot answer is not permission. That is distinct from *no
    /// policy installed*, which yields `Allow` (ADR-0008 decision 5).
    /// pass cancel so approval waits can be aborted.
    #[doc(hidden)]
    pub fn authorize(&self, params: &RunParams, call: &ToolCall, cancel: &Cancel) -> CallPlan {
        // Session-scoped tool blocking (tools enable/disable). Checked before args
        // parsing and before policy hooks: a disabled tool must never reach parsing,
        // `before_tool_call`, or `Tool::execute`. The provider still saw the tool in
        // the `tools` array (cache prefix unchanged); we simply refuse to run it and
        // hand the model an `is_error` result it can read and adapt to.
        if params.disabled_tools.contains(&call.name) {
            self.bus.emit(LiveEvent::Error {
                message: format!(
                    "tool '{}' (call {}): blocked — disabled for this session",
                    call.name, call.id.0
                ),
            });
            return CallPlan::Deny(format!(
                "blocked: tool '{}' is disabled for this session. \
                 Do not retry '{}'; use another tool or ask the user to re-enable it.",
                call.name, call.name
            ));
        }
        // the tool belongs to a plugin that is currently stopped. Its spec is
        // still in the `tools` array (never filtered — that would invalidate the level-1
        // cache prefix for a temporary condition), so the model can legitimately call it.
        // Refuse here, exactly like `disabled_tools`, and say the state is recoverable so
        // the agent can restart the plugin instead of giving up on the capability.
        if self.tools.blocked().contains(&call.name) {
            self.bus.emit(LiveEvent::Error {
                message: format!(
                    "tool '{}' (call {}): blocked — owning plugin is stopped",
                    call.name, call.id.0
                ),
            });
            return CallPlan::Deny(format!(
                "blocked: tool '{}' belongs to a plugin that is currently stopped. \
                 Start the plugin again before retrying '{}'.",
                call.name, call.name
            ));
        }
        // §4.1 treats `args_json` as cache-critical verbatim provider bytes, so a parse
        // failure here is a real defect (provider sent malformed JSON, or the bytes were
        // corrupted in transit). Surface it instead of silently substituting Null, which
        // would present the tool with empty args and produce a confusing downstream error.
        // invalid args must short-circuit to ToolResult(error) and must not
        // reach Tool::execute or policy hooks. Emit Error for observability, then deny.
        let args: serde_json::Value = match serde_json::from_str(&call.args_json) {
            Ok(v) => v,
            Err(e) => {
                self.bus.emit(LiveEvent::Error {
                    message: format!(
                        "tool '{}' (call {}): malformed args_json: {e}",
                        call.name, call.id.0
                    ),
                });
                return CallPlan::Deny(format!("malformed tool args_json: {e}"));
            }
        };
        if !args.is_object() {
            self.bus.emit(LiveEvent::Error {
                message: format!(
                    "tool '{}' (call {}): args_json is not a JSON object: {}",
                    call.name, call.id.0, call.args_json
                ),
            });
            return CallPlan::Deny(format!("tool args must be a JSON object, got: {}", args));
        }
        match self.hook_before_tool_call(&call.name, &args, &params.cwd) {
            HookVeto::Allow => CallPlan::Execute { args },
            HookVeto::Deny { reason } => CallPlan::Deny(reason),
            HookVeto::Ask { reason } => self.request_approval(params, call, args, &reason, cancel),
            // `Replace` permits the call with rewritten arguments. The plugin that rewrote
            // them has already judged them, so this does not re-ask.
            HookVeto::Replace { args: new_args } => CallPlan::Execute { args: new_args },
        }
    }

    /// ADR-0008 — hand an `Ask` to the approval mechanism and translate the user's answer.
    ///
    /// The `Approver` blocks this thread until `POST /approve` arrives (or the scope cache
    /// answers immediately), so no polling and no extra state machine here.
    ///
    /// pass cancel so ESC can abort the approval wait.
    fn request_approval(
        &self,
        params: &RunParams,
        call: &ToolCall,
        args: serde_json::Value,
        reason: &str,
        cancel: &Cancel,
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
        let ctx = kn9t_provider_core::ApprovalCtx {
            session: &params.session.0,
            sink: self.bus.as_ref(),
            cancel,
        };
        match self.approver.request(&dispatch, &params.cwd, reason, &ctx) {
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
#[doc(hidden)]
pub enum CallPlan {
    Execute { args: serde_json::Value },
    Deny(String),
}

/// A synthesized `is_error` tool result so no `ToolCall` is left without its `ToolResult`
/// (DESIGN sec.7.5 invariant; R-RCT-060).
#[doc(hidden)]
pub fn synth_error(id: &kn9t_provider_core::CallId, msg: &str) -> Content {
    Content::ToolResult {
        id: id.clone(),
        content: vec![Content::Text {
            text: msg.to_string(),
        }],
        is_error: true,
    }
}

/// Ensure tool result content is never empty (provider APIs reject empty content).
/// If the content vec is empty or contains only empty Text blocks, substitute a
/// placeholder so the API call doesn't fail with "message content cannot be empty".
#[doc(hidden)]
pub fn ensure_nonempty_content(content: Vec<Content>) -> Vec<Content> {
    // Check if content is effectively empty
    let is_empty = content.is_empty()
        || content.iter().all(|c| match c {
            Content::Text { text } => text.is_empty(),
            _ => false,
        });
    if is_empty {
        vec![Content::Text {
            text: "(no output)".to_string(),
        }]
    } else {
        content
    }
}

/// A zeroed, estimated `Assembled` used when the stream was cut before any usage arrived
/// and no message survives (R-RCT-050).
#[doc(hidden)]
pub fn estimated_assembled(model: &ModelRef) -> Assembled {
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

