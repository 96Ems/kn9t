//! The plugin-supplied Lua UI mechanism, exercised with a real plugin's Lua.
//!
//! `ask_user_ui.lua` is extracted verbatim from `plugins/kn9t-ask-user`
//! (`scripts/extract_ask_user_lua.py`), so this is not a hand-written
//! approximation: if the shipped plugin's Lua breaks, these tests fail.
//!
//! This is the proof that the mechanism works end to end — a plugin ships Lua,
//! the TUI renders it, and no ask-user-specific code exists in the TUI.

use kn9t_tui::lua::widgets::Widget;
use kn9t_tui::lua::LuaRuntime;
use kn9t_tui::reducer::PluginLuaOp;
use serde_json::json;

/// The Lua the real plugin sends over the wire.
const ASK_USER_LUA: &str = include_str!("ask_user_ui.lua");

fn runtime_with_ask_user(state: serde_json::Value) -> LuaRuntime {
    let rt = LuaRuntime::new().unwrap();
    rt.load_builtin();
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: "kn9t-ask-user".into(),
        source: ASK_USER_LUA.into(),
    });
    rt.apply_plugin_lua_op(&PluginLuaOp::SetState {
        plugin: "kn9t-ask-user".into(),
        state,
    });
    rt
}

/// Collect every text string in a widget tree, so assertions do not depend on
/// the exact nesting the plugin chose.
fn all_text(w: &Widget, out: &mut Vec<String>) {
    match w {
        Widget::Text { .. } => out.extend(w.text()),
        Widget::List { items, .. } => out.extend(
            items
                .iter()
                .map(|spans| spans.iter().map(|s| s.text.as_str()).collect::<String>()),
        ),
        Widget::Box { title, child, .. } => {
            if let Some(t) = title {
                out.push(t.clone());
            }
            if let Some(c) = child {
                all_text(c, out);
            }
        }
        Widget::Float { child, .. } => {
            if let Some(c) = child {
                all_text(c, out);
            }
        }
        Widget::Split { children, .. } => {
            for c in children {
                all_text(c, out);
            }
        }
        _ => {}
    }
}

fn texts(rt: &LuaRuntime) -> Vec<String> {
    let w = rt
        .build_plugin_view("kn9t-ask-user")
        .expect("plugin Lua must build");
    let mut out = Vec::new();
    all_text(&w, &mut out);
    out
}

/// The shipped Lua must load — this catches a syntax error in the plugin before
/// a user ever runs it.
#[test]
fn shipped_lua_loads_and_renders() {
    let rt = runtime_with_ask_user(json!({}));
    assert!(
        rt.build_plugin_view("kn9t-ask-user").is_ok(),
        "the plugin's shipped Lua must load"
    );
}

/// Idle is deliberately quiet: one line, no question text.
#[test]
fn idle_state_is_minimal() {
    let rt = runtime_with_ask_user(json!({"pending": false, "answered": 0}));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("idle")),
        "idle should say so, got {t:?}"
    );
}

#[test]
fn idle_reports_answered_count() {
    let rt = runtime_with_ask_user(json!({"pending": false, "answered": 3}));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("3")),
        "should report 3 answered, got {t:?}"
    );
}

/// A pending question must show its text — the core of the display.
#[test]
fn pending_question_is_shown() {
    let rt = runtime_with_ask_user(json!({
        "pending": true,
        "kind": "text",
        "question": "What is your name?"
    }));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("What is your name?")),
        "question must be visible, got {t:?}"
    );
    assert!(
        t.iter().any(|s| s.contains("text")),
        "kind should be shown, got {t:?}"
    );
}

/// Choice options become a list, so the shape matches what is being chosen.
#[test]
fn choice_options_are_listed() {
    let rt = runtime_with_ask_user(json!({
        "pending": true,
        "kind": "choice",
        "question": "Pick one",
        "options": ["Alpha", "Beta", "Gamma"]
    }));
    let t = texts(&rt);
    for want in ["Alpha", "Beta", "Gamma"] {
        assert!(
            t.iter().any(|s| s.contains(want)),
            "option {want} must be listed, got {t:?}"
        );
    }
}

/// A sequence shows position and a progress bar; a single question does not.
#[test]
fn sequence_shows_progress() {
    let rt = runtime_with_ask_user(json!({
        "pending": true,
        "kind": "text",
        "question": "Step three",
        "index": 3,
        "total": 5
    }));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("3/5")),
        "should show 3/5, got {t:?}"
    );
    assert!(
        t.iter().any(|s| s.contains('#') || s.contains('.')),
        "should draw a progress bar, got {t:?}"
    );
}

#[test]
fn single_question_has_no_progress_bar() {
    let rt = runtime_with_ask_user(json!({
        "pending": true,
        "kind": "confirm",
        "question": "Delete?",
        "total": 1
    }));
    let t = texts(&rt);
    assert!(
        !t.iter().any(|s| s.contains("1/1")),
        "a lone question should not show progress, got {t:?}"
    );
}

/// Robustness: the plugin's Lua must tolerate a state it did not expect rather
/// than erroring, since state and code are versioned independently.
#[test]
fn missing_fields_do_not_error() {
    for state in [
        json!({}),
        json!({"pending": true}),
        json!({"pending": true, "question": serde_json::Value::Null}),
        json!({"pending": true, "options": []}),
        json!({"pending": true, "index": 2}),
    ] {
        let rt = runtime_with_ask_user(state.clone());
        assert!(
            rt.build_plugin_view("kn9t-ask-user").is_ok(),
            "must tolerate {state}"
        );
    }
}

/// The mechanism's placement guarantee: the plugin gets a slot in the built-in
/// layout without the config naming it, and that slot never covers a native view.
#[test]
fn plugin_is_placed_by_the_builtin_layout() {
    use kn9t_tui::lua::widgets::{collect_natives, collect_plugin_slots, UiOutcome};
    use ratatui::layout::Rect;

    let rt = runtime_with_ask_user(json!({"pending": true, "question": "Hi?"}));

    let mut snap = kn9t_tui::lua::state::StateSnapshot::default();
    snap.plugin_views = rt.plugin_view_names();
    rt.update_state(&snap);
    rt.update_context(&kn9t_tui::lua::context::ContextStats::default());

    let area = Rect::new(0, 0, 140, 44);
    let root = match rt.build_ui_outcome(area.width, area.height) {
        UiOutcome::Ok(w) => w,
        other => panic!("built-in did not build: {other:?}"),
    };

    let mut slots = Vec::new();
    collect_plugin_slots(&root, area, &mut slots);
    assert_eq!(slots.len(), 1, "expected one slot, got {slots:?}");
    assert_eq!(slots[0].0, "kn9t-ask-user");

    let mut natives = Vec::new();
    collect_natives(&root, area, &mut natives);
    assert!(
        natives.iter().any(|(n, _)| n == "transcript"),
        "transcript must survive alongside the plugin view"
    );
    for (nn, nr) in &natives {
        assert!(
            slots[0].1.intersection(*nr).area() == 0,
            "plugin slot {:?} overlaps native '{nn}' {nr:?}",
            slots[0].1
        );
    }
}
