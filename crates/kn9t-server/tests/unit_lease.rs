//! Unit tests for the lease module (extracted from src/lease.rs).

use kn9t_server::lease::{AcquireResult, LeaseMap};
use std::time::Duration;

fn granted(r: AcquireResult) -> String {
    match r {
        AcquireResult::Granted(h) => h,
        AcquireResult::Busy => panic!("expected Granted, got Busy"),
    }
}

/// Baseline: with no activity, a lease idle-expires and the next write 409s
/// (this is the bug's precondition — an idle reader loses its lease).
#[test]
fn lease_idle_expires_without_activity() {
    let leases = LeaseMap::new(Duration::from_millis(50));
    let holder = granted(leases.acquire("s1", false));
    assert!(leases.holds("s1", &holder), "fresh lease is held");
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        !leases.holds("s1", &holder),
        "idle-expired lease is no longer held"
    );
}

/// The fix: a live SSE stream calls `touch()` on every heartbeat; this keeps
/// the lease warm across an idle period that would otherwise expire it, so the
/// client's next `prompt` still passes the `holds()` check (no silent 409).
#[test]
fn touch_keeps_lease_alive_past_idle_timeout() {
    let leases = LeaseMap::new(Duration::from_millis(50));
    let holder = granted(leases.acquire("s1", false));

    // Simulate ~4 heartbeats over a window (200ms) longer than the 50ms timeout,
    // touching each time as the owning SSE stream would.
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            leases.touch("s1", &holder),
            "owning stream refreshes the lease"
        );
    }

    // Total elapsed (~120ms) far exceeds the 50ms idle timeout, yet the lease
    // is still held because the stream kept it warm.
    assert!(
        leases.holds("s1", &holder),
        "touched lease survives the idle window"
    );
}

/// `touch()` only refreshes for the *current* holder — a stale former holder
/// (after a takeover) cannot keep a lease it no longer owns alive.
#[test]
fn touch_only_for_current_holder() {
    let leases = LeaseMap::new(Duration::from_secs(60));
    let holder_a = granted(leases.acquire("s1", false));
    let holder_b = granted(leases.acquire("s1", true)); // takeover
    assert_ne!(holder_a, holder_b);
    assert!(!leases.touch("s1", &holder_a), "stale holder cannot touch");
    assert!(leases.touch("s1", &holder_b), "current holder can touch");
    assert!(
        !leases.touch("missing", &holder_b),
        "unknown session is a no-op"
    );
}

/// When the owning stream ends it releases the lease (DESIGN §12.6): after
/// release, `holds()` is false and a fresh acquire succeeds.
#[test]
fn release_frees_lease_for_reacquire() {
    let leases = LeaseMap::new(Duration::from_secs(60));
    let holder = granted(leases.acquire("s1", false));
    assert!(leases.release("s1", &holder));
    assert!(!leases.holds("s1", &holder), "released lease is not held");
    // A new client can now acquire without takeover.
    let holder2 = granted(leases.acquire("s1", false));
    assert!(leases.holds("s1", &holder2));
}

// ── B7: holder tokens must be unforgeable ────────────────────────────────────

/// `mint_holder` used to build tokens as
/// `format!("lease-{}-{}", counter, Instant::now().elapsed().as_nanos())`.
/// `Instant::now().elapsed()` measures from *now*, so it is always ~0: every token was
/// `lease-{n}-0`, and `n` restarted at 1 on every server start.
///
/// That defeats the one thing the token is for (module header: telling "the current holder"
/// from "a stale former holder after a takeover"). A former holder — or anyone who guessed —
/// presents `lease-1-0` and is accepted as the writer.
#[test]
fn holder_tokens_are_not_guessable() {
    let leases = LeaseMap::new(Duration::from_secs(60));
    let real = granted(leases.acquire("s1", false));

    // The shape the old implementation produced, for every first lease of every server.
    for guess in ["lease-1-0", "lease-2-0", "lease-1", "lease-0-0"] {
        assert!(
            !leases.holds("s1", guess),
            "guessed token {guess:?} was accepted as the lease holder"
        );
    }
    assert!(leases.holds("s1", &real), "the real holder still holds");
}

/// Two leases must never mint the same token, including across `LeaseMap` instances — a
/// server restart resets the counter, and a client still holding an old token must not be
/// able to write to the new server's session of the same name.
#[test]
fn holder_tokens_are_unique_across_maps() {
    let a = LeaseMap::new(Duration::from_secs(60));
    let b = LeaseMap::new(Duration::from_secs(60));

    let t1 = granted(a.acquire("s1", false));
    let t2 = granted(a.acquire("s2", false));
    // Fresh map, same session name, same position in the counter sequence.
    let t3 = granted(b.acquire("s1", false));

    assert_ne!(t1, t2, "two sessions minted the same token");
    assert_ne!(
        t1, t3,
        "a restarted server re-minted a token it had issued before"
    );

    // The old token must not authorize writes against the new map.
    assert!(
        !b.holds("s1", &t1),
        "a token from a previous server was honoured"
    );
}

/// A takeover must invalidate the previous holder (the documented purpose of the token).
#[test]
fn takeover_invalidates_the_previous_holder() {
    let leases = LeaseMap::new(Duration::from_secs(60));
    let first = granted(leases.acquire("s1", false));
    let second = granted(leases.acquire("s1", true));

    assert_ne!(first, second);
    assert!(
        !leases.holds("s1", &first),
        "the ousted holder still writes"
    );
    assert!(leases.holds("s1", &second));
}
