//! Plugin-supplied Lua UI.
//!
//! A plugin that wants to draw in the TUI ships Lua source, not a fixed
//! placeholder vocabulary. It sends the source once (`ui_register_lua`), then
//! pushes state updates (`ui_set_state`) which are cheap JSON.
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
//! A plugin returns a widget tree; it does not choose where that tree lands.
//! `kn9t.plugin_views()` lists what is available and the user's `tui.lua`
//! decides whether and where to draw each one. Plugins cannot seize screen
//! space or hide the transcript.

use std::collections::BTreeMap;

use mlua::{Lua, Result as LuaResult, Table, Value as LuaValue};
use serde_json::Value as Json;

use super::widgets::Widget;

/// One plugin's registered UI.
struct PluginUi {
    /// The plugin's private environment (holds its `render` function).
    env: Table,
    /// Latest state pushed by the plugin, as a Lua value.
    state: LuaValue,
    /// Set when the chunk failed to load or `render` errored. Shown in place of
    /// the view so failures are visible rather than silent.
    error: Option<String>,
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
    pub fn register(&mut self, lua: &Lua, plugin: &str, source: &str) {
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

        self.uis
            .insert(plugin.to_string(), PluginUi { env, state, error });
    }

    /// Build the private environment a plugin's chunk runs in.
    ///
    /// It gets the standard library it needs for formatting, plus `kn9t.log`,
    /// and nothing that would let it reach the host UI's globals.
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

        // Namespaced logging so plugin output is attributable.
        let owner = plugin.to_string();
        let log = lua.create_function(move |_, msg: String| {
            crate::log!("plugin_ui {}: {}", owner, msg);
            Ok(())
        })?;
        let kn9t = lua.create_table()?;
        kn9t.set("log", log)?;
        kn9t.set("plugin", plugin)?;
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
                },
            );
        }
    }

    /// Remove a plugin's UI (plugin unloaded or session cleared).
    pub fn remove(&mut self, plugin: &str) {
        self.uis.remove(plugin);
    }

    pub fn clear(&mut self) {
        self.uis.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.uis.is_empty()
    }

    /// Names of registered plugins, in stable order.
    pub fn names(&self) -> Vec<&str> {
        self.uis.keys().map(|s| s.as_str()).collect()
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

    const SIMPLE: &str = r#"
        function render(state)
            return { type = "text", content = "hello " .. (state.who or "?") }
        end
    "#;

    #[test]
    fn registers_and_renders() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(&lua, "demo", SIMPLE);
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
        reg.register(&lua, "demo", SIMPLE);

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
        reg.register(
            &lua,
            "a",
            r#"
                shared = "from-a"
                function render(s)
                    return { type = "text", content = tostring(host_secret) }
                end
            "#,
        );
        reg.register(
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
        reg.register(&lua, "bad", "function render( this is not lua");

        let err = reg.build(&lua, "bad").unwrap_err();
        assert!(!err.is_empty(), "must carry a message to display");
    }

    #[test]
    fn missing_render_function_is_reported() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(&lua, "empty", "local x = 1");

        let err = reg.build(&lua, "empty").unwrap_err();
        assert!(err.contains("render"), "got: {err}");
    }

    #[test]
    fn runtime_error_in_render_is_reported() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(&lua, "boom", r#"function render(s) error("kaboom") end"#);

        let err = reg.build(&lua, "boom").unwrap_err();
        assert!(err.contains("kaboom"), "got: {err}");
    }

    /// One broken plugin must not stop a healthy one from rendering.
    #[test]
    fn broken_plugin_does_not_affect_others() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(&lua, "broken", "syntax ((( error");
        reg.register(&lua, "good", SIMPLE);
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
        reg.register(&lua, "demo", SIMPLE);
        reg.set_state(&lua, "demo", &json!({"who": "v1"}));
        reg.register(
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
            reg.register(&lua, n, SIMPLE);
        }
        assert_eq!(reg.names(), vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn remove_and_clear() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(&lua, "a", SIMPLE);
        reg.register(&lua, "b", SIMPLE);

        reg.remove("a");
        assert_eq!(reg.names(), vec!["b"]);
        reg.clear();
        assert!(reg.is_empty());
    }

    #[test]
    fn nested_json_state_becomes_lua_tables() {
        let lua = lua();
        let mut reg = PluginUiRegistry::new();
        reg.register(
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
}
