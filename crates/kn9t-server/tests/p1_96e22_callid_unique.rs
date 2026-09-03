//! 96E-22 TDD: host_api tool_execute must generate unique CallId per invocation.

use kn9t_core::{CallId, Cancel, SessionId, ModelRef, ToolRegistry, ToolSpec, Tool, ToolCtx, ToolOutput, ToolErr, Approver, Decision, ToolCall};
use kn9t_plugin::HostApi;
use kn9t_store::SqliteStore;
use kn9t_server::{state::ServerState, host_api::ServerHostApi};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct RecordingTool {
    spec: ToolSpec,
    recorded: Arc<Mutex<Vec<String>>>,
}

impl Tool for RecordingTool {
    fn spec(&self) -> &ToolSpec { &self.spec }
    fn execute(&self, _args: &serde_json::Value, ctx: &ToolCtx, _cancel: &Cancel) -> Result<ToolOutput, ToolErr> {
        self.recorded.lock().unwrap().push(ctx.call_id.0.clone());
        Ok(ToolOutput { content: vec![kn9t_core::Content::Text { text: "ok".into() }], details: None, is_error: false })
    }
}

struct AllowAll;
impl Approver for AllowAll {
    fn request(&self, _call: &ToolCall, _cwd: &std::path::Path, _reason: &str) -> Decision { Decision::Allow }
}

fn temp_state_with_recorder(recorded: Arc<Mutex<Vec<String>>>) -> Arc<ServerState> {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    std::mem::forget(tmp);
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let spec = ToolSpec {
        name: "bash".into(),
        description: "test".into(),
        schema: json!({}),
        hidden: false,
        effects: vec![],
        policy: Default::default(),
    };
    let tool = Arc::new(RecordingTool { spec: spec.clone(), recorded });
    let registry = ToolRegistry::from_tools(vec![tool as Arc<dyn Tool>]);
    let state = Arc::new(ServerState::new(store, "tok".into(), registry, vec![]));
    // Bypass approval (interactive would need a sink + client approval)
    *state.approver.write().unwrap() = Arc::new(AllowAll);
    state
}

#[test]
fn tool_execute_generates_unique_call_id_per_invocation() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let state = temp_state_with_recorder(recorded.clone());
    let sess = SessionId::new();
    let model = ModelRef { provider: "test".into(), id: "m".into() };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();

    let api = ServerHostApi { state: state.clone() };
    // Same tool name twice in same session must not share CallId
    let r1 = api.handle("plug", Some(&sess.0), "tool_execute", &json!({"session": sess.0, "name":"bash","args":{"cmd":"echo 1"}}));
    assert!(r1.is_ok(), "first tool_execute should succeed: {:?}", r1);
    let r2 = api.handle("plug", Some(&sess.0), "tool_execute", &json!({"session": sess.0, "name":"bash","args":{"cmd":"echo 2"}}));
    assert!(r2.is_ok(), "second tool_execute should succeed: {:?}", r2);

    let ids = recorded.lock().unwrap().clone();
    assert_eq!(ids.len(), 2, "both invocations should have executed");
    assert_ne!(ids[0], ids[1], "CallIds must be unique per invocation, got {:?} and {:?}", ids[0], ids[1]);
    assert!(ids[0].starts_with("plugin-bash-"), "call id should be plugin-bash-{{counter}}, got {}", ids[0]);
    assert!(ids[1].starts_with("plugin-bash-"), "call id should be plugin-bash-{{counter}}, got {}", ids[1]);
}

#[test]
fn live_tool_calls_not_overwritten_by_repeated_tool_execute() {
    // Verify the live_tool_calls table keeps distinct rows (no INSERT OR REPLACE collision)
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let state = temp_state_with_recorder(recorded.clone());
    let sess = SessionId::new();
    let model = ModelRef { provider: "test".into(), id: "m".into() };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();

    // Create a tool that emits progress (which populates live_tool_calls)
    // Our RecordingTool doesn't emit progress, so we test via direct DB: simulate two tool_execute that would have same old CallId
    // With fix, the call_ids are unique, so inserting both into live_tool_calls should keep 2 rows.
    // We simulate by directly inserting via store's live progress API using the recorded ids.

    let api = ServerHostApi { state: state.clone() };
    api.handle("plug", Some(&sess.0), "tool_execute", &json!({"session": sess.0, "name":"bash","args":{}})).unwrap();
    api.handle("plug", Some(&sess.0), "tool_execute", &json!({"session": sess.0, "name":"bash","args":{}})).unwrap();

    let ids = recorded.lock().unwrap().clone();
    // Manually drive live_tool_calls via store helper: begin_live_tool_call for each id
    for id in &ids {
        let _ = state.store.begin_live_tool_call(&sess, &CallId(id.clone()), "bash");
    }
    let _ = state.store.append_live_tool_progress(&sess, &CallId(ids[0].clone()), "first");
    let _ = state.store.append_live_tool_progress(&sess, &CallId(ids[1].clone()), "second");

    let rows: Vec<String> = state.store.query_strings("SELECT call_id FROM live_tool_calls WHERE session_id=?1 ORDER BY call_id", &[&sess.0]).unwrap_or_default();
    // Should have 2 distinct call_ids, not 1 overwritten
    assert_eq!(rows.len(), 2, "live_tool_calls should have 2 rows after fix, got {:?}", rows);
}
