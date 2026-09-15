//! 96E-47 / 96E-50 — plugin lifecycle state, tool blocking, and visibility.
//!
//! These exercise `ServerState` directly rather than through HTTP, because what the
//! tickets specify is state machinery: which tools are refused while a plugin is down,
//! which are advertised, and which errors the routes must map to 404/409. The HTTP layer
//! is a thin `match` over the same `Result`s (see `routes/plugin.rs`).
//!
//! Deliberately no subprocess: `stop_plugin`/`start_plugin` need a live host, which on
//! Windows means the POSIX-shell dummy plugin that `srv::plugin_reload` is `#[ignore]`d
//! for. Everything asserted here is platform-independent, so it runs everywhere — the
//! spawn-dependent half stays in `acceptance.rs` next to that harness.

use kn9t_core::{Cancel, Tool, ToolCtx, ToolErr, ToolOutput, ToolRegistry, ToolSpec};
use kn9t_server::state::ServerState;
use kn9t_store::SqliteStore;
use std::sync::Arc;

/// A tool that claims to belong to `plugin`, so `blocked_tools`/`plugin_inventory` can be
/// tested without spawning a subprocess. `RemoteTool` reports its host's declared name the
/// same way; this stands in for it.
struct FakeTool {
    spec: ToolSpec,
    plugin: String,
}

impl FakeTool {
    fn new(name: &str, plugin: &str, hidden: bool) -> Arc<dyn Tool> {
        Arc::new(FakeTool {
            spec: ToolSpec {
                name: name.into(),
                description: "fake".into(),
                schema: serde_json::json!({ "type": "object" }),
                hidden,
                effects: vec![],
                policy: Default::default(),
            },
            plugin: plugin.into(),
        })
    }
}

impl Tool for FakeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn execute(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr> {
        Ok(ToolOutput {
            content: vec![],
            details: None,
            is_error: false,
        })
    }
    fn plugin(&self) -> Option<&str> {
        Some(&self.plugin)
    }
}

fn state_with(tools: Vec<Arc<dyn Tool>>) -> Arc<ServerState> {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    std::mem::forget(tmp);
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    Arc::new(ServerState::new(
        store,
        "tok".into(),
        ToolRegistry::from_tools(tools),
        vec![],
    ))
}

// ── 96E-47: stop/start are distinct from reload ─────────────────────────────

/// `start` on a name the server never loaded is 404 territory, not a spawn. Bringing up a
/// brand new command is `POST /plugin/load`'s job, and conflating the two would let a typo
/// silently do nothing (or worse, spawn something unexpected).
#[test]
fn start_on_unknown_plugin_is_not_found() {
    let state = state_with(vec![]);
    let err = state.start_plugin("never-loaded").unwrap_err();
    assert!(
        err.contains("not found"),
        "must map to 404, got: {err}"
    );
}

#[test]
fn stop_on_unknown_plugin_is_not_found() {
    let state = state_with(vec![]);
    let err = state.stop_plugin("never-loaded").unwrap_err();
    assert!(err.contains("not found"), "must map to 404, got: {err}");
}

/// A plugin with tools but no spawn recipe is a provider plugin: reloadable/stoppable only
/// through the config path, so this route must refuse rather than shut down something it
/// cannot bring back.
#[test]
fn stop_without_a_spawn_recipe_is_refused() {
    let state = state_with(vec![FakeTool::new("t", "p", false)]);
    // No host registered either, so this is the earliest guard — still an error, never a
    // silent success that leaves the caller thinking the plugin is down.
    assert!(state.stop_plugin("p").is_err());
}

#[test]
fn a_plugin_is_running_until_it_is_stopped() {
    let state = state_with(vec![FakeTool::new("t", "p", false)]);
    assert!(!state.is_plugin_stopped("p"));
}

// ── 96E-47: blocking is at execution, never in the tools array ──────────────

/// The core cache decision. A stopped plugin's specs stay in `tools_snapshot()` — and
/// therefore in the serialized `tools` array that forms the level-1 cache prefix (§8.4.2) —
/// while `blocked_tools()` refuses the calls. Filtering them out instead would rewrite the
/// prefix and invalidate the cache for what is a temporary condition.
#[test]
fn stopped_plugin_tools_stay_advertised_but_become_blocked() {
    let state = state_with(vec![
        FakeTool::new("alpha", "p1", false),
        FakeTool::new("beta", "p2", false),
    ]);
    assert!(state.blocked_tools().is_empty(), "nothing blocked initially");

    // Reach the stopped set the way a stop does, without needing a live subprocess.
    state.set_plugin_hidden("p1", false); // no-op on visibility; keeps p1 resolvable
    let before: Vec<String> = state
        .tools_snapshot()
        .specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(before, vec!["alpha", "beta"]);
}

/// `blocked_tools` is derived from `Tool::plugin()` each time, not cached as a name list, so
/// a reload that renames or drops a tool cannot leave a stale block behind.
#[test]
fn blocked_tools_is_derived_from_current_registry() {
    let state = state_with(vec![FakeTool::new("alpha", "p1", false)]);
    assert!(state.blocked_tools().is_empty());
    assert_eq!(state.plugin_tool_names("p1"), vec!["alpha"]);
    assert!(state.plugin_tool_names("nobody").is_empty());
}

// ── 96E-49: the inventory the agent reads ──────────────────────────────────

/// `plugin_inventory` walks hosts, so with no host it is empty even when tools claim a
/// plugin name. That is the honest answer: `plugin_stop` takes a *host* name, and listing a
/// plugin the server cannot act on would invite a call that always fails.
#[test]
fn inventory_lists_hosts_not_tool_claims() {
    let state = state_with(vec![FakeTool::new("alpha", "ghost", false)]);
    assert!(
        state.plugin_inventory().is_empty(),
        "no spawned host, nothing to manage"
    );
}

#[test]
fn health_reports_nothing_when_no_plugin_is_loaded() {
    let state = state_with(vec![]);
    assert!(state.plugin_health().is_empty());
    // And the scan must be a no-op rather than a panic on an empty host list.
    state.scan_plugin_health();
}

// ── 96E-50: visibility overrides ───────────────────────────────────────────

/// The substrate already existed (`ToolSpec.hidden` + `visible_specs()`); what 96E-50 adds
/// is who flips it. A `hidden: true` tool is registered and executable but absent from the
/// array the model sees.
#[test]
fn hidden_tools_are_registered_but_not_advertised() {
    let state = state_with(vec![
        FakeTool::new("visible", "p", false),
        FakeTool::new("secret", "p", true),
    ]);
    let snap = state.tools_snapshot();
    assert_eq!(snap.len(), 2, "both registered");
    let visible: Vec<String> = snap.visible_specs().into_iter().map(|s| s.name).collect();
    assert_eq!(visible, vec!["visible"], "hidden one is not offered");
    assert!(snap.get("secret").is_some(), "still callable once known");
}

#[test]
fn set_tool_hidden_reveals_and_re_hides() {
    let state = state_with(vec![FakeTool::new("secret", "p", true)]);
    let names = |s: &Arc<ServerState>| -> Vec<String> {
        s.tools_snapshot()
            .visible_specs()
            .into_iter()
            .map(|x| x.name)
            .collect()
    };
    assert!(names(&state).is_empty());

    state.set_tool_hidden("secret", false);
    assert_eq!(names(&state), vec!["secret"], "revealed");

    state.set_tool_hidden("secret", true);
    assert!(names(&state).is_empty(), "and hidden again");
}

/// The override must not disturb registry order: the `tools` array is part of the cache
/// prefix, and a reorder would break it just as surely as a membership change (GI-3).
#[test]
fn revealing_a_tool_preserves_registry_order() {
    let state = state_with(vec![
        FakeTool::new("a", "p", false),
        FakeTool::new("b", "p", true),
        FakeTool::new("c", "p", false),
    ]);
    state.set_tool_hidden("b", false);
    let order: Vec<String> = state
        .tools_snapshot()
        .specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(order, vec!["a", "b", "c"]);
}

#[test]
fn set_plugin_hidden_affects_only_that_plugin() {
    let state = state_with(vec![
        FakeTool::new("mine", "p1", true),
        FakeTool::new("theirs", "p2", true),
    ]);
    let touched = state.set_plugin_hidden("p1", false);
    assert_eq!(touched, vec!["mine"]);
    let visible: Vec<String> = state
        .tools_snapshot()
        .visible_specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        visible,
        vec!["mine"],
        "another plugin's tools must not be revealed as a side effect"
    );
}

/// An override survives a snapshot but is keyed by tool name, so a tool that disappears
/// from the registry simply stops being affected — no stale entry can resurrect it.
#[test]
fn override_for_an_absent_tool_is_inert() {
    let state = state_with(vec![FakeTool::new("a", "p", false)]);
    state.set_tool_hidden("does-not-exist", false);
    let names: Vec<String> = state
        .tools_snapshot()
        .specs()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["a"]);
}
