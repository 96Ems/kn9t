//! Unit tests for slash — extracted from src/slash.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::commands::{drain_pending_commands, install_command_api, LuaCommandRegistry};
use kn9t_tui::slash::{fuzzy_match, SlashState, COMMANDS};

fn empty_lua() -> LuaCommandRegistry {
    LuaCommandRegistry::new()
}

#[test]
fn test_fuzzy_match() {
    assert!(fuzzy_match("model", ""));
    assert!(fuzzy_match("model", "m"));
    assert!(fuzzy_match("model", "mod"));
    assert!(fuzzy_match("model", "mdl"));
    assert!(fuzzy_match("model", "model"));
    assert!(!fuzzy_match("model", "x"));
    assert!(!fuzzy_match("model", "modelx"));
}

#[test]
fn activate_shows_all_builtins_with_no_lua_registered() {
    let mut state = SlashState::new();
    state.activate(&empty_lua());
    assert_eq!(state.matches.len(), COMMANDS.len());
}

/// A Lua command with no `slash=` must not appear in the dropdown — it's
/// palette-only, which is the whole point of making `slash` optional.
#[test]
fn lua_command_without_slash_is_excluded() {
    let lua = mlua::Lua::new();
    install_command_api(&lua).unwrap();
    lua.load(r#"kn9t.register_command({id = "x", handler = function() end})"#)
        .exec()
        .unwrap();
    let mut reg = LuaCommandRegistry::new();
    drain_pending_commands(&lua, &mut reg).unwrap();

    let mut state = SlashState::new();
    state.activate(&reg);
    assert_eq!(
        state.matches.len(),
        COMMANDS.len(),
        "no slash= means not in the dropdown"
    );
}

/// A Lua command WITH `slash=` must appear and be selectable, and its
/// stored name must have the leading slash already stripped (mirrors
/// `commands::drain_pending_commands`'s own stripping).
#[test]
fn lua_command_with_slash_is_included_and_selectable() {
    let lua = mlua::Lua::new();
    install_command_api(&lua).unwrap();
    lua.load(
        r#"
        kn9t.register_command({
            id = "view_diff", slash = "/view", handler = function() end,
        })
    "#,
    )
    .exec()
    .unwrap();
    let mut reg = LuaCommandRegistry::new();
    drain_pending_commands(&lua, &mut reg).unwrap();

    let mut state = SlashState::new();
    state.activate(&reg);
    assert_eq!(state.matches.len(), COMMANDS.len() + 1);

    state.set_query("view");
    let found = state.selected_command().expect("must match");
    assert_eq!(found.name, "view");
    assert!(found.is_lua);
}
