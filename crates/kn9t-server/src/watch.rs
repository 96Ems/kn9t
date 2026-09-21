//! R-SRV-CFG-110 — config file watcher: auto-reload on `config.toml` edits.
//!
//! An mtime poll on a dedicated OS thread, not `notify`. Three reasons:
//!
//! 1. `notify` is not in the DESIGN §15 dependency budget (~5 transitive crates),
//!    and Principle 5 is a budget, not a preference.
//! 2. Its platform backends want an event loop; GI-5 forbids async.
//! 3. The watched file is edited by a human in an editor. A 2 s poll is
//!    imperceptible, and the debounce below matters far more than latency: editors
//!    write in bursts (truncate-then-write, or write-temp-then-rename), so a
//!    naive "mtime changed → reload" fires mid-write on a half-written file.
//!
//! The debounce requires the mtime to be *stable* for one poll interval before
//! reloading, which is what makes a partial read unlikely — and harmless when it
//! happens: a truncated TOML fails to parse, and `reload_config` keeps the previous
//! config on any error, so a bad save is never destructive.
//!
//! The decision logic is [`Debouncer`], a pure state machine with no clock, no
//! filesystem and no `ServerState`, so it is tested directly rather than through a
//! live thread with sleeps.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::state::ServerState;

/// Poll interval. Also the debounce quiet-period.
const POLL: Duration = Duration::from_secs(2);

/// What the watcher should do after one poll.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing changed, or a change is still settling.
    Wait,
    /// The file has settled after a change — reload now.
    Reload,
}

/// Debounce state machine over successive mtime observations.
///
/// `None` observations (file missing) are deliberately *not* treated as a change:
/// an editor doing delete-then-write would otherwise trigger a reload on the gap,
/// against a file that does not exist yet.
#[derive(Debug)]
pub struct Debouncer {
    last_seen: Option<SystemTime>,
    /// A change observed but not yet acted on. `None` = steady state.
    pending: Option<SystemTime>,
}

impl Debouncer {
    pub fn new(initial: Option<SystemTime>) -> Self {
        Self {
            last_seen: initial,
            pending: None,
        }
    }

    /// Feed one observation; get the action for this poll.
    pub fn observe(&mut self, current: Option<SystemTime>) -> Action {
        let Some(current) = current else {
            return Action::Wait;
        };

        match (self.last_seen, self.pending) {
            // Unchanged, nothing pending: steady state.
            (Some(seen), None) if seen == current => Action::Wait,

            // Change first observed: arm the debounce, do not reload yet.
            (_, None) => {
                self.pending = Some(current);
                Action::Wait
            }

            // Pending change and mtime has settled: the write is complete.
            (_, Some(p)) if p == current => {
                self.pending = None;
                self.last_seen = Some(current);
                Action::Reload
            }

            // Pending change, mtime moved again: still being written. Re-arm.
            (_, Some(_)) => {
                self.pending = Some(current);
                Action::Wait
            }
        }
    }
}

/// Spawn the watcher thread. Returns immediately.
///
/// The thread exits when the server sets `stop_requested`, so it does not keep a
/// process alive past idle-exit. It holds an `Arc<ServerState>`, which is why it
/// must observe that flag rather than block forever.
pub fn spawn_config_watcher(state: Arc<ServerState>, path: PathBuf) {
    std::thread::Builder::new()
        .name("kn9t-config-watch".into())
        .spawn(move || run(state, path))
        .map(|_| ())
        .unwrap_or_else(|e| {
            crate::log!("config-watch: failed to spawn watcher thread: {e}");
        });
}

fn run(state: Arc<ServerState>, path: PathBuf) {
    let mut deb = Debouncer::new(mtime(&path));

    crate::log!(
        "config-watch: watching {} (poll {}s)",
        path.display(),
        POLL.as_secs()
    );

    loop {
        std::thread::sleep(POLL);

        if state.stop_requested.load(Ordering::SeqCst) {
            crate::log!("config-watch: stopping");
            return;
        }

        if deb.observe(mtime(&path)) == Action::Reload {
            reload(&state);
        }
    }
}

fn reload(state: &Arc<ServerState>) {
    crate::log!("config-watch: config.toml changed, reloading");
    match state.reload_config() {
        Ok((providers, models)) => {
            crate::log!("config-watch: reloaded {providers} provider(s), {models} model(s)");
        }
        Err(e) => {
            // Keep serving the old config. A syntax error mid-edit is expected and
            // must not take the server down.
            crate::log!("config-watch: reload failed, keeping previous config: {e}");
        }
    }
}

fn mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
