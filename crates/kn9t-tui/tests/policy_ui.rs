//! The `kn9t-policy` plugin's Lua UI, extracted verbatim from its Python source
//! (`scripts/extract_policy_lua.py`), so this is not a hand-written approximation.
//! The plugin lives in the kn9t-plugins repo —
//! https://github.com/96Ems/kn9t-plugins/tree/main/kn9t-policy.
//!
//! These tests lock the migration to the generic primitives: the grant input is
//! driven by `kn9t.on_text`, not by a per-character `on_key` loop, and the
//! shortcuts must still fire when nothing is being typed.

use kn9t_tui::lua::plugin_ui::PluginEffect;
use kn9t_tui::lua::widgets::Widget;
use kn9t_tui::lua::LuaRuntime;
use kn9t_tui::reducer::{PluginLuaOp, PluginPlacement};
use serde_json::json;

/// The Lua the real plugin sends over the wire.
const POLICY_LUA: &str = include_str!("policy_ui.lua");

const PLUGIN: &str = "kn9t-policy";

fn runtime_with_policy(state: serde_json::Value) -> LuaRuntime {
    let rt = LuaRuntime::new().unwrap();
    rt.load_builtin();
    rt.apply_plugin_lua_op(&PluginLuaOp::Register {
        plugin: PLUGIN.into(),
        source: POLICY_LUA.into(),
        placement: PluginPlacement {
            zone: Some("main".into()),
            ..Default::default()
        },
    });
    rt.apply_plugin_lua_op(&PluginLuaOp::SetState {
        plugin: PLUGIN.into(),
        state,
    });
    rt
}

fn state(grants: serde_json::Value) -> serde_json::Value {
    json!({ "mode": "normal", "grants": grants, "recent": [] })
}

/// Collect every text string in a widget tree, so assertions do not depend on
/// the exact nesting the plugin chose.
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
    let w = rt.build_plugin_view(PLUGIN).expect("policy Lua must build");
    let mut out = Vec::new();
    all_text(&w, &mut out);
    out
}

/// The shipped Lua must load — a syntax error would fail before a user runs it.
#[test]
fn shipped_lua_loads_and_renders() {
    let rt = runtime_with_policy(state(json!([])));
    assert!(
        rt.build_plugin_view(PLUGIN).is_ok(),
        "the plugin's shipped Lua must load"
    );
}

/// Typing goes through the single `kn9t.on_text` handler: `a` opens the input,
/// then characters accumulate, and the rendered input line shows them.
#[test]
fn typed_characters_land_in_the_grant_input() {
    let rt = runtime_with_policy(state(json!([])));
    assert!(rt.dispatch_plugin_key(PLUGIN, "a"), "`a` must start adding");
    assert!(
        rt.dispatch_plugin_text(PLUGIN, "h"),
        "on_text consumes while adding"
    );
    assert!(
        rt.dispatch_plugin_text(PLUGIN, "i"),
        "on_text consumes while adding"
    );

    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("hi")),
        "typed text must reach the input line, got {t:?}"
    );
}

/// Outside the input mode the text handler declines, so its characters fall
/// through to the shortcuts (`on_key` runs first) or the host.
#[test]
fn text_falls_through_when_not_adding() {
    let rt = runtime_with_policy(state(json!([])));
    assert!(rt.plugin_has_text(PLUGIN), "the view must register on_text");
    assert!(
        !rt.dispatch_plugin_text(PLUGIN, "z"),
        "an unbound character must fall through"
    );
}

/// Enter while adding notifies the backend with exactly what was typed.
#[test]
fn enter_notifies_with_the_typed_grant() {
    let rt = runtime_with_policy(state(json!([])));
    assert!(rt.dispatch_plugin_key(PLUGIN, "a"));
    for ch in ["c", "u", "r", "l", " ", "*"] {
        assert!(
            rt.dispatch_plugin_text(PLUGIN, ch),
            "`{ch}` must be consumed"
        );
    }
    assert!(
        rt.dispatch_plugin_key(PLUGIN, "Enter"),
        "Enter must consume"
    );

    let effects = rt.drain_plugin_effects();
    assert_eq!(effects.len(), 1, "expected one effect, got {effects:?}");
    match &effects[0] {
        PluginEffect::NotifyPlugin {
            plugin,
            event,
            data,
        } => {
            assert_eq!(plugin, PLUGIN);
            assert_eq!(event, "add_grant");
            assert_eq!(data.get("pattern").and_then(|v| v.as_str()), Some("curl *"));
        }
        other => panic!("expected NotifyPlugin, got {other:?}"),
    }
}

/// Esc clears a half-typed grant and then falls through, so it still releases
/// focus (it must not trap the user in the panel).
#[test]
fn escape_clears_the_input_and_releases_focus() {
    let rt = runtime_with_policy(state(json!([])));
    assert!(rt.dispatch_plugin_key(PLUGIN, "a"));
    assert!(rt.dispatch_plugin_text(PLUGIN, "x"));
    assert!(
        texts(&rt).iter().any(|s| s.contains("x")),
        "the character must be visible before Esc"
    );

    assert!(
        !rt.dispatch_plugin_key(PLUGIN, "Escape"),
        "Esc must fall through so the host releases focus"
    );
    assert!(
        !texts(&rt).iter().any(|s| s.contains('x')),
        "Esc must clear the half-typed grant"
    );
}

/// Navigation is local: `j` moves the selection down the grants list.
#[test]
fn j_moves_the_grant_selection() {
    let rt = runtime_with_policy(state(json!(["alpha", "beta"])));
    // Render once so `on_state` populates V.grants.
    let _ = texts(&rt);
    assert!(rt.dispatch_plugin_key(PLUGIN, "j"), "`j` must move");
    let t = texts(&rt);
    assert!(
        t.iter().any(|s| s.contains("> beta")),
        "`j` must select the second grant, got {t:?}"
    );
}
