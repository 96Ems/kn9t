//! B4 — the ReAct turn loop must be bounded.
//!
//! `ReactLoop::run` is `loop { turn += 1; ... }` with no ceiling. The only way out when the
//! model keeps emitting tool calls is `should_stop_after_turn`, whose default is `false`
//! (`NoopHookHost`, R-RCT-100) and whose panic fallback is *also* `false` (R-RCT-110). So a
//! model that re-issues the same tool call forever keeps the loop spending money forever,
//! with no operator-visible limit and nothing to abort it but ESC.
//!
//! `ReactConfig` already bounds truncation re-issues and compaction re-plans; the turn count
//! was the one unbounded axis. These tests pin the bound and, just as importantly, pin that
//! a normal short run is unaffected by it.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kn9t_core::{
    Cancel, Chunk, ProvErr, Provider, Request, SessionId, StopReason, Store, ToolRegistry,
};
use kn9t_react::{ReactConfig, ReactError, ReactLoop, RunParams};
use kn9t_test_support::{
    empty_read_map, test_model_spec, AllowAll, PlanScript, RecordingBus, StubStore,
};

/// How many provider calls the test tolerates before declaring the loop unbounded. Well above
/// any `max_turns` used here, so a green run never approaches it — but low enough that the
/// pre-fix behaviour fails in a second instead of burning the harness.
const RUNAWAY_GUARD: usize = 200;

/// A provider that answers *every* call with the same tool call, so the loop always has a
/// reason to take another turn. This is the shape of a model stuck in a retry rut.
///
/// It counts calls and panics past `RUNAWAY_GUARD`: without the guard an unbounded loop would
/// hang the test runner rather than fail it.
struct AlwaysToolCall {
    calls: Arc<AtomicUsize>,
}

impl AlwaysToolCall {
    fn new() -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(AlwaysToolCall {
                calls: calls.clone(),
            }),
            calls,
        )
    }
}

impl Provider for AlwaysToolCall {
    fn name(&self) -> &str {
        "always-tool-call"
    }

    fn stream(
        &self,
        _req: &Request,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(
            n <= RUNAWAY_GUARD,
            "provider called {n} times: the ReAct loop is unbounded (B4)"
        );
        // One tool call + a tool_use stop: exactly what makes `execute_turn` return
        // `Continue`. The tool name resolves to nothing, which is fine — an unknown tool
        // still yields a ToolResult and the loop still takes another turn.
        let chunks: Vec<Result<Chunk, ProvErr>> = vec![
            Ok(Chunk::ToolCall {
                idx: 0,
                id: kn9t_core::CallId(format!("call_{n}")),
                name: "loop_forever".into(),
            }),
            Ok(Chunk::ToolArgs {
                idx: 0,
                delta: "{}".into(),
            }),
            Ok(Chunk::Stop(StopReason::ToolUse)),
        ];
        Ok(Box::new(chunks.into_iter()))
    }
}

/// A provider that returns plain text and a clean stop: one turn and the loop idles.
struct OneAndDone {
    calls: Arc<AtomicUsize>,
}

impl Provider for OneAndDone {
    fn name(&self) -> &str {
        "one-and-done"
    }

    fn stream(
        &self,
        _req: &Request,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks: Vec<Result<Chunk, ProvErr>> = vec![
            Ok(Chunk::Text {
                idx: 0,
                delta: "done".into(),
            }),
            Ok(Chunk::Stop(StopReason::Stop)),
        ];
        Ok(Box::new(chunks.into_iter()))
    }
}

fn params(max_turns: u32) -> RunParams {
    let config = ReactConfig {
        max_turns,
        ..ReactConfig::default()
    };
    RunParams {
        session: SessionId::new(),
        model: test_model_spec(),
        thinking: kn9t_core::Thinking::Off,
        max_tokens: Some(4096),
        cwd: std::env::temp_dir(),
        config,
        read_map: empty_read_map(),
        system: None,
        cancel: None,
        disabled_tools: std::collections::HashSet::new(),
        reactivation_reminder: None,
    }
}

fn loop_with(provider: Arc<dyn Provider>, store: Arc<dyn Store>, bus: Arc<RecordingBus>) -> ReactLoop {
    ReactLoop {
        provider,
        store,
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(ToolRegistry::new()),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: None,
    }
}

/// The reproduction: a model that never stops asking for tools must not run forever.
#[test]
fn a_model_that_always_calls_a_tool_hits_the_turn_ceiling() {
    let (provider, calls) = AlwaysToolCall::new();
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let looop = loop_with(provider, store, bus.clone());

    let err = looop
        .run(params(3))
        .map(|_| ())
        .expect_err("an unbounded tool-call loop must terminate with an error");

    assert!(
        matches!(err, ReactError::TurnLimit),
        "expected TurnLimit, got {err:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "exactly max_turns provider calls, then refuse to continue"
    );
}

/// The ceiling scales with the config rather than being hardcoded.
#[test]
fn the_ceiling_follows_the_configured_max_turns() {
    for max in [1u32, 2, 5] {
        let (provider, calls) = AlwaysToolCall::new();
        let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
        let bus = Arc::new(RecordingBus::new());
        let looop = loop_with(provider, store, bus);

        let err = looop.run(params(max)).map(|_| ()).expect_err("must stop");
        assert!(matches!(err, ReactError::TurnLimit), "max={max}: {err:?}");
        assert_eq!(
            calls.load(Ordering::SeqCst) as u32,
            max,
            "max={max}: provider calls must equal the ceiling"
        );
    }
}

/// Hitting the ceiling still closes the turn properly: the operator sees why it stopped, and
/// the server still gets its `TurnFinishing`/`TurnEnded` pair (which is what clears the abort
/// handle — without it the session would wedge at 409, cf. B1/B6).
#[test]
fn hitting_the_ceiling_still_emits_the_end_of_turn_events() {
    let (provider, _calls) = AlwaysToolCall::new();
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let looop = loop_with(provider, store, bus.clone());

    let _ = looop.run(params(2));

    let kinds = bus.kinds();
    assert!(
        kinds.iter().any(|k| k == "TurnFinishing"),
        "the server needs TurnFinishing to clear the turn: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "TurnEnded"),
        "clients need TurnEnded: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "Error"),
        "the reason must be visible: {kinds:?}"
    );
}

/// The bound must not disturb a normal run. A single-turn conversation finishes on its own
/// terms, nowhere near the ceiling, and returns its real stop reason.
#[test]
fn a_normal_short_run_is_unaffected_by_the_ceiling() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(OneAndDone {
        calls: calls.clone(),
    });
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());
    let looop = loop_with(provider, store, bus);

    let stop = looop.run(params(100)).expect("a clean run must succeed");
    assert!(stop == StopReason::Stop, "real stop reason preserved");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one turn, one provider call");
}

/// The shipped default must be a real ceiling, not `u32::MAX` dressed up as one.
#[test]
fn the_default_config_has_a_finite_ceiling() {
    let c = ReactConfig::default();
    assert!(c.max_turns > 0, "a zero ceiling would refuse every turn");
    assert!(
        c.max_turns < 10_000,
        "default max_turns={} is not a meaningful bound",
        c.max_turns
    );
}
