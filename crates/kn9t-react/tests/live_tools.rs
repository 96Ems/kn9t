//! 96E-48 / 96E-47 — the live `ToolSource` and execution-time blocking.
//!
//! Two properties the tickets turn on, both invisible from the outside if you only look at
//! one turn:
//!
//! 1. The loop re-reads its tools per model call, so a plugin appearing or going away lands
//!    *mid-turn* instead of at the next prompt.
//! 2. A blocked tool is still advertised (the `tools` array, and therefore the level-1 cache
//!    prefix, is untouched) but refused before parsing, hooks, or `execute`.

#![allow(clippy::unwrap_used)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use kn9t_core::{
    Cancel, Content, Event, Provider, ProvErr, RequestPlan, SessionId, SessionSnapshot, Store,
    StoreErr, Tool, ToolCall, ToolCtx, ToolOutput, ToolRegistry, ToolSpec,
};
use kn9t_react::{ReactConfig, ReactLoop, RunParams, ToolSource};
use kn9t_test_support::{empty_read_map, test_model_spec, AllowAll, RecordingBus};

struct DummyStore;
impl Store for DummyStore {
    fn plan_request(&self, _s: &SessionId) -> Result<RequestPlan, StoreErr> {
        unreachable!()
    }
    fn append(&self, _s: &SessionId, _e: Event) -> Result<u64, StoreErr> {
        Ok(1)
    }
    fn snapshot(&self, _s: &SessionId) -> Result<SessionSnapshot, StoreErr> {
        unreachable!()
    }
}

struct DummyProvider;
impl Provider for DummyProvider {
    fn name(&self) -> &str {
        "dummy"
    }
    fn stream(
        &self,
        _r: &kn9t_core::Request,
        _c: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<kn9t_core::Chunk, ProvErr>> + Send>, ProvErr> {
        unreachable!()
    }
}

/// Counts its own invocations so a test can prove a call never reached `execute`.
struct Counted {
    spec: ToolSpec,
    hits: Arc<AtomicUsize>,
}

impl Counted {
    /// Hands back an `Arc<dyn Tool>` — what the registry takes — so it is not `new`.
    fn boxed(name: &str, hits: Arc<AtomicUsize>) -> Arc<dyn Tool> {
        Arc::new(Counted {
            spec: ToolSpec {
                name: name.into(),
                description: String::new(),
                schema: serde_json::json!({ "type": "object" }),
                hidden: false,
                effects: vec![],
                policy: Default::default(),
            },
            hits,
        })
    }
}

impl Tool for Counted {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn execute(
        &self,
        _a: &serde_json::Value,
        _c: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, kn9t_core::ToolErr> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![Content::Text { text: "ok".into() }],
            details: None,
            is_error: false,
        })
    }
}

/// A `ToolSource` a test can mutate between calls — the whole point of 96E-48 is that the
/// loop observes such a change without being rebuilt. Also counts snapshots, which is how
/// "re-read per call" is proven rather than assumed.
struct MutableTools {
    registry: Mutex<ToolRegistry>,
    blocked: Mutex<std::collections::HashSet<String>>,
    snapshots: Arc<AtomicUsize>,
}

impl MutableTools {
    fn new(registry: ToolRegistry) -> Arc<Self> {
        Arc::new(MutableTools {
            registry: Mutex::new(registry),
            blocked: Mutex::new(std::collections::HashSet::new()),
            snapshots: Arc::new(AtomicUsize::new(0)),
        })
    }
    fn set(&self, registry: ToolRegistry) {
        *self.registry.lock().unwrap() = registry;
    }
    fn block(&self, name: &str) {
        self.blocked.lock().unwrap().insert(name.to_string());
    }
}

impl ToolSource for MutableTools {
    fn snapshot(&self) -> ToolRegistry {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        self.registry.lock().unwrap().clone()
    }
    fn blocked(&self) -> std::collections::HashSet<String> {
        self.blocked.lock().unwrap().clone()
    }
}

fn loop_with(tools: Arc<dyn ToolSource>, bus: Arc<RecordingBus>) -> ReactLoop {
    ReactLoop {
        provider: Arc::new(DummyProvider),
        store: Arc::new(DummyStore),
        approver: Arc::new(AllowAll),
        tools,
        hooks: Arc::new(kn9t_react::NoopHookHost),
        bus,
        compactor: None,
    }
}

fn params() -> RunParams {
    RunParams {
        session: SessionId::new(),
        model: test_model_spec(),
        thinking: kn9t_core::Thinking::Off,
        max_tokens: None,
        cwd: std::env::temp_dir(),
        config: ReactConfig::default(),
        read_map: empty_read_map(),
        system: None,
        cancel: None,
        disabled_tools: std::collections::HashSet::new(),
        reactivation_reminder: None,
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: kn9t_core::CallId(id.into()),
        name: name.into(),
        args_json: "{}".into(),
    }
}

fn result_text(c: &Content) -> String {
    match c {
        Content::ToolResult { content, .. } => content
            .iter()
            .map(|x| match x {
                Content::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect(),
        _ => String::new(),
    }
}

fn is_error(c: &Content) -> bool {
    matches!(c, Content::ToolResult { is_error: true, .. })
}

// ── 96E-48: the registry is live ────────────────────────────────────────────

/// A tool added after the loop was built is dispatchable without rebuilding the loop. Before
/// 96E-48, `ReactLoop.tools` was a clone taken once per turn, so this call would have hit
/// "unknown tool" until the next prompt.
#[test]
fn a_tool_added_after_the_loop_was_built_is_dispatchable() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::new());
    let looop = loop_with(source.clone(), Arc::new(RecordingBus::new()));
    let p = params();

    // Not there yet: the model can name it, but nothing can run it.
    let out = looop.run_tool_batch(&p, &[call("c1", "late")], &Cancel::new());
    assert!(is_error(&out[0]));
    assert!(
        result_text(&out[0]).contains("unknown tool"),
        "got: {}",
        result_text(&out[0])
    );

    // A plugin loads mid-turn.
    source.set(ToolRegistry::from_tools(vec![Counted::boxed(
        "late",
        hits.clone(),
    )]));

    let out = looop.run_tool_batch(&p, &[call("c2", "late")], &Cancel::new());
    assert!(!is_error(&out[0]), "the new tool must be reachable at once");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

/// And the converse: a tool that disappears (plugin unloaded) stops resolving, rather than
/// the loop holding a stale `Arc` that dispatches into a dead subprocess.
#[test]
fn a_tool_removed_mid_turn_stops_resolving() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::from_tools(vec![Counted::boxed(
        "doomed",
        hits.clone(),
    )]));
    let looop = loop_with(source.clone(), Arc::new(RecordingBus::new()));
    let p = params();

    assert!(!is_error(
        &looop.run_tool_batch(&p, &[call("c1", "doomed")], &Cancel::new())[0]
    ));

    source.set(ToolRegistry::new());

    let out = looop.run_tool_batch(&p, &[call("c2", "doomed")], &Cancel::new());
    assert!(is_error(&out[0]));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "not executed a second time");
}

/// The batch takes ONE snapshot for all its calls. Re-snapshotting per call would make two
/// calls in the same batch resolve against different registries if a plugin changed in
/// between — results would depend on thread scheduling.
#[test]
fn one_batch_dispatches_against_a_single_snapshot() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::from_tools(vec![
        Counted::boxed("a", hits.clone()),
        Counted::boxed("b", hits.clone()),
    ]));
    let counter = source.snapshots.clone();
    counter.store(0, Ordering::SeqCst);
    let looop = loop_with(source, Arc::new(RecordingBus::new()));
    let p = params();

    looop.run_tool_batch(&p, &[call("c1", "a"), call("c2", "b")], &Cancel::new());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "one coherent registry per batch"
    );
}

// ── 96E-47: blocked, but still advertised ──────────────────────────────────

/// A stopped plugin's tool is refused with a message that names the recoverable cause, and
/// `execute` is never reached. Distinct from `disabled_tools` (a session-scoped user choice)
/// both in wording and in origin.
#[test]
fn a_blocked_tool_is_refused_before_execution() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::from_tools(vec![Counted::boxed(
        "gone",
        hits.clone(),
    )]));
    source.block("gone");
    let looop = loop_with(source, Arc::new(RecordingBus::new()));
    let p = params();

    let out = looop.run_tool_batch(&p, &[call("c1", "gone")], &Cancel::new());
    assert!(is_error(&out[0]));
    let msg = result_text(&out[0]);
    assert!(
        msg.contains("stopped"),
        "the reason must be actionable, got: {msg}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "must not reach the dead plugin"
    );
}

/// The point of blocking at execution: the spec stays in the array the provider sees, so the
/// cached prefix is untouched while the plugin is down.
#[test]
fn blocking_does_not_remove_the_tool_from_the_advertised_set() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::from_tools(vec![
        Counted::boxed("kept", hits.clone()),
        Counted::boxed("blocked", hits.clone()),
    ]));
    source.block("blocked");

    let advertised: Vec<String> = source
        .snapshot()
        .visible_specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        advertised,
        vec!["kept", "blocked"],
        "both still offered, in the original order (GI-3)"
    );
}

#[test]
fn blocking_one_tool_leaves_its_siblings_runnable() {
    let hits = Arc::new(AtomicUsize::new(0));
    let source = MutableTools::new(ToolRegistry::from_tools(vec![
        Counted::boxed("ok", hits.clone()),
        Counted::boxed("nope", hits.clone()),
    ]));
    source.block("nope");
    let looop = loop_with(source, Arc::new(RecordingBus::new()));
    let p = params();

    let out = looop.run_tool_batch(&p, &[call("c1", "ok"), call("c2", "nope")], &Cancel::new());
    assert!(!is_error(&out[0]), "unrelated tool still works");
    assert!(is_error(&out[1]));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

// ── the sub-agent filter still composes (96E-17) ────────────────────────────

/// `FilteredTools` narrows a live source per snapshot, not once, so a child session keeps
/// observing lifecycle changes for the tools it was granted — while still never seeing the
/// ones it was not.
#[test]
fn filtered_source_grants_a_subset_yet_stays_live() {
    let hits = Arc::new(AtomicUsize::new(0));
    let inner = MutableTools::new(ToolRegistry::from_tools(vec![Counted::boxed(
        "granted",
        hits.clone(),
    )]));
    let filtered: Arc<dyn ToolSource> = Arc::new(kn9t_react::FilteredTools {
        inner: inner.clone(),
        names: vec!["granted".into()],
    });
    let looop = loop_with(filtered, Arc::new(RecordingBus::new()));
    let p = params();

    assert!(!is_error(
        &looop.run_tool_batch(&p, &[call("c1", "granted")], &Cancel::new())[0]
    ));

    // A tool added later that is NOT in the grant stays invisible to this loop.
    inner.set(ToolRegistry::from_tools(vec![
        Counted::boxed("granted", hits.clone()),
        Counted::boxed("sneaky", hits.clone()),
    ]));
    let out = looop.run_tool_batch(&p, &[call("c2", "sneaky")], &Cancel::new());
    assert!(is_error(&out[0]), "the grant is still enforced");
    assert!(result_text(&out[0]).contains("unknown tool"));
}

#[test]
fn static_tools_wraps_a_registry_and_blocks_nothing() {
    let hits = Arc::new(AtomicUsize::new(0));
    let src = kn9t_react::static_tools(ToolRegistry::from_tools(vec![Counted::boxed(
        "t",
        hits.clone(),
    )]));
    assert_eq!(src.snapshot().len(), 1);
    assert!(src.blocked().is_empty(), "default posture is permissive");
}
