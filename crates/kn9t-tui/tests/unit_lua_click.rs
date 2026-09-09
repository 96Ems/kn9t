//! Unit tests for lua/click — extracted from src/lua/click.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::click::{drain_pending_clicks, install_click_api, ClickRegistry};
use mlua::{Lua, Table};

fn lua_with_api() -> Lua {
    let lua = Lua::new();
    install_click_api(&lua).unwrap();
    lua
}

#[test]
fn on_click_registers_a_handler() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(r#"kn9t.on_click("row1", function(x, y, btn) return true end)"#)
        .exec()
        .unwrap();
    assert_eq!(drain_pending_clicks(&lua, &mut reg).unwrap(), 1);

    assert!(reg.has("row1"));
    assert!(reg.dispatch(&lua, "row1", 3, 0, "left"));
}

#[test]
fn unregistered_id_is_not_consumed() {
    let lua = lua_with_api();
    let reg = ClickRegistry::new();
    assert!(!reg.dispatch(&lua, "nope", 0, 0, "left"));
}

/// The whole point of `on_click`: a list widget needs the row index, which
/// is exactly what local_y gives it for a single-column list.
#[test]
fn coordinates_are_passed_through_local_to_the_widget() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(
        r#"
        LAST = nil
        kn9t.on_click("list", function(x, y, btn)
            LAST = {x=x, y=y, btn=btn}
            return true
        end)
    "#,
    )
    .exec()
    .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();

    reg.dispatch(&lua, "list", 7, 2, "right");

    let last: Table = lua.globals().get("LAST").unwrap();
    assert_eq!(last.get::<u16>("x").unwrap(), 7);
    assert_eq!(last.get::<u16>("y").unwrap(), 2);
    assert_eq!(last.get::<String>("btn").unwrap(), "right");
}

#[test]
fn returning_false_falls_through_to_rust() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(
        r#"
        fired = false
        kn9t.on_click("card", function() fired = true; return false end)
    "#,
    )
    .exec()
    .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();

    assert!(!reg.dispatch(&lua, "card", 0, 0, "left"), "not consumed");
    assert!(
        lua.globals().get::<bool>("fired").unwrap(),
        "handler still ran"
    );
}

#[test]
fn handler_error_does_not_consume_or_panic() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(r#"kn9t.on_click("bad", function() error("boom") end)"#)
        .exec()
        .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();

    assert!(!reg.dispatch(&lua, "bad", 0, 0, "left"));
}

#[test]
fn remove_click_removes_a_binding() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(r#"kn9t.on_click("row1", function() end)"#)
        .exec()
        .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();
    assert!(reg.has("row1"));

    lua.load(r#"kn9t.remove_click("row1")"#).exec().unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();
    assert!(!reg.has("row1"), "binding removed");
}

#[test]
fn rebinding_replaces_the_previous_handler() {
    let lua = lua_with_api();
    let mut reg = ClickRegistry::new();

    lua.load(r#"kn9t.on_click("row1", function() return true end)"#)
        .exec()
        .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();

    // Hot-reload rebinds the same id: must not accumulate handlers.
    lua.load(r#"kn9t.on_click("row1", function() return false end)"#)
        .exec()
        .unwrap();
    drain_pending_clicks(&lua, &mut reg).unwrap();

    assert_eq!(reg.len(), 1);
    assert!(
        !reg.dispatch(&lua, "row1", 0, 0, "left"),
        "new handler is live"
    );
}
