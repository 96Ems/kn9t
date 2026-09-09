//! Lua sandbox — remove dangerous functions by default.
//!
//! The sandbox removes:
//! - `io` module (file I/O)
//! - `os.execute`, `os.exit`, `os.getenv`, `os.remove`, `os.rename`, `os.tmpname`
//! - `loadfile`, `dofile` (arbitrary file execution)
//! - `load` with bytecode (only string source allowed)
//! - `debug` module (can escape sandbox)
//! - `package` module (can load arbitrary C modules)
//!
//! The sandbox keeps:
//! - `print` (redirected to TUI log)
//! - `pairs`, `ipairs`, `next`
//! - `type`, `tostring`, `tonumber`
//! - `string`, `table`, `math` modules
//! - `os.time`, `os.date`, `os.difftime`, `os.clock` (safe time functions)
//! - `error`, `pcall`, `xpcall`, `assert`
//! - `select`, `rawequal`, `rawget`, `rawset`, `rawlen`
//! - `setmetatable`, `getmetatable`
//! - `collectgarbage` (with limited modes)

use mlua::{Function, Lua, Result as LuaResult, Table, Value};

/// Apply sandbox restrictions to a Lua state.
///
/// This should be called on a fresh Lua instance before loading any user code.
pub fn apply_sandbox(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    // Remove dangerous modules entirely
    globals.set("io", Value::Nil)?;
    globals.set("debug", Value::Nil)?;
    globals.set("package", Value::Nil)?;

    // Remove dangerous file-loading functions
    globals.set("loadfile", Value::Nil)?;
    globals.set("dofile", Value::Nil)?;

    // Restrict os module to safe functions only
    sandbox_os(lua)?;

    // Replace print with a version that logs to TUI
    sandbox_print(lua)?;

    // Restrict load() to only accept string source (no bytecode)
    sandbox_load(lua)?;

    // Restrict collectgarbage to safe modes
    sandbox_collectgarbage(lua)?;

    // 96E-43: Install panel API (kn9t.register_panel, etc.)
    super::panels::install_panel_api(lua)?;

    // Install keymap API (kn9t.map / kn9t.unmap)
    super::keymap::install_keymap_api(lua)?;

    // Install click API (kn9t.on_click / kn9t.remove_click)
    super::click::install_click_api(lua)?;

    // Install command API (kn9t.register_command / kn9t.unregister_command)
    super::commands::install_command_api(lua)?;

    Ok(())
}

/// Create a sandbox from scratch (for re-initialization on reload).
pub fn create_sandbox() -> LuaResult<Lua> {
    let lua = Lua::new();
    apply_sandbox(&lua)?;
    Ok(lua)
}

/// Sandbox the `os` module: keep only time-related functions.
fn sandbox_os(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    let os: Table = globals.get("os")?;

    // Save the safe functions
    let time: Function = os.get("time")?;
    let date: Function = os.get("date")?;
    let difftime: Function = os.get("difftime")?;
    let clock: Function = os.get("clock")?;

    // Create a new restricted os table
    let safe_os = lua.create_table()?;
    safe_os.set("time", time)?;
    safe_os.set("date", date)?;
    safe_os.set("difftime", difftime)?;
    safe_os.set("clock", clock)?;

    // Replace the os module
    globals.set("os", safe_os)?;

    Ok(())
}

/// Replace `print` with a version that logs to the TUI log file.
fn sandbox_print(lua: &Lua) -> LuaResult<()> {
    let print_fn = lua.create_function(|_, args: mlua::Variadic<Value>| {
        let parts: Vec<String> = args
            .iter()
            .map(|v| match v {
                Value::Nil => "nil".to_string(),
                Value::Boolean(b) => b.to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                Value::String(s) => s
                    .to_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| "<invalid utf8>".to_string()),
                Value::Table(_) => "<table>".to_string(),
                Value::Function(_) => "<function>".to_string(),
                Value::Thread(_) => "<thread>".to_string(),
                Value::LightUserData(_) => "<userdata>".to_string(),
                Value::UserData(_) => "<userdata>".to_string(),
                Value::Error(e) => format!("<error: {}>", e),
                _ => "<unknown>".to_string(),
            })
            .collect();

        let msg = parts.join("\t");
        crate::log!("[lua] {}", msg);
        Ok(())
    })?;

    lua.globals().set("print", print_fn)?;
    Ok(())
}

/// Restrict `load` to only accept string source code (no bytecode).
fn sandbox_load(lua: &Lua) -> LuaResult<()> {
    let original_load: Function = lua.globals().get("load")?;

    let safe_load = lua.create_function(move |_lua, args: mlua::Variadic<Value>| {
        // First argument must be a string (source code, not bytecode)
        match args.first() {
            Some(Value::String(s)) => {
                let source = s
                    .to_str()
                    .map_err(|e| mlua::Error::runtime(e.to_string()))?;
                // Check for bytecode signature (Lua 5.4: "\x1bLua")
                if source.starts_with('\x1b') {
                    return Err(mlua::Error::runtime("bytecode loading is disabled"));
                }
                // Call the original load with the validated source
                original_load.call::<Value>(args)
            }
            Some(Value::Function(_)) => {
                // Reader function - disallow to prevent bytecode injection
                Err(mlua::Error::runtime(
                    "load() with reader function is disabled",
                ))
            }
            _ => Err(mlua::Error::runtime("load() requires a string argument")),
        }
    })?;

    lua.globals().set("load", safe_load)?;
    Ok(())
}

/// Restrict `collectgarbage` to safe modes only.
fn sandbox_collectgarbage(lua: &Lua) -> LuaResult<()> {
    let safe_gc = lua.create_function(|lua, args: mlua::Variadic<Value>| {
        let mode = match args.first() {
            Some(Value::String(s)) => s
                .to_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "collect".to_string()),
            _ => "collect".to_string(),
        };

        // Only allow safe GC operations
        match mode.as_str() {
            "collect" | "count" | "isrunning" => {
                // These are safe - call the real collectgarbage
                let gc: Function = lua.globals().get("_collectgarbage")?;
                gc.call::<Value>(args)
            }
            _ => Err(mlua::Error::runtime(format!(
                "collectgarbage mode '{}' is disabled",
                mode
            ))),
        }
    })?;

    // Save original as _collectgarbage
    let original: Function = lua.globals().get("collectgarbage")?;
    lua.globals().set("_collectgarbage", original)?;
    lua.globals().set("collectgarbage", safe_gc)?;

    Ok(())
}

