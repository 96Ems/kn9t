//! 96E-24 TDD: templated page primitive with writable placeholders.

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
    Arc::new(ServerState::new(
        store,
        "tok".into(),
        ToolRegistry::new(),
        vec![],
    ))
}

#[test]
fn declare_and_write_placeholder_cheap_update() {
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

    // Declare page with 3 placeholders
    let layout = json!([
        {"placeholder_id":"status","kind":"text","default":"idle"},
        {"placeholder_id":"progress","kind":"bar","default":0},
        {"placeholder_id":"items","kind":"list","default":[]}
    ]);
    let r = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"dash","layout": layout}),
    );
    assert!(r.is_ok(), "declare should succeed: {:?}", r);

    // TUI receives declare_page UiDirective
    let ev = sub
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("declare_page event");
    match ev {
        kn9t_core::Event::UiDirective {
            target, op, plugin, ..
        } => {
            assert_eq!(target, "dash");
            assert_eq!(op, "declare_page");
            assert_eq!(plugin, "my-plugin");
        }
        other => panic!(
            "expected UiDirective declare_page, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }

    // Cheap update: only one placeholder at a time
    let r2 = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"dash","placeholder_id":"progress","value": 42}),
    );
    assert!(r2.is_ok(), "write should succeed: {:?}", r2);
    let ev2 = sub
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("write event");
    match ev2 {
        kn9t_core::Event::UiDirective {
            target,
            op,
            payload,
            ..
        } => {
            assert_eq!(target, "dash");
            assert_eq!(op, "write_placeholder");
            assert_eq!(
                payload.get("placeholder_id").and_then(|v| v.as_str()),
                Some("progress")
            );
            assert_eq!(payload.get("value"), Some(&json!(42)));
        }
        other => panic!(
            "expected write_placeholder, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }

    // Verify host registry state
    let page = state
        .ui_pages
        .get("my-plugin", &sess.0, "dash")
        .expect("page should exist");
    assert_eq!(page.values.get("progress"), Some(&json!(42)));
    assert_eq!(page.values.get("status"), Some(&json!("idle")));
}

#[test]
fn write_to_undeclared_placeholder_or_wrong_type_is_rejected() {
    let state = temp_state();
    let sess = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();
    let api = ServerHostApi {
        state: state.clone(),
    };

    let layout = json!([{"placeholder_id":"count","kind":"number"}]);
    api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"p1","layout": layout}),
    )
    .unwrap();

    // Undeclared placeholder
    let r = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"p1","placeholder_id":"ghost","value": 1}),
    );
    assert!(
        r.is_err(),
        "undeclared placeholder must be rejected: {:?}",
        r
    );

    // Wrong type: expects number, got string
    let r2 = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"p1","placeholder_id":"count","value": "not a number"}),
    );
    assert!(r2.is_err(), "wrong type must be rejected: {:?}", r2);

    // Bar out of range
    let layout2 = json!([{"placeholder_id":"bar1","kind":"bar"}]);
    api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"p2","layout": layout2}),
    )
    .unwrap();
    let r3 = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"p2","placeholder_id":"bar1","value": 150}),
    );
    assert!(r3.is_err(), "bar out of 0..100 must be rejected");

    // List expects array, got string
    let layout3 = json!([{"placeholder_id":"lst","kind":"list"}]);
    api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"p3","layout": layout3}),
    )
    .unwrap();
    let r4 = api.handle(
        "my-plugin",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"p3","placeholder_id":"lst","value": "not-array"}),
    );
    assert!(r4.is_err(), "list wrong type must be rejected");
}

#[test]
fn page_scoped_to_plugin_and_session_no_cross_access() {
    let state = temp_state();
    let sess_a = SessionId::new();
    let sess_b = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess_a, ".", &model).unwrap();
    kn9t_store::create_session(&state.store, &sess_b, ".", &model).unwrap();
    let api = ServerHostApi {
        state: state.clone(),
    };

    let layout = json!([{"placeholder_id":"x","kind":"text"}]);
    // Plugin1 declares in sess_a
    api.handle(
        "plugin1",
        Some(&sess_a.0),
        "ui_declare_page",
        &json!({"session":sess_a.0,"page_id":"shared","layout": layout.clone()}),
    )
    .unwrap();
    // Plugin2 tries to write to plugin1's page in same session — must be rejected
    let r = api.handle(
        "plugin2",
        Some(&sess_a.0),
        "ui_write_placeholder",
        &json!({"session":sess_a.0,"page_id":"shared","placeholder_id":"x","value":"hijack"}),
    );
    assert!(r.is_err(), "cross-plugin write must be rejected: {:?}", r);

    // Same plugin but different session — must be rejected (page is session-scoped)
    let r2 = api.handle("plugin1", Some(&sess_b.0), "ui_write_placeholder", &json!({"session":sess_b.0,"page_id":"shared","placeholder_id":"x","value":"cross-session"}));
    assert!(
        r2.is_err(),
        "cross-session write must be rejected: {:?}",
        r2
    );

    // Also declare same page_id in sess_b by same plugin is allowed (separate instance)
    let r3 = api.handle(
        "plugin1",
        Some(&sess_b.0),
        "ui_declare_page",
        &json!({"session":sess_b.0,"page_id":"shared","layout": layout}),
    );
    assert!(
        r3.is_ok(),
        "same page_id in different session should be allowed: {:?}",
        r3
    );

    // Verify isolation: sess_b's page write works, sess_a's value unaffected
    api.handle(
        "plugin1",
        Some(&sess_b.0),
        "ui_write_placeholder",
        &json!({"session":sess_b.0,"page_id":"shared","placeholder_id":"x","value":"b-val"}),
    )
    .unwrap();
    let page_a = state.ui_pages.get("plugin1", &sess_a.0, "shared").unwrap();
    assert!(page_a.values.get("x").is_none() || page_a.values.get("x") != Some(&json!("b-val")));
    let page_b = state.ui_pages.get("plugin1", &sess_b.0, "shared").unwrap();
    assert_eq!(page_b.values.get("x"), Some(&json!("b-val")));
}

#[test]
fn pages_torn_down_on_clear_and_session_end() {
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

    let layout = json!([{"placeholder_id":"x","kind":"text"}]);
    api.handle(
        "p",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"tmp","layout": layout}),
    )
    .unwrap();
    assert_eq!(state.ui_pages.count(), 1);
    // Drain declare event
    let _ = sub.recv_timeout(std::time::Duration::from_millis(200));

    // ui_clear_page tears down
    let r = api.handle(
        "p",
        Some(&sess.0),
        "ui_clear_page",
        &json!({"session":sess.0,"page_id":"tmp"}),
    );
    assert!(r.is_ok(), "clear should succeed: {:?}", r);
    assert_eq!(state.ui_pages.count(), 0);
    // TUI receives clear_page
    let ev = sub
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("clear_page event");
    match ev {
        kn9t_core::Event::UiDirective { op, .. } => assert_eq!(op, "clear_page"),
        other => panic!(
            "expected clear_page, got {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }
    // Writing after clear must be rejected
    let r2 = api.handle(
        "p",
        Some(&sess.0),
        "ui_write_placeholder",
        &json!({"session":sess.0,"page_id":"tmp","placeholder_id":"x","value":"after"}),
    );
    assert!(r2.is_err(), "write after clear must be rejected");

    // Session end teardown: declare again then simulate delete
    let layout2 = json!([{"placeholder_id":"x","kind":"text"}]);
    api.handle(
        "p",
        Some(&sess.0),
        "ui_declare_page",
        &json!({"session":sess.0,"page_id":"tmp2","layout": layout2}),
    )
    .unwrap();
    assert_eq!(state.ui_pages.count(), 1);
    state.ui_pages.clear_session(&sess.0);
    assert_eq!(state.ui_pages.count(), 0);
}
