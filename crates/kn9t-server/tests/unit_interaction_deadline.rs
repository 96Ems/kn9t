//! B10 — a blocking wait must have an absolute deadline, not only a `Cancel`.
//!
//! `interaction_request` and the approval path both block a plugin's worker thread on a
//! condvar until a client answers. Cancellation was the only escape:
//!
//! ```ignore
//! let cancel = crate::turn::get_cancel(&self.state, session).unwrap_or_else(Cancel::new);
//! ```
//!
//! When `get_cancel` returns `None` — no turn registered (the op ran outside a turn, or the
//! turn had already been released) — that fallback is a `Cancel` nobody holds a clone of, so
//! nothing can ever fire it. With no timeout either, the wait was unbounded: the plugin's
//! worker thread was gone for the life of the process, and with it every op that plugin
//! serialises on that thread.
//!
//! The invariant: a wait always terminates, by response, by cancel, or by deadline.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kn9t_core::Cancel;
use kn9t_server::interaction::InteractionRegistry;

/// A wait nobody will ever answer, and whose `Cancel` nobody holds, must still end.
///
/// This is the bug: `Cancel::new()` as a fallback is inert by construction.
#[test]
fn an_unanswerable_wait_gives_up_on_its_deadline() {
    let reg = Arc::new(InteractionRegistry::new());
    let (_id, handle) = reg.create("s1", "p1", &serde_json::json!({"q": "?"}));

    // Exactly the production fallback: a fresh Cancel, unreachable by anyone else.
    let orphan = Cancel::new();
    let started = Instant::now();
    let out = reg.wait_until(&handle, &orphan, Some(Duration::from_millis(300)));

    assert!(out.is_none(), "an expired wait must not report a response");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait ignored its deadline: this is the hang that takes out the plugin's worker \
         thread for the life of the process"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "the wait returned before its deadline"
    );
}

/// The deadline must not truncate a legitimate answer that arrives in time.
#[test]
fn a_response_within_the_deadline_is_returned() {
    let reg = Arc::new(InteractionRegistry::new());
    let (id, handle) = reg.create("s1", "p1", &serde_json::json!({}));

    let r = reg.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        r.resolve(id, serde_json::json!({"answer": "yes"}));
    });

    let out = reg.wait_until(&handle, &Cancel::new(), Some(Duration::from_secs(10)));
    assert_eq!(out, Some(serde_json::json!({"answer": "yes"})));
}

/// Cancellation still wins immediately — the deadline is an addition, not a replacement
/// (96E-39: ESC must abort a pending interaction).
#[test]
fn cancellation_still_aborts_before_the_deadline() {
    let reg = Arc::new(InteractionRegistry::new());
    let (_id, handle) = reg.create("s1", "p1", &serde_json::json!({}));

    let cancel = Cancel::new();
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        c.cancel();
    });

    let started = Instant::now();
    let out = reg.wait_until(&handle, &cancel, Some(Duration::from_secs(30)));
    assert!(out.is_none(), "a cancelled wait yields no response");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation did not take effect promptly"
    );
}

/// An expired wait must not leave its slot behind: `has_pending` is what tells the host
/// whether an id is still live, and a leaked entry makes every later lookup lie.
#[test]
fn an_expired_wait_cleans_up_its_slot() {
    let reg = Arc::new(InteractionRegistry::new());
    let (id, handle) = reg.create("s1", "p1", &serde_json::json!({}));
    assert!(reg.has_pending(id));

    let _ = reg.wait_until(&handle, &Cancel::new(), Some(Duration::from_millis(150)));

    assert!(
        !reg.has_pending(id),
        "the expired slot was leaked; pending_count would grow without bound"
    );
    assert_eq!(reg.pending_count(), 0);
}

/// `None` keeps the old unbounded behaviour available for callers that genuinely have one
/// (a live turn whose `Cancel` is registered), so the deadline is opt-in per call site.
#[test]
fn no_deadline_still_waits_for_a_response() {
    let reg = Arc::new(InteractionRegistry::new());
    let (id, handle) = reg.create("s1", "p1", &serde_json::json!({}));

    let r = reg.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        r.resolve(id, serde_json::json!({"ok": true}));
    });

    let out = reg.wait_until(&handle, &Cancel::new(), None);
    assert_eq!(out, Some(serde_json::json!({"ok": true})));
}
