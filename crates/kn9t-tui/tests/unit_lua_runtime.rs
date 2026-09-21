// Extracted from src/lua/mod.rs — the #[cfg(test)] mod tests block.
// Tests for LuaRuntime: load_dir, ConfigSource, sandbox, lua_value_to_json,
// build_ui_outcome, and the render_ui memoisation cache.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use mlua::{Lua, Value};

use kn9t_tui::lua::state::StateSnapshot;
use kn9t_tui::lua::widgets;
use kn9t_tui::lua::{lua_value_to_json, ConfigSource, LuaRuntime};

// ── local helpers ─────────────────────────────────────────────────────────────

/// Write `src` to a temp file and load it into a fresh runtime.
fn runtime_with(src: &str) -> (LuaRuntime, PathBuf) {
    // A counter alongside the timestamp: two tests in the same process can
    // legitimately land on the same nanosecond under enough parallelism,
    // and a collided path means one test's `render_ui` silently becomes
    // another's, which is exactly the kind of one-in-a-thousand flake that
    // is miserable to reproduce. `fetch_add` guarantees uniqueness within
    // this process regardless of clock resolution.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut path = std::env::temp_dir();
    path.push(format!(
        "kn9t_lua_test_{}_{}_{:?}.lua",
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, src).unwrap();
    let rt = LuaRuntime::new().unwrap();
    rt.load_file(&path);
    (rt, path)
}

/// A fresh, uniquely-named temp directory for `load_dir` tests. Same
/// collision-avoidance reasoning as `runtime_with`'s path.
fn unique_temp_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut path = std::env::temp_dir();
    path.push(format!(
        "kn9t_lua_dir_test_{}_{}_{:?}",
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// A config with `render_ui` that counts its own calls, so a test can tell
/// a cache hit (Lua not called) from a rebuild (Lua called) without any
/// Rust-side instrumentation of the Lua call boundary itself.
const COUNTING_UI: &str = r#"
    CALLS = 0
    function render_ui(w, h)
        CALLS = CALLS + 1
        return { type = "text", content = "n=" .. CALLS }
    end
"#;

fn call_count(rt: &LuaRuntime) -> i64 {
    rt.get_global::<i64>("CALLS").unwrap_or(-1)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn load_dir_runs_files_in_alphabetical_order() {
    let dir = unique_temp_dir();
    // Deliberately written out of order on disk: alphabetical load order
    // must not depend on creation order, only on filename. This alone is
    // NOT proof the sort matters — `read_dir` already returns entries
    // sorted on some filesystems (observed on NTFS), so this passing does
    // not distinguish "sorted correctly" from "never sorted, got lucky".
    // `lua_files_in_sorts_regardless_of_readdir_order` below is the real
    // proof, checking the sort directly against a reversed input.
    std::fs::write(dir.join("90_last.lua"), "ORDER = (ORDER or '') .. 'c'").unwrap();
    std::fs::write(dir.join("00_first.lua"), "ORDER = (ORDER or '') .. 'a'").unwrap();
    std::fs::write(dir.join("10_second.lua"), "ORDER = (ORDER or '') .. 'b'").unwrap();

    let rt = LuaRuntime::new().unwrap();
    assert!(rt.load_dir(&dir));

    let order: String = rt.get_global("ORDER").unwrap();
    assert_eq!(order, "abc");

    std::fs::remove_dir_all(&dir).ok();
}

/// Direct proof the sort works, entirely decoupled from `read_dir`: a
/// filesystem-backed version of this test was tried first and passed even
/// with the `.sort()` call deleted, because `read_dir` on that filesystem
/// already returned entries alphabetically. Calling `sorted()` on a
/// synthetic, deliberately-reversed `Vec` cannot have that problem.
#[test]
fn sorted_reverses_a_reverse_ordered_input() {
    let input = vec![
        PathBuf::from("z_last.lua"),
        PathBuf::from("m_mid.lua"),
        PathBuf::from("a_first.lua"),
    ];
    let output = ConfigSource::sorted(input);
    assert_eq!(
        output,
        vec![
            PathBuf::from("a_first.lua"),
            PathBuf::from("m_mid.lua"),
            PathBuf::from("z_last.lua"),
        ]
    );
}

/// The filesystem-backed version, kept alongside the direct proof above
/// as an integration check that `lua_files_in` actually calls `sorted`
/// (not just that `sorted` itself works) and filters to `.lua` only.
#[test]
fn lua_files_in_sorts_and_filters_to_lua_extension() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("z_last.lua"), "").unwrap();
    std::fs::write(dir.join("m_mid.lua"), "").unwrap();
    std::fs::write(dir.join("a_first.lua"), "").unwrap();
    std::fs::write(dir.join("not_lua.txt"), "").unwrap();

    let files: Vec<String> = ConfigSource::lua_files_in(&dir)
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(files, vec!["a_first.lua", "m_mid.lua", "z_last.lua"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// A later file must see an earlier file's plain global — this is the
/// entire point of loading all files into one shared Lua state instead of
/// each in its own sandboxed environment.
#[test]
fn load_dir_files_share_globals() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("00_theme.lua"), "SHARED = 42").unwrap();
    std::fs::write(dir.join("10_uses.lua"), "USES_SHARED = SHARED + 1").unwrap();

    let rt = LuaRuntime::new().unwrap();
    assert!(rt.load_dir(&dir));

    let v: i64 = rt.get_global("USES_SHARED").unwrap();
    assert_eq!(v, 43);

    std::fs::remove_dir_all(&dir).ok();
}

/// An empty directory is not an error — same treatment as a missing
/// `tui.lua`, so an accidentally-created empty `tui/` does not brick the
/// TUI with a spurious error banner.
#[test]
fn load_dir_empty_is_not_an_error() {
    let dir = unique_temp_dir();
    let rt = LuaRuntime::new().unwrap();
    assert!(rt.load_dir(&dir));
    assert!(rt.last_error().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

/// A broken file must stop the load right there and report ITS error, not
/// let later files run against a half-initialized earlier one and produce
/// a confusing, unrelated error instead.
#[test]
fn load_dir_stops_at_first_broken_file() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("00_ok.lua"), "X = 1").unwrap();
    std::fs::write(dir.join("10_broken.lua"), "this is not lua {{{{").unwrap();
    std::fs::write(dir.join("20_unreached.lua"), "Y = 2").unwrap();

    let rt = LuaRuntime::new().unwrap();
    assert!(!rt.load_dir(&dir));
    assert!(
        rt.last_error().is_some(),
        "a broken file must leave an error for the status line"
    );

    let x: i64 = rt.get_global("X").unwrap();
    assert_eq!(x, 1);
    assert!(
        rt.get_global::<i64>("Y").is_none(),
        "the file after the broken one must not have run"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Non-`.lua` files in the directory must be ignored, not attempted as
/// Lua source (a stray `.bak`/`.md`/`README` sitting next to the configs
/// must not break loading).
#[test]
fn load_dir_ignores_non_lua_files() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("00_ok.lua"), "X = 1").unwrap();
    std::fs::write(dir.join("README.md"), "not lua at all {{{{").unwrap();

    let rt = LuaRuntime::new().unwrap();
    assert!(rt.load_dir(&dir));

    std::fs::remove_dir_all(&dir).ok();
}

/// `ConfigSource::resolve` must prefer a non-empty directory over the
/// single-file path, but fall back to the file when the directory is
/// empty or absent — an empty `tui/` must not silently replace a real
/// `tui.lua` with nothing.
#[test]
fn config_source_prefers_nonempty_dir_over_file() {
    let dir = unique_temp_dir();
    let file = dir.join("../tui_single_file_test.lua");
    std::fs::write(&file, "x = 1").unwrap();

    // Absent/empty dir -> the file wins.
    let empty_dir = dir.join("empty_subdir_does_not_exist");
    assert_eq!(
        ConfigSource::resolve(&empty_dir, &file),
        Some(ConfigSource::File(file.clone()))
    );

    // Directory exists but has no .lua files -> the file still wins.
    assert_eq!(
        ConfigSource::resolve(&dir, &file),
        Some(ConfigSource::File(file.clone()))
    );

    // Directory has a .lua file -> the directory wins.
    std::fs::write(dir.join("00_x.lua"), "x = 1").unwrap();
    assert_eq!(
        ConfigSource::resolve(&dir, &file),
        Some(ConfigSource::Dir(dir.clone()))
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&dir).ok();
}

/// `load_config` is what `App::init_lua` actually calls — it must route
/// to whichever `ConfigSource::resolve` picks, not just expose the two
/// primitives separately and trust every caller to pick correctly.
#[test]
fn load_config_routes_to_dir_when_populated() {
    let dir = unique_temp_dir();
    let file = dir.join("../tui_single_2.lua");
    std::fs::write(&file, "FROM_FILE = true").unwrap();
    std::fs::write(dir.join("00_a.lua"), "FROM_DIR = true").unwrap();

    let rt = LuaRuntime::new().unwrap();
    assert!(rt.load_config(&dir, &file));

    // Use eval_bool to check the Lua globals directly, bypassing ever_loaded check
    assert!(
        rt.eval_bool("return FROM_DIR == true").unwrap_or(false),
        "directory source must have loaded"
    );
    assert!(
        rt.eval_bool("return FROM_FILE == nil").unwrap_or(false),
        "the file must not ALSO load when the directory takes precedence"
    );

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn broken_lua_reports_failed_not_notdefined() {
    // Regression: a syntax error on the FIRST load left ever_loaded=false,
    // which looked like "no Lua config" and silently restored the full Rust
    // chrome (sidebar and all) instead of surfacing the error.
    let (rt, path) = runtime_with("function render_ui(w, h) this is not lua");
    let outcome = rt.build_ui_outcome(100, 40);
    std::fs::remove_file(&path).ok();

    match outcome {
        widgets::UiOutcome::Failed(msg) => {
            assert!(!msg.is_empty(), "error must carry a message to display");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn runtime_error_in_render_ui_is_failed() {
    let (rt, path) = runtime_with(r#"function render_ui(w, h) error("boom") end"#);
    let outcome = rt.build_ui_outcome(100, 40);
    std::fs::remove_file(&path).ok();

    assert!(
        matches!(outcome, widgets::UiOutcome::Failed(_)),
        "a throwing render_ui must not fall back to the Rust chrome"
    );
}

#[test]
fn invalid_tree_from_render_ui_is_failed() {
    // Returns a table, but not a widget Rust can parse.
    let (rt, path) = runtime_with(r#"function render_ui(w, h) return { nope = 1 } end"#);
    let outcome = rt.build_ui_outcome(100, 40);
    std::fs::remove_file(&path).ok();

    assert!(matches!(outcome, widgets::UiOutcome::Failed(_)));
}

#[test]
fn config_without_render_ui_is_notdefined() {
    // Valid Lua that simply does not define render_ui: Rust layout is correct.
    let (rt, path) = runtime_with("local x = 1");
    let outcome = rt.build_ui_outcome(100, 40);
    std::fs::remove_file(&path).ok();

    assert!(
        matches!(outcome, widgets::UiOutcome::NotDefined),
        "no render_ui means Rust owns layout, which is not an error"
    );
}

#[test]
fn valid_render_ui_is_ok() {
    let (rt, path) = runtime_with(
        r#"function render_ui(w, h)
             return { type = "native", view = "transcript" }
           end"#,
    );
    let outcome = rt.build_ui_outcome(100, 40);
    std::fs::remove_file(&path).ok();

    assert!(matches!(outcome, widgets::UiOutcome::Ok(_)));
}

#[test]
fn test_sandbox_no_io() {
    let rt = LuaRuntime::new().unwrap();

    // io should be nil
    assert_eq!(
        rt.eval_bool("return io == nil"),
        Some(true),
        "io must be removed from the sandbox"
    );

    // os.execute should be nil
    assert_eq!(
        rt.eval_bool("return os.execute == nil"),
        Some(true),
        "os.execute must be removed from the sandbox"
    );
}

#[test]
fn test_sandbox_safe_functions_available() {
    let rt = LuaRuntime::new().unwrap();

    // Safe functions should exist
    assert_eq!(
        rt.eval_bool("return type(pairs) == 'function'"),
        Some(true),
        "pairs must be available in the sandbox"
    );

    assert_eq!(
        rt.eval_bool("return type(tostring) == 'function'"),
        Some(true),
        "tostring must be available in the sandbox"
    );
}

#[test]
fn test_lua_to_json_table() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("name", "test").unwrap();
    t.set("count", 42).unwrap();

    let json = lua_value_to_json(&Value::Table(t)).unwrap();
    assert_eq!(json["name"], "test");
    assert_eq!(json["count"], 42);
}

#[test]
fn test_lua_to_json_array() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set(1, "a").unwrap();
    t.set(2, "b").unwrap();
    t.set(3, "c").unwrap();

    let json = lua_value_to_json(&Value::Table(t)).unwrap();
    assert!(json.is_array());
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], "a");
}

#[test]
fn test_call_string_undefined() {
    let rt = LuaRuntime::new().unwrap();
    // Never loaded, should return None
    assert!(rt.call_string("undefined_func").is_none());
}

#[test]
fn test_error_does_not_panic() {
    let rt = LuaRuntime::new().unwrap();

    // Load invalid Lua — should not panic, should return false.
    // We use load_source indirectly via load_file with a temp file.
    let mut path = std::env::temp_dir();
    path.push(format!(
        "kn9t_err_test_{}_{}.lua",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "this is not valid lua {{{{").unwrap();
    let ok = rt.load_file(&path);
    assert!(!ok, "invalid Lua must return false, not panic");
    assert!(rt.last_error().is_some(), "error must be recorded");

    // The runtime must still be usable after the error.
    std::fs::write(&path, "VALID = 1").unwrap();
    rt.clear_error();
    let ok2 = rt.load_file(&path);
    assert!(ok2, "valid Lua must succeed after a previous error");

    std::fs::remove_file(&path).ok();
}

/// The whole point of the cache: an unchanged fingerprint must not re-run
/// `render_ui`. Without this, the fix could regress to "cache exists but
/// nothing populates the fingerprint correctly" and still call Lua every
/// frame while merely *looking* like it has a cache.
#[test]
fn identical_state_reuses_cached_tree_without_calling_lua() {
    let (rt, path) = runtime_with(COUNTING_UI);

    let first = rt.build_ui_outcome(100, 40);
    assert!(matches!(first, widgets::UiOutcome::Ok(_)));
    assert_eq!(call_count(&rt), 1, "first call must run Lua");

    for _ in 0..5 {
        let outcome = rt.build_ui_outcome(100, 40);
        assert!(matches!(outcome, widgets::UiOutcome::Ok(_)));
    }
    assert_eq!(
        call_count(&rt),
        1,
        "identical width/height/state must hit the cache, not re-run render_ui"
    );

    std::fs::remove_file(&path).ok();
}

/// A fingerprint field that actually changes (message_count, via
/// `update_state`) must force a rebuild — the cache must not be a wall
/// that never comes down.
#[test]
fn changed_message_count_invalidates_the_cache() {
    let (rt, path) = runtime_with(COUNTING_UI);

    rt.update_state(&StateSnapshot {
        message_count: 1,
        ..Default::default()
    });
    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 1);

    rt.update_state(&StateSnapshot {
        message_count: 2,
        ..Default::default()
    });
    rt.build_ui_outcome(100, 40);
    assert_eq!(
        call_count(&rt),
        2,
        "a real state change must invalidate the cache"
    );

    std::fs::remove_file(&path).ok();
}

/// A terminal resize must always force a rebuild, even with otherwise
/// identical state — width/height are `render_ui`'s own parameters.
#[test]
fn resize_invalidates_the_cache() {
    let (rt, path) = runtime_with(COUNTING_UI);

    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 1);
    rt.build_ui_outcome(101, 40);
    assert_eq!(call_count(&rt), 2, "width change must invalidate");
    rt.build_ui_outcome(101, 41);
    assert_eq!(call_count(&rt), 3, "height change must invalidate");

    std::fs::remove_file(&path).ok();
}

/// `kn9t.invalidate()` is the only way to force a rebuild for state Rust's
/// fingerprint cannot see (a Lua-local toggle). It must actually work, or
/// every `SHOW.foo = not SHOW.foo` keybind in a config silently does
/// nothing until some unrelated field changes.
#[test]
fn manual_invalidate_forces_a_rebuild() {
    let (rt, path) = runtime_with(COUNTING_UI);

    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 1);
    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 1, "still cached before invalidate");

    rt.eval_bool("kn9t.invalidate(); return true").unwrap();
    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 2, "invalidate() must force a rebuild");

    std::fs::remove_file(&path).ok();
}

/// Saving the config file (hot-reload) must not resurrect a tree built by
/// the *previous* file just because the fingerprint happens to match.
#[test]
fn reload_invalidates_the_cache_even_with_unchanged_fingerprint() {
    let (rt, path) = runtime_with(COUNTING_UI);

    rt.build_ui_outcome(100, 40);
    assert_eq!(call_count(&rt), 1);

    // Re-load the exact same source, simulating a no-op save.
    rt.load_file(&path);
    rt.build_ui_outcome(100, 40);
    assert_eq!(
        call_count(&rt),
        1,
        "CALLS resets to 0 then increments to 1 on the reload's own first \
         build - a stale cache would have skipped this call and left the \
         counter's Lua global at its pre-reload value of 1 too, so this \
         assertion alone doesn't distinguish them"
    );
    // The real proof: build again without reloading. If the reload had
    // populated the cache from a stale pre-reload tree, this second call
    // would still report a cache hit with count staying at 1 - which is
    // indistinguishable from the correct behaviour above. So instead,
    // assert the source can be observably different post-reload.
    std::fs::write(
        &path,
        "CALLS = 100\nfunction render_ui(w,h) CALLS = CALLS + 1; return {type='text', content='x'} end",
    )
    .unwrap();
    rt.load_file(&path);
    rt.build_ui_outcome(100, 40);
    assert_eq!(
        call_count(&rt),
        101,
        "post-reload build must run the NEW render_ui, not a cached tree \
         from the old one"
    );

    std::fs::remove_file(&path).ok();
}
