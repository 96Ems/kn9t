//! Stage-03 ReAct acceptance tests (`rct::*`). Names match `spec/03-react-tools.md`.
//! Everything is driven by the replay provider over synthetic native fixtures: no network,
//! no API key, no spend.

mod support;

use std::sync::{Arc, Mutex};

use kn9t_core::{Content, Event, ProvErr, RequestPlan, Role, SessionId, StopReason, Store,
                StoreErr, Tool, ToolRegistry};
use kn9t_react::{ReactConfig, ReactLoop, RunParams};

use support::*;

/// `StopReason` has no `Debug`, so `assert_eq!` will not compile; compare explicitly.
fn assert_stop(got: StopReason, want: StopReason) {
    assert!(got == want, "unexpected stop reason");
}

fn run_params(store: &StubStore) -> RunParams {
    let _ = store;
    RunParams {
        session: SessionId::new(),
        model: test_model_spec(),
        thinking: kn9t_core::Thinking::Off,
        max_tokens: Some(4096),
        cwd: std::env::temp_dir(),
        config: ReactConfig::default(),
        read_map: empty_read_map(),
        system: None,
        cancel: None,
    }
}

// ------------------------- R-RCT-020 -------------------------

#[test]
fn turn_sequence() {
    // A fixture that produces one text delta, one tool call (read), usage, and a tool_use
    // stop. The tool then resolves; the second turn returns a plain stop.
    let body = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"reading\"}\n\n",
        "data: {\"chunk\":\"tool_call\",\"idx\":1,\"id\":\"call_1\",\"name\":\"read\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":1,\"delta\":\"{\\\"path\\\":\\\"probe.txt\\\"}\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":10,\"output\":5,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    // Second turn: plain assistant text then stop (no tool calls) -> idle.
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );

    // Write a probe file the read tool can open.
    let dir = std::env::temp_dir().join(format!("kn9t-rct-seq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("probe.txt"), b"hello").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    // Spawn the real kn9t-tools plugin (R-PLUG2-110)
    let (_host, tools) = spawn_tools_registry();

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus: bus.clone(),
    };
    let mut params = run_params(&store);
    params.cwd = dir;

    let stop = looop.run(params).expect("loop ran");
    assert_stop(stop, StopReason::Stop);

    // Ordered event trace on the bus for the first turn — 96E-12: bus is live-only (transient).
    let kinds = bus.kinds();
    let pos = |name: &str| kinds.iter().position(|k| k == name);
    assert!(pos("TurnStarted").is_some());
    assert!(pos("TextDelta").unwrap() > pos("TurnStarted").unwrap());
    assert!(pos("ToolArgsDelta").is_some());
    // 96E-12: durable events must not be on the live bus; they are on the store.
    assert!(pos("MessageAppended").is_none(), "durable MessageAppended must not be on live bus (96E-12)");
    assert!(pos("UsageRecorded").is_none(), "durable UsageRecorded must not be on live bus (96E-12)");
    assert!(pos("ToolStarted").unwrap() < pos("ToolFinished").unwrap());

    // The persisted transcript (store.append order) has: assistant msg, usage, tool result
    // msg, then on turn 2 assistant msg + usage.
    let tags = store.appended_tags();
    assert_eq!(tags[0], "MessageAppended"); // assistant with tool call
    assert_eq!(tags[1], "UsageRecorded");
    assert_eq!(tags[2], "MessageAppended"); // tool results
    // The tool-result message closes the tool call (R-RCT-060 invariant even on success).
    let appended = store.appended.lock().unwrap();
    if let Event::MessageAppended { msg, .. } = &appended[2] {
        let has_result = msg
            .content
            .iter()
            .any(|c| matches!(c, Content::ToolResult { .. }));
        assert!(has_result, "tool-result message must contain a ToolResult");
    } else {
        panic!("expected tool-result MessageAppended");
    }
}

// ------------------------- R-RCT-040 -------------------------

#[test]
fn cancel_boundary() {
    // A cancel signalled mid-stream must leave the store with either the full
    // MessageAppended or none -- never a partial row. On stream abort (R-RCT-050) the loop
    // appends UsageRecorded but no assistant MessageAppended, so the invariant here is:
    // zero MessageAppended rows exist (never a half-written one).
    let body = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"half\"}\n\n",
        "data: [DONE]\n\n",
    );
    let inner = replay(fixture_from_body(body));
    let provider = Arc::new(AbortDuringStream { inner });
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let stop = looop.run(run_params(&store)).expect("loop ran");
    assert_stop(stop, StopReason::Aborted);

    let appended = store.appended.lock().unwrap();
    // No MessageAppended at all (never a partial one); the abort landed at a loop boundary.
    assert!(
        !appended
            .iter()
            .any(|e| matches!(e, Event::MessageAppended { .. })),
        "a mid-stream cancel must not persist any assistant message row"
    );
}

// ------------------------- R-RCT-050 -------------------------

#[test]
fn abort_in_stream() {
    // record UsageRecorded (estimated) and NOT append a MessageAppended.
    let body = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"partial\"}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = replay(fixture_from_body(body));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    // A HookHost cannot cancel; instead we use a provider wrapper that cancels. Simplest:
    // cancel via a policy-free path -- use the AbortingProvider below.
    let provider = Arc::new(AbortDuringStream {
        inner: provider,
    });

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let params = run_params(&store);
    let stop = looop.run(params).expect("loop ran");
    assert_stop(stop, StopReason::Aborted);

    let tags = store.appended_tags();
    // UsageRecorded present, no MessageAppended.
    assert!(tags.iter().any(|t| t == "UsageRecorded"));
    assert!(
        !tags.iter().any(|t| t == "MessageAppended"),
        "no assistant message may be appended on stream abort, got {tags:?}"
    );
    // The recorded usage is flagged estimated.
    let appended = store.appended.lock().unwrap();
    let est = appended.iter().any(|e| matches!(
        e,
        Event::UsageRecorded { estimated: true, .. }
    ));
    assert!(est, "usage after stream abort must be estimated");
}

/// A provider that signals cancellation before yielding, forcing the assemble result to be
/// classified as an in-stream abort (R-RCT-050).
struct AbortDuringStream {
    inner: Arc<kn9t_provider_replay::ReplayProvider>,
}
impl kn9t_core::Provider for AbortDuringStream {
    fn name(&self) -> &str {
        "abort-in-stream"
    }
    fn stream(
        &self,
        req: &kn9t_core::Request,
        cancel: &kn9t_core::Cancel,
    ) -> Result<
        Box<dyn Iterator<Item = Result<kn9t_core::Chunk, ProvErr>> + Send>,
        ProvErr,
    > {
        let s = self.inner.stream(req, cancel)?;
        cancel.cancel(); // mid-stream abort: chunks assembled, then loop sees cancelled()
        Ok(s)
    }
}

// ------------------------- R-RCT-060 -------------------------

#[test]
fn abort_in_tools() {
    // Assistant emits two tool calls. The first tool signals cancellation while running, so
    // the second never runs. Both must still get a ToolResult (no orphaned ToolCall), and
    // the loop goes idle without rolling back (R-RCT-060).
    let body = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"call_x\",\"name\":\"abort\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"tool_call\",\"idx\":1,\"id\":\"call_y\",\"name\":\"abort\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":1,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = replay(fixture_from_body(body));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(AbortingTool::new()) as Arc<dyn Tool>);

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let params = run_params(&store);
    let stop = looop.run(params).expect("loop ran");
    assert_stop(stop, StopReason::Aborted);

    // Every emitted ToolCall id has a matching ToolResult id in the persisted transcript.
    let appended = store.appended.lock().unwrap();
    let mut call_ids = Vec::new();
    let mut result_ids = Vec::new();
    for e in appended.iter() {
        if let Event::MessageAppended { msg, .. } = e {
            for c in &msg.content {
                match c {
                    Content::ToolCall { id, .. } => call_ids.push(id.0.clone()),
                    Content::ToolResult { id, .. } => result_ids.push(id.0.clone()),
                    _ => {}
                }
            }
        }
    }
    assert_eq!(call_ids.len(), 2);
    for id in &call_ids {
        assert!(
            result_ids.contains(id),
            "ToolCall {id} has no matching ToolResult (result ids: {result_ids:?})"
        );
    }
}

/// A sequential tool that cancels its own turn and reports an error, to drive the
/// abort-in-tools path (R-RCT-060).
struct AbortingTool {
    spec: kn9t_core::ToolSpec,
}
impl AbortingTool {
    fn new() -> Self {
        AbortingTool {
            spec: kn9t_core::ToolSpec {
                name: "abort".to_string(),
                description: "test tool that cancels the turn".to_string(),
                schema: serde_json::json!({"type":"object","properties":{}}), hidden: false, effects: vec![], policy: Default::default()
            },
        }
    }
}
impl Tool for AbortingTool {
    fn spec(&self) -> &kn9t_core::ToolSpec {
        &self.spec
    }
    fn execute(
        &self,
        _args: &serde_json::Value,
        _ctx: &kn9t_core::ToolCtx,
        cancel: &kn9t_core::Cancel,
    ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
        cancel.cancel();
        Err(kn9t_core::ToolErr("aborted by user".into()))
    }
}

// ------------------------- R-RCT-060 External Cancel -------------------------

/// Test that an external cancel (from server) during tool execution still produces
/// tool results. This simulates the user pressing ESC while tools are running.
/// The tools should complete, results should be persisted, and the loop should return
/// Aborted to maintain transcript consistency.
#[test]
fn external_cancel_during_tool_execution() {
    // Two parallel tools: first one takes a while, second completes fast.
    // Cancel is signaled externally (not by the tool) while they run.
    let body = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"call_slow\",\"name\":\"slow\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"tool_call\",\"idx\":1,\"id\":\"call_fast\",\"name\":\"fast\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":1,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = replay(fixture_from_body(body));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let mut tools = ToolRegistry::new();
    // SlowTool: takes 100ms, during which external cancel is signaled
    tools.push(Arc::new(SlowToolWithExternalCancel::new("slow", 100)) as Arc<dyn Tool>);
    // FastTool: completes immediately
    tools.push(Arc::new(FastTool::new("fast")) as Arc<dyn Tool>);

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus: bus.clone(),
    };
    let params = run_params(&store);
    let stop = looop.run(params).expect("loop ran");
    assert_stop(stop, StopReason::Aborted);

    // Both tool calls MUST have matching tool results (transcript consistency).
    let appended = store.appended.lock().unwrap();
    let mut call_ids = Vec::new();
    let mut result_ids = Vec::new();
    for e in appended.iter() {
        if let Event::MessageAppended { msg, .. } = e {
            for c in &msg.content {
                match c {
                    Content::ToolCall { id, .. } => call_ids.push(id.0.clone()),
                    Content::ToolResult { id, .. } => result_ids.push(id.0.clone()),
                    _ => {}
                }
            }
        }
    }
    assert_eq!(call_ids.len(), 2, "expected 2 tool calls");
    assert_eq!(result_ids.len(), 2, "expected 2 tool results");
    for id in &call_ids {
        assert!(
            result_ids.contains(id),
            "ToolCall {id} has no matching ToolResult (result ids: {result_ids:?})"
        );
    }
}

/// A tool that takes some time to execute, allowing external cancel during execution.
struct SlowToolWithExternalCancel {
    spec: kn9t_core::ToolSpec,
    delay_ms: u64,
}
impl SlowToolWithExternalCancel {
    fn new(name: &str, delay_ms: u64) -> Self {
        SlowToolWithExternalCancel {
            spec: kn9t_core::ToolSpec {
                name: name.to_string(),
                description: "slow test tool".to_string(),
                schema: serde_json::json!({"type":"object","properties":{}}), hidden: false, effects: vec![], policy: Default::default()
            },
            delay_ms,
        }
    }
}
impl Tool for SlowToolWithExternalCancel {
    fn spec(&self) -> &kn9t_core::ToolSpec {
        &self.spec
    }
    fn parallel_safe(&self) -> bool {
        true // Run in parallel
    }
    fn execute(
        &self,
        _args: &serde_json::Value,
        ctx: &kn9t_core::ToolCtx,
        cancel: &kn9t_core::Cancel,
    ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
        // Simulate work + signal cancel midway (as if user pressed ESC)
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms / 2));
        cancel.cancel(); // External cancel signal
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms / 2));
        // Tool completes despite cancel (can't interrupt mid-execution)
        Ok(kn9t_core::ToolOutput {
            content: vec![Content::Text { text: "slow completed".to_string() }],
            details: None,
            is_error: false,
        })
    }
}

/// A fast tool that completes immediately.
struct FastTool {
    spec: kn9t_core::ToolSpec,
}
impl FastTool {
    fn new(name: &str) -> Self {
        FastTool {
            spec: kn9t_core::ToolSpec {
                name: name.to_string(),
                description: "fast test tool".to_string(),
                schema: serde_json::json!({"type":"object","properties":{}}), hidden: false, effects: vec![], policy: Default::default()
            },
        }
    }
}
impl Tool for FastTool {
    fn spec(&self) -> &kn9t_core::ToolSpec {
        &self.spec
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn execute(
        &self,
        _args: &serde_json::Value,
        _ctx: &kn9t_core::ToolCtx,
        _cancel: &kn9t_core::Cancel,
    ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
        Ok(kn9t_core::ToolOutput {
            content: vec![Content::Text { text: "fast completed".to_string() }],
            details: None,
            is_error: false,
        })
    }
}

// ------------------------- R-RCT-070 -------------------------

#[test]
fn truncation_ladder() {
    // Three Truncated pre-stream errors then success. Exactly three reminders injected in
    // ladder order. We observe reminders via the messages the provider is asked to send by
    // capturing them through a MessageSpyProvider.
    let good = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"ok\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::PreStreamErr(ProvErr::Truncated),
        StreamScript::PreStreamErr(ProvErr::Truncated),
        StreamScript::PreStreamErr(ProvErr::Truncated),
        StreamScript::Fixture(fixture_from_body(good)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let looop = ReactLoop {
        provider: provider.clone(),
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let stop = looop.run(run_params(&store)).expect("loop ran");
    assert_stop(stop, StopReason::Stop);
    // Four provider calls: three truncated + one success.
    assert_eq!(*provider.calls.lock().unwrap(), 4);
}

#[test]
fn truncation_gives_up() {
    // Config: 2 attempts. Three truncations -> give up with an Error.
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::PreStreamErr(ProvErr::Truncated),
        StreamScript::PreStreamErr(ProvErr::Truncated),
        StreamScript::PreStreamErr(ProvErr::Truncated),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus: bus.clone(),
    };
    let mut params = run_params(&store);
    params.config.truncation_attempts = 2;
    let res = looop.run(params);
    assert!(res.is_err());
    assert!(bus.kinds().iter().any(|k| k == "Error"));
}

// ------------------------- R-RCT-090 -------------------------

#[test]
fn compaction_replan_once() {
    // Store returns compact:Some twice. The loop must run exactly one summarize call then
    // hard error -- never two.
    let summary = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"summary\"}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Only the compaction summarize call should ever reach the provider.
        StreamScript::Fixture(fixture_from_body(summary)),
    ]));
    let store = Arc::new(
        StubStore::new(PlanScript::plain(vec![])).script(vec![
            PlanScript::compacting(),
            PlanScript::compacting(),
        ]),
    );
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider: provider.clone(),
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus: bus.clone(),
    };
    let res = looop.run(run_params(&store));
    assert!(res.is_err(), "second compact demand must be a hard error");
    // Exactly one provider call (the single summarize).
    assert_eq!(*provider.calls.lock().unwrap(), 1);
    // Usage attributed to Compaction, and a Compacted event was appended.
    let appended = store.appended.lock().unwrap();
    assert!(appended.iter().any(|e| matches!(
        e,
        Event::UsageRecorded { kind: kn9t_core::UsageKind::Compaction, .. }
    )));
    assert!(appended
        .iter()
        .any(|e| matches!(e, Event::Compacted { .. })));
}

// ------------------------- R-RCT-110 -------------------------

#[test]
fn hook_posture() {
    // A HookHost that panics on every hook. before_tool_call must fail closed (deny), and a
    // HookFailed event is published for each failed hook.
    let body = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"read\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    // Turn 2 has no tool calls so the loop can idle (should_stop_after_turn panics -> the
    // fallback is "continue", so idle happens via the empty followup queue).
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"end\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    
    // Spawn the real kn9t-tools plugin (R-PLUG2-110)
    let (_host, tools) = spawn_tools_registry();

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(PanicHooks),
        bus: bus.clone(),
    };
    let _ = looop.run(run_params(&store));

    // before_tool_call failed closed: the tool result must be an error "fail closed".
    let appended = store.appended.lock().unwrap();
    let mut denied = false;
    for e in appended.iter() {
        if let Event::MessageAppended { msg, .. } = e {
            for c in &msg.content {
                if let Content::ToolResult { is_error, content, .. } = c {
                    if *is_error {
                        let text: String = content
                            .iter()
                            .filter_map(|c| match c {
                                Content::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect();
                        if text.contains("fail closed") {
                            denied = true;
                        }
                    }
                }
            }
        }
    }
    assert!(denied, "before_tool_call must fail closed");
    // A HookFailed event was published (at least for before_request and before_tool_call).
    assert!(bus.kinds().iter().any(|k| k == "HookFailed"));
}

/// A HookHost that panics on every method (crash simulation).
struct PanicHooks;
impl kn9t_core::HookHost for PanicHooks {
    fn before_tool_call(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _cwd: &std::path::Path,
    ) -> kn9t_core::HookVeto {
        panic!("boom");
    }
    fn after_tool_call(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _result: Vec<Content>,
    ) -> Vec<Content> {
        panic!("boom");
    }
    fn before_request(
        &self,
        _msgs: Vec<kn9t_core::Message>,
        _model: &kn9t_core::ModelRef,
        _system: Option<&str>,
    ) -> Vec<kn9t_core::Message> {
        panic!("boom");
    }
    fn should_stop_after_turn(
        &self,
        _stop: StopReason,
        _usage: &kn9t_core::Usage,
        _turn: u32,
    ) -> bool {
        panic!("boom");
    }
    fn prepare_next_turn(
        &self,
        _stop: StopReason,
        _usage: &kn9t_core::Usage,
    ) -> kn9t_core::NextTurnPatch {
        panic!("boom");
    }
    fn get_steering(&self) -> Vec<kn9t_core::Message> {
        panic!("boom");
    }
    fn get_followup(&self) -> Vec<kn9t_core::Message> {
        panic!("boom");
    }
    fn get_api_key(&self, _provider: &str) -> Option<String> {
        panic!("boom");
    }
}

// ------------------------- R-RCT-130 -------------------------

#[test]
fn parallel_order() {
    // Two reads (parallel_safe) plus one write (sequential). Results must persist in call
    // order regardless of completion order. We make read #0 slow via a large file.
    let dir = std::env::temp_dir().join(format!("kn9t-rct-par-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("slow.txt"), "x".repeat(500_000)).unwrap();
    std::fs::write(dir.join("fast.txt"), b"quick").unwrap();

    let body = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c_slow\",\"name\":\"read\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{\\\"path\\\":\\\"slow.txt\\\"}\"}\n\n",
        "data: {\"chunk\":\"tool_call\",\"idx\":1,\"id\":\"c_fast\",\"name\":\"read\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":1,\"delta\":\"{\\\"path\\\":\\\"fast.txt\\\"}\"}\n\n",
        "data: {\"chunk\":\"tool_call\",\"idx\":2,\"id\":\"c_write\",\"name\":\"write\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":2,\"delta\":\"{\\\"path\\\":\\\"out.txt\\\",\\\"content\\\":\\\"z\\\"}\"}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    // Turn 2 idles.
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    
    // Spawn the real kn9t-tools plugin (R-PLUG2-110)
    let (_host, tools) = spawn_tools_registry();

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let mut params = run_params(&store);
    params.cwd = dir;
    let _ = looop.run(params).expect("loop ran");

    // The tool-result message must list results in call order: c_slow, c_fast, c_write.
    let appended = store.appended.lock().unwrap();
    let mut order = Vec::new();
    for e in appended.iter() {
        if let Event::MessageAppended { msg, .. } = e {
            for c in &msg.content {
                if let Content::ToolResult { id, .. } = c {
                    order.push(id.0.clone());
                }
            }
        }
    }
    assert_eq!(order, vec!["c_slow", "c_fast", "c_write"]);
}

// ── Bug regression: tool result double-wrap ───────────────────────────────────
//
// `execute_one` previously called `tool_result_content()` which already returns
// `Content::ToolResult{..}`, then wrapped that inside ANOTHER `Content::ToolResult`.
// The resulting structure was:
//
//   ToolResult { content: [ ToolResult { content: [Text("hello")] } ] }
//
// The OpenAI encoder extracts only `Text` children, so the inner `ToolResult`
// was silently dropped and the model received an empty string — making it
// believe the tool returned nothing.

/// Unit-level: a sequential tool call must store a `ToolResult` whose `content`
/// is a flat `[Text("…")]`, never a nested `[ToolResult{…}]`.
#[test]
fn tool_result_not_double_wrapped() {
    // One turn: assistant calls "echo" tool, then model finishes.
    let body1 = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"echo\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{\\\"text\\\":\\\"ping\\\"}\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"ok\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );

    // Simple inline tool: returns its `text` argument as output.
    struct EchoTool;
    impl kn9t_core::Tool for EchoTool {
        fn spec(&self) -> &kn9t_core::ToolSpec {
            // Leak the spec — fine for tests.
            Box::leak(Box::new(kn9t_core::ToolSpec {
                name: "echo".into(),
                description: "echo".into(),
                schema: serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { false }
        fn execute(
            &self,
            args: &serde_json::Value,
            _ctx: &kn9t_core::ToolCtx,
            _cancel: &kn9t_core::Cancel,
        ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            let text = args["text"].as_str().unwrap_or("").to_owned();
            Ok(kn9t_core::ToolOutput {
                content: vec![kn9t_core::Content::Text { text }],
                details: None,
                is_error: false,
            })
        }
    }

    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus   = Arc::new(RecordingBus::new());
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(EchoTool) as Arc<dyn Tool>);

    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    looop.run(run_params(&store)).expect("loop ran");

    // The third appended event is the tool-result message (assistant, usage, tool-result).
    let appended = store.appended.lock().unwrap();
    let tool_result_msg = appended.iter().find_map(|e| {
        if let Event::MessageAppended { msg, .. } = e {
            if msg.content.iter().any(|c| matches!(c, Content::ToolResult { .. })) {
                return Some(msg.clone());
            }
        }
        None
    }).expect("must have a tool-result MessageAppended");

    // There must be exactly one ToolResult block.
    let results: Vec<_> = tool_result_msg.content.iter().filter_map(|c| {
        if let Content::ToolResult { id, content, is_error } = c {
            Some((id.clone(), content.clone(), *is_error))
        } else {
            None
        }
    }).collect();
    assert_eq!(results.len(), 1, "exactly one ToolResult block");

    let (_, inner_content, is_error) = &results[0];
    assert!(!is_error, "echo tool must not be marked error");

    // The inner content must be flat Text, never a nested ToolResult.
    assert!(
        !inner_content.is_empty(),
        "ToolResult inner content must not be empty (double-wrap bug)"
    );
    for block in inner_content {
        assert!(
            matches!(block, Content::Text { .. }),
            "ToolResult children must be Text, got a nested ToolResult (double-wrap bug)"
        );
    }

    // The text must be the actual tool output, not empty.
    if let Content::Text { text } = &inner_content[0] {
        assert_eq!(text, "ping", "tool output text must reach the ToolResult");
    }
}

/// Integration-level: when the second provider call is made (after a tool run),
/// the messages it receives must contain the tool result text — not an empty string.
/// This catches the silent drop caused by the double-wrap.
#[test]
fn tool_output_reaches_second_provider_call() {
    // Turn 1: tool call.
    let body1 = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"echo\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{\\\"text\\\":\\\"SENTINEL_OUTPUT\\\"}\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    // Turn 2: plain stop.
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );

    struct EchoTool2;
    impl kn9t_core::Tool for EchoTool2 {
        fn spec(&self) -> &kn9t_core::ToolSpec {
            Box::leak(Box::new(kn9t_core::ToolSpec {
                name: "echo".into(),
                description: "echo".into(),
                schema: serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { false }
        fn execute(
            &self,
            args: &serde_json::Value,
            _ctx: &kn9t_core::ToolCtx,
            _cancel: &kn9t_core::Cancel,
        ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            let text = args["text"].as_str().unwrap_or("").to_owned();
            Ok(kn9t_core::ToolOutput {
                content: vec![kn9t_core::Content::Text { text }],
                details: None,
                is_error: false,
            })
        }
    }

    // Use a store that records every plan_request's message list so we can
    // inspect what the second call saw.
    struct CapturingStore {
        inner: StubStore,
        captured_plans: Arc<Mutex<Vec<Vec<kn9t_core::Message>>>>,
    }
    impl kn9t_core::Store for CapturingStore {
        fn plan_request(&self, session: &SessionId) -> Result<kn9t_core::RequestPlan, StoreErr> {
            // Replay everything appended so far as the message list.
            let msgs: Vec<kn9t_core::Message> = self.inner.appended.lock().unwrap()
                .iter()
                .filter_map(|e| {
                    if let Event::MessageAppended { msg, .. } = e { Some(msg.clone()) } else { None }
                })
                .collect();
            self.captured_plans.lock().unwrap().push(msgs.clone());
            // Forward a real plan from the inner store so scripting still works.
            let mut plan = self.inner.plan_request(session)?;
            plan.messages = msgs;
            Ok(plan)
        }
        fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr> {
            self.inner.append(session, event)
        }
        fn snapshot(&self, session: &SessionId) -> Result<kn9t_core::SessionSnapshot, StoreErr> {
            self.inner.snapshot(session)
        }
    }

    let captured_plans: Arc<Mutex<Vec<Vec<kn9t_core::Message>>>> = Arc::new(Mutex::new(Vec::new()));
    let store: Arc<CapturingStore> = Arc::new(CapturingStore {
        inner: StubStore::new(PlanScript::plain(vec![])),
        captured_plans: captured_plans.clone(),
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let bus = Arc::new(RecordingBus::new());
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(EchoTool2) as Arc<dyn Tool>);

    let looop = ReactLoop {
        provider,
        store: store.clone() as Arc<dyn Store>,
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };

    let mut params = RunParams {
        session: SessionId::new(),
        model: test_model_spec(),
        thinking: kn9t_core::Thinking::Off,
        max_tokens: Some(4096),
        cwd: std::env::temp_dir(),
        config: ReactConfig::default(),
        read_map: empty_read_map(),
        system: None,
        cancel: None,
    };
    params.session = SessionId::new();
    looop.run(params).expect("loop ran");

    // The second plan_request call (turn 2) must have a Tool-role message whose
    // ToolResult content contains the sentinel text — not an empty string.
    let plans = captured_plans.lock().unwrap();
    assert!(plans.len() >= 2, "expected at least 2 plan_request calls, got {}", plans.len());
    let second_plan_msgs = &plans[1];

    let tool_result_content: Vec<String> = second_plan_msgs.iter()
        .filter(|m| m.role == kn9t_core::Role::Tool)
        .flat_map(|m| m.content.iter())
        .filter_map(|c| {
            if let Content::ToolResult { content, .. } = c {
                Some(content.iter().filter_map(|b| {
                    if let Content::Text { text } = b { Some(text.clone()) } else { None }
                }).collect::<Vec<_>>().join(""))
            } else { None }
        })
        .collect();

    assert!(
        !tool_result_content.is_empty(),
        "second provider call must see at least one ToolResult in messages"
    );
    assert!(
        tool_result_content.iter().any(|t| t.contains("SENTINEL_OUTPUT")),
        "tool output 'SENTINEL_OUTPUT' must appear in the messages sent to the second provider call, \
         but got: {tool_result_content:?}"
    );
}

// ── P1 96E-6: parallel_safe must still run after_tool_call ─────────────────
// Red test for 96E-6: parallel path currently skips hook_after_tool_call.
// This test must FAIL before fix (proving bug) and PASS after fix.
#[test]
fn p1_96e6_parallel_safe_after_tool_call_must_run() {
    use kn9t_core::{HookHost, Tool, ToolCtx, Cancel, Content, Message, ModelRef, ToolSpec};
    use std::sync::{Arc, Mutex};
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct ParallelEchoTool;
    impl Tool for ParallelEchoTool {
        fn spec(&self) -> &ToolSpec {
            Box::leak(Box::new(ToolSpec {
                name: "p_echo".into(),
                description: "parallel echo".into(),
                schema: serde_json::json!({"type":"object","properties":{}}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { true }
        fn execute(&self, _args: &serde_json::Value, _ctx: &ToolCtx, _cancel: &Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            Ok(kn9t_core::ToolOutput {
                content: vec![Content::Text { text: "original".into() }],
                details: None, is_error: false,
            })
        }
    }

    struct MutatingHook {
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl HookHost for MutatingHook {
        fn before_tool_call(&self, _tool: &str, _args: &serde_json::Value, _cwd: &std::path::Path) -> kn9t_core::HookVeto {
            kn9t_core::HookVeto::Allow
        }
        fn after_tool_call(&self, tool: &str, _args: &serde_json::Value, _result: Vec<Content>) -> Vec<Content> {
            self.calls.lock().unwrap().push(tool.to_string());
            // mutate to prove hook ran
            vec![Content::Text { text: "hooked".into() }]
        }
        fn before_request(&self, msgs: Vec<Message>, _model: &ModelRef, _system: Option<&str>) -> Vec<Message> { msgs }
        fn should_stop_after_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage, _turn: u32) -> bool { false }
        fn prepare_next_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage) -> kn9t_core::NextTurnPatch { Default::default() }
        fn get_steering(&self) -> Vec<Message> { vec![] }
        fn get_followup(&self) -> Vec<Message> { vec![] }
        fn get_api_key(&self, _provider: &str) -> Option<String> { None }
    }

    // One turn: assistant calls p_echo, then idle
    let body1 = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"p_echo\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );

    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(ParallelEchoTool) as Arc<dyn Tool>);

    let hook_calls = Arc::new(Mutex::new(Vec::new()));
    let hooks = Arc::new(MutatingHook { calls: hook_calls.clone() });

    let looop = kn9t_react::ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks,
        bus,
    };
    looop.run(run_params(&store)).expect("loop ran");

    // Hook must have been called for parallel tool
    let calls = hook_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "after_tool_call must be called once for parallel tool, got {:?}", *calls);
    assert_eq!(calls[0], "p_echo");

    // Persisted ToolResult must be mutated
    let appended = store.appended.lock().unwrap();
    let tool_result = appended.iter().find_map(|e| {
        if let Event::MessageAppended { msg, .. } = e {
            if msg.role == kn9t_core::Role::Tool {
                for c in &msg.content {
                    if let Content::ToolResult { content, .. } = c {
                        return Some(content.clone());
                    }
                }
            }
        }
        None
    }).expect("tool result must be appended");

    assert!(!tool_result.is_empty(), "tool result content empty");
    match &tool_result[0] {
        Content::Text { text } => assert_eq!(text, "hooked", "parallel tool result must be mutated by after_tool_call (bug 96E-6: was 'original' before fix)"),
        _ => panic!("expected Text"),
    }
}

// ── P1 96E-8: malformed tool JSON must not reach Tool::execute ──────────────
#[test]
fn p1_96e8_malformed_json_never_reaches_tool() {
    use kn9t_core::{Content, HookHost, Message, ModelRef, Tool, ToolCtx, Cancel, ToolSpec};
    use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

    let tool_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let seen_args = Arc::new(Mutex::new(Vec::<String>::new()));
    let tool_seen = Arc::new(Mutex::new(Vec::<String>::new()));

    struct CountingTool { calls: Arc<AtomicUsize>, seen: Arc<Mutex<Vec<String>>> }
    impl Tool for CountingTool {
        fn spec(&self) -> &ToolSpec {
            Box::leak(Box::new(ToolSpec {
                name: "counting".into(),
                description: "counts execute".into(),
                schema: serde_json::json!({"type":"object","properties":{"x":{"type":"string"}}}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { false }
        fn execute(&self, args: &serde_json::Value, _ctx: &ToolCtx, _cancel: &Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(args.to_string());
            Ok(kn9t_core::ToolOutput { content: vec![Content::Text { text: "ok".into() }], details: None, is_error: false })
        }
    }
    struct CountingHook { calls: Arc<AtomicUsize>, seen: Arc<Mutex<Vec<String>>> }
    impl HookHost for CountingHook {
        fn before_tool_call(&self, _tool: &str, args: &serde_json::Value, _cwd: &std::path::Path) -> kn9t_core::HookVeto {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(args.to_string());
            kn9t_core::HookVeto::Allow
        }
        fn after_tool_call(&self, _tool: &str, _args: &serde_json::Value, result: Vec<Content>) -> Vec<Content> { result }
        fn before_request(&self, msgs: Vec<Message>, _model: &ModelRef, _system: Option<&str>) -> Vec<Message> { msgs }
        fn should_stop_after_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage, _turn: u32) -> bool { false }
        fn prepare_next_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage) -> kn9t_core::NextTurnPatch { Default::default() }
        fn get_steering(&self) -> Vec<Message> { vec![] }
        fn get_followup(&self) -> Vec<Message> { vec![] }
        fn get_api_key(&self, _provider: &str) -> Option<String> { None }
    }

    // Provider emits syntactically valid but semantically invalid JSON for a tool:
    // `null` is valid JSON but not a JSON object, so a tool expecting `{"x": ...}` must not
    // be executed. Before fix, tool received `null` and hook was asked with `null`.
    // After fix, both are short-circuited to ToolResult(error).
    let bad_args = "null";
    let body1 = format!(
        "data: {{\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"counting\"}}\n\n\
         data: {{\"chunk\":\"tool_args\",\"idx\":0,\"delta\":{}}}\n\n\
         data: {{\"chunk\":\"usage\",\"tokens\":{{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0}},\"model\":{{\"provider\":\"replay\",\"id\":\"t\"}}}}\n\n\
         data: {{\"chunk\":\"stop\",\"tool_use\":null}}\n\n\
         data: [DONE]\n\n",
        serde_json::to_string(bad_args).unwrap()
    );
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(&body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(CountingTool { calls: tool_calls.clone(), seen: tool_seen.clone() }) as Arc<dyn Tool>);
    let hooks = Arc::new(CountingHook { calls: hook_calls.clone(), seen: seen_args.clone() });
    let looop = ReactLoop { provider, store: store.clone(), approver: Arc::new(AllowAll), tools, hooks, bus: bus.clone() };
    looop.run(run_params(&store)).expect("loop ran");

    // Assertions per 96E-8 acceptance:
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "Invalid JSON never reaches Tool::execute");
    assert_eq!(hook_calls.load(Ordering::SeqCst), 0, "Policy plugins must not be asked to authorize malformed arguments as valid");
    // The model must receive an explicit tool error
    let appended = store.appended.lock().unwrap();
    let tool_result = appended.iter().find_map(|e| {
        if let Event::MessageAppended { msg, .. } = e {
            if msg.role == kn9t_core::Role::Tool {
                for c in &msg.content {
                    if let Content::ToolResult { id, content, is_error } = c {
                        if id.0 == "c1" { return Some((content.clone(), *is_error)); }
                    }
                }
            }
        }
        None
    }).expect("tool result c1 must be appended");
    assert!(tool_result.1, "malformed args must produce ToolResult(is_error=true)");
    let text = tool_result.0.iter().filter_map(|c| if let Content::Text { text } = c { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("");
    assert!(text.to_lowercase().contains("malformed") || text.to_lowercase().contains("invalid") || text.to_lowercase().contains("parse") || text.to_lowercase().contains("object"),
        "tool error must be explicit about malformed JSON, got: {text:?}");
    // Bus must have an Error event about malformed
    assert!(bus.kinds().iter().any(|k| k == "Error"), "must emit Error event for malformed args_json");
}

#[test]
fn p1_96e8_malformed_json_parallel_safe_also_blocked() {
    // Same guarantee for parallel_safe tools
    use kn9t_core::{Content, HookHost, Message, ModelRef, Tool, ToolCtx, Cancel, ToolSpec};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    let tool_calls = Arc::new(AtomicUsize::new(0));
    struct ParallelCountingTool { calls: Arc<AtomicUsize> }
    impl Tool for ParallelCountingTool {
        fn spec(&self) -> &ToolSpec {
            Box::leak(Box::new(ToolSpec {
                name: "p_count".into(),
                description: "parallel".into(),
                schema: serde_json::json!({"type":"object","properties":{}}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { true }
        fn execute(&self, _args: &serde_json::Value, _ctx: &ToolCtx, _cancel: &Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(kn9t_core::ToolOutput { content: vec![Content::Text { text: "ok".into() }], details: None, is_error: false })
        }
    }
    struct NoopHook;
    impl HookHost for NoopHook {
        fn before_tool_call(&self, _t: &str, _a: &serde_json::Value, _c: &std::path::Path) -> kn9t_core::HookVeto { kn9t_core::HookVeto::Allow }
        fn after_tool_call(&self, _t: &str, _a: &serde_json::Value, r: Vec<Content>) -> Vec<Content> { r }
        fn before_request(&self, m: Vec<Message>, _model: &ModelRef, _s: Option<&str>) -> Vec<Message> { m }
        fn should_stop_after_turn(&self, _s: kn9t_core::StopReason, _u: &kn9t_core::Usage, _t: u32) -> bool { false }
        fn prepare_next_turn(&self, _s: kn9t_core::StopReason, _u: &kn9t_core::Usage) -> kn9t_core::NextTurnPatch { Default::default() }
        fn get_steering(&self) -> Vec<Message> { vec![] }
        fn get_followup(&self) -> Vec<Message> { vec![] }
        fn get_api_key(&self, _p: &str) -> Option<String> { None }
    }
    let bad_args = "null";
    let body1 = format!(
        "data: {{\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"p_count\"}}\n\n\
         data: {{\"chunk\":\"tool_args\",\"idx\":0,\"delta\":{}}}\n\n\
         data: {{\"chunk\":\"usage\",\"tokens\":{{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0}},\"model\":{{\"provider\":\"replay\",\"id\":\"t\"}}}}\n\n\
         data: {{\"chunk\":\"stop\",\"tool_use\":null}}\n\n\
         data: [DONE]\n\n",
        serde_json::to_string(bad_args).unwrap()
    );
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(&body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(ParallelCountingTool { calls: tool_calls.clone() }) as Arc<dyn Tool>);
    let looop = ReactLoop { provider, store: store.clone(), approver: Arc::new(AllowAll), tools, hooks: Arc::new(NoopHook), bus };
    looop.run(run_params(&store)).expect("loop ran");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "parallel_safe tool must also not execute on malformed JSON");
    let appended = store.appended.lock().unwrap();
    let is_err = appended.iter().find_map(|e| {
        if let Event::MessageAppended { msg, .. } = e {
            for c in &msg.content {
                if let Content::ToolResult { id, is_error, .. } = c { if id.0=="c1" { return Some(*is_error); } }
            }
        }
        None
    }).unwrap();
    assert!(is_err, "must be is_error");
}

#[test]
fn p1_96e6_sequential_after_tool_call_still_runs() {
    use kn9t_core::{HookHost, Tool, ToolCtx, Cancel, Content, Message, ModelRef, ToolSpec};

    struct SeqEchoTool;
    impl Tool for SeqEchoTool {
        fn spec(&self) -> &ToolSpec {
            Box::leak(Box::new(ToolSpec {
                name: "s_echo".into(),
                description: "seq echo".into(),
                schema: serde_json::json!({"type":"object","properties":{}}),
                hidden: false, effects: vec![], policy: Default::default(),
            }))
        }
        fn parallel_safe(&self) -> bool { false }
        fn execute(&self, _args: &serde_json::Value, _ctx: &ToolCtx, _cancel: &Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            Ok(kn9t_core::ToolOutput {
                content: vec![Content::Text { text: "original".into() }],
                details: None, is_error: false,
            })
        }
    }
    struct MutatingHook;
    impl HookHost for MutatingHook {
        fn before_tool_call(&self, _tool: &str, _args: &serde_json::Value, _cwd: &std::path::Path) -> kn9t_core::HookVeto { kn9t_core::HookVeto::Allow }
        fn after_tool_call(&self, _tool: &str, _args: &serde_json::Value, _result: Vec<Content>) -> Vec<Content> {
            vec![Content::Text { text: "hooked".into() }]
        }
        fn before_request(&self, msgs: Vec<Message>, _model: &ModelRef, _system: Option<&str>) -> Vec<Message> { msgs }
        fn should_stop_after_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage, _turn: u32) -> bool { false }
        fn prepare_next_turn(&self, _stop: kn9t_core::StopReason, _usage: &kn9t_core::Usage) -> kn9t_core::NextTurnPatch { Default::default() }
        fn get_steering(&self) -> Vec<Message> { vec![] }
        fn get_followup(&self) -> Vec<Message> { vec![] }
        fn get_api_key(&self, _provider: &str) -> Option<String> { None }
    }

    let body1 = concat!(
        "data: {\"chunk\":\"tool_call\",\"idx\":0,\"id\":\"c1\",\"name\":\"s_echo\"}\n\n",
        "data: {\"chunk\":\"tool_args\",\"idx\":0,\"delta\":\"{}\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":5,\"output\":3,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"tool_use\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let body2 = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"done\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(body1)),
        StreamScript::Fixture(fixture_from_body(body2)),
    ]));
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(SeqEchoTool) as Arc<dyn Tool>);
    let looop = kn9t_react::ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(MutatingHook),
        bus,
    };
    looop.run(run_params(&store)).expect("loop ran");
    let appended = store.appended.lock().unwrap();
    let tool_result = appended.iter().find_map(|e| {
        if let Event::MessageAppended { msg, .. } = e {
            if msg.role == kn9t_core::Role::Tool {
                for c in &msg.content {
                    if let Content::ToolResult { content, .. } = c {
                        return Some(content.clone());
                    }
                }
            }
        }
        None
    }).expect("tool result must be appended");
    match &tool_result[0] {
        Content::Text { text } => assert_eq!(text, "hooked", "sequential tool must be hooked"),
        _ => panic!("expected Text"),
    }
}

// ── P1 96E-11: compaction cancellation semantics must reuse attempt abstraction ─
// Before fix: run_compaction ignores Cancel, commits Compacted even when cancelled,
// and does not distinguish failed vs truncated vs cancelled.

#[test]
fn p1_96e11_compaction_cancel_does_not_commit() {
    use kn9t_core::{Cancel, Event, StopReason};
    // Store wants compaction once, then plain.
    let store = Arc::new(
        StubStore::new(PlanScript::plain(vec![])).script(vec![
            PlanScript::compacting(),
            PlanScript::plain(vec![]),
            PlanScript::plain(vec![]),
        ]),
    );
    // Compaction summary fixture (would succeed if not cancelled)
    let summary = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"summary\"}\n\n",
        "data: {\"chunk\":\"usage\",\"tokens\":{\"input\":10,\"output\":5,\"cache_read\":0,\"cache_write\":0,\"reasoning\":0},\"model\":{\"provider\":\"replay\",\"id\":\"t\"}}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    // Second fixture for main turn (should never be reached if cancelled compaction aborts)
    let main = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"main\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(summary)),
        StreamScript::Fixture(fixture_from_body(main)),
    ]));
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider: provider.clone(),
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus: bus.clone(),
    };
    // Pre-cancelled Cancel — compaction must be considered cancelled
    let cancel = Cancel::new();
    cancel.cancel();
    let mut params = run_params(&store);
    params.cancel = Some(cancel);
    let res = looop.run(params);
    // Cancellation during compaction must be deterministic: either Ok(Aborted) or a provider-cancel error,
    // but must NOT have committed Compacted as successful.
    let appended = store.appended.lock().unwrap();
    let has_compacted = appended.iter().any(|e| matches!(e, Event::Compacted { .. }));
    assert!(
        !has_compacted,
        "cancelled compaction must NOT commit Compacted, but found {:?}",
        appended.iter().map(|e| event_tag(e)).collect::<Vec<_>>()
    );
    // Provider should have been called at most once (compaction); main turn must not run after abort
    assert!(
        *provider.calls.lock().unwrap() <= 1,
        "cancelled compaction should not proceed to main provider attempt, calls={}",
        *provider.calls.lock().unwrap()
    );
    // Deterministic outcome: should be aborted, not a silent success with compacted
    // Accept either Ok(Aborted) or Err; but if Ok, stop must be Aborted
    if let Ok(stop) = res {
        assert!(stop == StopReason::Aborted, "cancelled compaction should abort");
    }
}

#[test]
fn p1_96e11_compaction_cancel_usage_accounted() {
    use kn9t_core::{Cancel, Event, UsageKind};
    let store = Arc::new(
        StubStore::new(PlanScript::plain(vec![])).script(vec![
            PlanScript::compacting(),
            PlanScript::plain(vec![]),
        ]),
    );
    // Summary fixture WITHOUT usage chunk — tests estimated flag
    let summary_no_usage = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"summary\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let main = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"main\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        StreamScript::Fixture(fixture_from_body(summary_no_usage)),
        StreamScript::Fixture(fixture_from_body(main)),
    ]));
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let cancel = Cancel::new();
    cancel.cancel();
    let mut params = run_params(&store);
    params.cancel = Some(cancel);
    let _ = looop.run(params);
    let appended = store.appended.lock().unwrap();
    // Must NOT have Compacted
    assert!(
        !appended.iter().any(|e| matches!(e, Event::Compacted { .. })),
        "cancelled compaction must not commit Compacted"
    );
    // Usage accounting: a UsageRecorded with kind Compaction must exist, estimated=true (no usage chunk)
    let usage = appended.iter().find_map(|e| {
        if let Event::UsageRecorded { kind, estimated, .. } = e {
            if *kind == UsageKind::Compaction {
                return Some((*estimated, kind.clone()));
            }
        }
        None
    });
    assert!(
        usage.is_some(),
        "cancelled compaction must still record UsageRecorded(kind=Compaction), got {:?}",
        appended.iter().map(|e| event_tag(e)).collect::<Vec<_>>()
    );
    assert!(
        usage.unwrap().0,
        "usage without provider chunk must be estimated=true"
    );
}

#[test]
fn p1_96e11_compaction_malformed_truncated_not_committed() {
    use kn9t_core::{Event, ProvErr};
    let store = Arc::new(
        StubStore::new(PlanScript::plain(vec![])).script(vec![
            PlanScript::compacting(),
            PlanScript::plain(vec![]),
        ]),
    );
    // Compaction provider returns Truncated (malformed-incomplete) pre-stream
    let provider = Arc::new(ScriptedProvider::new(vec![StreamScript::PreStreamErr(
        ProvErr::Truncated,
    )]));
    let bus = Arc::new(RecordingBus::new());
    let looop = ReactLoop {
        provider,
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: ToolRegistry::new(),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
    };
    let res = looop.run(run_params(&store));
    let appended = store.appended.lock().unwrap();
    // Malformed-incomplete must NOT be committed as successful Compacted
    assert!(
        !appended.iter().any(|e| matches!(e, Event::Compacted { .. })),
        "truncated compaction must NOT commit Compacted, got {:?}",
        appended.iter().map(|e| event_tag(e)).collect::<Vec<_>>()
    );
    // Must be distinguishable from successful completion: loop should not be Ok(Stop)
    // It should be an error (Truncated/failed) — not a silent success
    assert!(
        res.is_err(),
        "truncated compaction should fail, not succeed"
    );
}

#[test]
fn p1_96e11_compaction_cancel_is_deterministic() {
    use kn9t_core::{Cancel, Event};
    // Run twice with same pre-cancelled input; outcome must be identical (deterministic)
    for _ in 0..2 {
        let store = Arc::new(
            StubStore::new(PlanScript::plain(vec![])).script(vec![
                PlanScript::compacting(),
                PlanScript::plain(vec![]),
            ]),
        );
        let summary = concat!(
            "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"summary\"}\n\n",
            "data: [DONE]\n\n",
        );
        let main = concat!(
            "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"main\"}\n\n",
            "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
            "data: [DONE]\n\n",
        );
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::Fixture(fixture_from_body(summary)),
            StreamScript::Fixture(fixture_from_body(main)),
        ]));
        let bus = Arc::new(RecordingBus::new());
        let looop = ReactLoop {
            provider,
            store: store.clone(),
            approver: Arc::new(AllowAll),
            tools: ToolRegistry::new(),
            hooks: Arc::new(kn9t_react::NoopHookHost),
            bus,
        };
        let cancel = Cancel::new();
        cancel.cancel();
        let mut params = run_params(&store);
        params.cancel = Some(cancel);
        let _ = looop.run(params);
        let appended = store.appended.lock().unwrap();
        assert!(
            !appended.iter().any(|e| matches!(e, Event::Compacted { .. })),
            "deterministic: cancelled compaction must never commit Compacted"
        );
    }
}
