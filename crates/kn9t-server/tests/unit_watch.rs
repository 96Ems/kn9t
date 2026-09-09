//! Extracted unit tests for `watch::Debouncer` (R-SRV-CFG-110).
//!
//! These were previously in `src/watch.rs` as `#[cfg(test)] mod tests`.

use std::time::{Duration, SystemTime};

use kn9t_server::watch::{Action, Debouncer};

fn t(secs: u64) -> Option<SystemTime> {
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

/// A file that never changes never reloads.
#[test]
fn steady_state_never_reloads() {
    let mut d = Debouncer::new(t(100));
    for _ in 0..5 {
        assert_eq!(d.observe(t(100)), Action::Wait);
    }
}

/// R-SRV-CFG-110: a change reloads only after the mtime has settled — never on
/// the poll that first observes it.
#[test]
fn change_reloads_after_settling() {
    let mut d = Debouncer::new(t(100));
    // First observation of the new mtime: arm only.
    assert_eq!(d.observe(t(200)), Action::Wait);
    // Same mtime again: the write has settled.
    assert_eq!(d.observe(t(200)), Action::Reload);
    // And it does not fire twice for one change.
    assert_eq!(d.observe(t(200)), Action::Wait);
}

/// A burst of writes (editor truncate-then-write) collapses to ONE reload,
/// and none of them fire mid-burst.
#[test]
fn burst_collapses_to_one_reload() {
    let mut d = Debouncer::new(t(100));
    assert_eq!(d.observe(t(200)), Action::Wait);
    assert_eq!(d.observe(t(201)), Action::Wait);
    assert_eq!(d.observe(t(202)), Action::Wait);
    // Settled at last value.
    assert_eq!(d.observe(t(202)), Action::Reload);
    assert_eq!(d.observe(t(202)), Action::Wait);
}

/// A missing file is not a change: delete-then-write reloads once, for the
/// write, not for the gap.
#[test]
fn missing_file_is_not_a_change() {
    let mut d = Debouncer::new(t(100));
    assert_eq!(d.observe(None), Action::Wait);
    assert_eq!(d.observe(None), Action::Wait);
    // Reappears with a new mtime.
    assert_eq!(d.observe(t(300)), Action::Wait);
    assert_eq!(d.observe(t(300)), Action::Reload);
}

/// Starting with no file at all (config created after server start) still
/// arms and fires once.
#[test]
fn created_after_start() {
    let mut d = Debouncer::new(None);
    assert_eq!(d.observe(t(50)), Action::Wait);
    assert_eq!(d.observe(t(50)), Action::Reload);
}
