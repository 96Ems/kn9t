//! B5 / B6 — the turn registry must identify *which* turn it is talking about.
//!
//! `aborts` does two jobs at once: it is the cancellation registry (`abort`, `get_cancel`)
//! and it is the "is a turn running" flag that `/prompt`, `/steer` and `/compact` gate on.
//! Both were keyed by session id alone, with `insert`/`remove` and no identity check, so
//! two turns that overlap by even a few microseconds are indistinguishable.
//!
//! The three failures reproduced here:
//!
//! 1. `abort` reads the map, drops the lock, then fires the `Cancel`. A turn that finishes
//!    in that window gets its ESC swallowed *and* — worse — the next turn inherits it.
//! 2. `clear_cancel` removed by session id, so turn A's teardown cleared turn B's
//!    registration: `is_turn_running` then reported idle while B was mid-stream, and a
//!    concurrent `/prompt` was accepted, which is exactly the transcript corruption the
//!    409 exists to prevent.
//! 3. A late ESC aimed at turn A cancelled turn B, which had done nothing wrong.
//!
//! The fix gives every turn a monotonic id and makes both writes compare-and-swap, so a
//! stale handle can only ever be a no-op.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use kn9t_server::state::ServerState;
use kn9t_server::turn::{self, TurnSlot};

/// The registry is pure in-memory state, so a bare store is enough; no provider, no model.
/// The `TempDir` is returned so it outlives the state.
fn state() -> (Arc<ServerState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(kn9t_store::SqliteStore::open(&tmp.path().join("kn9t.db")).unwrap());
    let token = kn9t_server::auth::generate_token();
    let state = ServerState::new(store, token, Default::default(), Vec::new());
    (Arc::new(state), tmp)
}

/// A turn's registration is scoped to that turn: dropping the guard of an *older* turn
/// must not deregister the one that is currently running.
///
/// This is B6. Turn A ends, its teardown runs `clear_cancel(session)`, and because the key
/// was just the session id it wiped turn B's entry. `is_turn_running` then said "idle" for
/// a session with a live provider stream attached to it.
#[test]
fn a_finished_turn_does_not_deregister_its_successor() {
    let (state, _tmp) = state();
    let session = "s-successor";

    let a = TurnSlot::register(&state, session);
    assert!(turn::is_turn_running(&state, session));

    // B takes over (a new prompt after A reported done). Registering must displace A.
    let b = TurnSlot::register(&state, session);
    assert!(turn::is_turn_running(&state, session));

    // A's teardown lands late. It must be a no-op: B owns the slot now.
    drop(a);
    assert!(
        turn::is_turn_running(&state, session),
        "turn A's teardown deregistered turn B — a concurrent /prompt would now be accepted \
         mid-stream and corrupt the transcript"
    );

    // Only B's own teardown clears it.
    drop(b);
    assert!(!turn::is_turn_running(&state, session));
}

/// An abort aimed at a turn that has already finished must not touch the turn that
/// replaced it.
///
/// This is B5.2. `abort` looked up by session id and fired whatever `Cancel` it found, so
/// an ESC in flight for turn A killed turn B.
#[test]
fn a_late_abort_does_not_cancel_the_next_turn() {
    let (state, _tmp) = state();
    let session = "s-late-abort";

    let a = TurnSlot::register(&state, session);
    let a_id = a.id();
    let a_cancel = a.cancel().clone();
    drop(a); // A finished before the user's ESC arrived.

    let b = TurnSlot::register(&state, session);
    let b_cancel = b.cancel().clone();

    // The ESC was raised against A. It must not land on B.
    turn::abort_turn(&state, session, Some(a_id));

    assert!(
        !b_cancel.cancelled(),
        "an abort aimed at the previous turn cancelled the current one"
    );
    assert!(
        !a_cancel.cancelled(),
        "A was already gone; nothing to cancel"
    );

    // An abort with no id still means "whatever is running now" (the /abort route).
    turn::abort_turn(&state, session, None);
    assert!(
        b_cancel.cancelled(),
        "an unqualified abort must hit the running turn"
    );
}

/// `abort` on the currently-running turn works, by id and unqualified.
#[test]
fn aborting_the_running_turn_fires_its_cancel() {
    let (state, _tmp) = state();
    let session = "s-abort-live";

    let guard = TurnSlot::register(&state, session);
    let cancel = guard.cancel().clone();

    turn::abort_turn(&state, session, Some(guard.id()));
    assert!(cancel.cancelled());
}

/// Turn ids are unique per turn, so two turns are never confusable — including across
/// sessions, since the counter is server-wide.
#[test]
fn turn_ids_are_unique() {
    let (state, _tmp) = state();

    let a = TurnSlot::register(&state, "s1");
    let b = TurnSlot::register(&state, "s2");
    let a2 = {
        let g = TurnSlot::register(&state, "s1");
        g.id()
    };

    assert_ne!(a.id(), b.id());
    assert_ne!(a.id(), a2);
    assert_ne!(b.id(), a2);
}

/// `get_cancel` hands out the running turn's handle, and stops doing so once it ends.
///
/// The host_api ops (`session_prompt`, `interaction_request`, `provider_complete`,
/// `tool_execute`) all call this; a `None` there means they fall back to a fresh `Cancel`
/// nobody can ever fire (B10), so the transition must be exact.
#[test]
fn get_cancel_tracks_the_running_turn() {
    let (state, _tmp) = state();
    let session = "s-getcancel";

    assert!(turn::get_cancel(&state, session).is_none());

    let guard = TurnSlot::register(&state, session);
    let handed = turn::get_cancel(&state, session).expect("a turn is running");
    // Same underlying flag, not a copy.
    handed.cancel();
    assert!(guard.cancel().cancelled());

    drop(guard);
    assert!(turn::get_cancel(&state, session).is_none());
}

/// The guard clears the slot even when the turn thread unwinds.
///
/// This is the B1 blast radius closed structurally: a panic anywhere in the turn used to
/// skip `clear_cancel` and wedge the session at 409 forever. `Drop` runs during unwind, so
/// the registration cannot outlive the turn.
#[test]
fn a_panicking_turn_still_releases_the_slot() {
    let (state, _tmp) = state();
    let session = "s-panic";

    let st = state.clone();
    let handle = std::thread::spawn(move || {
        let _guard = TurnSlot::register(&st, "s-panic");
        panic!("turn thread died");
    });
    assert!(handle.join().is_err(), "the thread was supposed to panic");

    assert!(
        !turn::is_turn_running(&state, session),
        "a panicking turn left its registration behind — every later /prompt would 409"
    );
}
