//! The background poll loop, started once per repository by the `get_steering`
//! hook — see `bootstrap.rs` for why a lifecycle hook rather than a tool call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kn9t_plugin_sdk::ctx::HostApiClient;

use crate::diff::{self, DiffTarget};
use crate::git;
use crate::ui;

/// How often to re-poll while a session is open. Status is cheap
/// (`--porcelain=v2` on a typical repo is a few ms), so this can be short
/// without meaningfully loading the machine.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Re-read the diff every Nth status poll.
///
/// `git diff` is markedly more expensive than `git status` on a large working
/// tree, and a review panel does not need 3-second freshness the way a branch
/// indicator does.
const DIFF_EVERY: u32 = 4;

/// Shared state that the Lua UI can update via requests.
#[derive(Default)]
pub struct PollerState {
    pub diff_target: DiffTarget,
    pub force_refresh: bool,
}

type SharedState = Arc<Mutex<PollerState>>;

/// Global registry of poller states per cwd.
static POLLER_STATES: Mutex<Option<HashMap<PathBuf, SharedState>>> = Mutex::new(None);

/// Get or create a shared state for a cwd.
fn get_or_create_state(cwd: &PathBuf) -> SharedState {
    let mut guard = POLLER_STATES.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.entry(cwd.clone())
        .or_insert_with(|| Arc::new(Mutex::new(PollerState::default())))
        .clone()
}

/// Set the diff target for a repository. Called from tool handlers.
pub fn set_diff_target(cwd: &PathBuf, target: DiffTarget) {
    let state = get_or_create_state(cwd);
    let mut s = state.lock().unwrap();
    s.diff_target = target;
    s.force_refresh = true;
}

/// Get the current diff target for a repository.
pub fn get_diff_target(cwd: &PathBuf) -> DiffTarget {
    let state = get_or_create_state(cwd);
    state.lock().unwrap().diff_target.clone()
}

/// Start the background poller for one repository, if one is not already
/// running for it. Returns immediately either way.
///
/// Guarded per-`cwd` (not globally) so a host juggling multiple sessions in
/// different repositories gets one poller each, not one poller pinned to
/// whichever repo happened to bootstrap first.
///
/// Called from a hook that fires every turn, so this is deliberately cheap and
/// idempotent: the common case is "already running, do nothing".
pub fn ensure_started(host: HostApiClient, cwd: PathBuf) {
    use std::collections::HashSet;
    static STARTED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    let mut guard = STARTED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if !set.insert(cwd.clone()) {
        return; // Already running for this cwd.
    }
    drop(guard);

    let shared_state = get_or_create_state(&cwd);
    std::thread::spawn(move || run(host, cwd, shared_state));
}

fn run(host: HostApiClient, cwd: PathBuf, shared_state: SharedState) {
    const MAX_CONSECUTIVE_FAILURES: u32 = 10;
    let mut consecutive_failures = 0u32;

    let mut tick = 0u32;
    let mut files: Vec<diff::DiffFile> = Vec::new();
    let mut last_target = DiffTarget::default();

    loop {
        let registered = host
            .call(
                "ui_register_lua",
                serde_json::json!({
                    "source": ui::LUA_SOURCE,
                    "placement": "main",
                    "title": "Git",
                    "rows": 30,
                }),
            )
            .is_ok();

        let state = git::collect(&cwd);

        // Check if we need to refresh (target changed or force refresh)
        let (current_target, force_refresh) = {
            let mut s = shared_state.lock().unwrap();
            let target = s.diff_target.clone();
            let force = s.force_refresh;
            s.force_refresh = false;
            (target, force)
        };

        let target_changed = current_target != last_target;
        last_target = current_target.clone();

        // Refresh the diff on the first pass, every DIFF_EVERY ticks,
        // when target changes, or on force refresh.
        if tick % DIFF_EVERY == 0 || target_changed || force_refresh {
            files = diff::collect_with_target(&cwd, &current_target);
        }
        tick = tick.wrapping_add(1);

        let payload = ui::state_to_json(state.as_ref(), &files, &current_target);
        let pushed = host
            .call("ui_set_state", serde_json::json!({ "state": payload }))
            .is_ok();

        if registered || pushed {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                return;
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
