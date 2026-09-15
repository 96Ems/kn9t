//! B3 — a provider-reported context overflow must never spin the attempt loop.
//!
//! `turn.rs` handles `Attempt::ContextOverflow` with a bare `continue`, and `one_attempt`
//! only charges `replans` when **the store** asked to compact (`plan.compact.is_some()`).
//! So when the provider reports overflow while the store's local estimate says there is
//! nothing to compact — exactly what a stale or wrong `ctx_window` produces, since the
//! store compacts at `0.80 * ctx_window` from its own token count — the loop re-issues a
//! byte-identical request forever. Every iteration is a billed provider call, and the
//! cancel flag is only read after the stream, so ESC does not break out either.
//!
//! Each test caps the provider at a small number of calls and panics past it, so a
//! regression fails in milliseconds instead of hanging the suite.

#![allow(clippy::unwrap_used)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use kn9t_core::{
    Cancel, Chunk, CompactSpan, CompactionPlan, Compactor, Content, Message, MsgId, ProvErr,
    Provider, Request, Role, StopReason, ToolRegistry,
};
use kn9t_react::{ReactConfig, ReactLoop, RunParams};
use kn9t_test_support::{
    empty_read_map, fixture_from_body, test_model_spec, AllowAll, PlanScript, RecordingBus,
    ScriptedProvider, StreamScript, StubStore,
};

/// Hard ceiling on provider calls. A correct loop stays far below this; a looping one trips
/// the panic immediately and the test fails fast instead of running until the harness dies.
const CALL_CEILING: usize = 12;

/// A provider that always reports `ContextOverflow` pre-stream, counting attempts and
/// refusing to be called more than `CALL_CEILING` times.
struct AlwaysOverflow {
    calls: Arc<AtomicUsize>,
}

impl Provider for AlwaysOverflow {
    fn name(&self) -> &str {
        "always-overflow"
    }

    fn stream(
        &self,
        _req: &Request,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(
            n <= CALL_CEILING,
            "provider called {n} times — the attempt loop is not bounded on ContextOverflow \
             (B3: `continue` without charging `replans`)"
        );
        Err(ProvErr::ContextOverflow)
    }
}

/// A compactor that always succeeds, so the legitimate re-plan path can be exercised.
struct OkCompactor {
    calls: Arc<AtomicUsize>,
}

impl Compactor for OkCompactor {
    fn compact(
        &self,
        span: CompactSpan,
        _model: &kn9t_core::ModelRef,
    ) -> Result<CompactionPlan, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompactionPlan {
            summary: Message {
                id: MsgId::new(),
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: format!("summary of {} msgs", span.messages.len()),
                }],
                silent: false,
            },
            handoff: None,
        })
    }
}

fn params(config: ReactConfig) -> RunParams {
    RunParams {
        session: kn9t_core::SessionId::new(),
        model: test_model_spec(),
        thinking: kn9t_core::Thinking::Off,
        max_tokens: Some(1024),
        cwd: std::env::temp_dir(),
        config,
        read_map: empty_read_map(),
        system: None,
        cancel: None,
        disabled_tools: std::collections::HashSet::new(),
        reactivation_reminder: None,
    }
}

/// The bug, reduced: provider says "overflow", store offers nothing to compact.
///
/// Nothing about the request changes between iterations, so the only correct outcome is a
/// bounded failure. Before the fix this looped forever, billing a provider call each time.
#[test]
fn provider_overflow_with_nothing_to_compact_terminates() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(AlwaysOverflow {
        calls: calls.clone(),
    });
    // `plain` => `plan.compact` is always `None`: the store sees nothing worth compacting.
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let looop = ReactLoop {
        provider,
        store,
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(ToolRegistry::new()),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: None,
    };

    let result = looop.run(params(ReactConfig::default()));

    assert!(
        result.is_err(),
        "an unrecoverable context overflow must end the turn, got Ok"
    );
    let n = calls.load(Ordering::SeqCst);
    assert!(
        n <= CALL_CEILING,
        "provider was called {n} times for a deterministically identical request"
    );
}

/// Same, with a compactor installed. The compactor cannot help — the store never asks for
/// compaction — so this must still terminate rather than spin.
#[test]
fn provider_overflow_terminates_even_with_a_compactor_installed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let compactions = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(AlwaysOverflow {
        calls: calls.clone(),
    });
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])));
    let bus = Arc::new(RecordingBus::new());

    let looop = ReactLoop {
        provider,
        store,
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(ToolRegistry::new()),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: Some(Arc::new(OkCompactor {
            calls: compactions.clone(),
        })),
    };

    let result = looop.run(params(ReactConfig::default()));

    assert!(result.is_err(), "must terminate, got Ok");
    assert!(
        calls.load(Ordering::SeqCst) <= CALL_CEILING,
        "provider call count unbounded"
    );
}

/// The legitimate path must keep working: the store demands compaction once, the compactor
/// answers, the re-plan comes back clean, and the turn completes. This is the regression
/// guard for the fix — bounding the overflow loop must not break R-RCT-090's single replan.
#[test]
fn store_demanded_compaction_still_replans_once_and_succeeds() {
    let body = concat!(
        "data: {\"chunk\":\"text\",\"idx\":0,\"delta\":\"ok\"}\n\n",
        "data: {\"chunk\":\"stop\",\"stop\":null}\n\n",
        "data: [DONE]\n\n",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![StreamScript::Fixture(
        fixture_from_body(body),
    )]));
    // First `plan_request` demands compaction; the default (plain) answers the re-plan.
    let store = Arc::new(StubStore::new(PlanScript::plain(vec![])).script(vec![
        PlanScript::compacting(),
    ]));
    let compactions = Arc::new(AtomicUsize::new(0));
    let bus = Arc::new(RecordingBus::new());

    let looop = ReactLoop {
        provider: provider.clone(),
        store: store.clone(),
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(ToolRegistry::new()),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: Some(Arc::new(OkCompactor {
            calls: compactions.clone(),
        })),
    };

    let stop = looop
        .run(params(ReactConfig::default()))
        .expect("compaction then success must not error");

    assert!(matches!(stop, StopReason::Stop), "unexpected stop reason");
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        1,
        "exactly one compaction sub-turn (R-RCT-090)"
    );
    assert_eq!(
        *provider.calls.lock().unwrap(),
        1,
        "one real provider call after the re-plan"
    );
    let tags = store.appended_tags();
    assert!(
        tags.iter().any(|t| t == "Compacted"),
        "the compaction must be committed, got {tags:?}"
    );
}

/// A store that keeps demanding compaction is the pre-existing `CompactionLoop` guard
/// (R-RCT-090). Kept here so the new bound cannot be mistaken for this one.
#[test]
fn repeated_compaction_demand_is_still_refused() {
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    // Every `plan_request` demands compaction, including the re-plan.
    let store = Arc::new(StubStore::new(PlanScript::compacting()));
    let compactions = Arc::new(AtomicUsize::new(0));
    let bus = Arc::new(RecordingBus::new());

    let looop = ReactLoop {
        provider,
        store,
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(ToolRegistry::new()),
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: Some(Arc::new(OkCompactor {
            calls: compactions.clone(),
        })),
    };

    let result = looop.run(params(ReactConfig::default()));
    assert!(
        result.is_err(),
        "a second compact demand is fatal (R-RCT-090), got Ok"
    );
}
