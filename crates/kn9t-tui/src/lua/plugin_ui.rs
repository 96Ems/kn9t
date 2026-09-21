//! Plugin-supplied Lua UI.
//!
//! A plugin that wants to draw in the TUI ships Lua source, not a fixed
//! placeholder vocabulary. It sends the source once (`ui_register_lua`), then
//! pushes state updates (`ui_set_state`) which are cheap JSON.
//!
//! # Interaction
//!
//! A plugin view is not render-only: it declares `id="..."` on widgets and
//! binds `kn9t.on_click(id, fn)` / `kn9t.on_key(key, fn)` / `kn9t.on_text(fn)`
//! inside its own environment. The registries are keyed by `(plugin, id)`, so
//! plugins cannot collide with or reach each other's handlers.
//!
//! `kn9t.respond(payload)` answers a plugin's own interaction; the host keeps
//! the transport and the cancel.
//!
//! Keys reach a plugin only while it holds focus (`focused_plugin` on the app),
//! so a plugin cannot silently swallow global keys like `Ctrl+C`. This is the
//! same "extend, don't replace" contract as `kn9t.map`: a handler returning
//! `false` falls through to the host.
//!
//! Handlers cannot mutate host state directly — Lua callbacks have no `&mut
//! App` — so side effects are queued as [`PluginEffect`] and drained by the app
//! loop, mirroring `kn9t.action` in `keymap.rs`.
//!
//! # Isolation
//!
//! Each plugin's chunk runs with its own environment table, so plugins cannot
//! see or clobber each other's definitions — or the user's `~/.kn9t/tui.lua`.
//! This is collision avoidance, not a security boundary: plugins are native
//! executables (`Command::new`) and already hold full OS privileges, so
//! restricting their Lua would protect nothing.
//!
//! What the isolation *does* guarantee is that a buggy plugin degrades to a
//! visible error in its own panel instead of corrupting unrelated UI.
//!
//! # Who decides placement
//!
//! A plugin returns a widget tree and may *request* a zone (`placement`,
//! `title`, `rows`, `cols` on `ui_register_lua`); it does not choose where the
//! tree lands. `kn9t.state.plugin_view_specs` surfaces those requests so a
//! config can route by zone rather than by plugin name — but the config decides
//! whether to honour them. Plugins cannot seize screen space or hide the
//! transcript.

use std::collections::BTreeMap;

use mlua::{Function, Lua, Result as LuaResult, Table, Value as LuaValue};
use serde_json::Value as Json;

use super::widgets::Widget;
use crate::reducer::PluginPlacement;

/// Host global holding per-plugin click handlers: `{ [plugin] = { [id] = fn } }`.
///
/// Kept in host globals rather than in each plugin's env so a plugin cannot
/// enumerate or overwrite another plugin's bindings — the plugin only ever
/// reaches this through the `kn9t.on_click` closure, which pins the owner.
const CLICKS_KEY: &str = "_kn9t_plugin_clicks";

/// Host global holding per-plugin key handlers: `{ [plugin] = { [key] = fn } }`.
const KEYS_KEY: &str = "_kn9t_plugin_keys";

/// Host global holding per-plugin text handlers: `{ [plugin] = { fn = fn } }`.
/// Separate from `KEYS_KEY`: text entry is every printable glyph, not one binding.
const TEXT_KEY: &str = "_kn9t_plugin_text";

/// Host global holding queued outbound effects from plugin views.
const QUEUE_KEY: &str = "_kn9t_plugin_queue";

/// An effect a plugin view asked the host to perform.
///
/// Queued rather than applied inline: Lua callbacks run with no access to
/// `&mut App`, exactly like `kn9t.action` in `keymap.rs`.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginEffect {
    /// Append text to the user's input box.
    InsertInput { plugin: String, text: String },
    /// Notify the plugin backend (Rust process) of a UI interaction.
    /// The plugin receives this via HostMsg::Event if it subscribed to "ui_interaction".
    NotifyPlugin {
        plugin: String,
        event: String,
        data: serde_json::Value,
    },
    /// Answer the pending interaction this view renders.
    Respond {
        plugin: String,
        payload: serde_json::Value,
    },
}

/// Fetch (creating if absent) `globals[key][plugin]`.
fn plugin_scoped_table(lua: &Lua, key: &str, plugin: &str) -> LuaResult<Table> {
    let globals = lua.globals();
    let root: Table = match globals.get::<Option<Table>>(key)? {
        Some(t) => t,
        None => {
            let t = lua.create_table()?;
            globals.set(key, t.clone())?;
            t
        }
    };
    let scoped: Table = match root.get::<Option<Table>>(plugin)? {
        Some(t) => t,
        None => {
            let t = lua.create_table()?;
            root.set(plugin, t.clone())?;
            t
        }
    };
    Ok(scoped)
}

/// Fetch (creating if absent) the outbound effect queue.
fn host_queue(lua: &Lua) -> LuaResult<Table> {
    let globals = lua.globals();
    match globals.get::<Option<Table>>(QUEUE_KEY)? {
        Some(t) => Ok(t),
        None => {
            let t = lua.create_table()?;
            globals.set(QUEUE_KEY, t.clone())?;
            Ok(t)
        }
    }
}

/// Drain queued plugin-view effects, in call order.
pub fn drain_effects(lua: &Lua) -> Vec<PluginEffect> {
    let Ok(Some(queue)) = lua.globals().get::<Option<Table>>(QUEUE_KEY) else {
        return Vec::new();
    };
    let len = queue.len().unwrap_or(0);
    let mut out = Vec::new();
    for i in 1..=len {
        let Ok(entry) = queue.get::<Table>(i) else {
            continue;
        };
        let plugin: String = entry.get("plugin").unwrap_or_default();
        let op: String = entry.get("op").unwrap_or_default();
        if op == "insert_input" {
            let text: String = entry.get("text").unwrap_or_default();
            out.push(PluginEffect::InsertInput { plugin, text });
        } else if op == "notify_plugin" {
            let event: String = entry.get("event").unwrap_or_default();
            let data: LuaValue = entry.get("data").unwrap_or(LuaValue::Nil);
            let data_json = lua_to_json(&data);
            out.push(PluginEffect::NotifyPlugin {
                plugin,
                event,
                data: data_json,
            });
        } else if op == "respond" {
            let payload: LuaValue = entry.get("payload").unwrap_or(LuaValue::Nil);
            out.push(PluginEffect::Respond {
                plugin,
                payload: lua_to_json(&payload),
            });
        }
    }
    for i in 1..=len {
        let _ = queue.set(i, LuaValue::Nil);
    }
    out
}

/// Convert a Lua value to serde_json::Value.
fn lua_to_json(v: &LuaValue) -> serde_json::Value {
    match v {
        LuaValue::Nil => serde_json::Value::Null,
        LuaValue::Boolean(b) => serde_json::Value::Bool(*b),
        LuaValue::Integer(i) => serde_json::Value::Number((*i).into()),
        LuaValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        LuaValue::String(s) => {
            let str_val = s.to_str().map(|s| s.to_string()).unwrap_or_default();
            serde_json::Value::String(str_val)
        }
        LuaValue::Table(t) => {
            // Check if it's an array (sequential integer keys starting at 1)
            let len = t.len().unwrap_or(0);
            if len > 0 {
                let mut arr = Vec::new();
                let mut is_array = true;
                for i in 1..=len {
                    if let Ok(val) = t.get::<LuaValue>(i) {
                        arr.push(lua_to_json(&val));
                    } else {
                        is_array = false;
                        break;
                    }
                }
                if is_array {
                    return serde_json::Value::Array(arr);
                }
            }
            // Otherwise treat as object
            let mut map = serde_json::Map::new();
            if let Ok(pairs) = t
                .clone()
                .pairs::<String, LuaValue>()
                .collect::<Result<Vec<_>, _>>()
            {
                for (k, val) in pairs {
                    map.insert(k, lua_to_json(&val));
                }
            }
            serde_json::Value::Object(map)
        }
        _ => serde_json::Value::Null,
    }
}

/// One plugin's registered UI.
struct PluginUi {
    /// The plugin's private environment (holds its `render` function).
    env: Table,
    /// Latest state pushed by the plugin, as a Lua value.
    state: LuaValue,
    /// Set when the chunk failed to load or `render` errored. Shown in place of
    /// the view so failures are visible rather than silent.
    error: Option<String>,
    /// What the plugin asked for, layout-wise. Surfaced to Lua so a config can
    /// route by zone instead of by plugin name.
    placement: PluginPlacement,
    /// Hash of the Lua source, so re-registering with identical source skips
    /// re-execution and preserves view state (V).
    source_hash: u64,
}

/// All plugin-supplied UIs, keyed by `plugin` name.
///
/// `BTreeMap` so iteration order is stable: panels must not reshuffle between
/// frames just because of hash ordering.
#[derive(Default)]
pub struct PluginUiRegistry {
    uis: BTreeMap<String, PluginUi>,
}

impl PluginUiRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a plugin's Lua UI.
    ///
    /// A load failure is recorded rather than returned: one broken plugin must
    /// not abort the frame or block other plugins from registering.
    ///
    /// If the source is identical to the previously registered source (same hash),
    /// we skip re-execution to preserve view state (`V`). This allows plugins to
    /// idempotently re-register every poll without resetting cursor/mode/etc.
    pub fn register(&mut self, lua: &Lua, plugin: &str, source: &str, placement: PluginPlacement) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        let new_hash = hasher.finish();

        crate::log!(
            "plugin_ui register: plugin={} placement={:?}",
            plugin,
            placement.zone
        );

        // If source unchanged, just update placement (in case it changed) and return.
        if let Some(existing) = self.uis.get_mut(plugin) {
            if existing.source_hash == new_hash {
                existing.placement = placement;
                return;
            }
        }

        // A re-register is a hot-reload: the new chunk rebinds what it wants, so
        // anything left from the previous version would be a stale closure over
        // an env nothing else references.
        Self::forget_handlers(lua, plugin);

        let env = match self.prepare_env(lua, plugin) {
            Ok(e) => e,
            Err(e) => {
                crate::log!("plugin_ui {}: env setup failed: {}", plugin, e);
                return;
            }
        };

        let chunk_name = format!("@plugin:{plugin}");
        let error = match lua
            .load(source)
            .set_name(chunk_name)
            .set_environment(env.clone())
            .exec()
        {
            Ok(()) => {
                // A chunk that loads but defines no `render` is a plugin bug;
                // report it now rather than drawing nothing every frame.
                match env.get::<Option<mlua::Function>>("render") {
                    Ok(Some(_)) => None,
                    _ => Some("no render() function defined".to_string()),
                }
            }
            Err(e) => {
                crate::log!("plugin_ui {}: load failed: {}", plugin, e);
                Some(format!("{e}"))
            }
        };

        // Preserve any state already pushed, so registration order and state
        // order do not have to match.
        let state = self
            .uis
            .get(plugin)
            .map(|u| u.state.clone())
            .unwrap_or(LuaValue::Nil);

        self.uis.insert(
            plugin.to_string(),
            PluginUi {
                env,
                state,
                error,
                placement,
                source_hash: new_hash,
            },
        );
    }

    /// Build the private environment a plugin's chunk runs in.
    ///
    /// It gets the standard library it needs for formatting, plus the
    /// interaction API (`log`, `on_click`, `on_key`, `on_text`, `respond`,
    /// `insert_input`), and nothing that would let it reach the host UI's globals.
    ///
    /// Every callback closes over `plugin`, so a plugin's own name is not a
    /// parameter it could forge to reach another plugin's registry.
    fn prepare_env(&self, lua: &Lua, plugin: &str) -> LuaResult<Table> {
        let env = lua.create_table()?;
        let globals = lua.globals();

        // Pure helpers only, plus the sandboxed `os` (already stripped down to
        // time/date/difftime/clock by `sandbox::apply_sandbox` on the shared globals
        // this reads from - no `os.execute`/`os.exit`/`os.getenv` leaks through).
        // Deliberately no `kn9t` table from globals: a plugin must not read session
        // state or call HTTP through the UI layer.
        for name in [
            "assert", "error", "ipairs", "math", "next", "os", "pairs", "pcall", "select",
            "string", "table", "tonumber", "tostring", "type", "unpack", "xpcall",
        ] {
            if let Ok(v) = globals.get::<LuaValue>(name) {
                env.set(name, v)?;
            }
        }

        let kn9t = lua.create_table()?;

        // Namespaced logging so plugin output is attributable.
        let owner = plugin.to_string();
        let log = lua.create_function(move |_, msg: String| {
            crate::log!("plugin_ui {}: {}", owner, msg);
            Ok(())
        })?;
        kn9t.set("log", log)?;
        kn9t.set("plugin", plugin)?;

        // kn9t.on_click(id, fn) — bind a handler for a widget carrying that
        // `id`. Scoped to this plugin, so ids need only be unique within the
        // view, not globally.
        let owner = plugin.to_string();
        let on_click = lua.create_function(move |lua, (id, func): (String, Function)| {
            plugin_scoped_table(lua, CLICKS_KEY, &owner)?.set(id, func)?;
            Ok(())
        })?;
        kn9t.set("on_click", on_click)?;

        // kn9t.on_key(key, fn) — bind a key while this view holds focus.
        let owner = plugin.to_string();
        let on_key = lua.create_function(move |lua, (key, func): (String, Function)| {
            if !crate::keybind::is_valid_key_string(&key) {
                crate::log!("plugin_ui {}: ignoring unparseable key '{}'", owner, key);
                return Ok(false);
            }
            plugin_scoped_table(lua, KEYS_KEY, &owner)?.set(key, func)?;
            Ok(true)
        })?;
        kn9t.set("on_key", on_key)?;

        // kn9t.insert_input(text) — the one host mutation a view may request.
        // Deliberately narrow: it is how a review panel hands its collected
        // comments to the prompt, which is the workflow that motivated giving
        // plugin views input at all.
        let owner = plugin.to_string();
        let insert_input = lua.create_function(move |lua, text: String| {
            let queue = host_queue(lua)?;
            let entry = lua.create_table()?;
            entry.set("plugin", owner.as_str())?;
            entry.set("op", "insert_input")?;
            entry.set("text", text)?;
            queue.push(entry)?;
            Ok(())
        })?;
        kn9t.set("insert_input", insert_input)?;

        // kn9t.notify(data) — send an event to the plugin backend (Rust process).
        // The plugin must subscribe to "ui_interaction" events in its Hello.
        // `data` should be a table with at least an `event` field.
        let owner = plugin.to_string();
        let notify = lua.create_function(move |lua, data: Table| {
            let queue = host_queue(lua)?;
            let entry = lua.create_table()?;
            entry.set("plugin", owner.as_str())?;
            entry.set("op", "notify_plugin")?;
            let event: String = data.get("event").unwrap_or_default();
            entry.set("event", event)?;
            entry.set("data", data)?;
            queue.push(entry)?;
            Ok(())
        })?;
        kn9t.set("notify", notify)?;

        // kn9t.on_text(fn) — printable characters while this view holds focus.
        // A handler returning `false` falls through to `on_key`/the host.
        let owner = plugin.to_string();
        let on_text = lua.create_function(move |lua, func: Function| {
            plugin_scoped_table(lua, TEXT_KEY, &owner)?.set("fn", func)?;
            Ok(())
        })?;
        kn9t.set("on_text", on_text)?;

        // kn9t.respond(payload) — answer the pending interaction this view renders.
        let owner = plugin.to_string();
        let respond = lua.create_function(move |lua, payload: LuaValue| {
            let queue = host_queue(lua)?;
            let entry = lua.create_table()?;
            entry.set("plugin", owner.as_str())?;
            entry.set("op", "respond")?;
            entry.set("payload", payload)?;
            queue.push(entry)?;
            Ok(())
        })?;
        kn9t.set("respond", respond)?;

        env.set("kn9t", kn9t)?;

        // Lua looks up globals through `_ENV`; point it at the env itself so
        // top-level assignments stay local to this plugin.
        env.set("_G", env.clone())?;

        Ok(env)
    }

    /// Push new state for a plugin. Cheap: JSON in, Lua value out, no rendering.
    pub fn set_state(&mut self, lua: &Lua, plugin: &str, state: &Json) {
        let value = match json_to_lua(lua, state) {
            Ok(v) => v,
            Err(e) => {
                crate::log!("plugin_ui {}: bad state: {}", plugin, e);
                return;
            }
        };
        if let Some(ui) = self.uis.get_mut(plugin) {
            ui.state = value;
        } else {
            // State can arrive before the Lua chunk; keep it so the first
            // render after registration already has data.
            let env = match self.prepare_env(lua, plugin) {
                Ok(e) => e,
                Err(_) => return,
            };
            self.uis.insert(
                plugin.to_string(),
                PluginUi {
                    env,
                    state: value,
                    error: Some("awaiting ui_register_lua".to_string()),
                    placement: PluginPlacement::default(),
                    source_hash: 0,
                },
            );
        }
    }

    /// Remove a plugin's UI (plugin unloaded or session cleared).
    ///
    /// Also drops its handlers: leaving them bound would let a stale closure
    /// fire for a view that is no longer on screen.
    pub fn remove(&mut self, lua: &Lua, plugin: &str) {
        self.uis.remove(plugin);
        Self::forget_handlers(lua, plugin);
    }

    pub fn clear(&mut self, lua: &Lua) {
        let names: Vec<String> = self.uis.keys().cloned().collect();
        for name in names {
            Self::forget_handlers(lua, &name);
        }
        self.uis.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.uis.is_empty()
    }

    /// Names of registered plugins, in stable order.
    pub fn names(&self) -> Vec<&str> {
        self.uis.keys().map(|s| s.as_str()).collect()
    }

    /// `(name, placement)` for every registered view, in stable order.
    ///
    /// This is what lets a config route by zone rather than by plugin name.
    pub fn views(&self) -> Vec<(&str, &PluginPlacement)> {
        self.uis
            .iter()
            .map(|(name, ui)| (name.as_str(), &ui.placement))
            .collect()
    }

    /// Drop every click/key/text handler owned by `plugin`.
    fn forget_handlers(lua: &Lua, plugin: &str) {
        for key in [CLICKS_KEY, KEYS_KEY, TEXT_KEY] {
            if let Ok(Some(root)) = lua.globals().get::<Option<Table>>(key) {
                let _ = root.set(plugin, LuaValue::Nil);
            }
        }
    }

    /// Dispatch a click to `plugin`'s handler for `id`, with rect-local coords.
    ///
    /// Returns `true` when consumed. A missing handler, an explicit `false`, or
    /// an error all mean "not consumed", so the host still gets its chance —
    /// the same contract as `ClickRegistry::dispatch`.
    pub fn dispatch_click(
        &self,
        lua: &Lua,
        plugin: &str,
        id: &str,
        local_x: u16,
        local_y: u16,
        button: &str,
    ) -> bool {
        let Ok(handlers) = plugin_scoped_table(lua, CLICKS_KEY, plugin) else {
            return false;
        };
        let Ok(Some(func)) = handlers.get::<Option<Function>>(id) else {
            return false;
        };
        match func.call::<LuaValue>((local_x, local_y, button)) {
            Ok(LuaValue::Boolean(false)) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log!("plugin_ui {}: on_click '{}' error: {}", plugin, id, e);
                false
            }
        }
    }

    /// Dispatch a key to `plugin`'s handler. Returns `true` when consumed.
    pub fn dispatch_key(&self, lua: &Lua, plugin: &str, key: &str) -> bool {
        let Ok(handlers) = plugin_scoped_table(lua, KEYS_KEY, plugin) else {
            return false;
        };
        let Ok(Some(func)) = handlers.get::<Option<Function>>(key) else {
            return false;
        };
        match func.call::<LuaValue>(key) {
            Ok(LuaValue::Boolean(false)) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log!("plugin_ui {}: on_key '{}' error: {}", plugin, key, e);
                false
            }
        }
    }

    /// Whether `plugin` has a handler bound for `key`.
    ///
    /// Lets the caller tell "focused plugin ignores this key" from "focused
    /// plugin consumed it" without running the handler.
    pub fn has_key(&self, lua: &Lua, plugin: &str, key: &str) -> bool {
        plugin_scoped_table(lua, KEYS_KEY, plugin)
            .and_then(|t| t.get::<Option<Function>>(key))
            .map(|f| f.is_some())
            .unwrap_or(false)
    }

    /// Whether `plugin` installed a printable-character handler.
    pub fn has_text(&self, lua: &Lua, plugin: &str) -> bool {
        plugin_scoped_table(lua, TEXT_KEY, plugin)
            .and_then(|t| t.get::<Option<Function>>("fn"))
            .map(|f| f.is_some())
            .unwrap_or(false)
    }

    /// Dispatch one printable character to `plugin`'s text handler. Returns
    /// `true` when consumed; an explicit `false` or an error falls through.
    pub fn dispatch_text(&self, lua: &Lua, plugin: &str, ch: &str) -> bool {
        let Ok(handlers) = plugin_scoped_table(lua, TEXT_KEY, plugin) else {
            return false;
        };
        let Ok(Some(func)) = handlers.get::<Option<Function>>("fn") else {
            return false;
        };
        match func.call::<LuaValue>(ch) {
            Ok(LuaValue::Boolean(false)) => false,
            Ok(_) => true,
            Err(e) => {
                crate::log!("plugin_ui {}: on_text error: {}", plugin, e);
                false
            }
        }
    }

    /// Call a plugin's `render(state)` and parse the widget tree.
    ///
    /// Returns `Err` with a displayable message when the plugin is broken, so
    /// the caller can show the error in the space the view would have taken.
    pub fn build(&self, lua: &Lua, plugin: &str) -> Result<Widget, String> {
        let ui = self
            .uis
            .get(plugin)
            .ok_or_else(|| format!("no UI registered for {plugin}"))?;

        if let Some(ref e) = ui.error {
            return Err(e.clone());
        }

        let func: mlua::Function = ui
            .env
            .get("render")
            .map_err(|_| "no render() function".to_string())?;

        let table: Table = func
            .call(ui.state.clone())
            .map_err(|e| format!("render() failed: {e}"))?;

        super::widgets::parse_widget(lua, &table).map_err(|e| format!("invalid widget: {e}"))
    }
}

/// Convert JSON into a Lua value.
///
/// Objects become tables; arrays become 1-based sequences, which is what Lua
/// code expects from `ipairs`.
fn json_to_lua(lua: &Lua, v: &Json) -> LuaResult<LuaValue> {
    Ok(match v {
        Json::Null => LuaValue::Nil,
        Json::Bool(b) => LuaValue::Boolean(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                LuaValue::Integer(i)
            } else {
                LuaValue::Number(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => LuaValue::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.set(i + 1, json_to_lua(lua, item)?)?;
            }
            LuaValue::Table(t)
        }
        Json::Object(map) => {
            let t = lua.create_table()?;
            for (k, val) in map {
                t.set(k.as_str(), json_to_lua(lua, val)?)?;
            }
            LuaValue::Table(t)
        }
    })
}
