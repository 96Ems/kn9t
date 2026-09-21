//! B8 — a `parallel_safe` tool that ignores `Cancel` must not freeze the turn forever.
//!
//! `run_tool_batch` splits the batch: `parallel_safe` tools go on OS threads, the rest run
//! sequentially. The sequential path checks `cancel.cancelled()` before dispatching
//! (R-RCT-060), but the parallel path never did — not before `thread::spawn`, not while
//! running, and the collection was a bare `h.join()` with no deadline.
//!
//! So one hung tool (a wedged plugin subprocess, a socket with no timeout) froze
//! `run_tool_batch` for the life of the process. ESC did nothing: the turn was already
//! inside `join`. No `TurnFinishing` was ever emitted, so on the server the session's turn
//! slot stayed claimed and every later `/prompt` answered 409 — the session was gone.
//!
//! The invariant these tests pin: cancelling a batch always yields a result for every call,
//! within a bounded time, whatever the tool decides to do.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kn9t_core::{
    CallId, Cancel, Content, Event, ProvErr, Provider, RequestPlan, SessionId, SessionSnapshot,
    Store, StoreErr, Tool, ToolCall, ToolCtx, ToolErr, ToolOutput, ToolRegistry, ToolSpec,
};
use kn9t_react::{ReactConfig, ReactLoop, RunParams};
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

/// A `parallel_safe` tool that never returns and never looks at `Cancel` — the wedged
/// plugin subprocess, modelled honestly. `entered` proves it really started.
struct Hanging {
    spec: ToolSpec,
    entered: Arc<AtomicBool>,
}

impl Hanging {
    fn boxed(name: &str, entered: Arc<AtomicBool>) -> Arc<dyn Tool> {
        Arc::new(Hanging {
            spec: ToolSpec {
                name: name.into(),
                description: String::new(),
                schema: serde_json::json!({ "type": "object" }),
                hidden: false,
                effects: vec![],
                policy: Default::default(),
            },
            entered,
        })
    }
}

impl Tool for Hanging {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn execute(
        &self,
        _a: &serde_json::Value,
        _c: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr> {
        self.entered.store(true, Ordering::SeqCst);
        // Deliberately ignores `cancel`, like a tool blocked in a syscall.
        loop {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A well-behaved `parallel_safe` tool, to prove the normal path is untouched.
struct Quick {
    spec: ToolSpec,
    hits: Arc<AtomicUsize>,
}

impl Quick {
    fn boxed(name: &str, hits: Arc<AtomicUsize>) -> Arc<dyn Tool> {
        Arc::new(Quick {
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

impl Tool for Quick {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    fn execute(
        &self,
        _a: &serde_json::Value,
        _c: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![Content::Text { text: "ok".into() }],
            details: None,
            is_error: false,
        })
    }
}

fn loop_with(registry: ToolRegistry, bus: Arc<RecordingBus>) -> ReactLoop {
    ReactLoop {
        provider: Arc::new(DummyProvider),
        store: Arc::new(DummyStore),
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(registry),
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
        cwd: std::env::current_dir().unwrap(),
        config: ReactConfig::default(),
        read_map: empty_read_map(),
        system: None,
        cancel: None,
        disabled_tools: Default::default(),
        reactivation_reminder: None,
    }
}

fn call(name: &str, id: &str) -> ToolCall {
    ToolCall {
        id: CallId(id.into()),
        name: name.into(),
        args_json: "{}".into(),
    }
}

fn result_ids(results: &[Content]) -> Vec<String> {
    results
        .iter()
        .filter_map(|c| match c {
            Content::ToolResult { id, .. } => Some(id.0.clone()),
            _ => None,
        })
        .collect()
}

/// Run `run_tool_batch` on another thread so the test itself cannot hang: if the batch does
/// not come back within `budget`, that IS the bug.
fn batch_with_deadline(
    loop_: Arc<ReactLoop>,
    calls: Vec<ToolCall>,
    cancel: Cancel,
    budget: Duration,
) -> Option<Vec<Content>> {
    let out = Arc::new(Mutex::new(None));
    let sink = out.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let p = params();
        let r = loop_.run_tool_batch(&p, &calls, &cancel);
        *sink.lock().unwrap() = Some(r);
        let _ = tx.send(());
    });
    match rx.recv_timeout(budget) {
        Ok(()) => out.lock().unwrap().take(),
        Err(_) => None,
    }
}

/// The bug: a hung `parallel_safe` tool + a fired `Cancel` must still produce results.
///
/// Before the fix this blocked in `h.join()` forever and the batch never returned.
#[test]
fn a_cancelled_batch_returns_even_if_a_parallel_tool_hangs() {
    let entered = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::from_tools(vec![Hanging::boxed("hang", entered.clone())]);
    let loop_ = Arc::new(loop_with(registry, Arc::new(RecordingBus::new())));

    let cancel = Cancel::new();
    // Fire ESC shortly after the tool is in flight.
    {
        let c = cancel.clone();
        let entered = entered.clone();
        std::thread::spawn(move || {
            while !entered.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(50));
            c.cancel();
        });
    }

    let started = Instant::now();
    let results = batch_with_deadline(
        loop_,
        vec![call("hang", "c1")],
        cancel,
        Duration::from_secs(20),
    );

    let results = results.expect(
        "run_tool_batch never returned: a hung parallel_safe tool wedged the turn. ESC cannot \
         land (the turn is inside join), no TurnFinishing is emitted, and the session's turn \
         slot stays claimed — every later prompt 409s.",
    );
    assert!(
        entered.load(Ordering::SeqCst),
        "the tool should have started"
    );

    // DESIGN §7.5 / R-RCT-060: every ToolCall gets a ToolResult, even an abandoned one.
    assert_eq!(
        result_ids(&results),
        vec!["c1".to_string()],
        "the abandoned call must still get a synthesized result"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "batch took too long to give up"
    );
}

/// A hung tool must not deny its siblings their results either: the batch reports what it
/// has instead of waiting on the slowest possible outcome.
#[test]
fn a_hung_tool_does_not_swallow_its_siblings_results() {
    let entered = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::from_tools(vec![
        Hanging::boxed("hang", entered.clone()),
        Quick::boxed("quick", hits.clone()),
    ]);
    let loop_ = Arc::new(loop_with(registry, Arc::new(RecordingBus::new())));

    let cancel = Cancel::new();
    {
        let c = cancel.clone();
        let entered = entered.clone();
        std::thread::spawn(move || {
            while !entered.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(50));
            c.cancel();
        });
    }

    let results = batch_with_deadline(
        loop_,
        vec![call("hang", "c1"), call("quick", "c2")],
        cancel,
        Duration::from_secs(20),
    )
    .expect("run_tool_batch never returned with a hung tool in the batch");

    // R-RCT-130: results come back in the model's call order, one per call.
    assert_eq!(
        result_ids(&results),
        vec!["c1".to_string(), "c2".to_string()]
    );
}

/// Non-regression: with no cancellation and well-behaved tools, nothing changes — both run
/// in parallel and both results come back in call order.
#[test]
fn a_healthy_parallel_batch_is_unaffected() {
    let hits = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::from_tools(vec![
        Quick::boxed("a", hits.clone()),
        Quick::boxed("b", hits.clone()),
    ]);
    let loop_ = Arc::new(loop_with(registry, Arc::new(RecordingBus::new())));

    let results = batch_with_deadline(
        loop_,
        vec![call("a", "c1"), call("b", "c2")],
        Cancel::new(),
        Duration::from_secs(20),
    )
    .expect("a healthy batch must return promptly");

    assert_eq!(
        result_ids(&results),
        vec!["c1".to_string(), "c2".to_string()]
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2, "both tools should have run");
    let errored = results
        .iter()
        .any(|c| matches!(c, Content::ToolResult { is_error, .. } if *is_error));
    assert!(!errored, "a healthy batch must not synthesize errors");
}

/// The configured grace actually changes when a hung tool is abandoned — proof the knob is
/// wired through `ReactConfig`, not just stored.
#[test]
fn the_configured_grace_bounds_the_abandon_wait() {
    let entered = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::from_tools(vec![Hanging::boxed("hang", entered.clone())]);
    let loop_ = Arc::new(loop_with(registry, Arc::new(RecordingBus::new())));

    let cancel = Cancel::new();
    cancel.cancel(); // already cancelled: the grace starts on the first poll

    let mut p = params();
    p.config.tool_cancel_grace = Duration::from_millis(200);

    let out = Arc::new(Mutex::new(None));
    let sink = out.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let l = loop_.clone();
    std::thread::spawn(move || {
        let r = l.run_tool_batch(&p, &[call("hang", "c1")], &cancel);
        *sink.lock().unwrap() = Some(r);
        let _ = tx.send(());
    });

    let started = Instant::now();
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a 200ms grace must not take 10s");
    let elapsed = started.elapsed();

    let results = out.lock().unwrap().take().expect("results");
    assert_eq!(result_ids(&results), vec!["c1".to_string()]);
    assert!(
        elapsed < Duration::from_millis(1500),
        "waited {elapsed:?}: the configured 200ms grace was ignored in favour of the old \
         hardcoded 1.5s"
    );
}
