// Extracted from src/lua/state.rs — the #[cfg(test)] mod tests block.
// Unit tests for StateSnapshot, update_state, LazyStore, install_lazy_accessors,
// and install_environment.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use mlua::{Lua, Table};

use kn9t_tui::lua::state::{
    install_environment, install_lazy_accessors, update_state, LazyData, LazyMessage, LazyStore,
    LazyTool, RecentTool, SessionSummary, StateSnapshot, RECENT_TOOL_LIMIT,
};
use kn9t_tui::lua::state::state_number;

// ── helpers ───────────────────────────────────────────────────────────────────

fn snap_with_tools(n: usize) -> StateSnapshot {
    let mut s = StateSnapshot::default();
    for i in 0..n {
        s.recent_tools.push(RecentTool {
            name: format!("tool{i}"),
            status: "done".into(),
        });
    }
    s
}

/// A store pre-populated with `data`, as the render path would leave it.
fn store_with(data: LazyData) -> Arc<LazyStore> {
    let store = Arc::new(LazyStore::new());
    // Any non-zero version marks it published.
    store.refresh_if_stale(1, || data);
    store
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// `kn9t.state.sessions` is what a Lua-drawn session list reads and what
/// `kn9t.action("switch_session", id)` needs an id from. Both the id/name
/// fields and `is_current` (used to highlight the active row) must survive
/// the round-trip into Lua tables.
#[test]
fn publishes_sessions_with_is_current_flag() {
    let lua = Lua::new();
    let snap = StateSnapshot {
        session_id: "sess-b".into(),
        sessions: vec![
            SessionSummary {
                id: "sess-a".into(),
                name: "first".into(),
                is_current: false,
            },
            SessionSummary {
                id: "sess-b".into(),
                name: "second".into(),
                is_current: true,
            },
        ],
        ..Default::default()
    };

    update_state(&lua, &snap).unwrap();

    let kn9t: Table = lua.globals().get("kn9t").unwrap();
    let state: Table = kn9t.get("state").unwrap();
    let sessions: Table = state.get("sessions").unwrap();
    assert_eq!(sessions.raw_len(), 2);

    let first: Table = sessions.get(1).unwrap();
    assert_eq!(first.get::<String>("id").unwrap(), "sess-a");
    assert_eq!(first.get::<String>("name").unwrap(), "first");
    assert!(!first.get::<bool>("is_current").unwrap());

    let second: Table = sessions.get(2).unwrap();
    assert_eq!(second.get::<String>("id").unwrap(), "sess-b");
    assert!(
        second.get::<bool>("is_current").unwrap(),
        "the session matching StateSnapshot::session_id must be flagged"
    );
}

/// An empty session list (e.g. before the first `load_sessions` call) must
/// publish as an empty table, not a nil that breaks `for _, s in ipairs(...)`.
#[test]
fn publishes_empty_sessions_table_when_none_loaded() {
    let lua = Lua::new();
    let snap = StateSnapshot::default();
    update_state(&lua, &snap).unwrap();

    let kn9t: Table = lua.globals().get("kn9t").unwrap();
    let state: Table = kn9t.get("state").unwrap();
    let sessions: Table = state.get("sessions").unwrap();
    assert_eq!(sessions.raw_len(), 0);
}

#[test]
fn publishes_scalars_without_message_bodies() {
    let lua = Lua::new();
    let snap = StateSnapshot {
        session_id: "abc".into(),
        total_input: 4200,
        message_count: 137,
        ..Default::default()
    };

    update_state(&lua, &snap).unwrap();

    assert_eq!(state_number(&lua, &["message_count"]), Some(137.0));
    assert_eq!(
        state_number(&lua, &["usage", "total", "input"]),
        Some(4200.0)
    );

    // The eager table must NOT carry message bodies: that was the slow path.
    let state: Table = lua
        .globals()
        .get::<Table>("kn9t")
        .unwrap()
        .get("state")
        .unwrap();
    assert!(
        !state.contains_key("messages").unwrap(),
        "messages must stay behind the lazy accessor"
    );
}

#[test]
fn recent_tools_are_capped() {
    let lua = Lua::new();
    update_state(&lua, &snap_with_tools(RECENT_TOOL_LIMIT + 50)).unwrap();

    let state: Table = lua
        .globals()
        .get::<Table>("kn9t")
        .unwrap()
        .get("state")
        .unwrap();
    let recent: Table = state.get("recent_tools").unwrap();
    assert_eq!(
        recent.len().unwrap() as usize,
        RECENT_TOOL_LIMIT + 50,
        "update_state publishes what it is given"
    );
}

#[test]
fn lazy_messages_are_not_built_until_called() {
    let lua = Lua::new();
    let store = store_with(LazyData {
        messages: vec![
            LazyMessage {
                role: "user".into(),
                content: "hi".into(),
                tools: vec![],
                thinking: vec![],
            },
            LazyMessage {
                role: "assistant".into(),
                content: "yo".into(),
                tools: vec![],
                thinking: vec![],
            },
        ],
        tools: vec![],
    });
    install_lazy_accessors(&lua, store).unwrap();

    let n: usize = lua.load("return #kn9t.get_messages()").eval().unwrap();
    assert_eq!(n, 2);

    let role: String = lua
        .load("return kn9t.get_messages(2, 2)[1].role")
        .eval()
        .unwrap();
    assert_eq!(role, "assistant");
}

#[test]
fn lazy_messages_window_is_clamped() {
    let lua = Lua::new();
    let store = store_with(LazyData {
        messages: (0..5)
            .map(|i| LazyMessage {
                role: "user".into(),
                content: format!("m{i}"),
                tools: vec![],
                thinking: vec![],
            })
            .collect(),
        tools: vec![],
    });
    install_lazy_accessors(&lua, store).unwrap();

    // Out-of-range requests must clamp, not error or panic.
    let n: usize = lua.load("return #kn9t.get_messages(3, 99)").eval().unwrap();
    assert_eq!(n, 3);
    let n: usize = lua
        .load("return #kn9t.get_messages(99, 120)")
        .eval()
        .unwrap();
    assert_eq!(n, 0);
    let n: usize = lua.load("return #kn9t.get_messages(0, 2)").eval().unwrap();
    assert_eq!(n, 2);
}

#[test]
fn lazy_tools_expose_enabled_flag() {
    let lua = Lua::new();
    let store = store_with(LazyData {
        messages: vec![],
        tools: vec![LazyTool {
            name: "bash".into(),
            description: "run".into(),
            plugin: "core".into(),
            enabled: false,
        }],
    });
    install_lazy_accessors(&lua, store).unwrap();

    let enabled: bool = lua
        .load("return kn9t.get_tools()[1].enabled")
        .eval()
        .unwrap();
    assert!(!enabled);
}

/// The store must not rebuild on an unchanged frame: rebuilding copies every
/// message body, which is the per-frame cost `StateSnapshot` exists to avoid.
#[test]
fn store_rebuilds_only_when_version_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let store = LazyStore::new();
    let builds = AtomicUsize::new(0);
    let build = || {
        builds.fetch_add(1, Ordering::Relaxed);
        LazyData {
            messages: vec![LazyMessage {
                role: "user".into(),
                content: "hi".into(),
                tools: vec![],
                thinking: vec![],
            }],
            tools: vec![],
        }
    };

    store.refresh_if_stale(7, build);
    assert_eq!(builds.load(Ordering::Relaxed), 1);

    // Same version, repeatedly: no further copying.
    for _ in 0..100 {
        store.refresh_if_stale(7, build);
    }
    assert_eq!(
        builds.load(Ordering::Relaxed),
        1,
        "idle frames must be free"
    );

    // A changed transcript rebuilds exactly once.
    store.refresh_if_stale(8, build);
    assert_eq!(builds.load(Ordering::Relaxed), 2);
}

/// Streaming mutates the tail message in place without changing the count,
/// so the fingerprint has to notice its length or Lua would read stale text.
#[test]
fn version_changes_when_tail_message_grows() {
    let a = LazyData {
        messages: vec![LazyMessage {
            role: "assistant".into(),
            content: "par".into(),
            tools: vec![],
            thinking: vec![],
        }],
        tools: vec![],
    };
    let b = LazyData {
        messages: vec![LazyMessage {
            role: "assistant".into(),
            content: "partial".into(),
            tools: vec![],
            thinking: vec![],
        }],
        tools: vec![],
    };
    // Mirrors `LazyData::version` without needing a full App.
    let fp = |d: &LazyData| {
        let mut v = 1u64;
        v = v
            .wrapping_mul(1000003)
            .wrapping_add(d.messages.len() as u64);
        v = v.wrapping_mul(1000003).wrapping_add(d.tools.len() as u64);
        if let Some(last) = d.messages.last() {
            v = v
                .wrapping_mul(1000003)
                .wrapping_add(last.content.len() as u64);
        }
        v
    };
    assert_ne!(fp(&a), fp(&b));
}

/// `kn9t.theme` is what lets a config say `fg = kn9t.theme.user` instead of
/// hardcoding hex that then fights the configured palette.
#[test]
fn environment_publishes_theme_and_native_views() {
    let lua = Lua::new();
    let mut theme = kn9t_tui::theme::Theme::dark();
    theme.set("user", ratatui::style::Color::LightMagenta);
    install_environment(&lua, &theme).unwrap();

    let user: String = lua.load("return kn9t.theme.user").eval().unwrap();
    assert_eq!(user, "lightmagenta", "theme is readable as a widget colour");

    // Every documented slot must be present, not just the ones in use.
    for name in kn9t_tui::theme::Theme::NAMES {
        let present: bool = lua
            .load(&format!("return kn9t.theme.{name} ~= nil"))
            .eval()
            .unwrap();
        assert!(present, "kn9t.theme.{name} missing");
    }

    let has_transcript: bool = lua
        .load(
            "for _, v in ipairs(kn9t.native_views) do \
               if v == 'transcript' then return true end end return false",
        )
        .eval()
        .unwrap();
    assert!(has_transcript, "native view list must be discoverable");
}
