//! 96E-23 TDD: structured plugin→TUI UI directive primitive.
//!
//! Must be session-scoped (reuse 96E-21 routing fix), structured (non-text),
//! and PluginNotification must keep working.

use kn9t_core::ToolRegistry;
use kn9t_core::{ModelRef, SessionId};
use kn9t_plugin::HostApi;
use kn9t_server::{host_api::ServerHostApi, state::ServerState};
use kn9t_store::SqliteStore;
use serde_json::json;
use std::sync::Arc;

fn temp_state() -> Arc<ServerState> {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    std::mem::forget(tmp);
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let state = ServerState::new(store, "tok".into(), ToolRegistry::new(), vec![]);
    Arc::new(state)
}

#[test]
fn plugin_can_push_structured_directive_scoped_to_one_session() {
    let state = temp_state();
    let sess_a = SessionId::new();
    let sess_b = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess_a, ".", &model).unwrap();
    kn9t_store::create_session(&state.store, &sess_b, ".", &model).unwrap();

    let sub_a = state.buses.subscribe(&sess_a.0, 16);
    let sub_b = state.buses.subscribe(&sess_b.0, 16);

    let api = ServerHostApi {
        state: state.clone(),
    };
    let res = api.handle(
        "my-plugin",
        Some(&sess_a.0),
        "ui_directive",
        &json!({
            "session": sess_a.0,
            "target": "sidebar",
            "op": "show",
            "payload": {"panel":"debug","count":42}
        }),
    );
    assert!(res.is_ok(), "ui_directive should succeed, got {:?}", res);

    // Only sess_a should receive UiDirective
    let ev_a = sub_a
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("sess_a should receive UiDirective");
    match ev_a {
        kn9t_core::Event::UiDirective {
            plugin,
            target,
            op,
            payload,
        } => {
            assert_eq!(plugin, "my-plugin");
            assert_eq!(target, "sidebar");
            assert_eq!(op, "show");
            assert_eq!(payload, json!({"panel":"debug","count":42}));
        }
        other => panic!(
            "expected UiDirective on A, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }
    // sess_b must NOT receive anything (no broadcast)
    assert!(
        sub_b
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_none(),
        "sess_b must not receive directive for sess_a (no broadcast fallback)"
    );

    // Also ui_push alias should work (ticket says ui_push)
    let res2 = api.handle(
        "my-plugin",
        Some(&sess_b.0),
        "ui_push",
        &json!({
            "session": sess_b.0,
            "target": "main",
            "op": "update",
            "payload": {"text":"hello"}
        }),
    );
    assert!(res2.is_ok(), "ui_push alias should succeed");
    let ev_b = sub_b
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("sess_b should receive via ui_push alias");
    assert!(
        matches!(ev_b, kn9t_core::Event::UiDirective { .. }),
        "expected UiDirective via alias, got {}",
        serde_json::to_string(&ev_b).unwrap_or_default()
    );
}

#[test]
fn ui_directive_validation_rejects_missing_target_or_op() {
    let state = temp_state();
    let sess = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();
    let api = ServerHostApi { state };
    let r1 = api.handle(
        "p",
        Some(&sess.0),
        "ui_directive",
        &json!({"session": sess.0, "op":"show","payload":{}}),
    );
    assert!(r1.is_err(), "missing target must be rejected");
    let r2 = api.handle(
        "p",
        Some(&sess.0),
        "ui_directive",
        &json!({"session": sess.0, "target":"x","payload":{}}),
    );
    assert!(r2.is_err(), "missing op must be rejected");
    let r3 = api.handle(
        "p",
        Some(&sess.0),
        "ui_directive",
        &json!({"session": sess.0, "target":"","op":"show"}),
    );
    assert!(r3.is_err(), "empty target must be rejected");
}

#[test]
fn ui_directive_payload_is_opaque_structured_and_plugin_notification_still_works() {
    let state = temp_state();
    let sess = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();
    let sub = state.buses.subscribe(&sess.0, 16);
    let api = ServerHostApi {
        state: state.clone(),
    };

    // Structured, non-text payload (nested object/array) must be forwarded verbatim
    let complex = json!({"form":{"fields":[{"name":"age","type":"number"}],"title":"Enter age"},"extra":[1,2,3]});
    let res = api.handle(
        "p",
        Some(&sess.0),
        "ui_directive",
        &json!({"session":sess.0,"target":"page","op":"render","payload": complex.clone()}),
    );
    assert!(res.is_ok());
    let ev = sub.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    match ev {
        kn9t_core::Event::UiDirective { payload, .. } => assert_eq!(payload, complex),
        other => panic!(
            "expected UiDirective, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }

    // PluginNotification (free-text) must still work unchanged
    let sub2 = state.buses.subscribe(&sess.0, 16);
    // Emulate what plugin host does for PluginNotification: LiveEvent::PluginNotification
    use kn9t_core::{EventSink, LiveEvent};
    use kn9t_server::bus::SessionSink;
    let sink = SessionSink::with_store(
        state.buses.bus_for(&sess.0),
        state.store.clone(),
        sess.clone(),
    );
    sink.emit(LiveEvent::PluginNotification {
        payload: json!({"plugin":"p","message":"hello free text"}),
    });
    let ev2 = sub2
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    match ev2 {
        kn9t_core::Event::PluginNotification { payload } => {
            assert_eq!(
                payload.get("message").and_then(|v| v.as_str()),
                Some("hello free text")
            );
        }
        other => panic!(
            "expected PluginNotification, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }
}
