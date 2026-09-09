//! Unit tests for lua/panels — extracted from src/lua/panels.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::panels::{install_panel_api, process_pending_panels, PanelRegistry};
use mlua::{Function, Lua, Table};

#[test]
fn test_panel_registry_register() {
    let lua = Lua::new();
    let mut registry = PanelRegistry::new();

    let spec = lua.create_table().unwrap();
    spec.set("position", "right").unwrap();
    spec.set("width", 30).unwrap();

    registry.register(&lua, "test_panel", &spec).unwrap();

    assert_eq!(registry.len(), 1);
    let panel = registry.get("test_panel").unwrap();
    assert_eq!(panel.position, "right");
    assert_eq!(panel.width, Some(30));
}

#[test]
fn test_panel_registry_unregister() {
    let lua = Lua::new();
    let mut registry = PanelRegistry::new();

    let spec = lua.create_table().unwrap();
    registry.register(&lua, "test_panel", &spec).unwrap();
    assert_eq!(registry.len(), 1);

    registry.unregister("test_panel");
    assert_eq!(registry.len(), 0);
}

#[test]
fn test_panel_registry_visibility() {
    let lua = Lua::new();
    let mut registry = PanelRegistry::new();

    let spec = lua.create_table().unwrap();
    spec.set("visible", true).unwrap();
    registry.register(&lua, "test_panel", &spec).unwrap();

    assert!(registry.get("test_panel").unwrap().visible);

    registry.hide("test_panel");
    assert!(!registry.get("test_panel").unwrap().visible);

    registry.show("test_panel");
    assert!(registry.get("test_panel").unwrap().visible);

    registry.toggle("test_panel");
    assert!(!registry.get("test_panel").unwrap().visible);
}

#[test]
fn test_panel_registry_focus() {
    let lua = Lua::new();
    let mut registry = PanelRegistry::new();

    let spec = lua.create_table().unwrap();
    registry.register(&lua, "panel1", &spec).unwrap();
    registry.register(&lua, "panel2", &spec).unwrap();

    registry.set_focus(&lua, Some("panel1"));
    assert_eq!(registry.focused(), Some("panel1"));
    assert!(registry.get("panel1").unwrap().focused);
    assert!(!registry.get("panel2").unwrap().focused);

    registry.set_focus(&lua, Some("panel2"));
    assert_eq!(registry.focused(), Some("panel2"));
    assert!(!registry.get("panel1").unwrap().focused);
    assert!(registry.get("panel2").unwrap().focused);
}

#[test]
fn test_panel_by_position() {
    let lua = Lua::new();
    let mut registry = PanelRegistry::new();

    let right_spec = lua.create_table().unwrap();
    right_spec.set("position", "right").unwrap();
    right_spec.set("visible", true).unwrap();

    let left_spec = lua.create_table().unwrap();
    left_spec.set("position", "left").unwrap();
    left_spec.set("visible", true).unwrap();

    registry.register(&lua, "right1", &right_spec).unwrap();
    registry.register(&lua, "right2", &right_spec).unwrap();
    registry.register(&lua, "left1", &left_spec).unwrap();

    let right_panels = registry.by_position("right");
    assert_eq!(right_panels.len(), 2);

    let left_panels = registry.by_position("left");
    assert_eq!(left_panels.len(), 1);
}

#[test]
fn test_install_panel_api() {
    let lua = Lua::new();
    install_panel_api(&lua).unwrap();

    // Check that kn9t namespace exists
    let kn9t: Table = lua.globals().get("kn9t").unwrap();
    assert!(kn9t.get::<Function>("register_panel").is_ok());
    assert!(kn9t.get::<Function>("show_panel").is_ok());
    assert!(kn9t.get::<Function>("hide_panel").is_ok());
}

#[test]
fn test_process_pending_panels() {
    let lua = Lua::new();
    install_panel_api(&lua).unwrap();

    // Register a panel via Lua
    lua.load(
        r#"
        kn9t.register_panel("my_panel", {
            position = "bottom",
            height = 5,
            build = function()
                return { type = "text", content = "Hello" }
            end
        })
    "#,
    )
    .exec()
    .unwrap();

    let mut registry = PanelRegistry::new();
    process_pending_panels(&lua, &mut registry).unwrap();

    assert_eq!(registry.len(), 1);
    let panel = registry.get("my_panel").unwrap();
    assert_eq!(panel.position, "bottom");
    assert_eq!(panel.height, Some(5));
}
