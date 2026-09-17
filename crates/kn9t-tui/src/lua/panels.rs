//! Dynamic panel registry.
//!
//! Panels are no longer a closed set known at compile time.
//! Lua can register panels at runtime via `register_panel(id, spec)`.

use std::collections::HashMap;

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table};

use super::widgets::{parse_widget, Widget};

/// A dynamically registered panel.
#[derive(Debug)]
pub struct Panel {
    pub id: String,
    /// Position hint: "left", "right", "bottom", "floating", or "absolute"
    pub position: String,
    /// Absolute coordinates (used when position = "absolute")
    pub x: Option<u16>,
    pub y: Option<u16>,
    /// Size hints
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// Anchor point for relative positioning: "top-left", "top-right", "bottom-left", "bottom-right", "center"
    pub anchor: Option<String>,
    /// Whether the panel is currently visible
    pub visible: bool,
    /// Whether the panel has focus
    pub focused: bool,
    /// The widget tree builder function (stored in Lua registry)
    builder_key: Option<RegistryKey>,
    /// Key handler function (stored in Lua registry)
    on_key_key: Option<RegistryKey>,
    /// Focus handler function
    on_focus_key: Option<RegistryKey>,
    /// Blur handler function
    on_blur_key: Option<RegistryKey>,
}

impl Panel {
    /// Build the widget tree for this panel by calling the Lua builder function.
    pub fn build_widget(&self, lua: &Lua) -> Option<Widget> {
        let key = self.builder_key.as_ref()?;
        let func: Function = lua.registry_value(key).ok()?;
        let table: Table = func.call(()).ok()?;
        parse_widget(lua, &table).ok()
    }

    /// Call the on_key handler if defined. Returns true if the key was handled.
    pub fn handle_key(&self, lua: &Lua, key: &str, modifiers: &str) -> bool {
        let Some(key_ref) = self.on_key_key.as_ref() else {
            return false;
        };
        let Ok(func) = lua.registry_value::<Function>(key_ref) else {
            return false;
        };
        match func.call::<bool>((key, modifiers)) {
            Ok(handled) => handled,
            Err(e) => {
                crate::log!("Panel {}: on_key error: {}", self.id, e);
                false
            }
        }
    }

    /// Call the on_focus handler if defined.
    pub fn handle_focus(&self, lua: &Lua) {
        let Some(key_ref) = self.on_focus_key.as_ref() else {
            return;
        };
        let Ok(func) = lua.registry_value::<Function>(key_ref) else {
            return;
        };
        if let Err(e) = func.call::<()>(()) {
            crate::log!("Panel {}: on_focus error: {}", self.id, e);
        }
    }

    /// Call the on_blur handler if defined.
    pub fn handle_blur(&self, lua: &Lua) {
        let Some(key_ref) = self.on_blur_key.as_ref() else {
            return;
        };
        let Ok(func) = lua.registry_value::<Function>(key_ref) else {
            return;
        };
        if let Err(e) = func.call::<()>(()) {
            crate::log!("Panel {}: on_blur error: {}", self.id, e);
        }
    }
}

/// Registry of all dynamic panels.
#[derive(Debug, Default)]
pub struct PanelRegistry {
    panels: HashMap<String, Panel>,
    /// Order in which panels were registered (for consistent iteration)
    order: Vec<String>,
    /// Currently focused panel ID
    focused: Option<String>,
}

impl PanelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new panel from a Lua spec table.
    pub fn register(&mut self, lua: &Lua, id: &str, spec: &Table) -> LuaResult<()> {
        // Parse position (can be "left", "right", "floating", "absolute", etc.)
        let position: String = spec
            .get("position")
            .unwrap_or_else(|_| "floating".to_string());

        // Parse absolute coordinates (for position = "absolute" or fine-tuning)
        let x: Option<u16> = spec.get::<i64>("x").ok().map(|v| v as u16);
        let y: Option<u16> = spec.get::<i64>("y").ok().map(|v| v as u16);

        // Parse size hints
        let width: Option<u16> = spec.get::<i64>("width").ok().map(|w| w as u16);
        let height: Option<u16> = spec.get::<i64>("height").ok().map(|h| h as u16);

        // Parse anchor (for relative positioning from corners/center)
        let anchor: Option<String> = spec.get("anchor").ok();

        // Parse visibility
        let visible: bool = spec.get("visible").unwrap_or(true);

        // Store builder function in Lua registry
        let builder_key = if let Ok(func) = spec.get::<Function>("build") {
            Some(lua.create_registry_value(func)?)
        } else {
            None
        };

        // Store callbacks in Lua registry
        let on_key_key = if let Ok(func) = spec.get::<Function>("on_key") {
            Some(lua.create_registry_value(func)?)
        } else {
            None
        };

        let on_focus_key = if let Ok(func) = spec.get::<Function>("on_focus") {
            Some(lua.create_registry_value(func)?)
        } else {
            None
        };

        let on_blur_key = if let Ok(func) = spec.get::<Function>("on_blur") {
            Some(lua.create_registry_value(func)?)
        } else {
            None
        };

        let panel = Panel {
            id: id.to_string(),
            position,
            x,
            y,
            width,
            height,
            anchor,
            visible,
            focused: false,
            builder_key,
            on_key_key,
            on_focus_key,
            on_blur_key,
        };

        // Remove from order if re-registering
        self.order.retain(|x| x != id);
        self.order.push(id.to_string());

        self.panels.insert(id.to_string(), panel);

        crate::log!("Panel registered: {}", id);
        Ok(())
    }

    /// Unregister a panel.
    pub fn unregister(&mut self, id: &str) {
        self.panels.remove(id);
        self.order.retain(|x| x != id);
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
        crate::log!("Panel unregistered: {}", id);
    }

    /// Get a panel by ID.
    pub fn get(&self, id: &str) -> Option<&Panel> {
        self.panels.get(id)
    }

    /// Get a mutable panel by ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Panel> {
        self.panels.get_mut(id)
    }

    /// Iterate over all panels in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Panel> {
        self.order.iter().filter_map(|id| self.panels.get(id))
    }

    /// Iterate over visible panels in registration order.
    pub fn visible(&self) -> impl Iterator<Item = &Panel> {
        self.iter().filter(|p| p.visible)
    }

    /// Get panels by position.
    pub fn by_position(&self, position: &str) -> Vec<&Panel> {
        self.visible().filter(|p| p.position == position).collect()
    }

    /// Set focus to a panel.
    pub fn set_focus(&mut self, lua: &Lua, id: Option<&str>) {
        // Blur old focused panel
        if let Some(old_id) = &self.focused {
            if let Some(panel) = self.panels.get_mut(old_id) {
                panel.focused = false;
                panel.handle_blur(lua);
            }
        }

        // Focus new panel
        if let Some(new_id) = id {
            if let Some(panel) = self.panels.get_mut(new_id) {
                panel.focused = true;
                panel.handle_focus(lua);
            }
        }

        self.focused = id.map(String::from);
    }

    /// Get the currently focused panel ID.
    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Handle a key event, routing to the focused panel.
    /// Returns true if the key was handled.
    pub fn handle_key(&self, lua: &Lua, key: &str, modifiers: &str) -> bool {
        if let Some(id) = &self.focused {
            if let Some(panel) = self.panels.get(id) {
                return panel.handle_key(lua, key, modifiers);
            }
        }
        false
    }

    /// Show a panel.
    pub fn show(&mut self, id: &str) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.visible = true;
        }
    }

    /// Hide a panel.
    pub fn hide(&mut self, id: &str) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.visible = false;
        }
    }

    /// Toggle a panel's visibility.
    pub fn toggle(&mut self, id: &str) {
        if let Some(panel) = self.panels.get_mut(id) {
            panel.visible = !panel.visible;
        }
    }

    /// Clear all panels (on hot-reload).
    pub fn clear(&mut self) {
        self.panels.clear();
        self.order.clear();
        self.focused = None;
    }

    /// Check if any panels are registered.
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    /// Get count of registered panels.
    pub fn len(&self) -> usize {
        self.panels.len()
    }
}

/// Install panel API functions into Lua globals.
pub fn install_panel_api(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    // Create kn9t namespace if it doesn't exist
    let kn9t: Table = globals
        .get("kn9t")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // kn9t.register_panel(id, spec)
    // Note: The actual registration happens in Rust; this just stores the spec
    // for later retrieval. The App will call registry.register() during init/reload.
    let register_panel = lua.create_function(|lua, (id, spec): (String, Table)| {
        // Store in a global table for later processing
        let globals = lua.globals();
        let pending: Table = globals
            .get("_kn9t_pending_panels")
            .unwrap_or_else(|_| lua.create_table().unwrap());
        pending.set(id, spec)?;
        globals.set("_kn9t_pending_panels", pending)?;
        Ok(())
    })?;
    kn9t.set("register_panel", register_panel)?;

    // kn9t.unregister_panel(id)
    let unregister_panel = lua.create_function(|lua, id: String| {
        let globals = lua.globals();
        if let Ok(pending) = globals.get::<Table>("_kn9t_pending_panels") {
            pending.set(id.clone(), mlua::Value::Nil)?;
        }
        // Mark for unregistration
        let to_remove: Table = globals
            .get("_kn9t_panels_to_remove")
            .unwrap_or_else(|_| lua.create_table().unwrap());
        to_remove.set(to_remove.len()? + 1, id)?;
        globals.set("_kn9t_panels_to_remove", to_remove)?;
        Ok(())
    })?;
    kn9t.set("unregister_panel", unregister_panel)?;

    // kn9t.show_panel(id)
    let show_panel = lua.create_function(|lua, id: String| {
        let globals = lua.globals();
        let cmds: Table = globals
            .get("_kn9t_panel_cmds")
            .unwrap_or_else(|_| lua.create_table().unwrap());
        let cmd = lua.create_table()?;
        cmd.set("action", "show")?;
        cmd.set("id", id)?;
        cmds.set(cmds.len()? + 1, cmd)?;
        globals.set("_kn9t_panel_cmds", cmds)?;
        Ok(())
    })?;
    kn9t.set("show_panel", show_panel)?;

    // kn9t.hide_panel(id)
    let hide_panel = lua.create_function(|lua, id: String| {
        let globals = lua.globals();
        let cmds: Table = globals
            .get("_kn9t_panel_cmds")
            .unwrap_or_else(|_| lua.create_table().unwrap());
        let cmd = lua.create_table()?;
        cmd.set("action", "hide")?;
        cmd.set("id", id)?;
        cmds.set(cmds.len()? + 1, cmd)?;
        globals.set("_kn9t_panel_cmds", cmds)?;
        Ok(())
    })?;
    kn9t.set("hide_panel", hide_panel)?;

    // kn9t.toggle_panel(id)
    let toggle_panel = lua.create_function(|lua, id: String| {
        let globals = lua.globals();
        let cmds: Table = globals
            .get("_kn9t_panel_cmds")
            .unwrap_or_else(|_| lua.create_table().unwrap());
        let cmd = lua.create_table()?;
        cmd.set("action", "toggle")?;
        cmd.set("id", id)?;
        cmds.set(cmds.len()? + 1, cmd)?;
        globals.set("_kn9t_panel_cmds", cmds)?;
        Ok(())
    })?;
    kn9t.set("toggle_panel", toggle_panel)?;

    // kn9t.focus_panel(id)
    let focus_panel = lua.create_function(|lua, id: String| {
        let globals = lua.globals();
        globals.set("_kn9t_focus_panel", id)?;
        Ok(())
    })?;
    kn9t.set("focus_panel", focus_panel)?;

    globals.set("kn9t", kn9t)?;

    Ok(())
}

/// Process pending panel registrations from Lua.
/// Called by the App after loading/reloading the Lua config.
pub fn process_pending_panels(lua: &Lua, registry: &mut PanelRegistry) -> LuaResult<()> {
    let globals = lua.globals();

    // Process removals first
    if let Ok(to_remove) = globals.get::<Table>("_kn9t_panels_to_remove") {
        for (_, id) in to_remove.pairs::<i64, String>().flatten() {
            registry.unregister(&id);
        }
        globals.set("_kn9t_panels_to_remove", mlua::Value::Nil)?;
    }

    // Process registrations
    if let Ok(pending) = globals.get::<Table>("_kn9t_pending_panels") {
        let pending_count = pending.len().unwrap_or(0);
        if pending_count > 0 {
            crate::log!("Processing {} pending panels", pending_count);
        }
        for (id, spec) in pending.pairs::<String, Table>().flatten() {
            let position: String = spec.get("position").unwrap_or_else(|_| "?".to_string());
            crate::log!("Registering panel '{}' position={}", id, position);
            if let Err(e) = registry.register(lua, &id, &spec) {
                crate::log!("Failed to register panel '{}': {}", id, e);
            }
        }
        // Clear pending after processing
        globals.set("_kn9t_pending_panels", lua.create_table()?)?;
    }

    Ok(())
}

/// Process panel commands (show/hide/toggle) from Lua.
pub fn process_panel_commands(lua: &Lua, registry: &mut PanelRegistry) -> LuaResult<()> {
    let globals = lua.globals();

    if let Ok(cmds) = globals.get::<Table>("_kn9t_panel_cmds") {
        for (_, cmd) in cmds.pairs::<i64, Table>().flatten() {
            let action: String = cmd.get("action").unwrap_or_default();
            let id: String = cmd.get("id").unwrap_or_default();

            match action.as_str() {
                "show" => registry.show(&id),
                "hide" => registry.hide(&id),
                "toggle" => registry.toggle(&id),
                _ => {}
            }
        }
        globals.set("_kn9t_panel_cmds", lua.create_table()?)?;
    }

    // Process focus changes
    if let Ok(focus_id) = globals.get::<String>("_kn9t_focus_panel") {
        registry.set_focus(lua, Some(&focus_id));
        globals.set("_kn9t_focus_panel", mlua::Value::Nil)?;
    }

    Ok(())
}

