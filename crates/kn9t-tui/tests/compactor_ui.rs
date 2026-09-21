//! The `kn9t-compactor` plugin's Lua viewer, extracted verbatim from its
//! TypeScript source (`scripts/extract_compactor_lua.py`). The plugin lives in the
//! kn9t-plugins repo — https://github.com/96Ems/kn9t-plugins/tree/main/kn9t-compactor.
//!
//! The plugin runs two LLM passes; this only covers what the user sees while it
//! does: which tool calls were kept, summarized or dropped, and by what command.

use kn9t_tui::lua::widgets::Widget;
use kn9t_tui::lua::LuaRuntime;
use kn9t_tui::reducer::{PluginLuaOp, PluginPlacement};
use serde_json::json;

const COMPACTOR_LUA: &str = include_str!("compactor_ui.lua");
const PLUGIN: &str = "kn9t-compactor";

fn runtime_with_compactor(state: serde_json::Value) -> LuaRuntime {
    let rt = LuaRuntime::new().unwrap();
    rt.load_builtin();
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: PLUGIN.into(),
        source: COMPACTOR_LUA.into(),
        placement: PluginPlacement {
            zone: Some("sidebar".into()),
            ..Default::default()
        },
    });
    rt.apply_plugin_lua_op(&PluginLuaOp::SetState {
        plugin: PLUGIN.into(),
        state,
    });
    rt
}

/// Collect every text string in a widget tree.
fn all_text(w: &Widget, out: &mut Vec<String>) {
    match w {
        Widget::Text { .. } => out.extend(w.text()),
        Widget::List { items, .. } => out.extend(
            items
                .iter()
                .map(|spans| spans.iter().map(|s| s.text.as_str()).collect::<String>()),
        ),
        Widget::Box { title, child, .. } => {
            if let Some(t) = title {
                out.push(t.clone());
            }
            if let Some(c) = child {
                all_text(c, out);
            }
        }
        Widget::Float { child: Some(c), .. } => all_text(c, out),
        Widget::Split { children, .. } => {
            for c in children {
                all_text(c, out);
            }
        }
        _ => {}
    }
}

fn texts(rt: &LuaRuntime) -> Vec<String> {
    let w = rt
        .build_plugin_view(PLUGIN)
        .expect("compactor Lua must build");
    let mut out = Vec::new();
    all_text(&w, &mut out);
    out
}

fn state_with_decisions() -> serde_json::Value {
    json!({
        "status": "complete",
        "session_id": "sess-12345678",
        "messages_count": 7,
        "tool_calls_count": 3,
        "triage_done": true,
        "summary_preview": "all done",
        "decisions": [
            {"id": "t1", "action": "keep", "name": "bash", "preview": "git status"},
            {"id": "t2", "action": "drop", "name": "bash", "preview": "ls /tmp"},
            {"id": "t3", "action": "summarize", "name": "read", "preview": "src/main.rs"}
        ]
    })
}

/// The shipped Lua must load.
#[test]
fn shipped_lua_loads_and_renders() {
    let rt = runtime_with_compactor(state_with_decisions());
    assert!(rt.build_plugin_view(PLUGIN).is_ok());
}

/// A kept call must be identifiable: its tool name **and** the command it ran.
#[test]
fn kept_calls_show_the_name_and_the_command() {
    let rt = runtime_with_compactor(state_with_decisions());
    let t = texts(&rt);
    assert!(
        t.iter()
            .any(|s| s.contains("keep") && s.contains("bash") && s.contains("git status")),
        "a kept row must name the tool and its command, got {t:?}"
    );
}

/// The counts line is the at-a-glance answer to "how much is being kept?".
#[test]
fn counts_are_shown_per_action() {
    let rt = runtime_with_compactor(state_with_decisions());
    let t = texts(&rt);
    assert!(
        t.iter()
            .any(|s| s.contains("keep 1") && s.contains("drop 1")),
        "expected a per-action count line, got {t:?}"
    );
}

/// A decision the model left sparse (no name, no preview) must still render.
#[test]
fn sparse_decision_renders_without_error() {
    let rt = runtime_with_compactor(json!({
        "status": "triage",
        "triage_done": true,
        "decisions": [{"id": "abcdef012345", "action": "keep"}]
    }));
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("abcdef012345")),
        "a nameless decision falls back to the id, got {t:?}"
    );
}
