//! Unit tests extracted from `src/exec.rs` — covers `authorize`, `run_tool_batch`,
//! `synth_error`, `estimated_assembled`, and `ensure_nonempty_content`.
//!
//! Internal helpers are re-exported with `#[doc(hidden)] pub` in `src/lib.rs` so
//! this integration test binary can access them without a feature flag.

#![allow(clippy::unwrap_used)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use kn9t_core::{
    Cancel, Content, Event, HookHost, HookVeto, LiveEvent, Message,
    ModelRef, NextTurnPatch, Provider, ProvErr, RequestPlan, SessionId, SessionSnapshot,
    StopReason, Store, StoreErr, Tool, ToolCall, ToolCtx, ToolOutput, ToolRegistry, ToolSpec,
    Usage,
};
use kn9t_react::{
    ensure_nonempty_content, estimated_assembled, synth_error, CallPlan, ReactConfig, ReactLoop,
    RunParams,
};
use kn9t_test_support::{AllowAll, RecordingBus, empty_read_map, test_model_spec};

// ── Local stubs ───────────────────────────────────────────────────────────────

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
    ) -> Result<
        Box<dyn Iterator<Item = Result<kn9t_core::Chunk, ProvErr>> + Send>,
        ProvErr,
    > {
        unreachable!()
    }
}

struct CountingTool(Arc<AtomicUsize>);
impl Tool for CountingTool {
    fn spec(&self) -> &ToolSpec {
        Box::leak(Box::new(ToolSpec {
            name: "x".into(),
            description: "".into(),
            schema: serde_json::json!({}),
            hidden: false,
            effects: vec![],
            policy: Default::default(),
        }))
    }
    fn execute(
        &self,
        _a: &serde_json::Value,
        _c: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, kn9t_core::ToolErr> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![Content::Text { text: "ok".into() }],
            details: None,
            is_error: false,
        })
    }
}

struct CountingHook(Arc<AtomicUsize>);
impl HookHost for CountingHook {
    fn before_tool_call(
        &self,
        _t: &str,
        _a: &serde_json::Value,
        _c: &std::path::Path,
    ) -> HookVeto {
        self.0.fetch_add(1, Ordering::SeqCst);
        HookVeto::Allow
    }
    fn after_tool_call(
        &self,
        _t: &str,
        _a: &serde_json::Value,
        _cwd: &std::path::Path,
        r: Vec<Content>,
    ) -> Vec<Content> {
        r
    }
    fn before_request(
        &self,
        m: Vec<Message>,
        _model: &ModelRef,
        _s: Option<&str>,
    ) -> Vec<Message> {
        m
    }
    fn should_stop_after_turn(
        &self,
        _s: StopReason,
        _u: &Usage,
        _t: u32,
    ) -> bool {
        false
    }
    fn prepare_next_turn(&self, _s: StopReason, _u: &Usage) -> NextTurnPatch {
        Default::default()
    }
    fn get_steering(&self) -> Vec<Message> {
        vec![]
    }
    fn get_followup(&self) -> Vec<Message> {
        vec![]
    }
    fn get_api_key(&self, _p: &str) -> Option<String> {
        None
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_loop(
    tool_calls: Arc<AtomicUsize>,
    hook_calls: Arc<AtomicUsize>,
    bus: Arc<RecordingBus>,
) -> ReactLoop {
    let mut tools = ToolRegistry::new();
    tools.push(Arc::new(CountingTool(tool_calls)) as Arc<dyn Tool>);
    ReactLoop {
        provider: Arc::new(DummyProvider),
        store: Arc::new(DummyStore),
        approver: Arc::new(AllowAll),
        tools: kn9t_react::static_tools(tools),
        hooks: Arc::new(CountingHook(hook_calls)),
        bus,
        compactor: None,
    }
}

fn make_params() -> RunParams {
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

// ── malformed JSON must not reach Tool::execute ────────────────────────

#[test]
fn authorize_malformed_json_is_deny() {
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let bus = Arc::new(RecordingBus::new());
    let looop = make_loop(tool_calls.clone(), hook_calls.clone(), bus.clone());
    let params = make_params();

    // Case 1: syntactically invalid JSON
    let call_bad = ToolCall {
        id: kn9t_core::CallId("c1".into()),
        name: "x".into(),
        args_json: "{not valid json".into(),
    };
    let cancel = Cancel::new();
    let plan = looop.authorize(&params, &call_bad, &cancel);
    assert!(
        matches!(plan, CallPlan::Deny(_)),
        "malformed JSON must be Deny, got Execute"
    );
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        0,
        "tool must not be called for malformed"
    );
    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        0,
        "hook must not be called for malformed"
    );

    // Also check that run_tool_batch produces is_error ToolResult and does not call tool
    let batch = looop.run_tool_batch(&params, &[call_bad.clone()], &Cancel::new());
    assert_eq!(batch.len(), 1);
    match &batch[0] {
        Content::ToolResult { id, is_error, content } => {
            assert_eq!(id.0, "c1");
            assert!(is_error, "must be is_error");
            let txt = content
                .iter()
                .filter_map(|c| {
                    if let Content::Text { text } = c {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            assert!(
                txt.to_lowercase().contains("malformed"),
                "error must mention malformed, got {txt:?}"
            );
        }
        _ => panic!("expected ToolResult"),
    }
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        0,
        "run_tool_batch must not call tool for malformed"
    );

    // Case 2: valid JSON but not object (null)
    let call_null = ToolCall {
        id: kn9t_core::CallId("c2".into()),
        name: "x".into(),
        args_json: "null".into(),
    };
    let plan2 = looop.authorize(&params, &call_null, &cancel);
    assert!(matches!(plan2, CallPlan::Deny(_)), "null must be Deny");
    let batch2 = looop.run_tool_batch(&params, &[call_null], &Cancel::new());
    match &batch2[0] {
        Content::ToolResult { is_error, .. } => assert!(is_error),
        _ => panic!("expected ToolResult"),
    }
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0, "null must not reach tool");

    // Bus must have Error events
    let evs = bus.snapshot();
    assert!(
        evs.iter().any(|e| matches!(e, LiveEvent::Error { .. })),
        "must emit Error"
    );
}

// ── synth_error ───────────────────────────────────────────────────────────────

#[test]
fn test_synth_error_creates_tool_result() {
    let call_id = kn9t_core::CallId("call-123".into());
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

// ── estimated_assembled ───────────────────────────────────────────────────────

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
    assert!(matches!(assembled.message.role, kn9t_core::Role::Assistant));
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

// ── ensure_nonempty_content ───────────────────────────────────────────────────

/// Provider APIs (Anthropic, OpenAI) reject tool results with empty content.
/// `ensure_nonempty_content` must substitute a placeholder when the tool
/// returns an empty vec or only empty Text blocks.
#[test]
fn test_ensure_nonempty_content_empty_vec() {
    let result = ensure_nonempty_content(vec![]);
    assert_eq!(result.len(), 1);
    match &result[0] {
        Content::Text { text } => assert_eq!(text, "(no output)"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_ensure_nonempty_content_empty_text() {
    let input = vec![Content::Text { text: String::new() }];
    let result = ensure_nonempty_content(input);
    assert_eq!(result.len(), 1);
    match &result[0] {
        Content::Text { text } => assert_eq!(text, "(no output)"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_ensure_nonempty_content_multiple_empty_texts() {
    let input = vec![
        Content::Text { text: String::new() },
        Content::Text { text: String::new() },
    ];
    let result = ensure_nonempty_content(input);
    assert_eq!(result.len(), 1);
    match &result[0] {
        Content::Text { text } => assert_eq!(text, "(no output)"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_ensure_nonempty_content_preserves_nonempty() {
    let input = vec![Content::Text { text: "hello".into() }];
    let result = ensure_nonempty_content(input);
    assert_eq!(result.len(), 1);
    match &result[0] {
        Content::Text { text } => assert_eq!(text, "hello"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_ensure_nonempty_content_mixed_keeps_all() {
    // If at least one Text is non-empty, keep the original vec as-is
    let input = vec![
        Content::Text { text: String::new() },
        Content::Text { text: "data".into() },
    ];
    let result = ensure_nonempty_content(input);
    assert_eq!(result.len(), 2);
    match &result[1] {
        Content::Text { text } => assert_eq!(text, "data"),
        _ => panic!("expected Text"),
    }
}

