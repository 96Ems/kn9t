//! LiveEvent::TurnFinishing regression test: must not panic on conversion to Event.
//! lands on the turn thread the server never clears `aborts`, so `is_turn_running()` stays
//! true forever and every later `/prompt` is a 409. A transient display event must not be
//! able to brick a session.
//!
//! The three properties pinned here: publishing it does not panic, it is not forwarded to
//! subscribers (it is internal), and the `TurnEnded` that follows it still is.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use kn9t_core::{Bus, Event, EventSink, LiveEvent, StopReason};

fn tag(e: &Event) -> &'static str {
    match e {
        Event::TurnStarted { .. } => "turn_started",
        Event::TurnEnded { .. } => "turn_ended",
        Event::TextDelta { .. } => "text_delta",
        Event::TurnStatus { .. } => "turn_status",
        Event::Error { .. } => "error",
        _ => "other",
    }
}

/// Drain everything currently buffered for a subscriber.
fn drain(sub: &kn9t_core::Subscription) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = sub.try_recv() {
        out.push(e);
    }
    out
}

/// The reproduction: `Bus` is an `EventSink` (R-CORE-230), so the loop can legitimately hold
/// one directly. Emitting `TurnFinishing` through it must not unwind.
#[test]
fn emitting_turn_finishing_through_a_bus_does_not_panic() {
    let bus = Bus::new();
    let _sub = bus.subscribe(16);

    // Before the fix this panics inside `Event::from`, taking the turn thread with it.
    bus.emit(LiveEvent::TurnFinishing {
        turn: 1,
        stop: StopReason::Stop,
    });
}

/// Same through an `Arc<dyn EventSink>`, which is exactly how `ReactLoop` holds its bus.
#[test]
fn emitting_turn_finishing_through_a_dyn_sink_does_not_panic() {
    let bus: Arc<dyn EventSink> = Arc::new(Bus::new());
    bus.emit(LiveEvent::TurnFinishing {
        turn: 7,
        stop: StopReason::Aborted,
    });
}

/// It is internal: a subscriber (an SSE client, in production) must never observe it.
#[test]
fn turn_finishing_is_not_forwarded_to_subscribers() {
    let bus = Bus::new();
    let sub = bus.subscribe(16);

    bus.emit(LiveEvent::TurnFinishing {
        turn: 1,
        stop: StopReason::Stop,
    });

    assert!(
        drain(&sub).is_empty(),
        "TurnFinishing is internal and must not reach a client"
    );
}

/// But the `TurnEnded` that follows it is the client's actual end-of-turn signal, and the
/// events around it keep flowing — dropping the internal one must not swallow its neighbours.
#[test]
fn turn_ended_still_reaches_subscribers_after_a_turn_finishing() {
    let bus = Bus::new();
    let sub = bus.subscribe(16);

    // The real emission order at the end of a turn (kn9t-react/src/turn.rs).
    bus.emit(LiveEvent::TurnStatus {
        phase: "idle".into(),
        message: String::new(),
    });
    bus.emit(LiveEvent::TurnFinishing {
        turn: 3,
        stop: StopReason::Stop,
    });
    bus.emit(LiveEvent::TurnEnded {
        turn: 3,
        stop: StopReason::Stop,
    });

    let kinds: Vec<&str> = drain(&sub).iter().map(tag).collect();
    assert_eq!(
        kinds,
        vec!["turn_status", "turn_ended"],
        "only the internal TurnFinishing is dropped"
    );
}

/// The stop reason survives the trip, so a client can tell an abort from a clean stop.
#[test]
fn turn_ended_keeps_its_stop_reason() {
    let bus = Bus::new();
    let sub = bus.subscribe(16);

    bus.emit(LiveEvent::TurnFinishing {
        turn: 2,
        stop: StopReason::Aborted,
    });
    bus.emit(LiveEvent::TurnEnded {
        turn: 2,
        stop: StopReason::Aborted,
    });

    let got = sub.recv_timeout(Duration::from_secs(1)).expect("TurnEnded");
    match got {
        Event::TurnEnded { turn, stop } => {
            assert_eq!(turn, 2);
            assert!(stop == StopReason::Aborted, "stop reason must survive");
        }
        other => panic!("expected TurnEnded, got {}", tag(&other)),
    }
}
