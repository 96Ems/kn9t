//! Lua click handlers — widget `id`s bound to Lua callbacks.
//!
//! A widget carrying `id="..."` in its Lua table gets wrapped in
//! [`crate::lua::widgets::Widget::Clickable`]; `kn9t.on_click(id, function(x, y,
//! button) ... end)` registers what runs when that rect is clicked.
//!
//! Coordinates passed to the handler are local to the widget's own rect
//! (`mouse_x - rect.x`), not screen-absolute — a list at column 40 should not
//! have to know its own screen position to find out which row was clicked.
//!
//! Dispatch order mirrors floats-over-base-layout: the caller (`App::handle_click`)
//! is expected to check floating panels before the base `render_ui` tree, same as
//! rendering paints floats last (on top). A handler returning `false` falls
//! through to Rust's existing hardcoded click handling (tool cards, diff
//! viewer), matching how `kn9t.map` lets a keymap extend rather than replace.

use std::collections::HashMap;

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table, Value};

/// Registry of Lua click handlers, keyed by widget `id`.
#[derive(Debug, Default)]
pub struct ClickRegistry {
    handlers: HashMap<String, RegistryKey>,
}

impl ClickRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Whether any Lua handler is bound to `id`.
    pub fn has(&self, id: &str) -> bool {
        self.handlers.contains_key(id)
    }

    /// Bind `id` to a Lua function, replacing any previous binding.
    pub fn insert(&mut self, id: String, func_key: RegistryKey) {
        self.handlers.insert(id, func_key);
    }

    pub fn remove(&mut self, id: &str) {
        self.handlers.remove(id);
    }

    /// Invoke the handler bound to `id` with rect-local coordinates.
    ///
    /// Returns `true` when the click was consumed. A handler that returns
    /// `false` (or errors) is treated as *not* consumed, so Rust's existing
    /// click handling (tool cards, diff viewer) still gets a chance — the same
    /// "extend, don't replace" contract `KeymapRegistry::dispatch` has.
    pub fn dispatch(&self, lua: &Lua, id: &str, local_x: u16, local_y: u16, button: &str) -> bool {
        let Some(reg_key) = self.handlers.get(id) else {
            return false;
        };
        let Ok(func) = lua.registry_value::<Function>(reg_key) else {
            return false;
        };
        match func.call::<Value>((local_x, local_y, button)) {
            Ok(Value::Boolean(false)) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log!("Lua on_click '{}' error: {}", id, e);
                false
            }
        }
    }
}

/// Install `kn9t.on_click` / `kn9t.remove_click` into the Lua environment.
///
/// Registrations are queued in `kn9t._pending_clicks`, drained by
/// [`drain_pending_clicks`] — the same registry-then-drain idiom as
/// `kn9t.map`/`kn9t.unmap`, so a hot-reload replaces handlers instead of
/// accumulating duplicates.
pub fn install_click_api(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = globals
        .get("kn9t")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    let pending = lua.create_table()?;
    kn9t.set("_pending_clicks", pending)?;

    let on_click_fn = lua.create_function(|lua, (id, func): (String, Function)| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_clicks")?;
        pending.set(id, func)?;
        Ok(())
    })?;
    kn9t.set("on_click", on_click_fn)?;

    let remove_click_fn = lua.create_function(|lua, id: String| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_clicks")?;
        // false is the tombstone: distinguishes "unbind" from "never mentioned".
        pending.set(id, false)?;
        Ok(())
    })?;
    kn9t.set("remove_click", remove_click_fn)?;

    globals.set("kn9t", kn9t)?;
    Ok(())
}

/// Apply queued `kn9t.on_click` / `kn9t.remove_click` calls to `registry`.
///
/// Returns the number of handlers applied (added or removed).
pub fn drain_pending_clicks(lua: &Lua, registry: &mut ClickRegistry) -> LuaResult<usize> {
    let globals = lua.globals();
    let Ok(kn9t) = globals.get::<Table>("kn9t") else {
        return Ok(0);
    };
    let Ok(pending) = kn9t.get::<Table>("_pending_clicks") else {
        return Ok(0);
    };

    let mut applied = 0;
    let mut seen: Vec<String> = Vec::new();

    for pair in pending.pairs::<String, Value>() {
        let (id, value) = pair?;
        seen.push(id.clone());

        match value {
            Value::Function(f) => {
                registry.insert(id, lua.create_registry_value(f)?);
                applied += 1;
            }
            Value::Boolean(false) => {
                registry.remove(&id);
                applied += 1;
            }
            _ => {
                crate::log!("Lua on_click '{}': expected function, got other value", id);
            }
        }
    }

    for id in seen {
        pending.set(id, Value::Nil)?;
    }

    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
