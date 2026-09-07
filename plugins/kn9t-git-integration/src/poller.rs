//! The background poll loop, started once per repository by the `get_steering`
//! hook — see `bootstrap.rs` for why a lifecycle hook rather than a tool call.

use std::path::PathBuf;
use std::time::Duration;

use kn9t_plugin_sdk::ctx::HostApiClient;

use crate::diff;
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
    use std::sync::Mutex;
    static STARTED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

    let mut guard = STARTED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if !set.insert(cwd.clone()) {
        return; // Already running for this cwd.
    }
    drop(guard);

    std::thread::spawn(move || run(host, cwd));
}

fn run(host: HostApiClient, cwd: PathBuf) {
    // The `host` client is bound to the session id of the hook that
    // bootstrapped this thread (HostApiClient auto-injects it). There is no
    // "session ended" signal available to this thread — the plugin protocol
    // has no session-teardown hook — so this stops itself instead on repeated
    // push failure, which is what a closed/gone session looks like from here
    // (`ui_set_state` starts erroring once the session is gone).
    // Known limitation: a session that ends cleanly still costs up to
    // MAX_CONSECUTIVE_FAILURES * POLL_INTERVAL of pointless polling before
    // this notices. Acceptable for now; a real fix needs a session-lifecycle
    // signal the protocol does not currently provide.
    const MAX_CONSECUTIVE_FAILURES: u32 = 10;
    let mut consecutive_failures = 0u32;

    let mut tick = 0u32;
    let mut files: Vec<diff::DiffFile> = Vec::new();

    loop {
        // Re-sent every poll, deliberately.
        //
        // The obvious optimisation — send once, remember it with a flag — is
        // wrong: the *TUI* owns the registry, and it is a separate process that
        // can restart (or reload its Lua) at any time. When it does, its
        // `plugin_ui` map is empty again while this thread still believes it has
        // registered, so every subsequent `ui_set_state` lands on a plugin with
        // no `render()`. The panel then shows "awaiting ui_register_lua"
        // forever, with no way to recover short of restarting the server.
        //
        // The protocol has no "client reattached" signal to key off, so
        // idempotent re-sending is the only thing that self-heals. The cost is
        // ~12 KB over a local pipe every few seconds, and the host's
        // `register()` replaces the entry rather than accumulating.
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

        // Refresh the diff on the first pass and every DIFF_EVERY ticks after,
        // reusing the previous parse in between.
        if tick % DIFF_EVERY == 0 {
            files = diff::collect(&cwd);
        }
        tick = tick.wrapping_add(1);

        let payload = ui::state_to_json(state.as_ref(), &files);
        let pushed = host
            .call("ui_set_state", serde_json::json!({ "state": payload }))
            .is_ok();

        // Only a failure of *both* counts as "the session is gone": a transient
        // error on one of the two would otherwise creep toward the give-up
        // threshold during a perfectly healthy session.
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
