//! The plugin-supplied Lua UI mechanism, exercised with a real plugin's Lua.
//!
//! `ask_user_ui.lua` is extracted verbatim from `kn9t-ask-user`
//! (kn9t-plugins repo, https://github.com/96Ems/kn9t-plugins,
//! via `scripts/extract_ask_user_lua.py`), so this is not a hand-written
//! approximation: if the shipped plugin's Lua breaks, these tests fail.
//!
//! This is the proof that the mechanism works end to end — a plugin ships Lua,
//! the TUI renders it, and no ask-user-specific code exists in the TUI.

use kn9t_tui::lua::widgets::Widget;
use kn9t_tui::lua::LuaRuntime;
use kn9t_tui::reducer::{PluginLuaOp, PluginPlacement};
use serde_json::json;

/// The Lua the real plugin sends over the wire.
const ASK_USER_LUA: &str = include_str!("ask_user_ui.lua");

fn runtime_with_ask_user(state: serde_json::Value) -> LuaRuntime {
    let rt = LuaRuntime::new().unwrap();
    rt.load_builtin();
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: "kn9t-ask-user".into(),
        source: ASK_USER_LUA.into(),
        placement: Default::default(),
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

/// The 0-based `selected` of the first `list` in the tree, if any.
///
/// The list widget indexes `selected` from 0 while the plugin's cursor is
/// 1-based (it indexes the options array); getting that wrong makes the first
/// row unreachable, and the rendered text alone cannot show it.
fn first_list_selected(w: &Widget) -> Option<usize> {
    match w {
        Widget::List { selected, .. } => *selected,
        Widget::Box { child, .. } => child.as_deref().and_then(first_list_selected),
        Widget::Float { child, .. } => child.as_deref().and_then(first_list_selected),
        Widget::Split { children, .. } => children.iter().find_map(first_list_selected),
        _ => None,
    }
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

/// With no question pushed yet the view must still build: the host can render
/// one frame between registration and the first `ui_set_state`.
#[test]
fn empty_state_renders_without_error() {
    let rt = runtime_with_ask_user(json!({}));
    assert!(
        rt.build_plugin_view("kn9t-ask-user").is_ok(),
        "an empty state must not error"
    );
}

/// A pending question must show its text — the core of the display.
#[test]
fn pending_question_is_shown() {
    let rt = runtime_with_ask_user(json!({
        "kind": "text",
        "question": "What is your name?"
    }));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("What is your name?")),
        "question must be visible, got {t:?}"
    );
}

/// Multi-select must say how to toggle, or the checkboxes are a dead end.
#[test]
fn multi_hint_mentions_space() {
    let rt = runtime_with_ask_user(json!({
        "kind": "multi",
        "question": "Pick some",
        "options": [{"label": "Alpha"}, {"label": "Beta"}]
    }));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("Space")),
        "multi must advertise the Space toggle, got {t:?}"
    );
    assert!(
        t.iter().any(|s| s.contains("[ ] Alpha")),
        "multi options must render as checkboxes, got {t:?}"
    );
}

/// Choice options become a list, so the shape matches what is being chosen.
#[test]
fn choice_options_are_listed() {
    let rt = runtime_with_ask_user(json!({
        "kind": "choice",
        "question": "Pick one",
        "options": [
            {"label": "Alpha", "value": "a"},
            {"label": "Beta",  "value": "b"},
            {"label": "Gamma", "value": "g"}
        ]
    }));
    let t = texts(&rt);
    for want in ["Alpha", "Beta", "Gamma"] {
        assert!(
            t.iter().any(|s| s.contains(want)),
            "option {want} must be listed, got {t:?}"
        );
    }
}

/// `confirm` renders both answers, so the user can see what Enter will send.
#[test]
fn confirm_lists_yes_and_no() {
    let rt = runtime_with_ask_user(json!({
        "kind": "confirm",
        "question": "Delete these files?"
    }));
    let t = texts(&rt);
    for want in ["Yes", "No"] {
        assert!(
            t.iter().any(|s| s.contains(want)),
            "confirm must offer {want}, got {t:?}"
        );
    }
}

/// The first option must be the initial selection, or it can never be chosen.
#[test]
fn first_choice_is_selected_by_default() {
    let rt = runtime_with_ask_user(json!({
        "kind": "choice",
        "question": "Pick one",
        "options": [{"label": "Alpha"}, {"label": "Beta"}]
    }));
    let w = rt
        .build_plugin_view("kn9t-ask-user")
        .expect("plugin Lua must build");
    assert_eq!(
        first_list_selected(&w),
        Some(0),
        "the first option must be highlighted (list `selected` is 0-based)"
    );
}

/// `confirm` starts on Yes by default, which is also index 0.
#[test]
fn confirm_starts_on_yes() {
    let rt = runtime_with_ask_user(json!({
        "kind": "confirm",
        "question": "Proceed?"
    }));
    let w = rt
        .build_plugin_view("kn9t-ask-user")
        .expect("plugin Lua must build");
    assert_eq!(first_list_selected(&w), Some(0));
}

/// Confirm is a vertical list, so Up/Down must move it (Left/Right are aliases).
#[test]
fn confirm_moves_with_up_and_down() {
    let rt = runtime_with_ask_user(json!({"kind": "confirm", "question": "Proceed?"}));
    let selected = |rt: &LuaRuntime| {
        let w = rt.build_plugin_view("kn9t-ask-user").expect("view");
        first_list_selected(&w)
    };

    assert_eq!(selected(&rt), Some(0), "starts on Yes");
    assert!(
        rt.dispatch_plugin_key("kn9t-ask-user", "Down"),
        "Down handled"
    );
    assert_eq!(selected(&rt), Some(1), "Down selects No");
    assert!(rt.dispatch_plugin_key("kn9t-ask-user", "Up"), "Up handled");
    assert_eq!(selected(&rt), Some(0), "Up selects Yes");
}

/// A sequence shows position and a progress indicator; a single question does not.
#[test]
fn sequence_shows_progress() {
    let rt = runtime_with_ask_user(json!({
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
}

#[test]
fn single_question_has_no_progress_bar() {
    let rt = runtime_with_ask_user(json!({
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
        json!({"kind": "choice"}),
        json!({"kind": "text", "question": serde_json::Value::Null}),
        json!({"kind": "choice", "options": []}),
        json!({"kind": "multi", "options": [{"label": "only"}]}),
        json!({"kind": "text", "index": 2}),
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

    let rt = runtime_with_ask_user(json!({
        "kind": "choice",
        "question": "Hi?",
        "options": [{"label": "A"}, {"label": "B"}]
    }));
    // The plugin asks for the reserved bottom slot; the built-in layout is what
    // actually honours it.
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: "kn9t-ask-user".into(),
        source: ASK_USER_LUA.into(),
        placement: PluginPlacement {
            zone: Some("bottom".into()),
            rows: Some(6),
            ..Default::default()
        },
    });

    let mut snap = kn9t_tui::lua::state::StateSnapshot::default();
    snap.plugin_views = rt.plugin_view_names();
    // The built-in layout routes plugin views by their declared placement, so
    // the structured specs are what it actually reads; publishing only the bare
    // name list leaves it with nothing to place.
    snap.plugin_view_specs = rt
        .plugin_views()
        .into_iter()
        .map(|(name, p)| kn9t_tui::lua::state::PluginViewSpec {
            title: p.title.clone().unwrap_or_else(|| name.clone()),
            placement: p.zone.clone().unwrap_or_default(),
            rows: p.rows.unwrap_or(0),
            cols: p.cols.unwrap_or(0),
            name,
        })
        .collect();
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

/// Clearing a plugin view must invalidate the cached layout. The render
/// fingerprint does not read plugin specs, so without an explicit invalidation
/// the cleared view's box stayed on screen and rendered "no UI registered".
#[test]
fn clearing_a_view_drops_its_slot_from_the_cached_layout() {
    use kn9t_tui::lua::widgets::{collect_plugin_slots, UiOutcome};
    use ratatui::layout::Rect;

    let rt = LuaRuntime::new().unwrap();
    rt.load_builtin();
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: "kn9t-ask-user".into(),
        source: ASK_USER_LUA.into(),
        placement: PluginPlacement {
            zone: Some("bottom".into()),
            rows: Some(7),
            ..Default::default()
        },
    });
    rt.update_state(&snapshot_with_views(&rt));
    rt.update_context(&Default::default());

    let area = Rect::new(0, 0, 140, 44);
    let slot_count = |rt: &LuaRuntime| {
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };
        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);
        slots.len()
    };
    assert_eq!(slot_count(&rt), 1, "the view must have a slot");

    rt.apply_plugin_lua_op(&PluginLuaOp::Clear {
        plugin: "kn9t-ask-user".into(),
    });
    rt.update_state(&snapshot_with_views(&rt));

    assert_eq!(
        slot_count(&rt),
        0,
        "a cleared view must not keep its slot (stale render cache)"
    );
}

/// A snapshot carrying the runtime's current plugin views, so the built-in
/// layout has the structured specs it routes on.
fn snapshot_with_views(rt: &LuaRuntime) -> kn9t_tui::lua::state::StateSnapshot {
    let mut snap = kn9t_tui::lua::state::StateSnapshot::default();
    snap.plugin_views = rt.plugin_view_names();
    snap.plugin_view_specs = rt
        .plugin_views()
        .into_iter()
        .map(|(name, p)| kn9t_tui::lua::state::PluginViewSpec {
            title: p.title.clone().unwrap_or_else(|| name.clone()),
            placement: p.zone.clone().unwrap_or_default(),
            rows: p.rows.unwrap_or(0),
            cols: p.cols.unwrap_or(0),
            name,
        })
        .collect();
    snap
}
