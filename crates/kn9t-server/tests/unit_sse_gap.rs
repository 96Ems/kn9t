//! B9 — the attach dedup rule assumes no durable event is ever dropped from the bus ring.
//!
//! `build_attach_prelude` discards every buffered event with `seq <= head_seq`, on the
//! stated ground that "durable seqs are gapless" so anything at or below the watermark was
//! already emitted by the durable replay (`sse.rs` step 4).
//!
//! That holds only if the ring actually delivered it. The ring is bounded and evicts the
//! *oldest* entry when full (`kn9t-core/src/bus.rs`), and since durable echoes travel
//! that same ring. So on a slow attach — a large backlog, a busy agent — a durable
//! `MessageAppended` can be evicted before the flush reads it. The client then never gets it:
//! not in the replay (it is above `from`, but the replay had already run), and not live (it
//! was dropped). §5.1 self-healing explicitly does not cover durable events, so the hole is
//! permanent until something else refetches the snapshot.
//!
//! These tests pin the contract that makes such a hole detectable: the prelude reports the
//! contiguous range it actually delivered, so a client can tell it is missing something.

#![allow(clippy::unwrap_used)]

use kn9t_core::{Bus, Content, Event, Message, MsgId, Role, SessionId, Store};
use kn9t_server::sse::build_attach_prelude;
use kn9t_store::SqliteStore;
use std::sync::Arc;
use tempfile::TempDir;

fn temp_store() -> (Arc<SqliteStore>, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&tmp.path().join("kn9t.db")).unwrap();
    (Arc::new(store), tmp)
}

fn mk_msg(marker: &str) -> Event {
    Event::MessageAppended {
        seq: 0,
        msg: Message {
            id: MsgId::new(),
            role: Role::Assistant,
            content: vec![Content::Text {
                text: marker.into(),
            }],
            silent: false,
        },
    }
}

fn with_seq(e: &Event, seq: u64) -> Event {
    match e {
        Event::MessageAppended { msg, .. } => Event::MessageAppended {
            seq,
            msg: msg.clone(),
        },
        other => other.clone(),
    }
}

/// A durable event evicted from a full ring must be reported as a gap, not silently skipped.
///
/// The scenario: the client attaches from 0, the durable replay covers up to `head_seq`, and
/// meanwhile the agent commits further events that overflow the subscriber's ring. The oldest
/// of those is evicted. Before the fix, the prelude simply omitted it and said nothing — the
/// client had no way to know its transcript was short one message.
#[test]
fn an_evicted_durable_echo_is_reported_as_a_gap() {
    let (store, _tmp) = temp_store();
    let sid = SessionId::new();
    kn9t_store::create_session(
        &store,
        &sid,
        ".",
        &kn9t_core::ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        },
    )
    .unwrap();

    // Two durable events exist and will be replayed.
    let s1 = store.append(&sid, mk_msg("replayed-1")).unwrap();
    let s2 = store.append(&sid, mk_msg("replayed-2")).unwrap();

    let bus = Arc::new(Bus::new());
    // Capacity 2: the third live echo evicts the first.
    let sub = bus.subscribe(2);

    // Three durable echoes land while the attach is still assembling. seq s2+1 is the one
    // the ring will drop.
    bus.publish(with_seq(&mk_msg("live-a"), s2 + 1));
    bus.publish(with_seq(&mk_msg("live-b"), s2 + 2));
    bus.publish(with_seq(&mk_msg("live-c"), s2 + 3));

    let prelude = build_attach_prelude(&store, &sid.0, 0, &sub);

    assert_eq!(prelude.head_seq, s2, "replay watermark is the durable head");
    assert!(s1 < s2);

    // The client must be able to tell that s2+1 never arrived. `contiguous_through` is the
    // last seq the prelude can honestly claim to have delivered without a hole.
    assert_eq!(
        prelude.contiguous_through, s2,
        "the prelude claimed continuity past an evicted event: the client would advance its \
         cursor over a message it never received, and §5.1 self-healing does not cover \
         durable events, so the loss is permanent"
    );
    assert!(
        prelude.gap_detected,
        "an evicted durable echo must be surfaced as a gap so the client can re-snapshot"
    );
}

/// The common case: nothing is dropped, so no gap is reported and the cursor advances to the
/// newest event delivered.
#[test]
fn a_complete_prelude_reports_no_gap() {
    let (store, _tmp) = temp_store();
    let sid = SessionId::new();
    kn9t_store::create_session(
        &store,
        &sid,
        ".",
        &kn9t_core::ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        },
    )
    .unwrap();

    let _s1 = store.append(&sid, mk_msg("replayed-1")).unwrap();
    let s2 = store.append(&sid, mk_msg("replayed-2")).unwrap();

    let bus = Arc::new(Bus::new());
    let sub = bus.subscribe(64);
    bus.publish(with_seq(&mk_msg("live-a"), s2 + 1));
    bus.publish(with_seq(&mk_msg("live-b"), s2 + 2));

    let prelude = build_attach_prelude(&store, &sid.0, 0, &sub);

    assert!(
        !prelude.gap_detected,
        "nothing was dropped; no gap expected"
    );
    assert_eq!(
        prelude.contiguous_through,
        s2 + 2,
        "the cursor should advance to the last delivered durable event"
    );
}

/// Transient events carry no seq and are droppable by design (§5.1), so losing one must not
/// be reported as a durable gap.
#[test]
fn dropped_transients_are_not_a_gap() {
    let (store, _tmp) = temp_store();
    let sid = SessionId::new();
    kn9t_store::create_session(
        &store,
        &sid,
        ".",
        &kn9t_core::ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        },
    )
    .unwrap();
    let s1 = store.append(&sid, mk_msg("replayed")).unwrap();

    let bus = Arc::new(Bus::new());
    let sub = bus.subscribe(2);
    // Overflow the ring with seq-less transients.
    for i in 0..6 {
        bus.publish(Event::TurnStatus {
            phase: "thinking".into(),
            message: format!("tick {i}"),
        });
    }

    let prelude = build_attach_prelude(&store, &sid.0, 0, &sub);
    assert!(
        !prelude.gap_detected,
        "transient loss is permitted by §5.1 and must not trigger a re-snapshot"
    );
    assert_eq!(prelude.contiguous_through, s1);
}
