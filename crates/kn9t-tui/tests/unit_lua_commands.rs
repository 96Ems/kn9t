//! Unit tests for lua/commands — extracted from src/lua/commands.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::commands::{drain_pending_commands, install_command_api, LuaCommandRegistry};
use mlua::Lua;

fn lua_with_api() -> Lua {
    let lua = Lua::new();
    install_command_api(&lua).unwrap();
    lua
}

#[test]
fn register_command_is_drained_with_all_fields() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(
        r#"
        kn9t.register_command({
            id = "my_cmd",
            label = "My Command",
            description = "does a thing",
            category = "Custom",
            slash = "/mycmd",
            handler = function(args) end,
        })
    "#,
    )
    .exec()
    .unwrap();

    assert_eq!(drain_pending_commands(&lua, &mut reg).unwrap(), 1);
    let cmd = reg.get("my_cmd").expect("registered");
    assert_eq!(cmd.label, "My Command");
    assert_eq!(cmd.description, "does a thing");
    assert_eq!(cmd.category, "Custom");
    assert_eq!(
        cmd.slash.as_deref(),
        Some("mycmd"),
        "leading slash stripped"
    );
}

/// A spec with no `handler` must be rejected, not silently registered as
/// a command that can never run.
#[test]
fn register_command_without_handler_is_rejected() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(r#"kn9t.register_command({id = "broken", label = "x"})"#)
        .exec()
        .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();

    assert!(reg.get("broken").is_none());
}

/// A spec with no `id` must be rejected at registration time (mirrors
/// `kn9t.action` rejecting unknown names at the call site), not queued
/// and silently dropped later.
#[test]
fn register_command_without_id_is_rejected() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(r#"kn9t.register_command({label = "no id", handler = function() end})"#)
        .exec()
        .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();

    assert!(reg.is_empty());
}

#[test]
fn unregister_command_removes_it() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(r#"kn9t.register_command({id = "x", handler = function() end})"#)
        .exec()
        .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();
    assert!(reg.get("x").is_some());

    lua.load(r#"kn9t.unregister_command("x")"#).exec().unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();
    assert!(reg.get("x").is_none());
}

/// Re-registering the same id (hot-reload) must replace, not duplicate —
/// and must not shuffle its position in iteration order.
#[test]
fn reregistering_replaces_without_reordering() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(
        r#"
        kn9t.register_command({id = "a", label = "A1", handler = function() end})
        kn9t.register_command({id = "b", label = "B", handler = function() end})
    "#,
    )
    .exec()
    .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();

    lua.load(r#"kn9t.register_command({id = "a", label = "A2", handler = function() end})"#)
        .exec()
        .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();

    assert_eq!(reg.len(), 2);
    let ids: Vec<&str> = reg.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b"], "order must survive a re-register");
    assert_eq!(reg.get("a").unwrap().label, "A2", "label must update");
}

/// The handler must actually be callable through `run`, with the raw
/// argument string passed through — this is what a `/view diff` style
/// slash command needs to receive "diff" as its argument.
#[test]
fn run_calls_the_handler_with_args() {
    let lua = lua_with_api();
    let mut reg = LuaCommandRegistry::new();

    lua.load(
        r#"
        LAST_ARGS = nil
        kn9t.register_command({
            id = "view",
            handler = function(args) LAST_ARGS = args end,
        })
    "#,
    )
    .exec()
    .unwrap();
    drain_pending_commands(&lua, &mut reg).unwrap();

    reg.get("view").unwrap().run(&lua, "diff");
    assert_eq!(lua.globals().get::<String>("LAST_ARGS").unwrap(), "diff");
}
