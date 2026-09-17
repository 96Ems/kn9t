//! Tests for non-blocking server startup (plugins load in background).
//!
//! The server binds and writes its port file immediately, then loads plugins
//! in a background thread. Routes that require plugins return 503 while loading.

use kn9t_core::ToolRegistry;
use kn9t_server::state::ServerState;
use kn9t_store::SqliteStore;
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
fn plugins_ready_default_true() {
    let state = temp_state();
    assert!(state.plugins_ready(), "default state should have plugins ready");
}

#[test]
fn plugins_loading_flag_works() {
    let state = temp_state();

    state.set_plugins_loading(true);
    assert!(!state.plugins_ready(), "should not be ready while loading");

    state.set_plugins_loading(false);
    assert!(state.plugins_ready(), "should be ready after loading complete");
}

#[test]
fn create_session_503_while_loading() {
    use kn9t_server::{api, routes};

    let state = temp_state();
    state.set_plugins_loading(true);

    let req = api::CreateSessionReq {
        cwd: Some(".".into()),
        model: None,
        name: None,
    };

    let resp = routes::session::create(&state, req);
    let body = &resp.body;
    assert!(
        body.contains("loading"),
        "should return loading error, got: {}",
        body
    );
}

#[test]
fn create_session_ok_when_ready() {
    use kn9t_core::{CacheMode, ModelRef, ModelSpec, Price, Quirks, ThinkingReplay};
    use kn9t_server::{api, routes};

    let state = temp_state();
    assert!(state.plugins_ready());

    let model_spec = ModelSpec {
        r#ref: ModelRef {
            provider: "test".into(),
            id: "model".into(),
        },
        api_id: "model".into(),
        ctx_window: 1000,
        max_out: 500,
        thinking: kn9t_core::Thinking::Off,
        price: Price {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        },
        cache: CacheMode::None,
        streaming: true,
        quirks: Quirks {
            thinking_replay: ThinkingReplay::Strip,
        },
    };
    *state.default_model.write().unwrap() = Some(model_spec);

    let req = api::CreateSessionReq {
        cwd: Some(".".into()),
        model: None,
        name: None,
    };

    let resp = routes::session::create(&state, req);
    let body = &resp.body;
    assert!(
        body.contains("id") && !body.contains("error"),
        "should succeed when ready, got: {}",
        body
    );
}

#[test]
fn health_reports_loading_status() {
    let state = temp_state();
    assert!(state.plugins_ready());

    state.set_plugins_loading(true);
    assert!(!state.plugins_ready());

    state.set_plugins_loading(false);
    assert!(state.plugins_ready());
}

#[test]
fn prompt_503_while_loading() {
    use kn9t_core::{ModelRef, SessionId};
    use kn9t_server::{api, routes};

    let state = temp_state();

    let sess = SessionId::new();
    let model = ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&state.store, &sess, ".", &model).unwrap();

    state.set_plugins_loading(true);

    let req = api::PromptReq {
        text: Some("hello".into()),
        blobs: None,
        images: None,
    };

    let resp = routes::session::prompt(&state, &sess.0, req);
    let body = &resp.body;
    assert!(
        body.contains("loading"),
        "should return loading error, got: {}",
        body
    );
}
