//! The built-in Lua UI, embedded in the executable.
//!
//! kn9t ships as a single self-contained binary: the default UI lives in
//! `assets/default_tui.lua` and is compiled in, so a bare executable with no
//! config directory still has a complete interface.
//!
//! Load order at startup:
//!   1. `DEFAULT_TUI_LUA` (always) — the baseline UI.
//!   2. `~/.kn9t/tui.lua` (if present) — user overrides layered on top.
//!
//! Nothing is written to disk unless the user explicitly asks
//! (`kn9t-tui --export-config`), so running the binary has no side effects.

use std::path::Path;

/// The built-in UI, compiled into the binary.
pub const DEFAULT_TUI_LUA: &str = include_str!("../../assets/default_tui.lua");

/// Result of an explicit export request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    /// Written to disk.
    Written,
    /// Refused: a file is already there and `force` was not set.
    Exists,
    /// Failed with a message.
    Failed(String),
}

/// Write the built-in config to `path` so the user can edit it.
///
/// Refuses to clobber an existing file unless `force`, because an edited
/// config is worth more than our default.
pub fn export_config(path: &Path, force: bool) -> ExportOutcome {
    if path.exists() && !force {
        return ExportOutcome::Exists;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ExportOutcome::Failed(format!("could not create {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, DEFAULT_TUI_LUA) {
        Ok(()) => ExportOutcome::Written,
        Err(e) => ExportOutcome::Failed(format!("could not write {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kn9t_default_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p.push("tui.lua");
        p
    }

    #[test]
    fn embedded_default_is_not_empty() {
        assert!(
            DEFAULT_TUI_LUA.len() > 500,
            "default config looks truncated"
        );
    }

    #[test]
    fn embedded_default_defines_the_entry_points() {
        assert!(
            DEFAULT_TUI_LUA.contains("function render_ui"),
            "built-in must own the layout"
        );
        assert!(
            DEFAULT_TUI_LUA.contains("function render_status"),
            "built-in must own the status bar"
        );
    }

    /// The binary must be usable with no files on disk at all. This is the
    /// property that makes single-executable delivery work.
    #[test]
    fn builtin_alone_produces_a_valid_ui() {
        let rt = crate::lua::LuaRuntime::new().unwrap();
        assert!(rt.load_builtin(), "built-in config must load");

        rt.update_state(&crate::lua::state::StateSnapshot::default());
        rt.update_context(&crate::lua::context::ContextStats::default());

        for (w, h) in [(120u16, 40u16), (60, 20), (250, 80), (40, 10)] {
            match rt.build_ui_outcome(w, h) {
                crate::lua::widgets::UiOutcome::Ok(_) => {}
                other => panic!("built-in failed at {w}x{h}: {other:?}"),
            }
        }
    }

    /// First launch has no messages, no tokens, no model: nil arithmetic here
    /// would break the very first frame a new user sees.
    #[test]
    fn builtin_survives_completely_empty_state() {
        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        // Publish nothing at all: no kn9t.state, no kn9t.context.
        assert!(
            matches!(
                rt.build_ui_outcome(100, 30),
                crate::lua::widgets::UiOutcome::Ok(_)
            ),
            "built-in must tolerate missing state tables"
        );
    }

    /// The built-in must place the native views the app depends on. Without
    /// transcript and input the TUI is unusable, so a config that forgets them
    /// should fail loudly here rather than at runtime.
    #[test]
    fn builtin_places_required_native_views() {
        use crate::lua::widgets::{collect_natives, UiOutcome};
        use ratatui::layout::Rect;

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        rt.update_state(&crate::lua::state::StateSnapshot::default());
        rt.update_context(&crate::lua::context::ContextStats::default());

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut natives = Vec::new();
        collect_natives(&root, area, &mut natives);
        let names: Vec<&str> = natives.iter().map(|(n, _)| n.as_str()).collect();

        for required in ["transcript", "input"] {
            assert!(
                names.contains(&required),
                "built-in must place '{required}', got {names:?}"
            );
        }

        // Every placement must be inside the frame and non-degenerate.
        for (name, rect) in &natives {
            assert!(rect.width > 0 && rect.height > 0, "'{name}' has empty rect");
            assert!(
                rect.right() <= area.right() && rect.bottom() <= area.bottom(),
                "'{name}' at {rect:?} escapes {area:?}"
            );
        }
    }

    /// Native views must not overlap: two views writing the same cell means one
    /// silently wins, which shows up as a rendering glitch.
    #[test]
    fn builtin_native_views_do_not_overlap() {
        use crate::lua::widgets::{collect_natives, UiOutcome};
        use ratatui::layout::Rect;

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        rt.update_state(&crate::lua::state::StateSnapshot::default());
        rt.update_context(&crate::lua::context::ContextStats::default());

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut natives = Vec::new();
        collect_natives(&root, area, &mut natives);

        for (i, (an, a)) in natives.iter().enumerate() {
            for (bn, b) in natives.iter().skip(i + 1) {
                assert!(
                    a.intersection(*b).area() == 0,
                    "'{an}' {a:?} overlaps '{bn}' {b:?}"
                );
            }
        }
    }

    /// A user file only needs to define what it changes; the rest stays.
    #[test]
    fn user_config_layers_over_builtin() {
        let p = temp_path("layer");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // Override only the status bar.
        std::fs::write(&p, r#"function render_status() return "USER BAR" end"#).unwrap();

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        assert!(rt.load_file(&p));

        rt.update_state(&crate::lua::state::StateSnapshot::default());
        rt.update_context(&crate::lua::context::ContextStats::default());

        // render_ui still comes from the built-in.
        assert!(matches!(
            rt.build_ui_outcome(100, 30),
            crate::lua::widgets::UiOutcome::Ok(_)
        ));
        // render_status is the user's.
        let spans = rt.call_status_spans().expect("user render_status must run");
        assert_eq!(
            spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            "USER BAR"
        );

        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A broken user file must not take the built-in UI down with it.
    #[test]
    fn broken_user_config_is_reported_not_ignored() {
        let p = temp_path("broken");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "this is not lua {{{").unwrap();

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        assert!(!rt.load_file(&p), "syntax error must be reported");

        // Surfaced as Failed so the user sees the error banner.
        assert!(matches!(
            rt.build_ui_outcome(100, 30),
            crate::lua::widgets::UiOutcome::Failed(_)
        ));

        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn missing_user_config_is_not_an_error() {
        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        let missing = temp_path("absent");
        assert!(rt.load_file(&missing), "absent config is normal");
        assert!(matches!(
            rt.build_ui_outcome(100, 30),
            crate::lua::widgets::UiOutcome::Ok(_)
        ));
    }

    #[test]
    fn export_writes_the_builtin() {
        let p = temp_path("export");
        assert_eq!(export_config(&p, false), ExportOutcome::Written);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), DEFAULT_TUI_LUA);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn export_refuses_to_clobber_without_force() {
        let p = temp_path("clobber");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "-- mine").unwrap();

        assert_eq!(export_config(&p, false), ExportOutcome::Exists);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "-- mine");

        assert_eq!(export_config(&p, true), ExportOutcome::Written);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), DEFAULT_TUI_LUA);

        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Round-trip: exporting and loading that file must behave identically to
    /// the built-in, otherwise `--export-config` hands users a broken starting point.
    #[test]
    fn exported_file_loads_cleanly() {
        let p = temp_path("roundtrip");
        assert_eq!(export_config(&p, false), ExportOutcome::Written);

        let rt = crate::lua::LuaRuntime::new().unwrap();
        assert!(rt.load_file(&p), "exported config must load");
        rt.update_state(&crate::lua::state::StateSnapshot::default());
        rt.update_context(&crate::lua::context::ContextStats::default());
        assert!(matches!(
            rt.build_ui_outcome(120, 40),
            crate::lua::widgets::UiOutcome::Ok(_)
        ));

        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    // -- plugin views, end to end ------------------------------------------

    /// Build a runtime with the built-in config and one registered plugin.
    fn runtime_with_plugin(source: &str, state: serde_json::Value) -> crate::lua::LuaRuntime {
        use crate::reducer::PluginLuaOp;

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        rt.apply_plugin_lua_op(&PluginLuaOp::Register {
            plugin: "demo".into(),
            source: source.into(),
            placement: Default::default(),
        });
        rt.apply_plugin_lua_op(&PluginLuaOp::SetState {
            plugin: "demo".into(),
            state,
        });
        rt
    }

    /// Publish state including the plugin list, as a real frame would.
    fn publish(rt: &crate::lua::LuaRuntime) {
        publish_focused(rt, "");
    }

    /// Same, with `focused_plugin` set — the signal the built-in layout uses to
    /// decide how much room a view gets.
    fn publish_focused(rt: &crate::lua::LuaRuntime, focused: &str) {
        let snap = crate::lua::state::StateSnapshot {
            plugin_views: rt.plugin_view_names(),
            plugin_view_specs: rt
                .plugin_views()
                .into_iter()
                .map(|(name, p)| crate::lua::state::PluginViewSpec {
                    title: p.title.clone().unwrap_or_else(|| name.clone()),
                    placement: p.zone.clone().unwrap_or_default(),
                    rows: p.rows.unwrap_or(0),
                    cols: p.cols.unwrap_or(0),
                    name,
                })
                .collect(),
            focused_plugin: focused.to_string(),
            ..Default::default()
        };
        rt.update_state(&snap);
        rt.update_context(&crate::lua::context::ContextStats::default());
    }

    /// An interactive view is unusable in a status-sized strip, so focusing it
    /// must actually enlarge its slot. Without this the diff panel would render
    /// into 8 rows and be unreviewable.
    #[test]
    fn focusing_a_plugin_enlarges_its_slot() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use ratatui::layout::Rect;

        let rt = runtime_with_plugin(
            r#"function render(s) return {type="text", content="x"} end"#,
            serde_json::json!({}),
        );
        let area = Rect::new(0, 0, 120, 40);

        let slot_height = |rt: &crate::lua::LuaRuntime| {
            let root = match rt.build_ui_outcome(area.width, area.height) {
                UiOutcome::Ok(w) => w,
                other => panic!("built-in did not build: {other:?}"),
            };
            let mut slots = Vec::new();
            collect_plugin_slots(&root, area, &mut slots);
            assert_eq!(slots.len(), 1, "expected one slot, got {slots:?}");
            slots[0].1.height
        };

        publish_focused(&rt, "");
        let unfocused = slot_height(&rt);

        // `invalidate_ui` because focus is not part of the render fingerprint;
        // without it the cached tree from the previous build would be reused.
        publish_focused(&rt, "demo");
        rt.invalidate_ui();
        let focused = slot_height(&rt);

        assert!(
            focused > unfocused,
            "focused slot ({focused}) must be taller than unfocused ({unfocused})"
        );
        assert!(
            focused <= area.height,
            "focused slot ({focused}) must still fit in {area:?}"
        );
    }

    /// A plugin declaring `placement="main"` must land in the centre column, not
    /// the sidebar strip — that routing is the whole point of the placement
    /// hint, and it is what lets a full-size review panel exist without the
    /// config naming the plugin that provides it.
    #[test]
    fn placement_routes_a_view_to_the_main_column() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use crate::reducer::{PluginLuaOp, PluginPlacement};
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 120, 40);

        let slot_for = |zone: Option<&str>| {
            let rt = crate::lua::LuaRuntime::new().unwrap();
            rt.load_builtin();
            rt.apply_plugin_lua_op(&PluginLuaOp::Register {
                plugin: "demo".into(),
                source: r#"function render(s) return {type="text", content="x"} end"#.into(),
                placement: PluginPlacement {
                    zone: zone.map(String::from),
                    title: Some("Demo".into()),
                    rows: None,
                    cols: None,
                },
            });
            publish_focused(&rt, "");
            let root = match rt.build_ui_outcome(area.width, area.height) {
                UiOutcome::Ok(w) => w,
                other => panic!("built-in did not build: {other:?}"),
            };
            let mut slots = Vec::new();
            collect_plugin_slots(&root, area, &mut slots);
            assert_eq!(slots.len(), 1, "expected one slot, got {slots:?}");
            slots[0].1
        };

        let sidebar = slot_for(Some("sidebar"));
        let main = slot_for(Some("main"));
        // An unplaced view must still appear — defaulting to the sidebar beats
        // silently rendering nowhere.
        let unplaced = slot_for(None);

        assert_eq!(
            unplaced.x, sidebar.x,
            "an unplaced view defaults to the sidebar zone"
        );
        assert!(
            main.x < sidebar.x,
            "main-zone slot ({main:?}) must sit left of the sidebar strip ({sidebar:?})"
        );
        assert!(
            main.width > sidebar.width,
            "main-zone slot ({main:?}) must be wider than a sidebar slot ({sidebar:?})"
        );
    }

    /// A plugin's declared `rows` must be honoured when focused, so a panel that
    /// needs room can ask for it instead of the config guessing per plugin.
    #[test]
    fn declared_rows_are_used_when_focused() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use crate::reducer::{PluginLuaOp, PluginPlacement};
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 120, 40);
        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        rt.apply_plugin_lua_op(&PluginLuaOp::Register {
            plugin: "demo".into(),
            source: r#"function render(s) return {type="text", content="x"} end"#.into(),
            placement: PluginPlacement {
                zone: Some("sidebar".into()),
                title: None,
                rows: Some(17),
                cols: None,
            },
        });

        let height_now = |focused: &str| {
            publish_focused(&rt, focused);
            rt.invalidate_ui();
            let root = match rt.build_ui_outcome(area.width, area.height) {
                UiOutcome::Ok(w) => w,
                other => panic!("built-in did not build: {other:?}"),
            };
            let mut slots = Vec::new();
            collect_plugin_slots(&root, area, &mut slots);
            assert_eq!(slots.len(), 1);
            slots[0].1.height
        };

        let unfocused = height_now("");
        let focused = height_now("demo");
        assert!(focused > unfocused, "focus must enlarge the slot");
        // `collect_plugin_slots` reports the rect the plugin's own tree gets,
        // which is *inside* the framing box — so a declared 17 rows becomes 15
        // of content once the border takes one row top and bottom. Asserting the
        // inner number keeps this honest about what the plugin can draw in.
        assert_eq!(
            focused, 15,
            "the plugin asked for 17 rows; 2 go to the box border"
        );
    }

    /// The whole point of the mechanism: a plugin ships Lua, and the built-in
    /// config places it — with no per-plugin code anywhere in the TUI.
    #[test]
    fn registered_plugin_gets_a_slot_in_the_builtin_layout() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use ratatui::layout::Rect;

        let rt = runtime_with_plugin(
            r#"function render(s) return {type="text", content="hi "..(s.who or "?")} end"#,
            serde_json::json!({"who": "world"}),
        );
        publish(&rt);

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);

        assert_eq!(
            slots.len(),
            1,
            "the registered plugin must get exactly one slot, got {slots:?}"
        );
        assert_eq!(slots[0].0, "demo");
        let rect = slots[0].1;
        assert!(rect.width > 0 && rect.height > 0, "slot must be drawable");
        assert!(
            rect.right() <= area.right() && rect.bottom() <= area.bottom(),
            "slot {rect:?} escapes {area:?}"
        );
    }

    /// The slot must resolve to the plugin's own widget tree.
    #[test]
    fn plugin_slot_builds_the_plugins_widget_tree() {
        let rt = runtime_with_plugin(
            r#"function render(s) return {type="text", content="hi "..(s.who or "?")} end"#,
            serde_json::json!({"who": "world"}),
        );
        publish(&rt);

        match rt.build_plugin_view("demo").expect("should build") {
            w if w.text().is_some() => {
                assert_eq!(w.text().unwrap(), "hi world");
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// With no plugins registered the layout must be unchanged — the feature
    /// costs nothing when unused.
    #[test]
    fn no_plugins_means_no_slots_and_transcript_keeps_its_space() {
        use crate::lua::widgets::{collect_natives, collect_plugin_slots, UiOutcome};
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 120, 40);

        let bare = crate::lua::LuaRuntime::new().unwrap();
        bare.load_builtin();
        publish(&bare);
        let root = match bare.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);
        assert!(slots.is_empty(), "no plugins should mean no slots");

        let mut natives = Vec::new();
        collect_natives(&root, area, &mut natives);
        let transcript = natives
            .iter()
            .find(|(n, _)| n == "transcript")
            .expect("transcript must still be placed");
        assert!(
            transcript.1.height > 1,
            "transcript must keep usable height, got {:?}",
            transcript.1
        );
    }

    /// A plugin slot must never overlap a native view: overlapping rects mean
    /// one silently overwrites the other.
    #[test]
    fn plugin_slots_do_not_overlap_native_views() {
        use crate::lua::widgets::{collect_natives, collect_plugin_slots, UiOutcome};
        use ratatui::layout::Rect;

        let rt = runtime_with_plugin(
            r#"function render(s) return {type="text", content="x"} end"#,
            serde_json::json!({}),
        );
        publish(&rt);

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut natives = Vec::new();
        collect_natives(&root, area, &mut natives);
        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);

        for (pn, pr) in &slots {
            for (nn, nr) in &natives {
                assert!(
                    pr.intersection(*nr).area() == 0,
                    "plugin '{pn}' {pr:?} overlaps native '{nn}' {nr:?}"
                );
            }
        }
    }

    /// Several plugins each get their own slot, in stable order, without any of
    /// them being named in the config.
    #[test]
    fn multiple_plugins_each_get_a_distinct_slot() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use crate::reducer::PluginLuaOp;
        use ratatui::layout::Rect;

        let rt = crate::lua::LuaRuntime::new().unwrap();
        rt.load_builtin();
        for name in ["zeta", "alpha"] {
            rt.apply_plugin_lua_op(&PluginLuaOp::Register {
                plugin: name.into(),
                source: r#"function render(s) return {type="text", content="x"} end"#.into(),
                placement: Default::default(),
            });
        }
        publish(&rt);

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);

        let names: Vec<&str> = slots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"], "stable (sorted) order");

        // Distinct, non-overlapping rects.
        assert_ne!(slots[0].1, slots[1].1);
        assert_eq!(slots[0].1.intersection(slots[1].1).area(), 0);
    }

    /// A plugin removed via `ui_clear` must lose its slot, so a torn-down UI
    /// does not leave a hole in the layout.
    #[test]
    fn cleared_plugin_loses_its_slot() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use crate::reducer::PluginLuaOp;
        use ratatui::layout::Rect;

        let rt = runtime_with_plugin(
            r#"function render(s) return {type="text", content="x"} end"#,
            serde_json::json!({}),
        );
        rt.apply_plugin_lua_op(&PluginLuaOp::Clear {
            plugin: "demo".into(),
        });
        publish(&rt);

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("built-in did not build: {other:?}"),
        };

        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);
        assert!(slots.is_empty(), "cleared plugin must not keep a slot");
    }

    /// A broken plugin still gets a slot, so the error can be shown in place.
    /// Blanking the area would look like a layout bug and hide the cause.
    #[test]
    fn broken_plugin_keeps_its_slot_and_reports_the_error() {
        use crate::lua::widgets::{collect_plugin_slots, UiOutcome};
        use ratatui::layout::Rect;

        let rt = runtime_with_plugin("function render( not lua", serde_json::json!({}));
        publish(&rt);

        let area = Rect::new(0, 0, 120, 40);
        let root = match rt.build_ui_outcome(area.width, area.height) {
            UiOutcome::Ok(w) => w,
            other => panic!("a broken plugin must not break the layout: {other:?}"),
        };

        let mut slots = Vec::new();
        collect_plugin_slots(&root, area, &mut slots);
        assert_eq!(
            slots.len(),
            1,
            "broken plugin still needs somewhere to report"
        );

        let err = rt.build_plugin_view("demo").unwrap_err();
        assert!(!err.is_empty(), "must carry a displayable message");
    }
}
