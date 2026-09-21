//! Lua keymaps — key strings bound to Lua callbacks.
//!
//! Lua registers handlers with `kn9t.map("C-t", function() ... end)`.
//! Keys are matched *before* the built-in Rust actions, so a Lua map can
//! override any default binding. A handler returning `false` falls through
//! to the Rust action, which lets Lua extend behaviour without replacing it.
//!
//! Key syntax matches `keybind::parse_key`: `"C-t"`, `"A-Enter"`, `"F5"`.

use std::collections::HashMap;

use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table, Value};

/// Registry of Lua key handlers, keyed by canonical key string.
#[derive(Debug, Default)]
pub struct KeymapRegistry {
    maps: HashMap<String, RegistryKey>,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.maps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    /// Whether any Lua handler is bound to `key`.
    pub fn has(&self, key: &str) -> bool {
        self.maps.contains_key(key)
    }

    /// All bound keys (for help/which-key display).
    pub fn keys(&self) -> Vec<&str> {
        self.maps.keys().map(|s| s.as_str()).collect()
    }

    /// Bind `key` to a Lua function, replacing any previous binding.
    pub fn insert(&mut self, key: String, func_key: RegistryKey) {
        self.maps.insert(key, func_key);
    }

    pub fn remove(&mut self, key: &str) {
        self.maps.remove(key);
    }

    /// Invoke the handler for `key`.
    ///
    /// Returns `true` when the key was consumed. A handler that returns `false`
    /// (or errors) is treated as *not* consumed so Rust still runs its action.
    pub fn dispatch(&self, lua: &Lua, key: &str) -> bool {
        let Some(reg_key) = self.maps.get(key) else {
            return false;
        };
        let Ok(func) = lua.registry_value::<Function>(reg_key) else {
            return false;
        };
        match func.call::<Value>(()) {
            // Explicit `false` means "let Rust handle it too".
            Ok(Value::Boolean(false)) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log!("Lua keymap '{}' error: {}", key, e);
                false
            }
        }
    }
}

/// Install `kn9t.map` / `kn9t.unmap` into the Lua environment.
///
/// Registrations are queued in `kn9t._pending_maps` and drained by
/// [`drain_pending_maps`], mirroring how panels are registered.
pub fn install_keymap_api(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = globals
        .get("kn9t")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    let pending = lua.create_table()?;
    kn9t.set("_pending_maps", pending)?;

    let map_fn = lua.create_function(|lua, (key, func): (String, Function)| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_maps")?;
        pending.set(key, func)?;
        Ok(())
    })?;
    kn9t.set("map", map_fn)?;

    let unmap_fn = lua.create_function(|lua, key: String| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let pending: Table = kn9t.get("_pending_maps")?;
        // false is the tombstone: distinguishes "unbind" from "never mentioned".
        pending.set(key, false)?;
        Ok(())
    })?;
    kn9t.set("unmap", unmap_fn)?;

    // Queue of built-in actions requested from Lua, drained by the app loop.
    let actions = lua.create_table()?;
    kn9t.set("_pending_actions", actions)?;

    // kn9t.action("scroll_top") or kn9t.action("switch_session", id) — run a
    // built-in action from a Lua handler. Queued rather than immediate: Lua
    // callbacks have no access to &mut App.
    //
    // `arg` is optional and ignored by every action except ones that document
    // needing it (currently just `switch_session`); this keeps one dispatch
    // path instead of a second function for the one action that takes data.
    let action_fn = lua.create_function(|lua, (name, arg): (String, Option<String>)| {
        if !crate::keybind::is_valid_action_name(&name) {
            crate::log!("Lua: unknown action '{}'", name);
            return Ok(false);
        }
        let kn9t: Table = lua.globals().get("kn9t")?;
        let queue: Table = kn9t.get("_pending_actions")?;
        let entry = lua.create_table()?;
        entry.set("name", name)?;
        if let Some(a) = arg {
            entry.set("arg", a)?;
        }
        queue.push(entry)?;
        Ok(true)
    })?;
    kn9t.set("action", action_fn)?;

    // `kn9t.invalidate()` — force a UI rebuild next frame even though nothing
    // Rust can see changed. `render_ui`'s cache is fingerprinted from data Rust
    // already has (message/tool counts, scroll, cost, ...); it cannot see a
    // Lua-local toggle like `SHOW.sidebar` flip. A keymap handler that mutates
    // such state must call this, or the next redraw reuses the stale tree.
    let invalidate_fn = lua.create_function(|lua, ()| {
        let kn9t: Table = lua.globals().get("kn9t")?;
        let epoch: i64 = kn9t.get("_epoch").unwrap_or(0);
        kn9t.set("_epoch", epoch + 1)?;
        Ok(())
    })?;
    kn9t.set("invalidate", invalidate_fn)?;

    globals.set("kn9t", kn9t)?;
    Ok(())
}

/// Drain queued `kn9t.action(...)` calls, in call order.
///
/// Each entry is `(action_name, optional_arg)`; the arg is `None` for the
/// ~44 param-less actions and `Some(id)` for e.g. `switch_session`.
pub fn drain_pending_actions(lua: &Lua) -> Vec<(String, Option<String>)> {
    let Ok(kn9t) = lua.globals().get::<Table>("kn9t") else {
        return Vec::new();
    };
    let Ok(queue) = kn9t.get::<Table>("_pending_actions") else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let len = queue.len().unwrap_or(0);
    for i in 1..=len {
        if let Ok(entry) = queue.get::<Table>(i) {
            if let Ok(name) = entry.get::<String>("name") {
                let arg = entry.get::<String>("arg").ok();
                out.push((name, arg));
            }
        }
    }
    for i in 1..=len {
        let _ = queue.set(i, Value::Nil);
    }
    out
}

/// Read the `kn9t.invalidate()` epoch counter.
///
/// A plain integer read every frame — cheap enough to fold into the UI
/// fingerprint unconditionally rather than only checking it lazily.
pub fn read_epoch(lua: &Lua) -> u64 {
    let Ok(kn9t) = lua.globals().get::<Table>("kn9t") else {
        return 0;
    };
    kn9t.get::<i64>("_epoch").unwrap_or(0).max(0) as u64
}

/// Apply queued `kn9t.map` / `kn9t.unmap` calls to `registry`.
///
/// Returns the number of bindings applied (added or removed).
pub fn drain_pending_maps(lua: &Lua, registry: &mut KeymapRegistry) -> LuaResult<usize> {
    let globals = lua.globals();
    let Ok(kn9t) = globals.get::<Table>("kn9t") else {
        return Ok(0);
    };
    let Ok(pending) = kn9t.get::<Table>("_pending_maps") else {
        return Ok(0);
    };

    let mut applied = 0;
    let mut seen: Vec<String> = Vec::new();

    for pair in pending.pairs::<String, Value>() {
        let (key, value) = pair?;
        seen.push(key.clone());

        match value {
            Value::Function(f) => {
                // Reject keys we could never match, so typos surface in the log
                // instead of silently doing nothing.
                if !crate::keybind::is_valid_key_string(&key) {
                    crate::log!("Lua keymap: ignoring unparseable key '{}'", key);
                    continue;
                }
                registry.insert(key, lua.create_registry_value(f)?);
                applied += 1;
            }
            Value::Boolean(false) => {
                registry.remove(&key);
                applied += 1;
            }
            _ => {
                crate::log!("Lua keymap '{}': expected function, got other value", key);
            }
        }
    }

    for key in seen {
        pending.set(key, Value::Nil)?;
    }

    Ok(applied)
}
