//! The background poll loop, bootstrapped once per plugin process by the
//! first `git_status` tool call — see `tool.rs` for why it has to work this
//! way (no session-start hook, no `ctx` on `PluginHook::call`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kn9t_plugin_sdk::ctx::HostApiClient;

use crate::git;
use crate::ui;

/// How often to re-poll while a session is open. Git status is cheap
/// (`--porcelain=v2` on a typical repo is a few ms), so this can be short
/// without meaningfully loading the machine.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Start the background poller for one repository, if one is not already
/// running for it. Returns immediately either way — the actual polling
/// happens on a spawned thread.
///
/// Guarded per-`cwd` (not globally) so a host juggling multiple sessions in
/// different repositories gets one poller each, not one poller pinned to
/// whichever repo happened to bootstrap first.
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
    // Registered once per (host process, repo) pair; ui_register_lua on the
    // host side replaces any previous registration for this plugin, so
    // re-sending on every poll would be wasted work, not incorrect — this
    // just avoids that waste.
    let registered = AtomicBool::new(false);

    // The `host` client is bound to the session id of the tool call that
    // bootstrapped this thread (HostApiClient auto-injects it). There is no
    // "session ended" signal available to this thread — the plugin protocol
    // has no session-teardown hook either — so this stops itself instead on
    // repeated push failure, which is what a closed/gone session looks like
    // from here (`ui_set_state` starts erroring once the session is gone).
    // Known limitation: a session that ends cleanly still costs up to
    // MAX_CONSECUTIVE_FAILURES * POLL_INTERVAL of pointless polling before
    // this notices. Acceptable for a first version; a real fix needs a
    // session-lifecycle signal the protocol does not currently provide.
    const MAX_CONSECUTIVE_FAILURES: u32 = 10;
    let mut consecutive_failures = 0u32;

    loop {
        if !registered.load(Ordering::Relaxed) {
            if host
                .call(
                    "ui_register_lua",
                    serde_json::json!({ "source": ui::LUA_SOURCE }),
                )
                .is_ok()
            {
                registered.store(true, Ordering::Relaxed);
            }
        }

        let state = git::collect(&cwd);
        let payload = ui::state_to_json(state.as_ref());
        match host.call("ui_set_state", serde_json::json!({ "state": payload })) {
            Ok(_) => consecutive_failures = 0,
            Err(_) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return;
                }
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
