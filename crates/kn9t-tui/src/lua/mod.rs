//! Lua customization.
//!
//! In-process Lua embedding for UI scripting. Unlike the subprocess-based
//! plugin system (kn9t-plugin-sdk), this runs in the TUI process for per-frame
//! rendering performance.
//!
//! ## Safety model
//!
//! Lua scripts are sandboxed by default:
//! - No arbitrary file I/O (`io`, `os.execute`, `loadfile` removed)
//! - No network access, and no way to add one
//! - No process spawn
//!
//! Lua reaches the server through **actions**, not HTTP: a handler calls
//! `kn9t.action("refresh_tools")` and Rust performs the request. The earlier
//! `kn9t.http` client was removed — it was inert (never configured, never
//! drained, so its queue grew unbounded), its route whitelist named a `/abort`
//! endpoint that does not exist, and it put an exfiltration surface in a config
//! file people copy from each other. Actions are auditable and cannot be
//! pointed at an arbitrary host.
//!
//! ## Error handling
//!
//! A Lua error (syntax or runtime) never crashes the TUI:
//! - The error is logged and surfaced in the status line
//! - A broken `render_ui` shows an error shell, never a silent fallback
//! - Hot-reload continues watching for fixes

pub mod click;
pub mod commands;
pub mod context;
pub mod default_config;
pub mod keymap;
pub mod panels;
pub mod plugin_ui;
mod sandbox;
pub mod state;
mod watcher;
pub mod widgets;


use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use mlua::{Lua, Result as LuaResult, Value};

pub use sandbox::create_sandbox;
pub use watcher::WatcherHandle;

/// Spawn a file watcher for hot-reload.
///
/// Convenience wrapper that takes `Arc<LuaRuntime>`.
pub fn spawn_watcher(
    path: std::path::PathBuf,
    runtime: std::sync::Arc<LuaRuntime>,
    tx: std::sync::mpsc::Sender<crate::event::Event>,
) -> Option<WatcherHandle> {
    watcher::spawn_watcher(path, runtime, tx)
}

/// Spawn a directory watcher for hot-reload of a `tui/` split config.
///
/// Convenience wrapper that takes `Arc<LuaRuntime>`, mirroring
/// [`spawn_watcher`].
pub fn spawn_dir_watcher(
    dir: std::path::PathBuf,
    runtime: std::sync::Arc<LuaRuntime>,
    tx: std::sync::mpsc::Sender<crate::event::Event>,
) -> Option<WatcherHandle> {
    watcher::spawn_dir_watcher(dir, runtime, tx)
}

/// Lua runtime state, shared across the TUI.
///
/// Thread-safe via RwLock: the main thread reads during render,
/// the watcher thread writes on hot-reload.
pub struct LuaRuntime {
    inner: Arc<RwLock<LuaState>>,
    /// Backing store for `kn9t.get_messages` / `kn9t.get_tools`.
    ///
    /// Lives outside `LuaState` so the accessor closures keep working across a
    /// hot-reload: the sandbox is rebuilt, the store is not.
    lazy: Arc<state::LazyStore>,
    /// Palette published as `kn9t.theme`.
    ///
    /// Held here so `reload()` — which the watcher thread calls with no access
    /// to `App` — can republish it after resetting the sandbox.
    theme: Arc<RwLock<crate::theme::Theme>>,
    /// Memoized `render_ui()` output, keyed by a cheap fingerprint of what it
    /// could plausibly depend on.
    ///
    /// `render_ui` used to run unconditionally on every redraw — at 10 FPS
    /// while streaming, that's the full widget tree (every span, gauge, list
    /// item) rebuilt as fresh Lua tables every 100ms, whether or not anything
    /// visible had changed. This cache turns a `NotDefined`/no-op redraw into a
    /// handful of integer comparisons instead of a Lua call.
    ui_cache: RwLock<Option<(UiFingerprint, widgets::Widget)>>,
}

/// What `build_ui_outcome`'s result could depend on, cheap to compute every
/// frame from data Rust already has (no Lua call, no allocation beyond the
/// tuple itself).
///
/// Deliberately coarse: this is a *may-differ* check, not a proof of purity.
/// Lua-local state (`SHOW.sidebar`, `MAIN_VIEW`, ...) is invisible to Rust by
/// construction, which is exactly why `kn9t.invalidate()` exists as an escape
/// hatch — see `invalidate()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct UiFingerprint {
    width: u16,
    height: u16,
    message_count: usize,
    tool_count: usize,
    scroll: usize,
    streaming: bool,
    aborting: bool,
    /// Cost in integer hundredths of a cent, so the fingerprint stays `Eq`
    /// (floats are not `Eq`) without losing the precision that matters for
    /// "did the number on screen change".
    cost_centicents: i64,
    turn_input: usize,
    turn_output: usize,
    /// Whether the explorer column and the viewer pane are shown: they change the
    /// layout, so the tree must be rebuilt when either flips (PLAN §P7 L2).
    explorer_visible: bool,
    viewer_open: bool,
    /// Bumped by `kn9t.invalidate()`. Lua-local state the fingerprint above
    /// cannot see (toggles, `MAIN_VIEW`, ...) forces a rebuild by bumping this
    /// instead of being modeled field-by-field.
    manual_epoch: u64,
}

struct LuaState {
    lua: Lua,
    /// Last error from Lua execution, shown in status line.
    last_error: Option<String>,
    /// Where the user's config was loaded from, so `reload_all` and the
    /// watcher know what to re-read.
    config_source: Option<ConfigSource>,
    /// Whether the script loaded successfully at least once.
    ever_loaded: bool,
    /// Plugin-supplied UIs, each isolated in its own environment.
    plugin_ui: plugin_ui::PluginUiRegistry,
}

/// Where the user's Lua config comes from.
///
/// A single `tui.lua` file (the original, still fully supported) or a
/// `tui/` directory of numbered files (`00_theme.lua`, `10_header.lua`, ...)
/// loaded alphabetically into the same shared Lua state — so a later file's
/// top-level `local`-turned-global or plain global is visible to an earlier
/// file only if that earlier file actually made it global. This is plain
/// `dofile`-free layering, not a module system: see the crate docs on why
/// real `dofile`/`loadfile` stay removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    File(PathBuf),
    Dir(PathBuf),
}

impl ConfigSource {
    /// Prefer a `tui/` directory over a single `tui.lua` file when both
    /// could exist, but only if the directory actually has something to
    /// load — an empty or absent `tui/` must not silently replace a real
    /// `tui.lua` with nothing.
    pub fn resolve(dir: &std::path::Path, file: &std::path::Path) -> Option<Self> {
        if dir.is_dir() && Self::lua_files_in(dir).next().is_some() {
            return Some(ConfigSource::Dir(dir.to_path_buf()));
        }
        Some(ConfigSource::File(file.to_path_buf()))
    }

    /// `*.lua` files directly inside `dir`, sorted by filename so
    /// `00_theme.lua` loads before `90_render.lua`. Not recursive: a
    /// subdirectory is not part of the load order contract.
    pub fn lua_files_in(dir: &std::path::Path) -> impl Iterator<Item = PathBuf> {
        let unsorted: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("lua"))
            .collect();
        Self::sorted(unsorted).into_iter()
    }

    /// The sort in isolation, so a test can prove it works on an input
    /// constructed directly — not through `read_dir`, whose enumeration order
    /// is filesystem-dependent and was observed to already be alphabetical on
    /// at least one filesystem, which made a test through `read_dir` alone
    /// unable to tell "sorted correctly" from "never sorted, got lucky".
    pub fn sorted(mut files: Vec<PathBuf>) -> Vec<PathBuf> {
        files.sort();
        files
    }
}

impl LuaRuntime {
    /// Create a new Lua runtime with sandboxed environment.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();
        sandbox::apply_sandbox(&lua)?;

        let lazy = Arc::new(state::LazyStore::new());
        // Installed here (and again after each sandbox reset) so the documented
        // `kn9t.get_messages`/`kn9t.get_tools` are always callable. They were
        // previously only ever installed by their own tests, so any config that
        // used them hit a nil and fell into the error shell.
        state::install_lazy_accessors(&lua, lazy.clone())?;

        Ok(Self {
            inner: Arc::new(RwLock::new(LuaState {
                lua,
                last_error: None,
                config_source: None,
                ever_loaded: false,
                plugin_ui: plugin_ui::PluginUiRegistry::new(),
            })),
            lazy,
            theme: Arc::new(RwLock::new(crate::theme::Theme::default())),
            ui_cache: RwLock::new(None),
        })
    }

    /// Refresh the lazy-accessor store if the transcript changed.
    ///
    /// Cheap on an unchanged frame: one version compare, no copying.
    pub fn refresh_lazy(&self, version: u64, build: impl FnOnce() -> state::LazyData) {
        self.lazy.refresh_if_stale(version, build);
    }

    /// Publish `kn9t.theme` / `kn9t.native_views` into the current sandbox.
    ///
    /// The theme is remembered so a hot-reload can republish it without the
    /// watcher thread needing access to the app.
    pub fn install_environment(&self, theme: &crate::theme::Theme) {
        *safe_expect!(self.theme.write(), "poisoned") = theme.clone();
        self.publish_environment();
    }

    fn publish_environment(&self) {
        let theme =safe_expect!(self.theme.read(), "poisoned").clone();
        let st =safe_expect!(self.inner.read(), "poisoned");
        if let Err(e) = state::install_environment(&st.lua, &theme) {
            crate::log!("Failed to publish Lua environment: {}", e);
        }
    }

    /// Load a Lua source string into this runtime.
    ///
    /// `name` is used for error messages (chunk name).
    fn load_source(&self, source: &str, name: &str) -> bool {
        let mut state =safe_expect!(self.inner.write(), "poisoned");
        let result = match state.lua.load(source).set_name(name.to_string()).exec() {
            Ok(()) => {
                state.last_error = None;
                state.ever_loaded = true;
                crate::log!("Lua loaded: {}", name);
                true
            }
            Err(e) => {
                let msg = format!("Lua error: {}", e);
                crate::log!("{} ({})", msg, name);
                state.last_error = Some(msg);
                false
            }
        };
        // Dropped before touching `ui_cache` so the two locks are never held
        // nested in this direction; `build_ui_outcome` takes them in the same
        // order (inner, released, then ui_cache), which is what keeps this
        // deadlock-free against the watcher thread calling reload().
        drop(state);
        // A file that just (re)loaded may define a different `render_ui`, so
        // any cached tree is from a function that may no longer exist. Without
        // this, saving `tui.lua` with an unchanged fingerprint (same message
        // count, same scroll, ...) would keep showing the *previous* file's
        // layout until something else happened to invalidate it.
        *safe_expect!(self.ui_cache.write(), "poisoned") = None;
        result
    }

    /// Load the built-in UI embedded in the binary.
    ///
    /// This always runs first so kn9t is self-contained: a bare executable with
    /// no config directory still has a complete UI. A user file layered on top
    /// only needs to define what it wants to change.
    /// Run the built-in UI: every catalogue file, in filename order.
    ///
    /// The catalogue (`assets/tui/*.lua`) **is** the shipped default, so this is what a fresh
    /// install renders — not a legacy single file that could silently drift from it. See
    /// `default_config::DEFAULT_TUI_FILES`.
    ///
    /// A failing file does not stop the rest: the remaining files still define what they can,
    /// which is what lets the error shell draw something and name the real cause. The first
    /// failure is re-recorded after the loop, because `load_source` clears `last_error` on
    /// every success — otherwise an error in `00_theme.lua` would be erased by
    /// `90_render.lua` loading fine, and the screen would look like a config with no
    /// `render_ui` instead of a broken file.
    pub fn load_builtin(&self) -> bool {
        let mut first_failure: Option<String> = None;
        for (name, source) in default_config::DEFAULT_TUI_FILES {
            if !self.load_source(source, &format!("<builtin:{name}>")) && first_failure.is_none() {
                first_failure = Some(format!(
                    "built-in UI file {name} failed to load — the remaining files were still \
                     applied, so the screen shows the config minus this file"
                ));
            }
        }
        match first_failure {
            Some(msg) => {
                safe_expect!(self.inner.write(), "poisoned").last_error = Some(msg);
                false
            }
            None => true,
        }
    }

    /// Load and execute a user Lua config file, layered over the built-in.
    ///
    /// On error, stores the error message but does not panic.
    /// Returns `true` if loaded successfully.
    ///
    /// This is the single-file primitive; `load_config` is what decides
    /// between this and `load_dir` based on what actually exists on disk.
    pub fn load_file(&self, path: &PathBuf) -> bool {
        {
            let mut state =safe_expect!(self.inner.write(), "poisoned");
            state.config_source = Some(ConfigSource::File(path.clone()));
        }

        match std::fs::read_to_string(path) {
            Ok(source) => self.load_source(&source, &path.display().to_string()),
            Err(e) => {
                let mut state =safe_expect!(self.inner.write(), "poisoned");
                // A missing user config is normal: the built-in already ran.
                if e.kind() == std::io::ErrorKind::NotFound {
                    crate::log!("No user config at {} (using built-in)", path.display());
                    state.last_error = None;
                    true
                } else {
                    let msg = format!("Failed to read {}: {}", path.display(), e);
                    crate::log!("{}", msg);
                    state.last_error = Some(msg);
                    false
                }
            }
        }
    }

    /// Load every `*.lua` file directly inside `dir`, alphabetically, into
    /// this same Lua state — `00_theme.lua` before `90_render.lua`.
    ///
    /// All files share one `Lua` instance (this runtime's), so a plain global
    /// assigned in an earlier file is visible to a later one; a `local` is
    /// not, same as within a single file. There is no `dofile`/`require`
    /// here — see the module docs for why that boundary stays closed.
    ///
    /// Stops at the first file that fails to load, reporting THAT file's
    /// error rather than continuing past a broken file into others that
    /// might reference names it was supposed to define.
    pub fn load_dir(&self, dir: &std::path::Path) -> bool {
        {
            let mut state =safe_expect!(self.inner.write(), "poisoned");
            state.config_source = Some(ConfigSource::Dir(dir.to_path_buf()));
        }

        let files: Vec<PathBuf> = ConfigSource::lua_files_in(dir).collect();
        if files.is_empty() {
            // An empty tui/ is not an error - same as a missing tui.lua.
            let mut state =safe_expect!(self.inner.write(), "poisoned");
            state.last_error = None;
            return true;
        }

        for path in &files {
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    let mut state =safe_expect!(self.inner.write(), "poisoned");
                    let msg = format!("Failed to read {}: {}", path.display(), e);
                    crate::log!("{}", msg);
                    state.last_error = Some(msg);
                    return false;
                }
            };
            if !self.load_source(&source, &path.display().to_string()) {
                return false; // load_source already set last_error.
            }
        }
        true
    }

    /// Load the user's config from whichever of `dir`/`file` actually has
    /// content — see `ConfigSource::resolve` for the precedence rule.
    pub fn load_config(&self, dir: &std::path::Path, file: &std::path::Path) -> bool {
        match ConfigSource::resolve(dir, file) {
            Some(ConfigSource::Dir(d)) => self.load_dir(&d),
            Some(ConfigSource::File(f)) => self.load_file(&f),
            None => true,
        }
    }

    /// Reload from scratch: built-in first, then the user file.
    ///
    /// Used by hot-reload. Re-running the built-in means deleting a function
    /// from your config restores the default behaviour instead of leaving the
    /// previous definition stale in the Lua state.
    pub fn reload_all(&self) -> bool {
        let source =safe_expect!(self.inner.read(), "poisoned").config_source.clone();
        let builtin_ok = self.load_builtin();
        match source {
            Some(ConfigSource::File(p)) => self.load_file(&p) && builtin_ok,
            Some(ConfigSource::Dir(d)) => self.load_dir(&d) && builtin_ok,
            None => builtin_ok,
        }
    }

    /// Reload the config file (called by watcher on file change).
    ///
    /// Resets the sandbox, re-runs the built-in UI, then re-applies the user
    /// file. Re-running the built-in matters: if the user deletes a function
    /// from their config, the default must come back rather than leaving the
    /// previous definition stale in the Lua state.
    pub fn reload(&self) -> bool {
        {
            let mut state =safe_expect!(self.inner.write(), "poisoned");
            if let Err(e) = sandbox::apply_sandbox(&state.lua) {
                state.last_error = Some(format!("Failed to reset sandbox: {}", e));
                return false;
            }
            // `apply_sandbox` reuses the same Lua state and does not clear the
            // `kn9t` table, so the accessors do in fact survive today. They are
            // reinstalled anyway to make that independent of sandbox internals:
            // the day `apply_sandbox` starts from a fresh table (which is what
            // "reset the sandbox" implies), a documented API would otherwise go
            // nil after the user's first save, which is a miserable failure to
            // debug. Reinstalling is idempotent and costs two closures.
            if let Err(e) = state::install_lazy_accessors(&state.lua, self.lazy.clone()) {
                state.last_error = Some(format!("Failed to reinstall accessors: {}", e));
                return false;
            }
        }
        // Same reasoning for `kn9t.theme` / `kn9t.native_views`, with an added
        // reason: republishing picks up a theme change without a restart.
        self.publish_environment();
        self.reload_all()
    }

    /// Get the last error message, if any.
    pub fn last_error(&self) -> Option<String> {
        safe_expect!(self.inner.read(), "poisoned").last_error.clone()
    }

    /// Clear the last error (e.g., after displaying it).
    pub fn clear_error(&self) {
        safe_expect!(self.inner.write(), "poisoned").last_error = None;
    }

    /// Check if a Lua config was ever successfully loaded.
    pub fn is_loaded(&self) -> bool {
        safe_expect!(self.inner.read(), "poisoned").ever_loaded
    }

    /// Call a Lua function by name with no arguments, returning a string result.
    ///
    /// On error, logs and returns `None` (fallback to Rust behavior).
    pub fn call_string(&self, func_name: &str) -> Option<String> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return None;
        }

        let globals = state.lua.globals();
        match globals.get::<Value>(func_name) {
            Ok(Value::Function(f)) => match f.call::<String>(()) {
                Ok(s) => Some(s),
                Err(e) => {
                    crate::log!("Lua call {}() failed: {}", func_name, e);
                    None
                }
            },
            Ok(Value::Nil) => None, // Function not defined, use default.
            Ok(_) => {
                crate::log!("Lua {}: expected function, got other type", func_name);
                None
            }
            Err(e) => {
                crate::log!("Lua get {}: {}", func_name, e);
                None
            }
        }
    }

    /// Call a Lua function that returns a table, converting to JSON Value.
    ///
    /// Useful for getting structured config (e.g., keybinds, theme overrides).
    pub fn call_table(&self, func_name: &str) -> Option<serde_json::Value> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return None;
        }

        let globals = state.lua.globals();
        match globals.get::<Value>(func_name) {
            Ok(Value::Function(f)) => match f.call::<Value>(()) {
                Ok(v) => lua_value_to_json(&v),
                Err(e) => {
                    crate::log!("Lua call {}() failed: {}", func_name, e);
                    None
                }
            },
            Ok(Value::Table(t)) => lua_value_to_json(&Value::Table(t)),
            Ok(Value::Nil) => None,
            Ok(_) => {
                crate::log!("Lua {}: expected function or table", func_name);
                None
            }
            Err(e) => {
                crate::log!("Lua get {}: {}", func_name, e);
                None
            }
        }
    }

    /// Get a global value directly (for simple config values).
    pub fn get_global<T: mlua::FromLua>(&self, name: &str) -> Option<T> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return None;
        }

        state.lua.globals().get::<T>(name).ok()
    }

    /// Process pending panel registrations from Lua.
    pub fn process_panels(&self, registry: &mut panels::PanelRegistry) {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return;
        }

        if let Err(e) = panels::process_pending_panels(&state.lua, registry) {
            crate::log!("Failed to process pending panels: {}", e);
        }

        if let Err(e) = panels::process_panel_commands(&state.lua, registry) {
            crate::log!("Failed to process panel commands: {}", e);
        }
    }

    /// Apply queued `kn9t.map` / `kn9t.unmap` registrations.
    ///
    /// Returns the number of bindings applied. Called after (re)loading config.
    pub fn drain_keymaps(&self, registry: &mut keymap::KeymapRegistry) -> usize {
        let state =safe_expect!(self.inner.read(), "poisoned");
        match keymap::drain_pending_maps(&state.lua, registry) {
            Ok(n) => {
                if n > 0 {
                    crate::log!(
                        "Lua keymaps: {} applied ({} total bound)",
                        n,
                        registry.len()
                    );
                }
                n
            }
            Err(e) => {
                crate::log!("Failed to drain Lua keymaps: {}", e);
                0
            }
        }
    }

    /// Dispatch `key` to its Lua handler. Returns true if the key was consumed.
    pub fn dispatch_keymap(&self, registry: &keymap::KeymapRegistry, key: &str) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        registry.dispatch(&state.lua, key)
    }

    /// Apply queued `kn9t.on_click` / `kn9t.remove_click` registrations.
    ///
    /// Returns the number of handlers applied. Called after (re)loading config,
    /// same as `drain_keymaps`.
    pub fn drain_clicks(&self, registry: &mut click::ClickRegistry) -> usize {
        let state =safe_expect!(self.inner.read(), "poisoned");
        match click::drain_pending_clicks(&state.lua, registry) {
            Ok(n) => n,
            Err(e) => {
                crate::log!("Failed to drain Lua click handlers: {}", e);
                0
            }
        }
    }

    /// Dispatch a click at rect-local `(x, y)` to the handler bound to `id`.
    /// Returns true if the click was consumed.
    pub fn dispatch_click(
        &self,
        registry: &click::ClickRegistry,
        id: &str,
        x: u16,
        y: u16,
        button: &str,
    ) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        registry.dispatch(&state.lua, id, x, y, button)
    }

    /// Apply queued `kn9t.register_command` / `kn9t.unregister_command` calls.
    ///
    /// Returns the number of commands applied. Called after (re)loading
    /// config, same cadence as `drain_keymaps`/`drain_clicks`.
    pub fn drain_commands(&self, registry: &mut commands::LuaCommandRegistry) -> usize {
        let state =safe_expect!(self.inner.read(), "poisoned");
        match commands::drain_pending_commands(&state.lua, registry) {
            Ok(n) => n,
            Err(e) => {
                crate::log!("Failed to drain Lua commands: {}", e);
                0
            }
        }
    }

    /// Run a Lua-registered command's handler with the given argument string.
    pub fn run_lua_command(&self, cmd: &commands::LuaCommand, args: &str) {
        let state =safe_expect!(self.inner.read(), "poisoned");
        cmd.run(&state.lua, args);
    }

    /// Drain built-in actions requested via `kn9t.action(...)`.
    /// Drain queued `kn9t.action(...)` calls as `(Action, arg)` pairs.
    ///
    /// `arg` is `None` for the ~44 param-less actions and `Some(id)` for e.g.
    /// `switch_session`. Kept as a pair rather than folding `arg` into the enum
    /// so `Action` stays `Copy` (a `HashMap<KeyPattern, Action>` entry for a
    /// real keypress never has an argument to carry).
    pub fn drain_lua_actions(&self) -> Vec<(crate::keybind::Action, Option<String>)> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        keymap::drain_pending_actions(&state.lua)
            .into_iter()
            .filter_map(|(name, arg)| crate::keybind::parse_action(&name).map(|a| (a, arg)))
            .collect()
    }

    /// Build a widget tree for a panel.
    pub fn build_panel_widget(&self, panel: &panels::Panel) -> Option<widgets::Widget> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        panel.build_widget(&state.lua)
    }

    /// Build the ROOT widget tree for the whole screen via `render_ui(width, height)`.
    ///
    /// When Lua defines `render_ui`, it owns the entire frame: layout, panels and
    /// placement of native views (`{type="native", view="transcript"}`).
    ///
    /// The three outcomes are distinguished on purpose (see [`UiOutcome`]): a
    /// *broken* config must not silently fall back to the full Rust chrome, or
    /// the user sees a working-looking TUI and never learns their Lua is wrong.
    pub fn build_ui_outcome(&self, width: u16, height: u16) -> widgets::UiOutcome {
        use widgets::UiOutcome;

        let state =safe_expect!(self.inner.read(), "poisoned");
        // Check the load error *before* `ever_loaded`: when the very first load
        // fails, `ever_loaded` is still false, and treating that as "no Lua UI"
        // is what made a broken config silently show the full Rust chrome.
        if let Some(err) = state.last_error.clone() {
            return UiOutcome::Failed(err);
        }
        if !state.ever_loaded {
            return UiOutcome::NotDefined;
        }

        // Memoization: `render_ui` used to run unconditionally on every redraw
        // (10 FPS while streaming). The fingerprint is built from the same
        // `kn9t.state`/`kn9t.context` tables Lua itself reads, so "did anything
        // Lua could see change" is answered without calling into Lua at all on
        // a cache hit. `kn9t.invalidate()` (bumps `kn9t._epoch`) is the escape
        // hatch for Lua-local state this fingerprint cannot see.
        let fp = read_ui_fingerprint(&state.lua, width, height);
        {
            let cache =safe_expect!(self.ui_cache.read(), "poisoned");
            if let Some((cached_fp, cached_widget)) = cache.as_ref() {
                if *cached_fp == fp {
                    return UiOutcome::Ok(cached_widget.clone());
                }
            }
        }

        let func: mlua::Function = match state.lua.globals().get("render_ui") {
            Ok(f) => f,
            Err(_) => return UiOutcome::NotDefined,
        };
        let table: mlua::Table = match func.call((width, height)) {
            Ok(t) => t,
            Err(e) => {
                crate::log!("Lua render_ui error: {}", e);
                return UiOutcome::Failed(format!("render_ui: {e}"));
            }
        };
        // The root must name its type. Nested nodes may default to "text", but
        // a typeless root means the config returned something unintended, and
        // defaulting it would silently paint an empty screen.
        if table.get::<Option<String>>("type").ok().flatten().is_none() {
            return UiOutcome::Failed(
                "render_ui must return a widget with a `type` field".to_string(),
            );
        }
        match widgets::parse_widget(&state.lua, &table) {
            Ok(w) => {
                *safe_expect!(self.ui_cache.write(), "poisoned") = Some((fp, w.clone()));
                UiOutcome::Ok(w)
            }
            Err(e) => {
                crate::log!("Lua render_ui parse error: {}", e);
                UiOutcome::Failed(format!("render_ui returned an invalid tree: {e}"))
            }
        }
    }

    /// Handle a key event for the focused panel.
    /// Returns true if the key was handled.
    pub fn handle_panel_key(
        &self,
        registry: &panels::PanelRegistry,
        key: &str,
        modifiers: &str,
    ) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        registry.handle_key(&state.lua, key, modifiers)
    }

    /// Apply a plugin's TUI request: load its Lua, push state, or drop it.
    ///
    /// Failures are logged and recorded against the plugin rather than
    /// propagated: one broken plugin must not break the frame.
    pub fn apply_plugin_lua_op(&self, op: &crate::reducer::PluginLuaOp) {
        use crate::reducer::PluginLuaOp;
        let mut state =safe_expect!(self.inner.write(), "poisoned");
        // Split the borrow: `plugin_ui` needs `&Lua` while being mutated.
        let LuaState { lua, plugin_ui, .. } = &mut *state;
        match op {
            PluginLuaOp::Register {
                plugin,
                source,
                placement,
            } => {
                plugin_ui.register(lua, plugin, source, placement.clone());
            }
            PluginLuaOp::SetState { plugin, state } => {
                plugin_ui.set_state(lua, plugin, state);
            }
            PluginLuaOp::Clear { plugin } => {
                plugin_ui.remove(lua, plugin);
            }
        }
    }

    /// Names of plugins with a registered UI, in stable order.
    pub fn plugin_view_names(&self) -> Vec<String> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state
            .plugin_ui
            .names()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// `(name, placement)` for every registered view, in stable order.
    pub fn plugin_views(&self) -> Vec<(String, crate::reducer::PluginPlacement)> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state
            .plugin_ui
            .views()
            .into_iter()
            .map(|(name, p)| (name.to_string(), p.clone()))
            .collect()
    }

    /// Build one plugin's widget tree, or the error to display in its place.
    pub fn build_plugin_view(&self, plugin: &str) -> Result<widgets::Widget, String> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state.plugin_ui.build(&state.lua, plugin)
    }

    /// Dispatch a click at rect-local `(x, y)` to `plugin`'s handler for `id`.
    pub fn dispatch_plugin_click(
        &self,
        plugin: &str,
        id: &str,
        local_x: u16,
        local_y: u16,
        button: &str,
    ) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state
            .plugin_ui
            .dispatch_click(&state.lua, plugin, id, local_x, local_y, button)
    }

    /// Dispatch `key` to `plugin`'s handler. Returns true if consumed.
    pub fn dispatch_plugin_key(&self, plugin: &str, key: &str) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state.plugin_ui.dispatch_key(&state.lua, plugin, key)
    }

    /// Whether `plugin` binds `key` at all (without invoking the handler).
    pub fn plugin_has_key(&self, plugin: &str, key: &str) -> bool {
        let state =safe_expect!(self.inner.read(), "poisoned");
        state.plugin_ui.has_key(&state.lua, plugin, key)
    }

    /// Force a UI rebuild next frame.
    ///
    /// The Rust-side equivalent of `kn9t.invalidate()`: bumps the same epoch the
    /// render fingerprint folds in. Needed when host code changes something the
    /// fingerprint cannot observe — plugin focus, or a plugin's Lua-local state
    /// after one of its handlers ran.
    pub fn invalidate_ui(&self) {
        let state =safe_expect!(self.inner.read(), "poisoned");
        let Ok(kn9t) = state.lua.globals().get::<mlua::Table>("kn9t") else {
            return;
        };
        let epoch: i64 = kn9t.get("_epoch").unwrap_or(0);
        let _ = kn9t.set("_epoch", epoch + 1);
    }

    /// Drain side effects queued by plugin view handlers.
    pub fn drain_plugin_effects(&self) -> Vec<plugin_ui::PluginEffect> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        plugin_ui::drain_effects(&state.lua)
    }

    /// Update context stats for Lua status bar customization.
    pub fn update_context(&self, stats: &context::ContextStats) {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if let Err(e) = context::update_context(&state.lua, stats) {
            crate::log!("Failed to update context: {}", e);
        }
    }

    /// Publish the cheap per-frame snapshot to `kn9t.state`.
    ///
    /// Takes a pre-collected snapshot rather than `&App` so the caller controls
    /// how often the (bounded but non-free) collection happens.
    pub fn update_state(&self, snap: &state::StateSnapshot) {
        let lua_state =safe_expect!(self.inner.read(), "poisoned");
        if let Err(e) = state::update_state(&lua_state.lua, snap) {
            crate::log!("Failed to update Lua state: {}", e);
        }
    }

    /// Build the status line from Lua's `render_status()`.
    ///
    /// Returns the same [`widgets::TextSpan`] vocabulary as the widget tree, so
    /// a status segment supports every style a `text` widget does instead of a
    /// parallel colour-only type.
    ///
    /// Lua may return either a plain string or an array of segments:
    /// `{ {text="ctx ", fg="gray"}, {text="87%", fg="lightred", bold=true} }`.
    /// `color=` is accepted as a synonym of `fg=` for configs written against
    /// the older shape.
    pub fn call_status_spans(&self) -> Option<Vec<widgets::TextSpan>> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return None;
        }

        let func: mlua::Function = match state.lua.globals().get("render_status") {
            Ok(f) => f,
            Err(_) => return None,
        };

        match func.call::<Value>(()) {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| {
                vec![widgets::TextSpan {
                    text: s.to_string(),
                    style: widgets::WidgetStyle::default(),
                    syntax: None,
                }]
            }),
            Ok(Value::Table(t)) => {
                let spans = widgets::parse_status_spans(&t);
                // An empty table means "nothing to show", which is a valid
                // choice; only a non-table result falls back to Rust.
                Some(spans)
            }
            Ok(_) => None,
            Err(e) => {
                crate::log!("Lua render_status error: {}", e);
                None
            }
        }
    }

    /// Evaluate a Lua expression returning a boolean.
    ///
    /// For tests and diagnostics that need to assert against the live sandbox —
    /// notably the API-contract test, which checks that every documented global
    /// is actually present rather than merely implemented somewhere.
    pub fn eval_bool(&self, src: &str) -> Option<bool> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        match state.lua.load(src).eval::<bool>() {
            Ok(v) => Some(v),
            Err(e) => {
                crate::log!("Lua eval_bool failed: {}", e);
                None
            }
        }
    }

    /// Ask Lua how a tool's card should render, via `tool_mode(name)`.
    ///
    /// Returns the mode name (`diff`/`output`/`summary`/`streaming`), or `None`
    /// to use the built-in mapping. This is the seam that lets a plugin's tool
    /// pick a renderer: the choice used to be a hardcoded `match` on tool name
    /// inside `render.rs`, so only the four built-in tools could be styled.
    ///
    /// Called once per visible card per frame, so it must stay a cheap Lua call
    /// — a table lookup, not a computation.
    pub fn call_tool_mode(&self, tool: &str) -> Option<String> {
        let state =safe_expect!(self.inner.read(), "poisoned");
        if !state.ever_loaded {
            return None;
        }
        let func: mlua::Function = state.lua.globals().get("tool_mode").ok()?;
        match func.call::<Option<String>>(tool) {
            Ok(v) => v,
            Err(e) => {
                crate::log!("Lua tool_mode({}) failed: {}", tool, e);
                None
            }
        }
    }
}

impl Default for LuaRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create Lua runtime")
    }
}

/// Build the fingerprint `build_ui_outcome` uses to decide whether it can
/// reuse last frame's widget tree instead of calling into Lua.
///
/// Reads the exact tables `state::update_state`/`context::update_context`
/// already populated for Lua's own use — no separate bookkeeping to keep in
/// sync with what those functions publish. A field that isn't read here
/// simply isn't part of the "may-differ" check; when in doubt, add the field
/// rather than assume it can't affect the tree.
fn read_ui_fingerprint(lua: &Lua, width: u16, height: u16) -> UiFingerprint {
    let get = |kn9t: &mlua::Table, path: &[&str]| -> Option<mlua::Value> {
        let mut v: mlua::Value = mlua::Value::Table(kn9t.clone());
        for key in path {
            let t = v.as_table()?;
            v = t.get::<mlua::Value>(*key).ok()?;
        }
        Some(v)
    };
    let as_usize = |v: Option<mlua::Value>| -> usize { v.and_then(|v| v.as_usize()).unwrap_or(0) };
    let as_bool =
        |v: Option<mlua::Value>| -> bool { v.and_then(|v| v.as_boolean()).unwrap_or(false) };
    let as_f64 = |v: Option<mlua::Value>| -> f64 { v.and_then(|v| v.as_f64()).unwrap_or(0.0) };

    let Ok(kn9t) = lua.globals().get::<mlua::Table>("kn9t") else {
        return UiFingerprint {
            width,
            height,
            ..Default::default()
        };
    };

    let cost = as_f64(get(&kn9t, &["state", "usage", "cost"]));

    UiFingerprint {
        width,
        height,
        message_count: as_usize(get(&kn9t, &["state", "message_count"])),
        tool_count: as_usize(get(&kn9t, &["context", "tool_count"])),
        scroll: as_usize(get(&kn9t, &["state", "scroll"])),
        streaming: as_bool(get(&kn9t, &["state", "session", "streaming"])),
        aborting: as_bool(get(&kn9t, &["state", "session", "aborting"])),
        cost_centicents: (cost * 10_000.0).round() as i64,
        turn_input: as_usize(get(&kn9t, &["state", "usage", "turn", "input"])),
        turn_output: as_usize(get(&kn9t, &["state", "usage", "turn", "output"])),
        explorer_visible: as_bool(get(&kn9t, &["state", "explorer_visible"])),
        viewer_open: as_bool(get(&kn9t, &["state", "viewer_open"])),
        manual_epoch: keymap::read_epoch(lua),
    }
}

/// Convert a Lua value to a JSON value.
pub fn lua_value_to_json(v: &Value) -> Option<serde_json::Value> {
    match v {
        Value::Nil => Some(serde_json::Value::Null),
        Value::Boolean(b) => Some(serde_json::Value::Bool(*b)),
        Value::Integer(i) => Some(serde_json::Value::Number((*i).into())),
        Value::Number(n) => serde_json::Number::from_f64(*n).map(serde_json::Value::Number),
        Value::String(s) => s
            .to_str()
            .ok()
            .map(|s| serde_json::Value::String(s.to_string())),
        Value::Table(t) => {
            // Check if it's an array (sequential integer keys starting at 1).
            let mut is_array = true;
            let mut max_key = 0i64;
            for pair in t.clone().pairs::<Value, Value>() {
                if let Ok((Value::Integer(k), _)) = pair {
                    if k > max_key {
                        max_key = k;
                    }
                } else {
                    is_array = false;
                    break;
                }
            }

            if is_array && max_key > 0 {
                let mut arr = Vec::with_capacity(max_key as usize);
                for i in 1..=max_key {
                    if let Ok(v) = t.get::<Value>(i) {
                        if let Some(jv) = lua_value_to_json(&v) {
                            arr.push(jv);
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                Some(serde_json::Value::Array(arr))
            } else {
                let mut map = serde_json::Map::new();
                for (k, v) in t.clone().pairs::<Value, Value>().flatten() {
                    let key = match &k {
                        Value::String(s) => s.to_str().ok()?.to_string(),
                        Value::Integer(i) => i.to_string(),
                        _ => return None,
                    };
                    if let Some(jv) = lua_value_to_json(&v) {
                        map.insert(key, jv);
                    } else {
                        return None;
                    }
                }
                Some(serde_json::Value::Object(map))
            }
        }
        _ => None, // Function, userdata, etc. not convertible.
    }
}

/// Get the default Lua config path (~/.kn9t/tui.lua).
pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let mut path = PathBuf::from(home);
    path.push(".kn9t");
    path.push("tui.lua");
    Some(path)
}

/// Directory of split config files: `~/.kn9t/tui/`.
///
/// Sibling of [`default_config_path`]'s single `tui.lua` — see
/// `ConfigSource::resolve` for how the two are reconciled when both exist.
pub fn default_config_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let mut path = PathBuf::from(home);
    path.push(".kn9t");
    path.push("tui");
    Some(path)
}

