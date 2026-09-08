//! The background poll loop, started once per repository by the `get_steering`
//! hook — see `bootstrap.rs` for why a lifecycle hook rather than a tool call.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kn9t_plugin_sdk::ctx::HostApiClient;

use crate::diff::{self, DiffTarget};
use crate::git;
use crate::tool;
use crate::ui;

fn log_message(msg: &str) {
    // Try to write to ~/.kn9t/git-integration.log
    if let Ok(home) = std::env::var("HOME") {
        let log_path = PathBuf::from(home).join(".kn9t/git-integration.log");
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(file, "[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
        }
    } else if let Ok(temp) = std::env::var("TEMP") {
        // Windows fallback
        let log_path = PathBuf::from(temp).join("kn9t-git-integration.log");
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(file, "[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
        }
    }
    // Also stderr
    eprintln!("{}", msg);
}

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
    pub show_commit: Option<String>,  // SHA of commit to show (via git show)
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
    let target = state.lock().unwrap().diff_target.clone();
    target
}

/// Request loading a commit diff. Called from the event sink.
pub fn request_commit_diff(cwd: &PathBuf, sha: String) {
    let state = get_or_create_state(cwd);
    let mut s = state.lock().unwrap();
    s.show_commit = Some(sha);
    s.force_refresh = true;
}

/// Clear the requested commit diff. Called from the event sink.
pub fn clear_commit_diff(cwd: &PathBuf) {
    let state = get_or_create_state(cwd);
    let mut s = state.lock().unwrap();
    s.show_commit = None;
    s.force_refresh = true;
}

/// Start the background poller for a session, if one is not already running.
///
/// Guarded per-`session_id` so each TUI session gets its own poller, even if
/// they share the same cwd. This fixes the bug where only the first session
/// would see the git panel.
///
/// Called from a hook that fires every turn, so this is deliberately cheap and
/// idempotent: the common case is "already running, do nothing".
pub fn ensure_started(host: HostApiClient, cwd: PathBuf, session_id: Option<String>) {
    use std::collections::HashSet;
    static STARTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    // Use session_id if available, otherwise fall back to cwd string
    let key = session_id.unwrap_or_else(|| cwd.to_string_lossy().to_string());

    let mut guard = STARTED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if !set.insert(key) {
        return; // Already running for this session.
    }
    drop(guard);

    let shared_state = get_or_create_state(&cwd);
    std::thread::spawn(move || run(host, cwd, shared_state));
}

fn run(host: HostApiClient, cwd: PathBuf, shared_state: SharedState) {
    log_message(&format!("[git-poller] Starting poller for: {}", cwd.display()));
    const MAX_CONSECUTIVE_FAILURES: u32 = 10;
    let mut consecutive_failures = 0u32;

    let mut tick = 0u32;
    let mut files: Vec<diff::DiffFile> = Vec::new();
    let mut last_target = DiffTarget::default();
    let mut last_requested_sha: Option<String> = None;
    let mut commit_diffs: HashMap<String, Vec<diff::DiffFile>> = HashMap::new();

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
        
        // Check if a commit diff was requested (via event sink or fallback tmp file)
        let requested_sha = {
            let s = shared_state.lock().unwrap();
            s.show_commit.clone()
        };
        
        if let Some(ref sha) = requested_sha {
            if last_requested_sha.as_ref() != Some(sha) {
                log_message(&format!("[git-poller] Loading diff for commit: {}", sha));
                last_requested_sha = Some(sha.clone());
                commit_diffs.clear();
                
                match std::process::Command::new("git")
                    .current_dir(&cwd)
                    .args(["show", "--format=", sha])
                    .output()
                {
                    Ok(output) => {
                        log_message(&format!("[git-poller] git show exit code: {}", output.status));
                        match String::from_utf8(output.stdout) {
                            Ok(text) => {
                                log_message(&format!("[git-poller] Diff output: {} bytes", text.len()));
                                let commit_files = diff::parse(&text);
                                log_message(&format!("[git-poller] Parsed {} files", commit_files.len()));
                                commit_diffs.insert(sha.clone(), commit_files);
                            }
                            Err(e) => {
                                log_message(&format!("[git-poller] UTF-8 decode error: {}", e));
                            }
                        }
                    }
                    Err(e) => {
                        log_message(&format!("[git-poller] git show failed: {}", e));
                    }
                }
            }
        } else if last_requested_sha.is_some() {
            log_message("[git-poller] Clearing requested commit diff");
            commit_diffs.clear();
            last_requested_sha = None;
        }
        
        tick = tick.wrapping_add(1);

        let payload = ui::state_to_json(state.as_ref(), &files, &current_target, &commit_diffs);
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
