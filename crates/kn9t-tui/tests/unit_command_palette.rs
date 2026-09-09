use kn9t_tui::command_palette::{CommandPalette, COMMANDS};
use kn9t_tui::lua::commands::LuaCommandRegistry;

fn empty_lua() -> LuaCommandRegistry {
    LuaCommandRegistry::new()
}

#[test]
fn test_palette_open_shows_all() {
    let mut palette = CommandPalette::new();
    palette.open(&empty_lua());
    assert!(palette.active);
    assert_eq!(palette.matches.len(), COMMANDS.len());
}

#[test]
fn test_palette_filter() {
    let mut palette = CommandPalette::new();
    palette.open(&empty_lua());
    palette.set_query("search");
    assert!(palette.matches.len() < COMMANDS.len());
    assert!(palette.selected_command().is_some());
}

#[test]
fn test_palette_navigation() {
    let mut palette = CommandPalette::new();
    palette.open(&empty_lua());
    let initial = palette.selected;
    palette.select_next();
    assert_eq!(palette.selected, initial + 1);
    palette.select_prev();
    assert_eq!(palette.selected, initial);
}

/// The actual point of this refactor: a Lua-registered command must show
/// up in the palette alongside the built-ins, searchable the same way.
#[test]
fn lua_registered_command_appears_in_palette() {
    let lua = mlua::Lua::new();
    kn9t_tui::lua::commands::install_command_api(&lua).unwrap();
    lua.load(
        r#"
        kn9t.register_command({
            id = "my_thing", label = "My Thing", description = "does stuff",
            handler = function() end,
        })
    "#,
    )
    .exec()
    .unwrap();
    let mut reg = LuaCommandRegistry::new();
    kn9t_tui::lua::commands::drain_pending_commands(&lua, &mut reg).unwrap();

    let mut palette = CommandPalette::new();
    palette.open(&reg);
    assert_eq!(palette.matches.len(), COMMANDS.len() + 1);

    palette.set_query("My Thing");
    let found = palette.selected_command().expect("must match");
    assert_eq!(found.id, "my_thing");
    assert!(found.is_lua);
}
