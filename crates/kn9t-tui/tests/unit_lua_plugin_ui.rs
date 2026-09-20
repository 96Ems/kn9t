//! Unit tests for `kn9t_tui::lua::plugin_ui`.
//!
//! Extracted from the `#[cfg(test)] mod tests` block in
//! `src/lua/plugin_ui.rs` so they run as a proper integration-test binary
//! and are visible to `cargo test --test unit_lua_plugin_ui`.

#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::plugin_ui::{drain_effects, PluginEffect, PluginUiRegistry};
use kn9t_tui::lua::widgets::collect_clickable_areas;
use kn9t_tui::reducer::PluginPlacement;
use mlua::{Lua, Value as LuaValue};
use serde_json::json;

fn lua() -> Lua {
    Lua::new()
}

/// Register with no placement — the common case in these tests, which are
/// about behaviour rather than layout. Keeps every call site from repeating
/// `PluginPlacement::default()`.
trait RegisterExt {
    fn register_plain(&mut self, lua: &Lua, plugin: &str, source: &str);
}

impl RegisterExt for PluginUiRegistry {
    fn register_plain(&mut self, lua: &Lua, plugin: &str, source: &str) {
        self.register(lua, plugin, source, PluginPlacement::default());
    }
}

const SIMPLE: &str = r#"
    function render(state)
        return { type = "text", content = "hello " .. (state.who or "?") }
    end
"#;

#[test]
fn registers_and_renders() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "demo", SIMPLE);
    reg.set_state(&lua, "demo", &json!({"who": "world"}));

    let w = reg.build(&lua, "demo").expect("should render");
    match w {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello world"),
        other => panic!("expected text, got {other:?}"),
    }
}

/// State may arrive before the Lua source; the first render must still see it.
#[test]
fn state_before_register_is_kept() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.set_state(&lua, "demo", &json!({"who": "early"}));
    reg.register_plain(&lua, "demo", SIMPLE);

    let w = reg.build(&lua, "demo").unwrap();
    match w {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello early"),
        other => panic!("expected text, got {other:?}"),
    }
}

/// The core isolation property: plugins must not see each other, and must
/// not see or modify the host UI's globals.
#[test]
fn plugins_cannot_reach_globals_or_each_other() {
    let lua = lua();
    lua.globals().set("host_secret", "do-not-leak").unwrap();

    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "a",
        r#"
            shared = "from-a"
            function render(s)
                return { type = "text", content = tostring(host_secret) }
            end
        "#,
    );
    reg.register_plain(
        &lua,
        "b",
        r#"function render(s) return { type = "text", content = tostring(shared) } end"#,
    );

    // Plugin a cannot read a host global.
    match reg.build(&lua, "a").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "nil"),
        other => panic!("expected text, got {other:?}"),
    }
    // Plugin b cannot see plugin a's top-level assignment.
    match reg.build(&lua, "b").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "nil"),
        other => panic!("expected text, got {other:?}"),
    }
    // The host's own globals are untouched.
    let secret: String = lua.globals().get("host_secret").unwrap();
    assert_eq!(secret, "do-not-leak");
    assert!(
        lua.globals()
            .get::<Option<LuaValue>>("render")
            .unwrap()
            .is_none(),
        "plugin render() must not land in globals"
    );
}

#[test]
fn syntax_error_is_reported_not_panicked() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "bad", "function render( this is not lua");

    let err = reg.build(&lua, "bad").unwrap_err();
    assert!(!err.is_empty(), "must carry a message to display");
}

#[test]
fn missing_render_function_is_reported() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "empty", "local x = 1");

    let err = reg.build(&lua, "empty").unwrap_err();
    assert!(err.contains("render"), "got: {err}");
}

#[test]
fn runtime_error_in_render_is_reported() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "boom", r#"function render(s) error("kaboom") end"#);

    let err = reg.build(&lua, "boom").unwrap_err();
    assert!(err.contains("kaboom"), "got: {err}");
}

/// One broken plugin must not stop a healthy one from rendering.
#[test]
fn broken_plugin_does_not_affect_others() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "broken", "syntax ((( error");
    reg.register_plain(&lua, "good", SIMPLE);
    reg.set_state(&lua, "good", &json!({"who": "fine"}));

    assert!(reg.build(&lua, "broken").is_err());
    match reg.build(&lua, "good").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello fine"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn re_register_replaces_previous_definition() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "demo", SIMPLE);
    reg.set_state(&lua, "demo", &json!({"who": "v1"}));
    reg.register_plain(
        &lua,
        "demo",
        r#"function render(s) return { type = "text", content = "v2" } end"#,
    );

    match reg.build(&lua, "demo").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "v2"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn names_are_stable_order() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    for n in ["zeta", "alpha", "mid"] {
        reg.register_plain(&lua, n, SIMPLE);
    }
    assert_eq!(reg.names(), vec!["alpha", "mid", "zeta"]);
}

#[test]
fn remove_and_clear() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "a", SIMPLE);
    reg.register_plain(&lua, "b", SIMPLE);

    reg.remove(&lua, "a");
    assert_eq!(reg.names(), vec!["b"]);
    reg.clear(&lua);
    assert!(reg.is_empty());
}

#[test]
fn nested_json_state_becomes_lua_tables() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            function render(s)
                local n = #s.items
                return { type = "text",
                         content = s.meta.title .. ":" .. n .. ":" .. tostring(s.flag) }
            end
        "#,
    );
    reg.set_state(
        &lua,
        "demo",
        &json!({"items": [1, 2, 3], "meta": {"title": "T"}, "flag": true}),
    );

    match reg.build(&lua, "demo").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "T:3:true"),
        other => panic!("expected text, got {other:?}"),
    }
}

/// Arrays must be 1-based so `ipairs` and `#` behave as Lua authors expect.
/// Verified via set_state + render rather than calling the private json_to_lua directly.
#[test]
fn json_arrays_are_one_based() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            function render(s)
                return { type = "text",
                         content = s[1] .. "/" .. s[2] .. "/" .. tostring(#s) }
            end
        "#,
    );
    reg.set_state(&lua, "demo", &json!(["a", "b"]));

    match reg.build(&lua, "demo").unwrap() {
        w if w.text().is_some() => assert_eq!(w.text().unwrap(), "a/b/2"),
        other => panic!("expected text, got {other:?}"),
    }
}

// ── Interaction ──────────────────────────────────────────────────────────────

#[test]
fn plugin_key_handler_consumes_and_falls_through() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("j", function() return true end)
            kn9t.on_key("q", function() return false end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );

    assert!(reg.has_key(&lua, "demo", "j"));
    assert!(reg.dispatch_key(&lua, "demo", "j"), "true consumes");
    assert!(
        !reg.dispatch_key(&lua, "demo", "q"),
        "false falls through to the host"
    );
    assert!(!reg.has_key(&lua, "demo", "z"), "unbound key not claimed");
    assert!(!reg.dispatch_key(&lua, "demo", "z"));
}

/// An unparseable key must be refused at bind time, so a typo surfaces in
/// the log instead of creating a handler nothing can ever trigger.
#[test]
fn plugin_rejects_unparseable_key() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            OK = kn9t.on_key("NotAKey", function() return true end)
            function render(s) return { type = "text", content = tostring(OK) } end
        "#,
    );
    assert!(!reg.has_key(&lua, "demo", "NotAKey"));
    assert_eq!(
        reg.build(&lua, "demo").unwrap().text().unwrap(),
        "false",
        "on_key must report refusal to the plugin"
    );
}

/// The isolation property that makes per-plugin keying necessary: two
/// plugins may use the same id and the same key without interfering.
#[test]
fn handlers_are_scoped_per_plugin() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    let src = r#"
        HITS = 0
        kn9t.on_click("row", function() HITS = HITS + 1; return true end)
        kn9t.on_key("j", function() HITS = HITS + 1; return true end)
        function render(s) return { type = "text", content = tostring(HITS) } end
    "#;
    reg.register_plain(&lua, "a", src);
    reg.register_plain(&lua, "b", src);

    reg.dispatch_click(&lua, "a", "row", 0, 0, "left");
    reg.dispatch_key(&lua, "a", "j");

    assert_eq!(reg.build(&lua, "a").unwrap().text().unwrap(), "2");
    assert_eq!(
        reg.build(&lua, "b").unwrap().text().unwrap(),
        "0",
        "b must not see a's input"
    );
}

/// Removing a view must drop its handlers, or a stale closure keeps firing
/// for a panel that is no longer on screen.
#[test]
fn removing_a_view_drops_its_handlers() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("j", function() return true end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );
    assert!(reg.has_key(&lua, "demo", "j"));

    reg.remove(&lua, "demo");
    assert!(!reg.has_key(&lua, "demo", "j"), "handler is gone");
    assert!(!reg.dispatch_key(&lua, "demo", "j"));
}

/// A hot-reload replaces handlers rather than leaving the old ones bound.
#[test]
fn re_register_replaces_stale_handlers() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("j", function() return true end)
            function render(s) return { type = "text", content = "v1" } end
        "#,
    );
    assert!(reg.has_key(&lua, "demo", "j"));

    // The new version binds a different key and no longer binds `j`.
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("k", function() return true end)
            function render(s) return { type = "text", content = "v2" } end
        "#,
    );
    assert!(
        !reg.has_key(&lua, "demo", "j"),
        "binding from the old version must not survive"
    );
    assert!(reg.has_key(&lua, "demo", "k"));
}

/// A broken handler must not consume the key, or the TUI would feel dead.
#[test]
fn erroring_handler_does_not_consume() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("j", function() error("boom") end)
            kn9t.on_click("row", function() error("boom") end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );
    assert!(!reg.dispatch_key(&lua, "demo", "j"));
    assert!(!reg.dispatch_click(&lua, "demo", "row", 0, 0, "left"));
}

#[test]
fn insert_input_queues_an_effect() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("s", function() kn9t.insert_input("hello"); return true end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );

    assert!(drain_effects(&lua).is_empty(), "nothing queued yet");
    reg.dispatch_key(&lua, "demo", "s");

    assert_eq!(
        drain_effects(&lua),
        vec![PluginEffect::InsertInput {
            plugin: "demo".to_string(),
            text: "hello".to_string(),
        }]
    );
    assert!(drain_effects(&lua).is_empty(), "queue is consumed");
}

/// kn9t.notify queues a NotifyPlugin effect that forwards to the backend.
#[test]
fn notify_queues_a_notify_plugin_effect() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("n", function()
                kn9t.notify({ event = "test_event", sha = "abc123" })
                return true
            end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );

    assert!(drain_effects(&lua).is_empty(), "nothing queued yet");
    reg.dispatch_key(&lua, "demo", "n");

    let effects = drain_effects(&lua);
    assert_eq!(effects.len(), 1, "expected one effect, got {:?}", effects);
    match &effects[0] {
        PluginEffect::NotifyPlugin {
            plugin,
            event,
            data,
        } => {
            assert_eq!(plugin, "demo");
            assert_eq!(event, "test_event");
            assert_eq!(data.get("sha").and_then(|v| v.as_str()), Some("abc123"));
        }
        other => panic!("expected NotifyPlugin, got {:?}", other),
    }
}

/// kn9t.on_text receives printable characters; a `false` return falls through,
/// so a view can scope typing to one mode while `on_key` owns its commands.
#[test]
fn text_handler_receives_characters_and_can_fall_through() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_text(function(ch)
                if ch == "!" then return false end
                kn9t.insert_input(ch)
                return true
            end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );

    assert!(reg.has_text(&lua, "demo"));
    assert!(reg.dispatch_text(&lua, "demo", "a"));
    assert_eq!(
        drain_effects(&lua),
        vec![PluginEffect::InsertInput {
            plugin: "demo".to_string(),
            text: "a".to_string(),
        }]
    );
    // An explicit `false` falls through, unclaimed.
    assert!(!reg.dispatch_text(&lua, "demo", "!"));
    assert!(drain_effects(&lua).is_empty(), "nothing queued on fall-through");
}

/// A view that never called on_text must not claim characters.
#[test]
fn text_handler_is_absent_by_default() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(&lua, "demo", SIMPLE);
    assert!(!reg.has_text(&lua, "demo"));
    assert!(!reg.dispatch_text(&lua, "demo", "a"));
}

/// kn9t.respond queues a Respond effect with the view's opaque answer.
#[test]
fn respond_queues_a_respond_effect() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("Enter", function()
                kn9t.respond({ value = "yes", n = 2 })
                return true
            end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );

    assert!(drain_effects(&lua).is_empty(), "nothing queued yet");
    reg.dispatch_key(&lua, "demo", "Enter");

    let effects = drain_effects(&lua);
    assert_eq!(effects.len(), 1, "expected one effect, got {effects:?}");
    match &effects[0] {
        PluginEffect::Respond { plugin, payload } => {
            assert_eq!(plugin, "demo");
            assert_eq!(payload.get("value").and_then(|v| v.as_str()), Some("yes"));
            assert_eq!(payload.get("n").and_then(|v| v.as_i64()), Some(2));
        }
        other => panic!("expected Respond, got {other:?}"),
    }
}

/// Removing a view must drop its text handler too, or a stale closure fires for
/// a view that is no longer on screen.
#[test]
fn removing_a_view_drops_its_text_handler() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_text(function() return true end)
            function render(s) return { type = "text", content = "x" } end
        "#,
    );
    assert!(reg.has_text(&lua, "demo"));
    reg.remove(&lua, "demo");
    assert!(!reg.has_text(&lua, "demo"), "text handler is gone");
}

/// A plugin cannot forge another plugin's name on an effect: the owner is
/// closed over at bind time, not passed in.
#[test]
fn effects_are_attributed_to_the_calling_plugin() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    let src = r#"
        kn9t.on_key("s", function() kn9t.insert_input("from-" .. kn9t.plugin); return true end)
        function render(s) return { type = "text", content = "x" } end
    "#;
    reg.register_plain(&lua, "a", src);
    reg.register_plain(&lua, "b", src);

    reg.dispatch_key(&lua, "b", "s");
    assert_eq!(
        drain_effects(&lua),
        vec![PluginEffect::InsertInput {
            plugin: "b".to_string(),
            text: "from-b".to_string(),
        }]
    );
}

/// Interaction must not weaken the sandbox: the new API is additive.
#[test]
fn interactive_plugins_still_cannot_reach_host_globals() {
    let lua = lua();
    lua.globals().set("host_secret", "do-not-leak").unwrap();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_key("j", function() return true end)
            function render(s)
                return { type = "text", content = tostring(host_secret) .. "/" .. tostring(io) }
            end
        "#,
    );
    assert_eq!(reg.build(&lua, "demo").unwrap().text().unwrap(), "nil/nil");
}

// ── Placement ────────────────────────────────────────────────────────────────

/// A plugin's declared placement must survive registration and be readable,
/// since that is what lets a config route by zone instead of by name.
#[test]
fn placement_is_stored_and_exposed() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register(
        &lua,
        "demo",
        SIMPLE,
        PluginPlacement {
            zone: Some("main".into()),
            title: Some("Diff review".into()),
            rows: Some(24),
            cols: None,
        },
    );

    let views = reg.views();
    assert_eq!(views.len(), 1);
    let (name, p) = views[0];
    assert_eq!(name, "demo");
    assert_eq!(p.zone.as_deref(), Some("main"));
    assert_eq!(p.title.as_deref(), Some("Diff review"));
    assert_eq!(p.rows, Some(24));
    assert_eq!(p.cols, None);
}

/// State arriving before the source must not invent a placement: the plugin
/// has not spoken yet, so every hint stays empty.
#[test]
fn state_before_register_has_no_placement() {
    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.set_state(&lua, "demo", &json!({"who": "early"}));

    let views = reg.views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].1, &PluginPlacement::default());
}

/// End-to-end shape check: the tree a plugin returns must survive the host's
/// real widget parser, and its `id=` must resolve to a rect click dispatch
/// can find. Without this a view could bind `on_click` for an id that never
/// becomes a hit area, failing only as "clicking does nothing" at runtime.
#[test]
fn interactive_view_tree_parses_and_exposes_click_ids() {
    use ratatui::layout::Rect;

    let lua = lua();
    let mut reg = PluginUiRegistry::new();
    reg.register_plain(
        &lua,
        "demo",
        r#"
            kn9t.on_click("files", function() return true end)
            kn9t.on_click("body", function() return true end)
            function render(state)
                return {
                    type = "split", direction = "vertical",
                    children = {
                        { type = "text", content = "head", size = { fixed = 1 } },
                        { type = "split", direction = "horizontal", children = {
                            { type = "list", id = "files",
                              items = { { spans = { { text = "a.rs" } } } },
                              selected = 0, size = { fixed = 20 } },
                            { type = "list", id = "body",
                              items = { { spans = { { text = "+x", fg = "green" } } } },
                              offset = 0 },
                        }},
                    },
                }
            end
        "#,
    );

    let tree = reg.build(&lua, "demo").expect("host must parse the tree");

    let mut areas: Vec<(String, Rect)> = Vec::new();
    collect_clickable_areas(&tree, Rect::new(0, 0, 80, 24), &mut areas);

    let ids: Vec<&str> = areas.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"files"), "got ids: {ids:?}");
    assert!(ids.contains(&"body"), "got ids: {ids:?}");

    // Clicks are dispatched using these rects, so the geometry has to be
    // real, not merely present.
    let files = areas.iter().find(|(id, _)| id == "files").unwrap().1;
    assert_eq!(files.width, 20);
    assert_eq!(files.y, 1, "sits below the fixed-height header");
    let body = areas.iter().find(|(id, _)| id == "body").unwrap().1;
    assert_eq!(body.x, 20, "starts where the file list ends");

    assert!(reg.dispatch_click(&lua, "demo", "files", 0, 0, "left"));
    assert!(reg.dispatch_click(&lua, "demo", "body", 0, 0, "left"));
}
