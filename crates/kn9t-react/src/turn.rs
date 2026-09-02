//! The `impl ReactLoop` turn machinery (R-RCT-020..R-RCT-130). Split from `loop_.rs` only
//! to keep each file small; it is the same crate/type.

use kn9t_provider_core::{
    Cancel, Content, Event, LiveEvent, Message, MsgId, Role, StopReason, ToolCall, UsageKind,
};

use crate::loop_::{ReactError, ReactLoop, RunParams};

/// One provider attempt's classified outcome.
pub(crate) enum Attempt {
    Completed(crate::assembler::Assembled),
    /// Aborted mid-stream (R-RCT-050): partial usage kept, no MessageAppended.
    AbortedInStream(crate::assembler::Assembled),
    /// `ProvErr::Truncated` -> retry with a harsher reminder (R-RCT-070).
    Truncated,
    /// `ProvErr::ContextOverflow` -> compaction re-plan (R-RCT-080).
    ContextOverflow,
}

enum TurnOutcome {
    Continue,
    Idle(StopReason),
}

impl ReactLoop {
    /// Drive turns until the loop goes idle. Returns the final stop reason.
    pub fn run(&self, mut params: RunParams) -> Result<StopReason, ReactError> {
        let mut turn: u32 = 0;
        loop {
            turn += 1;
            self.bus.emit(LiveEvent::TurnStarted { turn });
            // Phase "thinking" is emitted by one_attempt when the provider is actually contacted
            // (exec.rs), not here — avoids flipping back to "thinking" immediately after a tool batch
            // before the next provider round (fix 4.3).
            // R-RCT-040: use external cancel if provided (server abort), else fresh per turn.
            // The external cancel allows the server to abort the entire run when user presses ESC.
            let cancel = params.cancel.clone().unwrap_or_else(Cancel::new);
            match self.execute_turn(&mut params, turn, &cancel) {
                Ok(TurnOutcome::Continue) => continue,
                Ok(TurnOutcome::Idle(stop)) => {
                    self.bus.emit(LiveEvent::TurnStatus { phase: "idle".into(), message: String::new() });
                    self.bus.emit(LiveEvent::TurnEnded { turn, stop });
                    return Ok(stop);
                }
                Err(e) => {
                    self.bus.emit(LiveEvent::TurnStatus { phase: "failed".into(), message: format!("{e:?}") });
                    self.bus.emit(LiveEvent::Error {
                        message: format!("{e:?}"),
                    });
                    self.bus.emit(LiveEvent::TurnEnded {
                        turn,
                        stop: StopReason::Aborted,
                    });
                    return Err(e);
                }
            }
        }
    }

    fn execute_turn(
        &self,
        params: &mut RunParams,
        turn: u32,
        cancel: &Cancel,
    ) -> Result<TurnOutcome, ReactError> {
        let mut reminders: Vec<Message> = Vec::new();
        let mut trunc_n: u32 = 0;
        let mut replans: u32 = 0;

        // Attempt loop: truncation retries (R-RCT-070) + context-overflow re-plans
        // (R-RCT-080/090).
        let assembled = loop {
            match self.one_attempt(params, cancel, &reminders, &mut replans)? {
                Attempt::Completed(a) => {
                    break a;
                }
                Attempt::AbortedInStream(a) => {
                    // R-RCT-050: record usage (estimated if the stream carried none); do NOT
                    // append the discarded partial assistant message.
                    self.record_usage(params, &a.usage, UsageKind::Main, !a.usage_reported)?;
                    return Ok(TurnOutcome::Idle(StopReason::Aborted));
                }
                Attempt::Truncated => {
                    trunc_n += 1;
                    if trunc_n > params.config.truncation_attempts {
                        self.bus.emit(LiveEvent::TurnStatus { phase: "failed".into(), message: format!("truncation ladder exhausted after {} attempts", trunc_n - 1) });
                        self.bus.emit(LiveEvent::Error { message: format!("truncation ladder exhausted after {} attempts", trunc_n - 1) });
                        return Err(ReactError::TruncationGaveUp);
                    }
                    let ladder = &params.config.truncation_ladder;
                    let idx = ((trunc_n - 1) as usize).min(ladder.len().saturating_sub(1));
                    let lines = ladder[idx];
                    self.bus.emit(LiveEvent::RetryAttempt { attempt: trunc_n, max: params.config.truncation_attempts, error: "truncated".into(), delay_ms: 0, retry_kind: "truncation".into() });
                    self.bus.emit(LiveEvent::TurnStatus { phase: "retrying".into(), message: format!("truncated — retry {}/{} with {} lines limit", trunc_n, params.config.truncation_attempts, lines) });
                    reminders.push(reminder_message(lines));
                    continue;
                }
                Attempt::ContextOverflow => {
                    // Compaction retry is emitted inside one_attempt (exec.rs) after incrementing replans
                    // (fix 4.2 off-by-one). Provider-overflow just loops to re-plan.
                    continue;
                }
            }
        };

        // Persist assistant message + main usage (R-RCT-020 step 5).
        self.append(params, Event::MessageAppended { seq: 0, msg: assembled.message.clone() })?;
        self.record_usage(params, &assembled.usage, UsageKind::Main, !assembled.usage_reported)?;

        let tool_calls = collect_tool_calls(&assembled.message);

        if tool_calls.is_empty() {
            // No tool calls (R-RCT-020 step 6).
            let stop_now = self.hook_should_stop(assembled.stop, &assembled.usage, turn);
            if stop_now {
                return Ok(TurnOutcome::Idle(assembled.stop));
            }
            let followup = self.collect_followup();
            if followup.is_empty() {
                return Ok(TurnOutcome::Idle(assembled.stop));
            }
            for m in followup {
                self.append(params, Event::MessageAppended { seq: 0, msg: m })?;
            }
            return Ok(TurnOutcome::Continue);
        }

        // Tool calls (R-RCT-020 step 7-9).
        self.bus.emit(LiveEvent::TurnStatus { phase: "tool".into(), message: format!("running {} tool(s)", tool_calls.len()) });
        let results = self.run_tool_batch(params, &tool_calls, cancel);
        // Persist tool results in the model's call order (R-RCT-020 step 8, R-RCT-130).
        let msg = Message {
            id: MsgId::new(),
            role: Role::Tool,
            content: results.clone(),
            silent: false,
        };
        self.append(params, Event::MessageAppended { seq: 0, msg })?;

        if cancel.cancelled() {
            // R-RCT-060: keep the transcript consistent (assistant msg + all results,
            // aborted ones synthesized), do not roll back, then go idle.
            return Ok(TurnOutcome::Idle(StopReason::Aborted));
        }

        // Steer queue (host first) then prepare_next_turn (R-RCT-020 step 9).
        let steer = self.collect_steering();
        for m in steer {
            self.append(params, Event::MessageAppended { seq: 0, msg: m })?;
        }
        self.apply_prepare_next_turn(params, assembled.stop, &assembled.usage);
        Ok(TurnOutcome::Continue)
    }
}

/// R-RCT-070 -- a harsher write-size reminder injected as a system-role message.
fn reminder_message(lines: u32) -> Message {
    Message {
        id: MsgId::new(),
        role: Role::System,
        content: vec![Content::Text {
            text: format!(
                "Your previous response was truncated. Write files in chunks of at most \
                 {lines} lines per tool call, then continue in a follow-up call."
            ),
        }],
        silent: false,
    }
}

/// The fully-accumulated tool calls in the assistant message, in emission order.
fn collect_tool_calls(msg: &Message) -> Vec<ToolCall> {
    msg.content
        .iter()
        .filter_map(|c| match c {
            Content::ToolCall { id, name, args_json } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                args_json: args_json.clone(),
            }),
            _ => None,
        })
        .collect()
}
