//! The Lua API contract: every symbol the built-in config documents must exist.
//!
//! This file exists because three documented APIs were dead at once:
//! `render_status()` was never called, `kn9t.get_messages`/`kn9t.get_tools` were
//! only ever installed by their own unit tests, and `kn9t.http` was a no-op
//! whose queue grew without bound. Each was documented in the header of
//! `assets/tui/90_render.lua`, so a user following that header wrote code against
//! nothing and landed in the red error shell.
//!
//! The rule these tests encode: **if the built-in config's header names it, it
//! must be non-nil in a freshly loaded runtime.** A unit test proving a function
//! works in isolation says nothing about whether it is wired up.

use kn9t_tui::lua::{state::StateSnapshot, LuaRuntime};
use kn9t_tui::theme::Theme;

/// A runtime in the same state the TUI puts it in at startup.
///
/// The order is the startup order, not an arbitrary one: the palette is published
/// before the config runs so `kn9t.theme.<slot>` is non-nil while the built-in builds
/// its palette table. With the calls reversed the built-in silently falls back to its
/// ANSI defaults and this suite certifies colours the TUI never draws.
fn booted() -> LuaRuntime {
    let rt = LuaRuntime::new().expect("runtime");
    rt.install_environment(&Theme::dark());
    rt.load_builtin();
    rt.update_state(&StateSnapshot::default());
    rt.update_context(&Default::default());
    rt
}

/// Assert a Lua expression evaluates truthy in the booted runtime.
fn truthy(rt: &LuaRuntime, expr: &str) -> bool {
    rt.eval_bool(&format!("return not not ({expr})"))
        .unwrap_or(false)
}

#[test]
fn every_documented_api_exists() {
    let rt = booted();

    // Callable entry points the header promises.
    for f in [
        "kn9t.get_messages",
        "kn9t.get_tools",
        "kn9t.map",
        "kn9t.unmap",
        "kn9t.action",
        "kn9t.on_click",
        "kn9t.remove_click",
        "kn9t.invalidate",
        "kn9t.register_command",
        "kn9t.unregister_command",
    ] {
        assert!(
            truthy(&rt, &format!("type({f}) == 'function'")),
            "{f} must be a function, not nil"
        );
    }

    // Tables the header promises.
    for t in [
        "kn9t.state",
        "kn9t.context",
        "kn9t.theme",
        "kn9t.native_views",
    ] {
        assert!(
            truthy(&rt, &format!("type({t}) == 'table'")),
            "{t} must be a table, not nil"
        );
    }
}

/// The accessors must be *callable*, not merely present: a function that throws
/// is as broken as a nil one, and both land the user in the error shell.
#[test]
fn documented_accessors_are_callable() {
    let rt = booted();
    assert!(
        truthy(&rt, "type(kn9t.get_messages()) == 'table'"),
        "get_messages() must return a table"
    );
    assert!(
        truthy(&rt, "type(kn9t.get_messages(1, 5)) == 'table'"),
        "get_messages(from, to) must accept a window"
    );
    assert!(
        truthy(&rt, "type(kn9t.get_tools()) == 'table'"),
        "get_tools() must return a table"
    );
}

/// Documented `kn9t.state` / `kn9t.context` fields must be published, or a
/// config reading them silently gets nil and renders blanks.
#[test]
fn documented_state_fields_are_published() {
    let rt = booted();

    for path in [
        "kn9t.state.session.id",
        "kn9t.state.session.streaming",
        "kn9t.state.usage.turn.input",
        "kn9t.state.usage.total.input",
        "kn9t.state.usage.cost",
        "kn9t.state.recent_tools",
        "kn9t.state.sessions",
        "kn9t.state.message_count",
        "kn9t.state.scroll",
        "kn9t.context.model",
        "kn9t.context.phase",
        "kn9t.context.input_height",
        "kn9t.context.user_count",
        "kn9t.context.assistant_count",
        "kn9t.context.tool_count",
    ] {
        assert!(
            truthy(&rt, &format!("{path} ~= nil")),
            "{path} must be published"
        );
    }
}

/// `render_status()` must actually be reachable from Rust. It was defined,
/// exported and unit-tested while the renderer ignored it entirely.
#[test]
fn builtin_status_bar_is_reachable() {
    let rt = booted();
    let spans = rt
        .call_status_spans()
        .expect("built-in render_status must be callable");
    assert!(
        !spans.is_empty(),
        "the built-in status bar must produce output, not an empty line"
    );
}

/// The built-in config must lay out a full screen, and place the native views it
/// needs to be usable.
#[test]
fn builtin_renders_a_usable_screen() {
    use kn9t_tui::lua::widgets::{collect_natives, UiOutcome};
    use ratatui::layout::Rect;

    let rt = booted();
    let area = Rect::new(0, 0, 120, 40);
    let root = match rt.build_ui_outcome(area.width, area.height) {
        UiOutcome::Ok(w) => w,
        other => panic!("built-in UI must build, got {other:?}"),
    };

    let mut natives = Vec::new();
    collect_natives(&root, area, &mut natives);
    let names: Vec<&str> = natives.iter().map(|(n, _)| n.as_str()).collect();

    // Without these three the session cannot be read, driven or diagnosed.
    for required in ["transcript", "input", "status"] {
        assert!(names.contains(&required), "missing native view {required}");
    }

    // Every placed view must be one Rust knows how to draw, or it leaves a hole.
    for (name, rect) in &natives {
        assert!(
            kn9t_tui::lua::widgets::NATIVE_VIEWS.contains(&name.as_str()),
            "built-in places unknown native view '{name}'"
        );
        assert!(rect.height > 0, "{name} was given no rows");
    }
}

/// Every colour the built-in config produces must have resolved.
///
/// This walks the *built tree* rather than scanning the source, so it tests what
/// Rust actually received. While `parse_color` lacked the bright colours,
/// `lightgreen`/`lightred` silently became `None` and the context gauge lost its
/// warn/danger signal with no error anywhere.
#[test]
fn builtin_palette_colors_all_resolve() {
    use kn9t_tui::lua::widgets::{UiOutcome, Widget};

    fn count_colors(w: &Widget, total: &mut usize) {
        // Any node carrying an explicit colour must have parsed it.
        match w {
            Widget::Text { spans, style, .. } => {
                if style.fg.is_some() {
                    *total += 1;
                }
                *total += spans.iter().filter(|s| s.style.fg.is_some()).count();
            }
            Widget::Gauge { style, .. } => {
                if style.fg.is_some() {
                    *total += 1;
                }
            }
            Widget::Box { child, style, .. } => {
                if style.fg.is_some() {
                    *total += 1;
                }
                if let Some(c) = child {
                    count_colors(c, total);
                }
            }
            Widget::Float { child, .. } => {
                if let Some(c) = child {
                    count_colors(c, total);
                }
            }
            Widget::Split { children, .. } => {
                for c in children {
                    count_colors(c, total);
                }
            }
            _ => {}
        }
    }

    let rt = booted();
    let root = match rt.build_ui_outcome(120, 40) {
        UiOutcome::Ok(w) => w,
        other => panic!("built-in UI must build, got {other:?}"),
    };

    let mut resolved = 0;
    count_colors(&root, &mut resolved);
    assert!(
        resolved > 8,
        "the built-in sidebar styles many nodes; only {resolved} colours resolved, \
         which means the parser is dropping names the config uses"
    );

    // The status bar is where the warn/danger palette is most load-bearing.
    let spans = rt.call_status_spans().expect("status bar");
    let coloured = spans.iter().filter(|s| s.style.fg.is_some()).count();
    assert!(
        coloured > 3,
        "status bar must be multi-coloured, got {coloured} coloured spans"
    );

    // The exact names the built-in relies on for its warn/danger states.
    for name in ["lightgreen", "lightred", "yellow", "cyan", "darkgray"] {
        assert!(
            kn9t_tui::theme::parse_color(name).is_some(),
            "built-in palette colour '{name}' does not parse"
        );
    }
}

/// The built-in status bar draws solid colour chips, which needs `bg` to survive
/// `parse_status_spans`. The comment on `call_status_spans` claims a segment supports
/// every style a `text` node does; this is the half of that claim the bar actually uses.
#[test]
fn builtin_status_chips_use_a_background() {
    let rt = booted();
    let spans = rt.call_status_spans().expect("status bar");
    assert!(
        spans.iter().any(|s| s.style.bg.is_some()),
        "no status segment carried a background, so the chips render as plain text"
    );
}

/// `tool_mode` is how a config (and therefore a plugin's tool) picks a card
/// renderer. The built-in must answer for the tools it claims to style.
#[test]
fn builtin_declares_tool_modes() {
    let rt = booted();
    for (tool, expected) in [
        ("edit", "diff"),
        ("write", "diff"),
        ("read", "summary"),
        ("bash", "streaming"),
    ] {
        assert_eq!(
            rt.call_tool_mode(tool).as_deref(),
            Some(expected),
            "tool_mode({tool})"
        );
    }
    // An unknown tool must fall through to the Rust default, not error.
    assert!(rt.call_tool_mode("some-plugin-tool").is_none());
}

/// The context gauge denominator must come from the server, not a constant.
/// `GET /models` always returned `ctx_window`; the TUI dropped it, so the
/// built-in config hardcoded 200k and read wrong on every other model
/// (TRACKING B3).
#[test]
fn context_window_is_published_when_known() {
    use kn9t_tui::lua::context::ContextStats;

    let rt = LuaRuntime::new().expect("runtime");
    rt.load_builtin();
    rt.install_environment(&Theme::dark());
    rt.update_state(&StateSnapshot::default());
    rt.update_context(&ContextStats {
        ctx_window: Some(1_000_000),
        ..Default::default()
    });

    assert!(
        truthy(&rt, "kn9t.context.ctx_window == 1000000"),
        "ctx_window must reach Lua verbatim"
    );

    // Unknown stays nil rather than 0, so Lua can tell "no data" from "empty"
    // and fall back to its own estimate instead of dividing by zero.
    rt.update_context(&ContextStats::default());
    assert!(
        truthy(&rt, "kn9t.context.ctx_window == nil"),
        "an unreported window must be nil, not zero"
    );
}

/// A hot-reload must not lose the documented globals. This currently holds
/// because the sandbox reset reuses the Lua state; the test pins the behaviour
/// so a future change to `apply_sandbox` cannot quietly break the API on save.
#[test]
fn documented_api_survives_reload() {
    let rt = booted();
    assert!(rt.reload(), "reload must succeed with no user file");

    for f in ["kn9t.get_messages", "kn9t.get_tools"] {
        assert!(
            truthy(&rt, &format!("type({f}) == 'function'")),
            "{f} must survive a hot-reload"
        );
    }
    for t in ["kn9t.theme", "kn9t.native_views"] {
        assert!(
            truthy(&rt, &format!("type({t}) == 'table'")),
            "{t} must survive a hot-reload"
        );
    }
    assert!(
        rt.call_status_spans().is_some(),
        "render_status must survive a hot-reload"
    );
}

/// Actions are the mechanism Lua uses to drive Rust. A name that validates but
/// has no handler is the failure mode `toggle_right` had for months.
#[test]
fn documented_actions_are_all_handled() {
    for name in [
        "scroll_top",
        "scroll_bottom",
        "session_picker",
        "open_models",
        "open_tools",
        "open_palette",
        "refresh_tools",
        // Diff review is a plugin panel now, reached by focusing it rather than
        // by host actions; `focus_plugin` is what replaced `open_diff`.
        "focus_plugin",
        "toggle_thinking",
        "new_session",
        "abort",
        "switch_session",
    ] {
        assert!(
            kn9t_tui::keybind::parse_action(name).is_some(),
            "action '{name}' is documented but does not parse"
        );
    }
}

/// `kn9t.action(name, arg)` is the only action-dispatch mechanism that carries
/// data. It must survive the full queue -> drain -> parse round trip with the
/// argument attached to the right call, not just parse as a name in isolation.
#[test]
fn switch_session_action_carries_its_argument() {
    let rt = booted();
    rt.eval_bool(r#"kn9t.action("switch_session", "sess-xyz"); return true"#)
        .expect("kn9t.action with an argument must not error");

    let drained = rt.drain_lua_actions();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].0, kn9t_tui::keybind::Action::SwitchSession);
    assert_eq!(drained[0].1.as_deref(), Some("sess-xyz"));
}

/// End-to-end: a widget carrying `id=` in a real `render_ui()` tree must be
/// findable by `collect_clickable_areas` AND dispatchable through
/// `kn9t.on_click`, using the runtime's actual public methods rather than
/// `click::ClickRegistry` in isolation. This is the seam a Lua-drawn,
/// click-to-switch session list depends on end to end.
#[test]
fn a_widget_with_id_is_clickable_end_to_end() {
    use kn9t_tui::lua::widgets::{collect_clickable_areas, UiOutcome};

    let mut path = std::env::temp_dir();
    path.push(format!(
        "kn9t_click_e2e_{}_{:?}.lua",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        r#"
        CLICKED = nil
        kn9t.on_click("my_row", function(x, y, btn)
            CLICKED = {x = x, y = y, btn = btn}
            return true
        end)
        function render_ui(w, h)
            return { type = "list", id = "my_row", items = {"a", "b", "c"} }
        end
    "#,
    )
    .unwrap();

    let rt = LuaRuntime::new().expect("runtime");
    assert!(rt.load_file(&path), "test config must load without error");
    std::fs::remove_file(&path).ok();

    let mut registry = kn9t_tui::lua::click::ClickRegistry::new();
    let applied = rt.drain_clicks(&mut registry);
    assert_eq!(applied, 1, "kn9t.on_click must register through the drain");

    let root = match rt.build_ui_outcome(20, 10) {
        UiOutcome::Ok(w) => w,
        other => panic!("render_ui must build, got {other:?}"),
    };

    let area = ratatui::layout::Rect::new(5, 3, 20, 10);
    let mut clicks = Vec::new();
    collect_clickable_areas(&root, area, &mut clicks);
    assert_eq!(clicks, vec![("my_row".to_string(), area)]);

    let (id, rect) = &clicks[0];
    let consumed = rt.dispatch_click(&registry, id, 2, 1, "left");
    assert!(
        consumed,
        "handler returned true, so the click must be consumed"
    );
    assert_eq!(rect, &area);

    assert!(
        rt.eval_bool("return CLICKED ~= nil and CLICKED.x == 2 and CLICKED.y == 1")
            .unwrap_or(false),
        "handler must have run with the coordinates passed to dispatch_click"
    );
}

/// End-to-end: `kn9t.register_command` must reach the runtime's own
/// `drain_commands`/`run_lua_command` path, using real `LuaRuntime` methods
/// rather than `commands::LuaCommandRegistry` in isolation.
#[test]
fn register_command_is_runnable_end_to_end() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "kn9t_cmd_e2e_{}_{:?}.lua",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        r#"
        RAN_WITH = nil
        kn9t.register_command({
            id = "view_diff",
            label = "View: Diff",
            slash = "/view",
            handler = function(args) RAN_WITH = args end,
        })
    "#,
    )
    .unwrap();

    let rt = LuaRuntime::new().expect("runtime");
    assert!(rt.load_file(&path), "test config must load without error");
    std::fs::remove_file(&path).ok();

    let mut registry = kn9t_tui::lua::commands::LuaCommandRegistry::new();
    let applied = rt.drain_commands(&mut registry);
    assert_eq!(
        applied, 1,
        "register_command must register through the drain"
    );

    let cmd = registry.get("view_diff").expect("must be registered");
    assert_eq!(cmd.slash.as_deref(), Some("view"));

    rt.run_lua_command(cmd, "diff");
    assert!(
        rt.eval_bool("return RAN_WITH == 'diff'").unwrap_or(false),
        "the handler must actually run with the argument passed to run_lua_command"
    );
}
