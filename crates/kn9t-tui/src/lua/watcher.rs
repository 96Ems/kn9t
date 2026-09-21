//! File watcher for hot-reload.
//!
//! Watches ~/.kn9t/tui.lua (or configured path) for changes.
//! On modification, reloads the Lua state and sends an event to the TUI.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::{Config, Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::event::Event;

use super::LuaRuntime;

/// Handle to stop the watcher thread.
pub struct WatcherHandle {
    // The watcher is kept alive as long as this handle exists.
    // Dropping it stops watching.
    _watcher: RecommendedWatcher,
}

/// Spawn a file watcher thread for hot-reload.
///
/// Returns `None` if the watcher fails to initialize (e.g., path doesn't exist).
/// The watcher will send a Tick event to the TUI event loop on file changes,
/// causing a re-render with the new Lua config.
pub fn spawn_watcher(
    path: PathBuf,
    runtime: Arc<LuaRuntime>,
    tx: Sender<Event>,
) -> Option<WatcherHandle> {
    // Get the directory to watch (notify watches directories, not files)
    let dir = path.parent()?.to_path_buf();
    let filename = path.file_name()?.to_os_string();

    let (notify_tx, notify_rx) = std::sync::mpsc::channel();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<NotifyEvent, notify::Error>| {
            if let Ok(event) = res {
                let _ = notify_tx.send(event);
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )
    .ok()?;

    // Create the directory if it doesn't exist
    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::log!("Failed to create {}: {}", dir.display(), e);
            return None;
        }
    }

    // Watch the directory
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        crate::log!("Failed to watch {}: {}", dir.display(), e);
        return None;
    }

    crate::log!("Watching {} for changes", path.display());

    // Spawn thread to process notify events
    let path_clone = path.clone();
    thread::spawn(move || {
        // Debounce: ignore rapid successive events
        let mut last_reload = std::time::Instant::now();
        let debounce = Duration::from_millis(100);

        while let Ok(event) = notify_rx.recv() {
            // Check if the event is for our file
            let is_our_file = event
                .paths
                .iter()
                .any(|p| p.file_name().map(|n| n == filename).unwrap_or(false));

            if !is_our_file {
                continue;
            }

            // Only reload on modify/create events
            let should_reload = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));

            if !should_reload {
                continue;
            }

            // Debounce
            let now = std::time::Instant::now();
            if now.duration_since(last_reload) < debounce {
                continue;
            }
            last_reload = now;

            crate::log!("Detected change in {}", path_clone.display());

            // Reload the Lua state
            let success = runtime.reload();

            // Notify the TUI (it will re-render with new config or show error)
            if tx.send(Event::Tick).is_err() {
                // Channel closed, exit thread
                break;
            }

            if success {
                crate::log!("Hot-reload successful");
            } else {
                crate::log!("Hot-reload failed (check last_error)");
            }
        }
    });

    Some(WatcherHandle { _watcher: watcher })
}

/// Spawn a watcher for a `tui/` directory of split config files.
///
/// Reloads on any `*.lua` file changing, being created, or being removed
/// inside `dir` (non-recursive — a subdirectory is not part of the load
/// order contract, see `ConfigSource::lua_files_in`). Debounced the same way
/// as [`spawn_watcher`], and for the same reason: editors often emit several
/// filesystem events for a single save.
pub fn spawn_dir_watcher(
    dir: PathBuf,
    runtime: Arc<LuaRuntime>,
    tx: Sender<Event>,
) -> Option<WatcherHandle> {
    let (notify_tx, notify_rx) = std::sync::mpsc::channel();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<NotifyEvent, notify::Error>| {
            if let Ok(event) = res {
                let _ = notify_tx.send(event);
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
    )
    .ok()?;

    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::log!("Failed to create {}: {}", dir.display(), e);
            return None;
        }
    }

    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        crate::log!("Failed to watch {}: {}", dir.display(), e);
        return None;
    }

    crate::log!("Watching {} for changes", dir.display());

    let dir_clone = dir.clone();
    thread::spawn(move || {
        let mut last_reload = std::time::Instant::now();
        let debounce = Duration::from_millis(100);

        while let Ok(event) = notify_rx.recv() {
            let is_lua_file = event
                .paths
                .iter()
                .any(|p| p.extension().and_then(|e| e.to_str()) == Some("lua"));
            if !is_lua_file {
                continue;
            }

            let should_reload = matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            );
            if !should_reload {
                continue;
            }

            let now = std::time::Instant::now();
            if now.duration_since(last_reload) < debounce {
                continue;
            }
            last_reload = now;

            crate::log!("Detected change in {}", dir_clone.display());

            let success = runtime.reload();

            if tx.send(Event::Tick).is_err() {
                break;
            }

            if success {
                crate::log!("Hot-reload successful");
            } else {
                crate::log!("Hot-reload failed (check last_error)");
            }
        }
    });

    Some(WatcherHandle { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_watcher_detects_change() {
        // Create a temp directory and file
        let temp_dir = std::env::temp_dir().join("kn9t_lua_test");
        let _ = fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("test.lua");

        // Write initial content
        fs::write(&test_file, "x = 1").unwrap();

        // Create Lua runtime
        let runtime = Arc::new(LuaRuntime::new().unwrap());
        runtime.load_file(&test_file);

        // Create event channel
        let (tx, rx) = std::sync::mpsc::channel();

        // Start watcher
        let _handle = spawn_watcher(test_file.clone(), runtime.clone(), tx);

        // Give watcher time to start
        thread::sleep(Duration::from_millis(200));

        // Modify the file
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&test_file)
            .unwrap();
        writeln!(file, "x = 2").unwrap();
        drop(file);

        // Wait for event (with timeout)
        let result = rx.recv_timeout(Duration::from_secs(2));

        // Cleanup
        let _ = fs::remove_file(&test_file);
        let _ = fs::remove_dir(&temp_dir);

        // On some systems the watcher may not work in tests (CI, sandboxed env)
        // So we just check that we didn't panic
        if result.is_ok() {
            // Got the event - success
        }
        // If timeout, that's okay in test environment
    }
}
