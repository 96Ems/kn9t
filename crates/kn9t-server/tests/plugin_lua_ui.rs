//! Plugin TUI display via plugin-supplied Lua.
//!
//! Plugins that want to draw in the TUI ship Lua (`ui_register_lua`) and push
//! state (`ui_set_state`). The server does not interpret either: it validates
//! the envelope and forwards a `UiDirective` on the session bus. The widget
//! vocabulary belongs to the TUI, so the host stays out of presentation.

use kn9t_core::{Event, ModelRef, SessionId, Subscription, ToolRegistry};
use kn9t_plugin::HostApi;
use kn9t_server::{host_api::ServerHostApi, state::ServerState};
use kn9t_store::SqliteStore;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

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

fn api_for(state: &Arc<ServerState>) -> ServerHostApi {
    ServerHostApi {
        state: state.clone(),
    }
}

/// Create a session and subscribe to its bus so emitted directives are visible.
fn session_with_bus(state: &Arc<ServerState>) -> (String, Subscription) {
    let sess = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();
    let sub = state.buses.subscribe(&sess.0, 64);
    (sess.0.clone(), sub)
}

/// Drain the bus, keeping only `UiDirective`s as `(plugin, op, payload)`.
fn directives(sub: &Subscription) -> Vec<(String, String, serde_json::Value)> {
    let mut out = Vec::new();
    while let Some(ev) = sub.recv_timeout(Duration::from_millis(200)) {
        if let Event::UiDirective {
            plugin,
            op,
            payload,
            ..
        } = ev
        {
            out.push((plugin, op, payload));
        }
    }
    out
}

#[test]
fn register_lua_forwards_source_to_tui() {
    let state = temp_state();
    let (sid, sub) = session_with_bus(&state);
    let api = api_for(&state);

    let source = r#"function render(s) return { type = "text", content = s.msg } end"#;
    let res = api
        .handle(
            "demo-plugin",
            Some(&sid),
            "ui_register_lua",
            &json!({"source": source}),
        )
        .expect("register should succeed");
    assert_eq!(res["ok"], json!(true));

    let found = directives(&sub).into_iter().any(|(plugin, op, payload)| {
        plugin == "demo-plugin"
            && op == "register_lua"
            && payload.get("source").and_then(|v| v.as_str()) == Some(source)
    });
    assert!(found, "register_lua must reach the TUI");
}

/// State updates carry arbitrary JSON: the plugin's own Lua decides how to show
/// it, so there is no fixed placeholder vocabulary or type validation here.
#[test]
fn set_state_accepts_arbitrary_json() {
    let state = temp_state();
    let (sid, sub) = session_with_bus(&state);
    let api = api_for(&state);

    api.handle(
        "demo-plugin",
        Some(&sid),
        "ui_set_state",
        &json!({"state": {
            "title": "Skills",
            "items": ["a", "b", "c"],
            "progress": 42,
            "nested": {"deep": {"ok": true}}
        }}),
    )
    .expect("set_state should succeed");

    let got = directives(&sub)
        .into_iter()
        .find(|(_, op, _)| op == "set_state")
        .map(|(_, _, p)| p)
        .expect("set_state must be forwarded");

    assert_eq!(got["state"]["items"][2], json!("c"));
    assert_eq!(got["state"]["nested"]["deep"]["ok"], json!(true));
    assert_eq!(got["state"]["progress"], json!(42));
}

#[test]
fn register_lua_requires_source() {
    let state = temp_state();
    let (sid, _sub) = session_with_bus(&state);
    let api = api_for(&state);

    let err = api
        .handle("demo-plugin", Some(&sid), "ui_register_lua", &json!({}))
        .expect_err("missing source must be rejected");
    assert!(err.contains("source"), "got: {err}");
}

#[test]
fn set_state_requires_state() {
    let state = temp_state();
    let (sid, _sub) = session_with_bus(&state);
    let api = api_for(&state);

    let err = api
        .handle("demo-plugin", Some(&sid), "ui_set_state", &json!({}))
        .expect_err("missing state must be rejected");
    assert!(err.contains("state"), "got: {err}");
}

/// A runaway plugin must not push unbounded source across the wire.
#[test]
fn oversized_source_is_rejected() {
    let state = temp_state();
    let (sid, _sub) = session_with_bus(&state);
    let api = api_for(&state);

    let huge = "-- pad\n".repeat(64 * 1024); // ~448 KB, over the 256 KB cap
    let err = api
        .handle(
            "demo-plugin",
            Some(&sid),
            "ui_register_lua",
            &json!({"source": huge}),
        )
        .expect_err("oversized source must be rejected");
    assert!(err.contains("too large"), "got: {err}");
}

/// Directives are session-scoped: one session's UI must not leak into another.
#[test]
fn directives_are_session_scoped() {
    let state = temp_state();
    let (sid_a, sub_a) = session_with_bus(&state);
    let (_sid_b, sub_b) = session_with_bus(&state);
    let api = api_for(&state);

    api.handle(
        "demo-plugin",
        Some(&sid_a),
        "ui_set_state",
        &json!({"state": {"only": "a"}}),
    )
    .unwrap();

    assert!(
        directives(&sub_a)
            .iter()
            .any(|(_, op, _)| op == "set_state"),
        "session A should receive its own directive"
    );
    assert!(
        !directives(&sub_b)
            .iter()
            .any(|(_, op, _)| op == "set_state"),
        "session B must not see session A's directive"
    );
}

/// Two plugins in one session stay attributable, so the TUI can keep their UIs
/// separate rather than merging them.
#[test]
fn directives_are_attributed_per_plugin() {
    let state = temp_state();
    let (sid, sub) = session_with_bus(&state);
    let api = api_for(&state);

    for name in ["plugin-one", "plugin-two"] {
        api.handle(
            name,
            Some(&sid),
            "ui_set_state",
            &json!({"state": {"who": name}}),
        )
        .unwrap();
    }

    let mut seen: Vec<String> = directives(&sub)
        .into_iter()
        .filter(|(_, op, _)| op == "set_state")
        .map(|(plugin, _, _)| plugin)
        .collect();
    seen.sort();
    assert_eq!(seen, vec!["plugin-one", "plugin-two"]);
}

#[test]
fn clear_tears_down_plugin_ui() {
    let state = temp_state();
    let (sid, sub) = session_with_bus(&state);
    let api = api_for(&state);

    api.handle("demo-plugin", Some(&sid), "ui_clear", &json!({}))
        .expect("clear should succeed");

    assert!(
        directives(&sub)
            .iter()
            .any(|(plugin, op, _)| op == "clear" && plugin == "demo-plugin"),
        "clear must reach the TUI"
    );
}

/// The old placeholder API is gone; calling it must fail loudly so a stale
/// plugin gets a clear error instead of silently drawing nothing.
#[test]
fn removed_placeholder_api_is_rejected() {
    let state = temp_state();
    let (sid, _sub) = session_with_bus(&state);
    let api = api_for(&state);

    for old in ["ui_declare_page", "ui_write_placeholder", "ui_clear_page"] {
        let err = api
            .handle(
                "demo-plugin",
                Some(&sid),
                old,
                &json!({"page_id": "p", "layout": []}),
            )
            .expect_err("removed API must be rejected");
        assert!(!err.is_empty(), "{old} should report an error");
    }
}

#[test]
fn ui_calls_require_a_session() {
    let state = temp_state();
    let api = api_for(&state);

    let err = api
        .handle("demo-plugin", None, "ui_set_state", &json!({"state": {}}))
        .expect_err("session-less call must be rejected");
    assert!(!err.is_empty());
}
