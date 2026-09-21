//! The two follow-ups to the B5/B6/B10 work:
//!
//! 1. `TurnSlot` owns the `IdleTracker` count as well as the `aborts` registration. They
//!    used to be two independent `turn_started()` / `turn_ended()` calls, so they could
//!    disagree: `GET /health` misreported `running_turns`, and an early `return` that skipped
//!    `turn_ended()` left the counter high **forever** — which pins the process alive, since
//!    `IdleTracker::should_exit` refuses to exit while a turn is "running".
//!
//! 2. The deadlines that were hardcoded `const`s (30 min approval, 2 min no-cancel, 1.5 s
//!    tool-cancel grace) are `[server]` config, because the right value depends on the
//!    deployment's tools and users rather than on kn9t.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use kn9t_server::config::ServerTimeouts;
use kn9t_server::state::ServerState;
use kn9t_server::turn::{self, TurnSlot};

fn state() -> (Arc<ServerState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(kn9t_store::SqliteStore::open(&tmp.path().join("kn9t.db")).unwrap());
    let token = kn9t_server::auth::generate_token();
    let state = ServerState::new(store, token, Default::default(), Vec::new());
    (Arc::new(state), tmp)
}

// ── 1. idle count and registration share one lifetime ────────────────────────

/// The guard raises the idle count on register and drops it on release — exactly once each.
#[test]
fn the_guard_owns_the_idle_count() {
    let (state, _tmp) = state();
    assert_eq!(state.idle.running_turns(), 0);

    let slot = TurnSlot::register(&state, "s1");
    assert_eq!(
        state.idle.running_turns(),
        1,
        "register must count the turn"
    );
    assert!(turn::is_turn_running(&state, "s1"));

    drop(slot);
    assert_eq!(state.idle.running_turns(), 0, "release must uncount it");
    assert!(!turn::is_turn_running(&state, "s1"));
}

/// A turn that ends by panicking must not leave the counter high.
///
/// This is the one that pinned the process: `should_exit()` returns false while
/// `running_turns > 0`, so a single skipped decrement disabled idle-exit permanently.
#[test]
fn a_panicking_turn_does_not_pin_the_process_alive() {
    let (state, _tmp) = state();

    let st = state.clone();
    let h = std::thread::spawn(move || {
        let _slot = TurnSlot::register(&st, "s-panic");
        panic!("turn died");
    });
    assert!(h.join().is_err());

    assert_eq!(
        state.idle.running_turns(),
        0,
        "the idle count survived a panicking turn: idle-exit is now disabled for the life of \
         the process"
    );
}

/// The early-return paths (`compose_loop` failure, compactor failure) release too, because
/// the guard covers them structurally rather than by a hand-placed call.
#[test]
fn an_early_return_releases_both_facts() {
    let (state, _tmp) = state();

    // Models a `spawn_turn` that bails after registering (compose failure).
    {
        let _slot = TurnSlot::register(&state, "s-early");
        assert_eq!(state.idle.running_turns(), 1);
        // early return
    }

    assert_eq!(state.idle.running_turns(), 0);
    assert!(!turn::is_turn_running(&state, "s-early"));
}

/// The sink's early release (on `TurnFinishing`) clears `aborts` but must NOT drop the idle
/// count: the thread is still working (auto-titling), and reaping the process there would
/// kill a turn that is finishing its bookkeeping.
///
/// This is why the two facts have different lifetimes, and the guard is what makes that
/// explicit instead of accidental.
#[test]
fn an_early_release_frees_the_slot_but_keeps_the_process_alive() {
    let (state, _tmp) = state();
    let slot = TurnSlot::register(&state, "s-split");

    // What SessionSink does when it intercepts TurnFinishing.
    turn::release_turn_for_test(&state, "s-split", slot.id());

    assert!(
        !turn::is_turn_running(&state, "s-split"),
        "a new prompt must be accepted as soon as the turn is logically over"
    );
    assert_eq!(
        state.idle.running_turns(),
        1,
        "the process must stay alive while the turn thread is still doing work (autotitle)"
    );

    drop(slot);
    assert_eq!(state.idle.running_turns(), 0);
}

/// Overlapping turns keep the count honest: two registrations, two releases.
#[test]
fn overlapping_turns_are_counted_independently() {
    let (state, _tmp) = state();

    let a = TurnSlot::register(&state, "s1");
    let b = TurnSlot::register(&state, "s2");
    assert_eq!(state.idle.running_turns(), 2);

    drop(a);
    assert_eq!(state.idle.running_turns(), 1);
    drop(b);
    assert_eq!(state.idle.running_turns(), 0);
}

// ── 2. the deadlines are configuration ───────────────────────────────────────

/// Defaults match what the hardcoded consts used to be, so behaviour is unchanged for
/// anyone who does not configure them.
#[test]
fn timeout_defaults_match_the_previous_constants() {
    let t = ServerTimeouts::default();
    assert_eq!(t.approval, Some(Duration::from_secs(30 * 60)));
    assert_eq!(t.interaction, Some(Duration::from_secs(30 * 60)));
    assert_eq!(t.interaction_no_cancel, Some(Duration::from_secs(2 * 60)));
    assert_eq!(t.tool_cancel_grace, Duration::from_millis(1500));
    assert_eq!(
        t.max_turns, None,
        "the default run is unbounded; a ceiling is opt-in"
    );
}

/// `[server]` values are honoured, and `0` means "no deadline" rather than "expire at once"
/// — the distinction matters, since a 0-second deadline would deny every approval instantly.
#[test]
fn server_config_sets_and_disables_timeouts() {
    let toml = r#"
[server]
approval_timeout_secs = 90
interaction_timeout_secs = 0
interaction_timeout_no_cancel_secs = 15
tool_cancel_grace_ms = 400
max_turns = 25
"#;
    let t = kn9t_server::config::parse_server_timeouts(toml).expect("parses");

    assert_eq!(t.approval, Some(Duration::from_secs(90)));
    assert_eq!(
        t.interaction, None,
        "0 must disable the deadline, not make it instant"
    );
    assert_eq!(t.interaction_no_cancel, Some(Duration::from_secs(15)));
    assert_eq!(t.tool_cancel_grace, Duration::from_millis(400));
    assert_eq!(t.max_turns, Some(25));
}

/// `max_turns = 0` means "unbounded", matching the other `[server]` knobs where `0` disables —
/// a zero ceiling would otherwise refuse every turn.
#[test]
fn server_config_zero_max_turns_means_unbounded() {
    let t =
        kn9t_server::config::parse_server_timeouts("[server]\nmax_turns = 0\n").expect("parses");
    assert_eq!(t.max_turns, None);
}

/// An absent `[server]` block leaves every default in place.
#[test]
fn absent_server_block_keeps_defaults() {
    let t = kn9t_server::config::parse_server_timeouts("").expect("parses");
    let d = ServerTimeouts::default();
    assert_eq!(t.approval, d.approval);
    assert_eq!(t.interaction, d.interaction);
    assert_eq!(t.interaction_no_cancel, d.interaction_no_cancel);
    assert_eq!(t.tool_cancel_grace, d.tool_cancel_grace);
    assert_eq!(t.max_turns, d.max_turns);
}

/// The state hands the configured grace to the loop, so a turn actually runs under it.
#[test]
fn react_config_carries_the_configured_tool_grace() {
    let (state, _tmp) = state();
    // Default first.
    assert_eq!(
        state.react_config().tool_cancel_grace,
        Duration::from_millis(1500)
    );

    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(kn9t_store::SqliteStore::open(&tmp.path().join("k.db")).unwrap());
    let configured = ServerState::new(store, "t".into(), Default::default(), Vec::new())
        .with_timeouts(ServerTimeouts {
            tool_cancel_grace: Duration::from_millis(250),
            ..ServerTimeouts::default()
        });

    assert_eq!(
        configured.react_config().tool_cancel_grace,
        Duration::from_millis(250),
        "the loop must run under the configured grace, not the compiled-in default"
    );
}

/// The state hands the configured turn ceiling to the loop. Default is `None` (unbounded);
/// a configured `[server] max_turns` reaches `ReactConfig`.
#[test]
fn react_config_carries_the_configured_max_turns() {
    let (state, _tmp) = state();
    assert_eq!(
        state.react_config().max_turns,
        None,
        "an unconfigured server runs an unbounded loop"
    );

    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(kn9t_store::SqliteStore::open(&tmp.path().join("k.db")).unwrap());
    let configured = ServerState::new(store, "t".into(), Default::default(), Vec::new())
        .with_timeouts(ServerTimeouts {
            max_turns: Some(42),
            ..ServerTimeouts::default()
        });

    assert_eq!(
        configured.react_config().max_turns,
        Some(42),
        "the loop must run under the configured ceiling"
    );
}
