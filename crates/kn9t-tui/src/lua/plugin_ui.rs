//! Plugin-supplied Lua UI.
//!
//! A plugin that wants to draw in the TUI ships Lua source, not a fixed
//! placeholder vocabulary. It sends the source once (`ui_register_lua`), then
//! pushes state updates (`ui_set_state`) which are cheap JSON.
//!
//! # Interaction
//!
//! A plugin view is not render-only: it declares `id="..."` on widgets and
//! binds `kn9t.on_click(id, fn)` / `kn9t.on_key(key, fn)` inside its own
//! environment. Both registries are keyed by `(plugin, id)` so two plugins can
//! use the same `id` or the same key without colliding, and a plugin can never
//! see or unbind another's handlers.
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

/// Host global holding queued outbound effects from plugin views.
const QUEUE_KEY: &str = "_kn9t_plugin_queue";

/// An effect a plugin view asked the host to perform.
///
/// Queued rather than applied inline: Lua callbacks run with no access to
/// `&mut App`, exactly like `kn9t.action` in `keymap.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEffect {
    /// Append text to the user's input box.
    InsertInput { plugin: String, text: String },
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
        }
    }
    for i in 1..=len {
        let _ = queue.set(i, LuaValue::Nil);
    }
    out
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
    pub fn register(
        &mut self,
        lua: &Lua,
        plugin: &str,
        source: &str,
        placement: PluginPlacement,
    ) {
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
    /// interaction API (`log`, `on_click`, `on_key`, `insert_input`), and
    /// nothing that would let it reach the host UI's globals.
    ///
    /// Every callback closes over `plugin`, so a plugin's own name is not a
    /// parameter it could forge to reach another plugin's registry.
    fn prepare_env(&self, lua: &Lua, plugin: &str) -> LuaResult<Table> {
        let env = lua.create_table()?;
        let globals = lua.globals();

        // Pure helpers only. Deliberately no `kn9t` table from globals: a
        // plugin must not read session state or call HTTP through the UI layer.
        for name in [
            "assert", "error", "ipairs", "math", "next", "pairs", "pcall", "select", "string",
            "table", "tonumber", "tostring", "type", "unpack", "xpcall",
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

    /// Drop every click/key handler owned by `plugin`.
    fn forget_handlers(lua: &Lua, plugin: &str) {
        for key in [CLICKS_KEY, KEYS_KEY] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lua() -> Lua {
        Lua::new()
    }

    /// Register with no placement — the common case in these tests, which are
    /// about behaviour rather than layout. Keeps every call site from repeating
    /// `PluginPlacement::default()`.
    trait RegisterExt {
        fn register_plain(&mut self, lua: &Lua, plugin: &str, source: &str);
    }

    impl RegisterExt for PluginUiRegistry {
        fn register_plain(&mut self, lua: &Lua, plugin: &str, source: &str) {
            self.register(lua, plugin, source, PluginPlacement::default());
        }
    }

    const SIMPLE: &str = r#"
        function render(state)
            return { type = "text", content = "hello " .. (state.who or "?") }
        end
    "#;

    #[test]
    fn registers_and_renders() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "demo", SIMPLE);
        reg.set_state(&lua, "demo", &json!({"who": "world"}));

        let w = reg.build(&lua, "demo").expect("should render");
        match w {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello world"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// State may arrive before the Lua source; the first render must still see it.
    #[test]
    fn state_before_register_is_kept() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.set_state(&lua, "demo", &json!({"who": "early"}));
        reg.register_plain(&lua, "demo", SIMPLE);

        let w = reg.build(&lua, "demo").unwrap();
        match w {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello early"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// The core isolation property: plugins must not see each other, and must
    /// not see or modify the host UI's globals.
    #[test]
    fn plugins_cannot_reach_globals_or_each_other() {
        let lua = lua();
        lua.globals().set("host_secret", "do-not-leak").unwrap();

        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "a",
            r#"
                shared = "from-a"
                function render(s)
                    return { type = "text", content = tostring(host_secret) }
                end
            "#,
        );
        reg.register_plain(
            &lua,
            "b",
            r#"function render(s) return { type = "text", content = tostring(shared) } end"#,
        );

        // Plugin a cannot read a host global.
        match reg.build(&lua, "a").unwrap() {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "nil"),
            other => panic!("expected text, got {other:?}"),
        }
        // Plugin b cannot see plugin a's top-level assignment.
        match reg.build(&lua, "b").unwrap() {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "nil"),
            other => panic!("expected text, got {other:?}"),
        }
        // The host's own globals are untouched.
        let secret: String = lua.globals().get("host_secret").unwrap();
        assert_eq!(secret, "do-not-leak");
        assert!(
            lua.globals()
                .get::<Option<LuaValue>>("render")
                .unwrap()
                .is_none(),
            "plugin render() must not land in globals"
        );
    }

    #[test]
    fn syntax_error_is_reported_not_panicked() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "bad", "function render( this is not lua");

        let err = reg.build(&lua, "bad").unwrap_err();
        assert!(!err.is_empty(), "must carry a message to display");
    }

    #[test]
    fn missing_render_function_is_reported() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "empty", "local x = 1");

        let err = reg.build(&lua, "empty").unwrap_err();
        assert!(err.contains("render"), "got: {err}");
    }

    #[test]
    fn runtime_error_in_render_is_reported() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "boom", r#"function render(s) error("kaboom") end"#);

        let err = reg.build(&lua, "boom").unwrap_err();
        assert!(err.contains("kaboom"), "got: {err}");
    }

    /// One broken plugin must not stop a healthy one from rendering.
    #[test]
    fn broken_plugin_does_not_affect_others() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "broken", "syntax ((( error");
        reg.register_plain(&lua, "good", SIMPLE);
        reg.set_state(&lua, "good", &json!({"who": "fine"}));

        assert!(reg.build(&lua, "broken").is_err());
        match reg.build(&lua, "good").unwrap() {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "hello fine"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn re_register_replaces_previous_definition() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "demo", SIMPLE);
        reg.set_state(&lua, "demo", &json!({"who": "v1"}));
        reg.register_plain(
            &lua,
            "demo",
            r#"function render(s) return { type = "text", content = "v2" } end"#,
        );

        match reg.build(&lua, "demo").unwrap() {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "v2"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn names_are_stable_order() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        for n in ["zeta", "alpha", "mid"] {
            reg.register_plain(&lua, n, SIMPLE);
        }
        assert_eq!(reg.names(), vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn remove_and_clear() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(&lua, "a", SIMPLE);
        reg.register_plain(&lua, "b", SIMPLE);

        reg.remove(&lua, "a");
        assert_eq!(reg.names(), vec!["b"]);
        reg.clear(&lua);
        assert!(reg.is_empty());
    }

    #[test]
    fn nested_json_state_becomes_lua_tables() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                function render(s)
                    local n = #s.items
                    return { type = "text",
                             content = s.meta.title .. ":" .. n .. ":" .. tostring(s.flag) }
                end
            "#,
        );
        reg.set_state(
            &lua,
            "demo",
            &json!({"items": [1, 2, 3], "meta": {"title": "T"}, "flag": true}),
        );

        match reg.build(&lua, "demo").unwrap() {
            w if w.text().is_some() => assert_eq!(w.text().unwrap(), "T:3:true"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// Arrays must be 1-based so `ipairs` and `#` behave as Lua authors expect.
    #[test]
    fn json_arrays_are_one_based() {
        let lua = lua();
        let v = json_to_lua(&lua, &json!(["a", "b"])).unwrap();
        let t = match v {
            LuaValue::Table(t) => t,
            _ => panic!("expected table"),
        };
        assert_eq!(t.get::<String>(1).unwrap(), "a");
        assert_eq!(t.get::<String>(2).unwrap(), "b");
        assert_eq!(t.len().unwrap(), 2);
    }

    // ── Interaction ─────────────────────────────────────────────────────────

    #[test]
    fn plugin_key_handler_consumes_and_falls_through() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("j", function() return true end)
                kn9t.on_key("q", function() return false end)
                function render(s) return { type = "text", content = "x" } end
            "#,
        );

        assert!(reg.has_key(&lua, "demo", "j"));
        assert!(reg.dispatch_key(&lua, "demo", "j"), "true consumes");
        assert!(
            !reg.dispatch_key(&lua, "demo", "q"),
            "false falls through to the host"
        );
        assert!(!reg.has_key(&lua, "demo", "z"), "unbound key not claimed");
        assert!(!reg.dispatch_key(&lua, "demo", "z"));
    }

    /// An unparseable key must be refused at bind time, so a typo surfaces in
    /// the log instead of creating a handler nothing can ever trigger.
    #[test]
    fn plugin_rejects_unparseable_key() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                OK = kn9t.on_key("NotAKey", function() return true end)
                function render(s) return { type = "text", content = tostring(OK) } end
            "#,
        );
        assert!(!reg.has_key(&lua, "demo", "NotAKey"));
        assert_eq!(
            reg.build(&lua, "demo").unwrap().text().unwrap(),
            "false",
            "on_key must report refusal to the plugin"
        );
    }

    /// The isolation property that makes per-plugin keying necessary: two
    /// plugins may use the same id and the same key without interfering.
    #[test]
    fn handlers_are_scoped_per_plugin() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        let src = r#"
            HITS = 0
            kn9t.on_click("row", function() HITS = HITS + 1; return true end)
            kn9t.on_key("j", function() HITS = HITS + 1; return true end)
            function render(s) return { type = "text", content = tostring(HITS) } end
        "#;
        reg.register_plain(&lua, "a", src);
        reg.register_plain(&lua, "b", src);

        reg.dispatch_click(&lua, "a", "row", 0, 0, "left");
        reg.dispatch_key(&lua, "a", "j");

        assert_eq!(reg.build(&lua, "a").unwrap().text().unwrap(), "2");
        assert_eq!(
            reg.build(&lua, "b").unwrap().text().unwrap(),
            "0",
            "b must not see a's input"
        );
    }

    /// Removing a view must drop its handlers, or a stale closure keeps firing
    /// for a panel that is no longer on screen.
    #[test]
    fn removing_a_view_drops_its_handlers() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("j", function() return true end)
                function render(s) return { type = "text", content = "x" } end
            "#,
        );
        assert!(reg.has_key(&lua, "demo", "j"));

        reg.remove(&lua, "demo");
        assert!(!reg.has_key(&lua, "demo", "j"), "handler is gone");
        assert!(!reg.dispatch_key(&lua, "demo", "j"));
    }

    /// A hot-reload replaces handlers rather than leaving the old ones bound.
    #[test]
    fn re_register_replaces_stale_handlers() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("j", function() return true end)
                function render(s) return { type = "text", content = "v1" } end
            "#,
        );
        assert!(reg.has_key(&lua, "demo", "j"));

        // The new version binds a different key and no longer binds `j`.
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("k", function() return true end)
                function render(s) return { type = "text", content = "v2" } end
            "#,
        );
        assert!(
            !reg.has_key(&lua, "demo", "j"),
            "binding from the old version must not survive"
        );
        assert!(reg.has_key(&lua, "demo", "k"));
    }

    /// A broken handler must not consume the key, or the TUI would feel dead.
    #[test]
    fn erroring_handler_does_not_consume() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("j", function() error("boom") end)
                kn9t.on_click("row", function() error("boom") end)
                function render(s) return { type = "text", content = "x" } end
            "#,
        );
        assert!(!reg.dispatch_key(&lua, "demo", "j"));
        assert!(!reg.dispatch_click(&lua, "demo", "row", 0, 0, "left"));
    }

    #[test]
    fn insert_input_queues_an_effect() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("s", function() kn9t.insert_input("hello"); return true end)
                function render(s) return { type = "text", content = "x" } end
            "#,
        );

        assert!(drain_effects(&lua).is_empty(), "nothing queued yet");
        reg.dispatch_key(&lua, "demo", "s");

        assert_eq!(
            drain_effects(&lua),
            vec![PluginEffect::InsertInput {
                plugin: "demo".to_string(),
                text: "hello".to_string(),
            }]
        );
        assert!(drain_effects(&lua).is_empty(), "queue is consumed");
    }

    /// A plugin cannot forge another plugin's name on an effect: the owner is
    /// closed over at bind time, not passed in.
    #[test]
    fn effects_are_attributed_to_the_calling_plugin() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        let src = r#"
            kn9t.on_key("s", function() kn9t.insert_input("from-" .. kn9t.plugin); return true end)
            function render(s) return { type = "text", content = "x" } end
        "#;
        reg.register_plain(&lua, "a", src);
        reg.register_plain(&lua, "b", src);

        reg.dispatch_key(&lua, "b", "s");
        assert_eq!(
            drain_effects(&lua),
            vec![PluginEffect::InsertInput {
                plugin: "b".to_string(),
                text: "from-b".to_string(),
            }]
        );
    }

    /// Interaction must not weaken the sandbox: the new API is additive.
    #[test]
    fn interactive_plugins_still_cannot_reach_host_globals() {
        let lua = lua();
        lua.globals().set("host_secret", "do-not-leak").unwrap();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_key("j", function() return true end)
                function render(s)
                    return { type = "text", content = tostring(host_secret) .. "/" .. tostring(io) }
                end
            "#,
        );
        assert_eq!(reg.build(&lua, "demo").unwrap().text().unwrap(), "nil/nil");
    }

    // ── Placement ───────────────────────────────────────────────────────────

    /// A plugin's declared placement must survive registration and be readable,
    /// since that is what lets a config route by zone instead of by name.
    #[test]
    fn placement_is_stored_and_exposed() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(
            &lua,
            "demo",
            SIMPLE,
            PluginPlacement {
                zone: Some("main".into()),
                title: Some("Diff review".into()),
                rows: Some(24),
                cols: None,
            },
        );

        let views = reg.views();
        assert_eq!(views.len(), 1);
        let (name, p) = views[0];
        assert_eq!(name, "demo");
        assert_eq!(p.zone.as_deref(), Some("main"));
        assert_eq!(p.title.as_deref(), Some("Diff review"));
        assert_eq!(p.rows, Some(24));
        assert_eq!(p.cols, None);
    }

    /// State arriving before the source must not invent a placement: the plugin
    /// has not spoken yet, so every hint stays empty.
    #[test]
    fn state_before_register_has_no_placement() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.set_state(&lua, "demo", &json!({"who": "early"}));

        let views = reg.views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].1, &PluginPlacement::default());
    }

    /// End-to-end shape check: the tree a plugin returns must survive the host's
    /// real widget parser, and its `id=` must resolve to a rect click dispatch
    /// can find. Without this a view could bind `on_click` for an id that never
    /// becomes a hit area, failing only as "clicking does nothing" at runtime.
    #[test]
    fn interactive_view_tree_parses_and_exposes_click_ids() {
        use ratatui::layout::Rect;

        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register_plain(
            &lua,
            "demo",
            r#"
                kn9t.on_click("files", function() return true end)
                kn9t.on_click("body", function() return true end)
                function render(state)
                    return {
                        type = "split", direction = "vertical",
                        children = {
                            { type = "text", content = "head", size = { fixed = 1 } },
                            { type = "split", direction = "horizontal", children = {
                                { type = "list", id = "files",
                                  items = { { spans = { { text = "a.rs" } } } },
                                  selected = 0, size = { fixed = 20 } },
                                { type = "list", id = "body",
                                  items = { { spans = { { text = "+x", fg = "green" } } } },
                                  offset = 0 },
                            }},
                        },
                    }
                end
            "#,
        );

        let tree = reg.build(&lua, "demo").expect("host must parse the tree");

        let mut areas: Vec<(String, Rect)> = Vec::new();
        super::super::widgets::collect_clickable_areas(&tree, Rect::new(0, 0, 80, 24), &mut areas);

        let ids: Vec<&str> = areas.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"files"), "got ids: {ids:?}");
        assert!(ids.contains(&"body"), "got ids: {ids:?}");

        // Clicks are dispatched using these rects, so the geometry has to be
        // real, not merely present.
        let files = areas.iter().find(|(id, _)| id == "files").unwrap().1;
        assert_eq!(files.width, 20);
        assert_eq!(files.y, 1, "sits below the fixed-height header");
        let body = areas.iter().find(|(id, _)| id == "body").unwrap().1;
        assert_eq!(body.x, 20, "starts where the file list ends");

        assert!(reg.dispatch_click(&lua, "demo", "files", 0, 0, "left"));
        assert!(reg.dispatch_click(&lua, "demo", "body", 0, 0, "left"));
    }
}
