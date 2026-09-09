//! Expose TUI state to Lua — session, usage, transcript summaries.
//!
//! Lua reads this to build its own UI; Rust no longer imposes any layout.
//!
//! # Performance
//!
//! `update_state` runs on *every frame* (and every streaming token), so it must
//! stay O(cheap). Deep-copying the transcript here cost ~3.6ms and ~2.5MB of
//! table churn per frame on a 200-message session, which is what made the Lua
//! TUI feel sluggish.
//!
//! So the split is:
//! - **eager**: scalars and small summaries Lua reads every frame.
//! - **lazy**: message bodies and tool output, fetched through functions
//!   (`kn9t.get_messages`, `kn9t.get_tools`) only when Lua actually asks.
//!
//! Anything unbounded belongs behind a function, not in the eager table.

use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table, Value};

use crate::app::App;
use crate::message_handler::ToolCard;

/// Cheap per-frame snapshot. Bounded size regardless of transcript length.
#[derive(Debug, Clone, Default)]
pub struct StateSnapshot {
    pub session_id: String,
    pub title: String,
    pub streaming: bool,
    pub aborting: bool,
    pub has_lease: bool,
    pub last_seq: u64,

    pub turn_input: usize,
    pub turn_output: usize,
    pub turn_cache_read: usize,
    pub turn_cache_write: usize,
    pub total_input: usize,
    pub total_output: usize,
    pub total_cache_read: usize,
    pub total_cache_write: usize,
    pub cost: f64,
    pub toks_per_sec: f64,

    pub scroll: usize,
    pub message_count: usize,
    /// Most recent tool calls, newest first — bounded by `RECENT_TOOL_LIMIT`.
    pub recent_tools: Vec<RecentTool>,
    /// All sessions known to this client, for a Lua-drawn session list.
    ///
    /// Already an in-memory cache (`SessionManager::sessions`, populated by
    /// `load_sessions`/create/delete) — publishing it costs nothing extra, no
    /// new HTTP call. Unlike `recent_tools` this is not separately bounded:
    /// the existing cache already is (it's "sessions this server told us
    /// about", not "sessions ever created").
    pub sessions: Vec<SessionSummary>,
    /// Plugins with a registered Lua UI, in stable order.
    ///
    /// Lets a config discover plugin views without hardcoding names, so a
    /// newly loaded plugin appears without editing `tui.lua`.
    pub plugin_views: Vec<String>,
    /// Same views with their declared placement, so a config can route by zone
    /// (`"sidebar"`/`"main"`/`"status"`) instead of matching plugin names.
    ///
    /// Kept alongside `plugin_views` rather than replacing it: the bare-name
    /// list is the simple case (draw everything somewhere) and a config that
    /// only needs that should not have to destructure entries.
    pub plugin_view_specs: Vec<PluginViewSpec>,
    /// Which plugin view currently has keyboard focus, empty when none.
    ///
    /// Published so a config can render focus affordances (a highlighted border
    /// on the focused panel) instead of guessing where keys are going.
    pub focused_plugin: String,
}

/// One plugin view as Lua sees it: its id plus whatever layout it asked for.
#[derive(Debug, Clone, Default)]
pub struct PluginViewSpec {
    /// Plugin name — the id to pass to `{type="plugin", plugin=...}`.
    pub name: String,
    /// `"sidebar"` | `"main"` | `"status"`, or empty when unspecified.
    pub placement: String,
    /// Display title; falls back to `name` when the plugin gave none, so a
    /// config can always render a heading without a nil check.
    pub title: String,
    /// Preferred size, 0 when unspecified.
    pub rows: u16,
    pub cols: u16,
}

/// A tool call reduced to what a sidebar needs. No args, no output.
#[derive(Debug, Clone)]
pub struct RecentTool {
    pub name: String,
    pub status: String,
}

/// A session reduced to what a sidebar list needs to display and to pass back
/// to `kn9t.action("switch_session", id)`.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub is_current: bool,
}

/// How many recent tool calls to publish eagerly. Keeps cost O(1) per frame
/// no matter how long the session runs.
pub const RECENT_TOOL_LIMIT: usize = 24;

impl StateSnapshot {
    /// Collect from the app. Walks messages back-to-front and stops as soon as
    /// the recent-tool budget is filled, so cost does not grow with history.
    pub fn collect(app: &App) -> Self {
        let messages = app.transcript.messages();

        let mut recent_tools = Vec::with_capacity(RECENT_TOOL_LIMIT);
        'outer: for msg in messages.iter().rev() {
            for card in msg.tools.iter().rev() {
                recent_tools.push(RecentTool {
                    name: card.name.clone(),
                    status: card.status.clone(),
                });
                if recent_tools.len() >= RECENT_TOOL_LIMIT {
                    break 'outer;
                }
            }
        }

        Self {
            session_id: app.session.state.session_id.clone(),
            title: app.session.session_title().unwrap_or("").to_string(),
            streaming: app.streaming,
            aborting: app.aborting,
            has_lease: app.session.state.lease.is_some(),
            last_seq: app.session.state.last_seq,

            turn_input: app.tokens.last_turn_input(),
            turn_output: app.tokens.last_turn_output(),
            turn_cache_read: app.tokens.last_turn_cache_read(),
            turn_cache_write: app.tokens.last_turn_cache_write(),
            total_input: app.tokens.tokens_in(),
            total_output: app.tokens.tokens_out(),
            total_cache_read: app.tokens.cache_read(),
            total_cache_write: app.tokens.cache_write(),
            cost: app.tokens.cost,
            toks_per_sec: app.tokens.last_toks_per_sec.unwrap_or(0.0),

            scroll: app.transcript.scroll(),
            message_count: messages.len(),
            recent_tools,
            sessions: app
                .session
                .sessions
                .iter()
                .map(|s| SessionSummary {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    is_current: s.id == app.session.state.session_id,
                })
                .collect(),
            plugin_views: app
                .lua_runtime
                .as_ref()
                .map(|rt| rt.plugin_view_names())
                .unwrap_or_default(),
            plugin_view_specs: app
                .lua_runtime
                .as_ref()
                .map(|rt| {
                    rt.plugin_views()
                        .into_iter()
                        .map(|(name, p)| PluginViewSpec {
                            // Title defaults to the name so Lua never has to
                            // handle a missing heading.
                            title: p.title.clone().unwrap_or_else(|| name.clone()),
                            placement: p.zone.clone().unwrap_or_default(),
                            rows: p.rows.unwrap_or(0),
                            cols: p.cols.unwrap_or(0),
                            name,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            focused_plugin: app.focused_plugin.clone().unwrap_or_default(),
        }
    }
}

/// Publish the cheap snapshot to `kn9t.state`.
pub fn update_state(lua: &Lua, snap: &StateSnapshot) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = match globals.get("kn9t") {
        Ok(t) => t,
        Err(_) => lua.create_table()?,
    };

    let state = lua.create_table()?;

    let session = lua.create_table()?;
    session.set("id", snap.session_id.as_str())?;
    session.set("title", snap.title.as_str())?;
    session.set("streaming", snap.streaming)?;
    session.set("aborting", snap.aborting)?;
    session.set("has_lease", snap.has_lease)?;
    session.set("last_seq", snap.last_seq)?;
    state.set("session", session)?;

    let usage = lua.create_table()?;
    let turn = lua.create_table()?;
    turn.set("input", snap.turn_input)?;
    turn.set("output", snap.turn_output)?;
    turn.set("cache_read", snap.turn_cache_read)?;
    turn.set("cache_write", snap.turn_cache_write)?;
    usage.set("turn", turn)?;
    let total = lua.create_table()?;
    total.set("input", snap.total_input)?;
    total.set("output", snap.total_output)?;
    total.set("cache_read", snap.total_cache_read)?;
    total.set("cache_write", snap.total_cache_write)?;
    usage.set("total", total)?;
    usage.set("cost", snap.cost)?;
    usage.set("toks_per_sec", snap.toks_per_sec)?;
    state.set("usage", usage)?;

    state.set("scroll", snap.scroll)?;
    state.set("message_count", snap.message_count)?;

    // Bounded list: name + status only, newest first.
    let recent = lua.create_table()?;
    for (i, t) in snap.recent_tools.iter().enumerate() {
        let e = lua.create_table()?;
        e.set("name", t.name.as_str())?;
        e.set("status", t.status.as_str())?;
        recent.set(i + 1, e)?;
    }
    state.set("recent_tools", recent)?;

    // Already an in-memory cache (SessionManager::sessions); publishing costs
    // no new HTTP round-trip. `is_current` is precomputed here rather than
    // making Lua compare against `state.session.id` itself, since string
    // comparison-by-convention is exactly the kind of thing that silently
    // drifts if the id field is ever renamed on one side only.
    let sessions = lua.create_table()?;
    for (i, s) in snap.sessions.iter().enumerate() {
        let e = lua.create_table()?;
        e.set("id", s.id.as_str())?;
        e.set("name", s.name.as_str())?;
        e.set("is_current", s.is_current)?;
        sessions.set(i + 1, e)?;
    }
    state.set("sessions", sessions)?;

    // Names only: the widget tree itself is built lazily at render time, so
    // listing views costs nothing per frame.
    let views = lua.create_table()?;
    for (i, name) in snap.plugin_views.iter().enumerate() {
        views.set(i + 1, name.as_str())?;
    }
    state.set("plugin_views", views)?;

    // Structured form: `{ {name=, placement=, title=, rows=, cols=}, ... }`.
    let specs = lua.create_table()?;
    for (i, spec) in snap.plugin_view_specs.iter().enumerate() {
        let t = lua.create_table()?;
        t.set("name", spec.name.as_str())?;
        t.set("placement", spec.placement.as_str())?;
        t.set("title", spec.title.as_str())?;
        t.set("rows", spec.rows)?;
        t.set("cols", spec.cols)?;
        specs.set(i + 1, t)?;
    }
    state.set("plugin_view_specs", specs)?;

    state.set("focused_plugin", snap.focused_plugin.as_str())?;

    kn9t.set("state", state)?;
    globals.set("kn9t", kn9t)?;

    Ok(())
}

/// Publish `kn9t.theme` and `kn9t.native_views`.
///
/// Installed once per (re)load rather than per frame: neither changes while the
/// UI runs, and a ricer needs them to reference palette colours by name instead
/// of hardcoding hex that then fights the configured theme.
pub fn install_environment(lua: &Lua, theme: &crate::theme::Theme) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = match globals.get("kn9t") {
        Ok(t) => t,
        Err(_) => lua.create_table()?,
    };

    let theme_table = lua.create_table()?;
    for name in crate::theme::Theme::NAMES {
        if let Some(color) = theme.get(name) {
            // Published as strings so the value can be handed straight back as a
            // widget `fg=`, which is what makes the palette composable.
            theme_table.set(*name, crate::theme::color_to_string(color))?;
        }
    }
    kn9t.set("theme", theme_table)?;

    let views = lua.create_table()?;
    for (i, v) in crate::lua::widgets::NATIVE_VIEWS.iter().enumerate() {
        views.set(i + 1, *v)?;
    }
    kn9t.set("native_views", views)?;

    globals.set("kn9t", kn9t)?;
    Ok(())
}

/// Data source for the lazy accessors, shared with the Lua closures.
#[derive(Default)]
pub struct LazyData {
    pub messages: Vec<LazyMessage>,
    pub tools: Vec<LazyTool>,
}

impl LazyData {
    /// Build from the app's current transcript and tool list.
    ///
    /// Called only on a version miss, never per frame.
    pub fn collect(app: &App) -> Self {
        Self {
            messages: app
                .transcript
                .messages()
                .iter()
                .map(|m| LazyMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                    tools: m.tools.clone(),
                })
                .collect(),
            tools: app
                .tools
                .iter()
                .map(|t| LazyTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    // Built-in tools have no owning plugin; "" reads better in
                    // Lua than a nil a config would have to guard on.
                    plugin: t.plugin.clone().unwrap_or_default(),
                    enabled: t.enabled,
                })
                .collect(),
        }
    }

    /// Fingerprint deciding when the store is rebuilt.
    ///
    /// Deliberately cheap: message/tool counts plus the length and status of the
    /// tail message. Hashing every body would cost as much as the copy it is
    /// meant to avoid. Streaming text mutates the last message, which the tail
    /// length captures.
    pub fn version(app: &App) -> u64 {
        let msgs = app.transcript.messages();
        let mut v = 1u64; // never 0: that means "never published"
        v = v.wrapping_mul(1000003).wrapping_add(msgs.len() as u64);
        v = v.wrapping_mul(1000003).wrapping_add(app.tools.len() as u64);
        if let Some(last) = msgs.last() {
            v = v
                .wrapping_mul(1000003)
                .wrapping_add(last.content.len() as u64);
            v = v
                .wrapping_mul(1000003)
                .wrapping_add(last.tools.len() as u64);
            for card in &last.tools {
                v = v.wrapping_mul(31).wrapping_add(card.status.len() as u64);
                v = v
                    .wrapping_mul(31)
                    .wrapping_add(card.output.as_ref().map_or(0, |o| o.len()) as u64);
            }
        }
        v
    }
}

pub struct LazyMessage {
    pub role: String,
    pub content: String,
    pub tools: Vec<ToolCard>,
}

pub struct LazyTool {
    pub name: String,
    pub description: String,
    pub plugin: String,
    pub enabled: bool,
}

/// Shared, mutable backing store for the lazy accessors.
///
/// The accessors are installed **once** and read through this handle, so
/// `kn9t.get_messages` is a live function rather than something re-installed per
/// frame. The store is refreshed only when the transcript actually changes
/// (see [`LazyStore::refresh_if_stale`]): copying message bodies every frame is
/// what cost ~3.5 ms/frame before `StateSnapshot` existed, and doing it again
/// here would undo that.
#[derive(Default)]
pub struct LazyStore {
    inner: std::sync::RwLock<LazyData>,
    /// Cheap fingerprint of the last publish; a mismatch means "rebuild".
    version: std::sync::atomic::AtomicU64,
}

impl LazyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the store when `version` differs from the last publish.
    ///
    /// `build` is only invoked on a miss, so an idle frame costs one atomic load.
    pub fn refresh_if_stale(&self, version: u64, build: impl FnOnce() -> LazyData) {
        use std::sync::atomic::Ordering;
        // Version 0 is reserved for "never published", so a genuinely empty
        // transcript still populates the store once.
        if self.version.load(Ordering::Relaxed) == version && version != 0 {
            return;
        }
        if let Ok(mut guard) = self.inner.write() {
            *guard = build();
            self.version.store(version, Ordering::Relaxed);
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, LazyData> {
        // A poisoned lock here would mean a panic inside a Lua callback; the
        // stale data is strictly better than taking the TUI down for it.
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }
}

/// Install `kn9t.get_messages(from, to)` and `kn9t.get_tools()`.
///
/// These build tables only when called, so a config that never touches message
/// bodies costs nothing per frame.
pub fn install_lazy_accessors(lua: &Lua, store: Arc<LazyStore>) -> LuaResult<()> {
    let globals = lua.globals();
    let kn9t: Table = match globals.get("kn9t") {
        Ok(t) => t,
        Err(_) => lua.create_table()?,
    };

    // kn9t.get_messages([from], [to]) -> array, 1-based inclusive slice.
    // Defaults to the whole transcript; callers are expected to window it.
    let d = store.clone();
    let get_messages =
        lua.create_function(move |lua, (from, to): (Option<usize>, Option<usize>)| {
            let d = d.read();
            let len = d.messages.len();
            let from = from.unwrap_or(1).max(1);
            let to = to.unwrap_or(len).min(len);
            let out = lua.create_table()?;
            if from > to {
                return Ok(out);
            }
            for (n, msg) in d.messages[from - 1..to].iter().enumerate() {
                let m = lua.create_table()?;
                m.set("role", msg.role.as_str())?;
                m.set("content", msg.content.as_str())?;
                if !msg.tools.is_empty() {
                    m.set("tools", tool_cards_to_lua(lua, &msg.tools)?)?;
                }
                out.set(n + 1, m)?;
            }
            Ok(out)
        })?;
    kn9t.set("get_messages", get_messages)?;

    let d = store.clone();
    let get_tools = lua.create_function(move |lua, ()| {
        let d = d.read();
        let out = lua.create_table()?;
        for (i, t) in d.tools.iter().enumerate() {
            let e = lua.create_table()?;
            e.set("name", t.name.as_str())?;
            e.set("description", t.description.as_str())?;
            e.set("plugin", t.plugin.as_str())?;
            e.set("enabled", t.enabled)?;
            out.set(i + 1, e)?;
        }
        Ok(out)
    })?;
    kn9t.set("get_tools", get_tools)?;

    globals.set("kn9t", kn9t)?;
    Ok(())
}

fn tool_cards_to_lua(lua: &Lua, cards: &[ToolCard]) -> LuaResult<Table> {
    let t = lua.create_table()?;
    for (i, card) in cards.iter().enumerate() {
        let c = lua.create_table()?;
        c.set("call_id", card.call_id.as_str())?;
        c.set("name", card.name.as_str())?;
        c.set("args", card.args.as_str())?;
        c.set("status", card.status.as_str())?;
        c.set("output", card.output.as_deref().unwrap_or(""))?;
        c.set("expanded", card.expanded)?;
        c.set(
            "active_tab",
            match card.active_tab {
                crate::message_handler::ToolTab::Progress => "progress",
                crate::message_handler::ToolTab::Output => "output",
                crate::message_handler::ToolTab::Input => "input",
            },
        )?;
        c.set("scroll_offset", card.scroll_offset)?;

        let progress = lua.create_table()?;
        for (j, line) in card.progress_lines.iter().enumerate() {
            progress.set(j + 1, line.as_str())?;
        }
        c.set("progress_lines", progress)?;

        t.set(i + 1, c)?;
    }
    Ok(t)
}

/// Read `kn9t.state.<path>` as a number, for tests and Rust-side probes.
pub fn state_number(lua: &Lua, path: &[&str]) -> Option<f64> {
    let mut cur: Value = lua.globals().get::<Table>("kn9t").ok()?.get("state").ok()?;
    for (i, key) in path.iter().enumerate() {
        let t = match cur {
            Value::Table(t) => t,
            _ => return None,
        };
        let next: Value = t.get(*key).ok()?;
        if i == path.len() - 1 {
            return match next {
                Value::Number(n) => Some(n),
                Value::Integer(n) => Some(n as f64),
                _ => None,
            };
        }
        cur = next;
    }
    None
}

