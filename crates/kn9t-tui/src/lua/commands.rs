//! Lua-registered palette/slash commands.
//!
//! `kn9t.register_command({id=, label=, description=, category=, slash=,
//! handler=function(args) ... end})` lets a config add entries to the command
//! palette and, if `slash` is given, the `/` autocomplete dropdown — both of
//! which were previously `pub const` Rust arrays with no way for Lua to
//! extend them.
//!
//! Mirrors `panels::PanelRegistry` exactly: register into a pending Lua table,
//! drain it into this registry on load/reload, store the handler as a
//! `RegistryKey` (a Lua closure cannot cross the FFI boundary any other way).
//!
//! Dispatch: Rust's built-in `execute_slash_command`/`execute_palette_command`
//! try their own `match` first; a Lua-registered `id` is only consulted from
//! the `_ => {}` fallthrough arm, EXCEPT that a Lua registration with the same
//! `id` as a built-in overwrites nothing in Rust — the built-in `match` still
//! wins. This was a deliberate simplification (see TRACKING/CHANGELOG): true
//! override-by-id would mean checking the Lua registry FIRST on every command,
//! adding a hashmap lookup to the hot Enter-key path for every one of the ~50
//! built-ins that never need it.

use std::collections::HashMap;

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table};

/// One Lua-registered command.
pub struct LuaCommand {
    pub id: String,
    pub label: String,
    pub description: String,
    pub category: String,
    /// If set, this command is also reachable by typing `/<slash>` (without
    /// the leading slash) in the slash-command dropdown.
    pub slash: Option<String>,
    handler_key: RegistryKey,
}

impl LuaCommand {
    /// Run this command's handler with the raw argument string (everything
    /// after the slash command name, or empty for a palette-only invocation).
    pub fn run(&self, lua: &Lua, args: &str) {
        let Ok(func) = lua.registry_value::<Function>(&self.handler_key) else {
            return;
        };
        if let Err(e) = func.call::<()>(args) {
            crate::log!("Lua command '{}' handler error: {}", self.id, e);
        }
    }
}

/// Registry of Lua-registered commands, keyed by id.
///
/// Order is preserved (insertion order) so the palette lists Lua commands in
/// the order a config declared them, not hash-randomized.
#[derive(Default)]
pub struct LuaCommandRegistry {
    commands: HashMap<String, LuaCommand>,
    order: Vec<String>,
}

impl LuaCommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// All registered commands, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &LuaCommand> {
        self.order.iter().filter_map(|id| self.commands.get(id))
    }

    pub fn get(&self, id: &str) -> Option<&LuaCommand> {
        self.commands.get(id)
    }

    /// Insert or replace a command. Replacing keeps its original position in
    /// `order` rather than moving it to the end, so a hot-reload that
    /// re-registers the same command does not reshuffle the palette.
    fn insert(&mut self, cmd: LuaCommand) {
        if !self.commands.contains_key(&cmd.id) {
            self.order.push(cmd.id.clone());
        }
        self.commands.insert(cmd.id.clone(), cmd);
    }

    fn remove(&mut self, id: &str) {
        self.commands.remove(id);
        self.order.retain(|existing| existing != id);
    }
}

/// Install `kn9t.register_command` / `kn9t.unregister_command`.
///
/// Same queue-then-drain idiom as panels/keymap/click: Lua cannot hold a
/// `RegistryKey` itself, so the spec (including the handler function) is
/// staged in a plain Lua table and only converted to a `LuaCommand` — which
/// does hold the `RegistryKey` — when [`drain_pending_commands`] runs on the
/// Rust side.
pub fn install_command_api(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = globals
        .get("kn9t")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    let pending = lua.create_table()?;
    kn9t.set("_pending_commands", pending)?;
    // Lua table iteration order over string keys is NOT the call order specs
    // were registered in; this array is, since it's appended to on every
    // register_command call. drain_pending_commands walks THIS to decide
    // iteration order, using `_pending_commands` only to look the spec up by
    // id — not to iterate it directly.
    let pending_order = lua.create_table()?;
    kn9t.set("_pending_command_order", pending_order)?;
    let pending_remove = lua.create_table()?;
    kn9t.set("_pending_command_removals", pending_remove)?;

    let register_fn = lua.create_function(|lua, spec: Table| {
        let id: String = spec.get("id").unwrap_or_default();
        if id.is_empty() {
            crate::log!("kn9t.register_command: spec has no 'id', ignoring");
            return Ok(());
        }
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_commands")?;
        pending.set(id.clone(), spec)?;
        let order: Table = kn9t.get("_pending_command_order")?;
        order.set(order.raw_len() + 1, id)?;
        Ok(())
    })?;
    kn9t.set("register_command", register_fn)?;

    let unregister_fn = lua.create_function(|lua, id: String| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_commands")?;
        pending.set(id.clone(), mlua::Value::Nil)?;
        let to_remove: Table = kn9t.get("_pending_command_removals")?;
        to_remove.set(to_remove.raw_len() + 1, id)?;
        Ok(())
    })?;
    kn9t.set("unregister_command", unregister_fn)?;

    globals.set("kn9t", kn9t)?;
    Ok(())
}

/// Apply queued `kn9t.register_command` / `kn9t.unregister_command` calls.
///
/// Returns the number of commands applied (added or removed).
pub fn drain_pending_commands(lua: &Lua, registry: &mut LuaCommandRegistry) -> LuaResult<usize> {
    let globals = lua.globals();
    let Ok(kn9t) = globals.get::<Table>("kn9t") else {
        return Ok(0);
    };

    let mut applied = 0;

    if let Ok(to_remove) = kn9t.get::<Table>("_pending_command_removals") {
        let len = to_remove.raw_len();
        for i in 1..=len {
            if let Ok(id) = to_remove.get::<String>(i) {
                registry.remove(&id);
                applied += 1;
            }
        }
        if len > 0 {
            kn9t.set("_pending_command_removals", lua.create_table()?)?;
        }
    }

    let Ok(pending) = kn9t.get::<Table>("_pending_commands") else {
        return Ok(applied);
    };
    let Ok(order) = kn9t.get::<Table>("_pending_command_order") else {
        return Ok(applied);
    };

    // Iterate the ORDER array, not `pending.pairs()`: Lua table iteration
    // over string keys is not registration order, and this registry promises
    // callers (see `LuaCommandRegistry::insert`) that iteration order matches
    // the order `kn9t.register_command` was called in.
    let mut seen: Vec<String> = Vec::new();
    let order_len = order.raw_len();
    for i in 1..=order_len {
        let Ok(id) = order.get::<String>(i) else {
            continue;
        };
        if seen.contains(&id) {
            continue; // A re-registration already appended a duplicate id.
        }
        seen.push(id.clone());

        let value: mlua::Value = pending.get(id.as_str())?;
        let mlua::Value::Table(spec) = value else {
            continue; // Nil means unregistered again before this drain ran.
        };

        let Ok(handler) = spec.get::<Function>("handler") else {
            crate::log!(
                "kn9t.register_command('{}'): missing 'handler' function, ignoring",
                id
            );
            continue;
        };
        let label: String = spec.get("label").unwrap_or_else(|_| id.clone());
        let description: String = spec.get("description").unwrap_or_default();
        let category: String = spec.get("category").unwrap_or_else(|_| "Lua".to_string());
        let slash: Option<String> = spec.get("slash").ok();
        // Accept a leading "/" for convenience, but store without it — the
        // slash dropdown's own matching (mirroring SlashCommand::name) never
        // includes the leading slash either.
        let slash = slash.map(|s| s.trim_start_matches('/').to_string());

        registry.insert(LuaCommand {
            id: id.clone(),
            label,
            description,
            category,
            slash,
            handler_key: lua.create_registry_value(handler)?,
        });
        applied += 1;
    }

    for id in &seen {
        pending.set(id.as_str(), mlua::Value::Nil)?;
    }
    if order_len > 0 {
        kn9t.set("_pending_command_order", lua.create_table()?)?;
    }

    Ok(applied)
}

