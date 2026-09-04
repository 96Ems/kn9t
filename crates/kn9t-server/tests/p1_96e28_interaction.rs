//! 96E-28 TDD red→green: generic client→host interaction primitive.

use kn9t_core::{SessionId, ModelRef};
use kn9t_plugin::HostApi;
use kn9t_store::SqliteStore;
use kn9t_server::{api, state::ServerState};
use kn9t_core::ToolRegistry;
use serde_json::json;
use std::sync::Arc;

fn temp_state() -> Arc<ServerState> {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    std::mem::forget(tmp); // keep alive for test duration via leak — simpler than scaffolding
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let state = ServerState::new(store, "tok".into(), ToolRegistry::new(), vec![]);
    Arc::new(state)
}

#[test]
fn unknown_interaction_id_is_rejected_400() {
    let state = temp_state();
    // No pending interaction → resolve must fail → route returns 400 unknown_interaction
    let req = api::UiRespondReq { id: 9999, payload: json!({"answer":"hi"}) };
    let resp = kn9t_server::routes::interaction::respond(&state, req);
    // JsonResp serializes to JSON with error code
    assert_eq!(resp.status, 400, "unknown id must be 400, got {} body={}", resp.status, resp.body);
}

#[test]
fn pending_interaction_blocks_and_resolves_via_host_api_and_route() {
    let state = temp_state();
    let session = SessionId::new();
    let model = ModelRef { provider: "test".into(), id: "m".into() };
    kn9t_store::create_session(&state.store, &session, ".", &model).unwrap();

    // Subscribe to bus to observe InteractionRequest SSE event
    let sub = state.buses.subscribe(&session.0, 16);

    // Plugin side: host_api interaction_request blocks on condvar; run on worker thread
    let state_c = state.clone();
    let sess = session.0.clone();
    let handle = std::thread::spawn(move || {
        let api = kn9t_server::host_api::ServerHostApi { state: state_c };
        // This will create the pending slot, emit SSE event, and block
        let res = api.handle("kn9t-ask-user", Some(&sess), "interaction_request", &json!({"payload": {"question":"choose","choices":["a","b"]}}));
        res.unwrap()
    });

    // Wait for SSE event to arrive (proves generic event is emitted verbatim)
    let ev = sub.recv_timeout(std::time::Duration::from_secs(2)).expect("InteractionRequest SSE not emitted");
    let (id, plugin, payload) = match ev {
        kn9t_core::Event::InteractionRequest { id, plugin, payload } => (id, plugin, payload),
        // `Event` deliberately has no Debug (GI-2: payloads are pure data), so
        // name the variant via its serde tag instead.
        other => panic!(
            "expected InteractionRequest, got {}",
            serde_json::to_value(&other).map(|v| v["kind"].to_string()).unwrap_or_else(|_| "<unknown>".into())
        ),
    };
    assert_eq!(plugin, "kn9t-ask-user");
    assert_eq!(payload, json!({"question":"choose","choices":["a","b"]}));

    // Client side: POST /ui-respond with opaque answer → must be accepted
    let req = api::UiRespondReq { id, payload: json!({"value":"b"}) };
    let resp = kn9t_server::routes::interaction::respond(&state, req);
    assert_eq!(resp.status, 200, "known id must be 200");

    // Plugin thread must unblock with the client's opaque payload
    let result = handle.join().unwrap();
    assert_eq!(result.get("payload"), Some(&json!({"value":"b"})), "payload must be forwarded verbatim");

    // Re-responding to same id must now be rejected (one-shot)
    let req2 = api::UiRespondReq { id, payload: json!({"value":"a"}) };
    let resp2 = kn9t_server::routes::interaction::respond(&state, req2);
    assert_eq!(resp2.status, 400, "reused id must be rejected after consumption");
}

#[test]
fn interaction_payload_is_opaque_not_interpreted_by_host() {
    // Host never validates shape — any JSON is accepted and forwarded verbatim
    let state = temp_state();
    let session = SessionId::new();
    let model = ModelRef { provider: "test".into(), id: "m".into() };
    kn9t_store::create_session(&state.store, &session, ".", &model).unwrap();
    let sub = state.buses.subscribe(&session.0, 16);
    let state_c = state.clone();
    let sess = session.0.clone();
    let payload = json!({"form":{"fields":[{"name":"age","type":"number"}],"title":"Enter age"}});
    let handle = std::thread::spawn(move || {
        let api = kn9t_server::host_api::ServerHostApi { state: state_c };
        api.handle("any-plugin", Some(&sess), "interaction_request", &json!({"payload": payload})).unwrap()
    });

    let ev = sub.recv_timeout(std::time::Duration::from_secs(2)).expect("InteractionRequest not emitted");
    if let kn9t_core::Event::InteractionRequest { id, .. } = ev {
        let req = api::UiRespondReq { id, payload: json!({"age": 42, "extra": [1,2,3]}) };
        let resp = kn9t_server::routes::interaction::respond(&state, req);
        assert_eq!(resp.status, 200);
        let result = handle.join().unwrap();
        assert_eq!(result.get("payload"), Some(&json!({"age": 42, "extra":[1,2,3]})));
    } else {
        panic!("expected InteractionRequest");
    }
}
