// Extracted from src/lua/keymap.rs — the #[cfg(test)] mod tests block.
// Unit tests for KeymapRegistry, install_keymap_api, drain_pending_maps,
// drain_pending_actions, and read_epoch.

#![allow(clippy::unwrap_used)]

use mlua::Lua;

use kn9t_tui::lua::keymap::{
    drain_pending_actions, drain_pending_maps, install_keymap_api, read_epoch, KeymapRegistry,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn lua_with_api() -> Lua {
    let lua = Lua::new();
    install_keymap_api(&lua).unwrap();
    lua
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn map_registers_a_handler() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("C-t", function() return true end)"#)
        .exec()
        .unwrap();
    assert_eq!(drain_pending_maps(&lua, &mut reg).unwrap(), 1);

    assert!(reg.has("C-t"));
    assert!(reg.dispatch(&lua, "C-t"), "handler consumes the key");
}

#[test]
fn unmapped_key_is_not_consumed() {
    let lua = lua_with_api();
    let reg = KeymapRegistry::new();
    assert!(!reg.dispatch(&lua, "C-x"));
}

#[test]
fn returning_false_falls_through_to_rust() {
    // This is the "extend, don't replace" path: Lua sees the key, then Rust
    // still runs its own action.
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(
        r#"
        fired = false
        kn9t.map("C-b", function() fired = true; return false end)
    "#,
    )
    .exec()
    .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    assert!(!reg.dispatch(&lua, "C-b"), "not consumed");
    assert!(
        lua.globals().get::<bool>("fired").unwrap(),
        "handler still ran"
    );
}

#[test]
fn handler_error_does_not_consume_or_panic() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("C-e", function() error("boom") end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    // A broken handler must not swallow the key, or the TUI would feel dead.
    assert!(!reg.dispatch(&lua, "C-e"));
}

#[test]
fn unmap_removes_a_binding() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("C-t", function() end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();
    assert!(reg.has("C-t"));

    lua.load(r#"kn9t.unmap("C-t")"#).exec().unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();
    assert!(!reg.has("C-t"), "binding removed");
}

#[test]
fn rebinding_replaces_the_previous_handler() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("C-t", function() return true end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    // Hot-reload rebinds the same key: must not accumulate handlers.
    lua.load(r#"kn9t.map("C-t", function() return false end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    assert_eq!(reg.len(), 1);
    assert!(!reg.dispatch(&lua, "C-t"), "new handler is the live one");
}

#[test]
fn unparseable_keys_are_rejected() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("NotAKey", function() end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    assert!(reg.is_empty(), "typo must not create a dead binding");
}

#[test]
fn draining_twice_does_not_reapply() {
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("F5", function() end)"#)
        .exec()
        .unwrap();
    assert_eq!(drain_pending_maps(&lua, &mut reg).unwrap(), 1);
    assert_eq!(
        drain_pending_maps(&lua, &mut reg).unwrap(),
        0,
        "queue is consumed"
    );
    assert!(reg.has("F5"), "binding survives the second drain");
}

#[test]
fn action_queues_builtin_by_name() {
    let lua = lua_with_api();
    lua.load(r#"kn9t.action("scroll_top")"#).exec().unwrap();
    assert_eq!(
        drain_pending_actions(&lua),
        vec![("scroll_top".to_string(), None)]
    );
}

#[test]
fn action_rejects_unknown_names() {
    let lua = lua_with_api();
    // Returns false to Lua *and* queues nothing.
    let ok: bool = lua.load(r#"return kn9t.action("nope")"#).eval().unwrap();
    assert!(!ok);
    assert!(drain_pending_actions(&lua).is_empty());
}

#[test]
fn actions_preserve_call_order_and_drain_once() {
    let lua = lua_with_api();
    lua.load(
        r#"
        kn9t.action("scroll_top")
        kn9t.action("open_models")
        kn9t.action("scroll_bottom")
    "#,
    )
    .exec()
    .unwrap();

    assert_eq!(
        drain_pending_actions(&lua),
        vec![
            ("scroll_top".to_string(), None),
            ("open_models".to_string(), None),
            ("scroll_bottom".to_string(), None),
        ]
    );
    assert!(drain_pending_actions(&lua).is_empty(), "queue is emptied");
}

/// `switch_session` is the first action that carries data. It must survive
/// the queue round-trip attached to the right call, not just be present
/// somewhere in the drained list.
#[test]
fn action_with_argument_round_trips() {
    let lua = lua_with_api();
    lua.load(
        r#"
        kn9t.action("scroll_top")
        kn9t.action("switch_session", "sess-42")
    "#,
    )
    .exec()
    .unwrap();

    assert_eq!(
        drain_pending_actions(&lua),
        vec![
            ("scroll_top".to_string(), None),
            ("switch_session".to_string(), Some("sess-42".to_string())),
        ]
    );
}

/// An action name must be rejected unless `execute_action` really handles it.
/// `toggle_right` used to pass validation and then do nothing, so a config
/// binding it looked correct and silently failed.
#[test]
fn every_accepted_action_name_is_dispatchable() {
    let lua = lua_with_api();
    // Accepted names round-trip to a real Action.
    for name in [
        "scroll_top",
        "focus_plugin",
        "open_tools",
        "session_picker",
        "refresh_tools",
        "switch_session",
    ] {
        assert!(
            kn9t_tui::keybind::parse_action(name).is_some(),
            "{name} must map to an Action"
        );
    }
    // A removed action is rejected at registration time, not silently queued.
    // `diff_next_hunk` belonged to the native diff viewer, which is gone:
    // diff review is now the `kn9t-git-integration` plugin panel, binding its
    // own keys through `kn9t.on_key` rather than through host actions.
    for gone in ["toggle_right", "diff_next_hunk", "open_diff", "diff_close"] {
        assert!(
            kn9t_tui::keybind::parse_action(gone).is_none(),
            "{gone} must no longer resolve to an Action"
        );
    }
    let ok: bool = lua
        .load(r#"return kn9t.action("toggle_right")"#)
        .eval()
        .unwrap();
    assert!(!ok, "an unhandled action name must be refused");
    assert!(drain_pending_actions(&lua).is_empty());
}

#[test]
fn action_from_inside_a_keymap_handler() {
    // The real path: key -> Lua handler -> queued builtin.
    let lua = lua_with_api();
    let mut reg = KeymapRegistry::new();

    lua.load(r#"kn9t.map("C-g", function() kn9t.action("scroll_top") end)"#)
        .exec()
        .unwrap();
    drain_pending_maps(&lua, &mut reg).unwrap();

    assert!(reg.dispatch(&lua, "C-g"));
    assert_eq!(
        drain_pending_actions(&lua),
        vec![("scroll_top".to_string(), None)]
    );
}

/// `kn9t.invalidate()` is the escape hatch for the render cache in
/// `lua::LuaRuntime::build_ui_outcome`: it must actually move the counter
/// Rust reads, or a toggle flip would redraw stale for up to 9 frames.
#[test]
fn invalidate_bumps_the_epoch_each_call() {
    let lua = lua_with_api();
    assert_eq!(
        read_epoch(&lua),
        0,
        "starts at zero, not nil-as-zero by luck"
    );

    lua.load("kn9t.invalidate()").exec().unwrap();
    assert_eq!(read_epoch(&lua), 1);

    lua.load("kn9t.invalidate(); kn9t.invalidate()")
        .exec()
        .unwrap();
    assert_eq!(read_epoch(&lua), 3);
}
