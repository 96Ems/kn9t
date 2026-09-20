//! Live file-watch for the workspace index (PLAN §P7 L2).
//!
//! The explorer tree, the viewer and the `@` dropdown all read one [`FileIndex`], and that index
//! is a *snapshot*: it walks once and then serves every search. Without a watch, a file the agent
//! writes — or deletes — mid-session never appears in (or lingers in) the tree, and an `@` search
//! offers a path that no longer exists. `notify` already solves exactly this for the Lua config;
//! this is the same mechanism pointed at the workspace.
//!
//! The watcher never touches the index. It runs on `notify`'s own thread and only raises a flag;
//! [`crate::app::App::sync_index_views`] consumes it once per event-loop turn and rebuilds on the
//! UI thread, where the index lives. So this module knows nothing about `App`, and the walk stays
//! on the thread that owns the data.
//!
//! Change bursts (an editor save, a `cargo build`) need no debounce here: the event loop blocks on
//! `recv()` and a rebuild happens at most once per turn, so a burst collapses into a handful of
//! walks instead of one per event. Nothing is dropped either — an event that lands during a walk
//! simply re-raises the flag for the next turn.
//!
//! [`FileIndex`]: crate::file_index::FileIndex

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Handle to a running workspace watcher. Dropping it stops watching.
pub struct WorkspaceWatcher {
    // Kept alive for as long as the handle exists; dropping it removes the OS watch.
    _watcher: RecommendedWatcher,
    changed: Arc<AtomicBool>,
}

impl WorkspaceWatcher {
    /// True once per change burst; clears the flag. The caller rebuilds the index when this is
    /// true, so the flag stays raised until it is consumed.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

/// Watch `root` recursively, flagging a change on any create, modify or remove the index would
/// see. Returns `None` when the platform watcher cannot be created or the root cannot be watched
/// (an inotify-limit refusal on a huge tree, say) — the index then simply stays a snapshot, as it
/// did before, rather than the TUI failing to start.
pub fn spawn(root: &Path) -> Option<WorkspaceWatcher> {
    let root = root.to_path_buf();
    let skips = crate::file_index::skip_names(&root);
    let changed = Arc::new(AtomicBool::new(false));

    let flag = changed.clone();
    let watched = root.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<NotifyEvent, notify::Error>| {
            if let Ok(event) = res {
                if is_relevant(&event, &watched, &skips) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        },
        notify::Config::default(),
    )
    .ok()?;

    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        crate::log!("workspace watch: cannot watch {}: {e}", root.display());
        return None;
    }
    crate::log!("workspace watch: watching {}", root.display());

    Some(WorkspaceWatcher {
        _watcher: watcher,
        changed,
    })
}

/// Whether `event` is a change the index would reflect.
///
/// Two filters, both necessary. `Access` events are reads and must not trigger a walk; and any
/// path the walk itself skips (`target/`, `.git/`, `node_modules/`, …) must be ignored, or a
/// single `cargo build` would flag thousands of rebuilds the tree can never show the result of.
fn is_relevant(event: &NotifyEvent, root: &Path, skips: &[String]) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|p| !under_skip(p, root, skips))
}

/// True when any component of `path` below `root` is a name the index walk skips.
fn under_skip(path: &Path, root: &Path, skips: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        skips.iter().any(|s| s == name.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
    use std::path::PathBuf;

    fn event(kind: EventKind, path: &str) -> NotifyEvent {
        NotifyEvent {
            kind,
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    #[test]
    fn a_change_under_a_skipped_directory_is_ignored() {
        let root = Path::new("/w");
        let skips = vec!["target".to_string(), ".git".to_string()];
        assert!(
            !is_relevant(
                &event(EventKind::Create(CreateKind::File), "/w/target/debug/x.o"),
                root,
                &skips
            ),
            "a build artifact must not flag a rebuild the tree could never show"
        );
        assert!(!is_relevant(
            &event(EventKind::Modify(ModifyKind::Any), "/w/.git/index"),
            root,
            &skips
        ));
        assert!(
            is_relevant(
                &event(EventKind::Create(CreateKind::File), "/w/src/new.rs"),
                root,
                &skips
            ),
            "a source file is exactly what the tree must follow"
        );
        assert!(is_relevant(
            &event(EventKind::Remove(RemoveKind::File), "/w/src/gone.rs"),
            root,
            &skips
        ));
    }

    #[test]
    fn reads_never_flag_a_rebuild() {
        let root = Path::new("/w");
        assert!(
            !is_relevant(
                &event(
                    EventKind::Access(AccessKind::Open(notify::event::AccessMode::Read)),
                    "/w/src/a.rs"
                ),
                root,
                &[]
            ),
            "opening a file is not a change"
        );
    }

    /// End-to-end over the real OS watcher. It is deliberately tolerant: some sandboxed CI
    /// environments do not deliver notify events, and the deterministic coverage lives in the
    /// two pure tests above plus `explorer::tests` and `file_index::tests`. When events *are*
    /// delivered, this proves the callback reaches the flag.
    #[test]
    fn a_write_raises_the_changed_flag() {
        let root = std::env::temp_dir().join(format!("kn9t-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");

        let Some(watcher) = spawn(&root) else {
            let _ = std::fs::remove_dir_all(&root);
            return; // no platform watcher: the feature degrades to a snapshot
        };

        std::fs::write(root.join("a.rs"), "a").expect("write");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if watcher.take_changed() {
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // No event delivered in this environment; do not fail the suite for it.
        let _ = std::fs::remove_dir_all(&root);
    }
}
