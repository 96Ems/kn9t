//! Extracted from src/sse.rs — unit tests for the attach-race prelude logic.

use kn9t_core::{Bus, Content, Event, Message, MsgId, Role, SessionId, Store};
use kn9t_server::sse::{build_attach_prelude, sse_frame};
use kn9t_store::SqliteStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

fn temp_store() -> (Arc<SqliteStore>, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    let store = SqliteStore::open(&db).unwrap();
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

#[test]
fn p1_96e7_attach_does_not_lose_interleaved_event() {
    // Deterministic race: subscribe first, then while read_durable_since is
    // sleeping between its two queries, a new durable event is committed and
    // published to the bus. With the buggy two-query implementation that event
    // is lost (durable miss + dedup discard). With the atomic snapshot it is not.
    std::env::set_var("KN9T_SSE_TEST_DELAY_MS", "80");

    let (store, _tmp) = temp_store();
    let bus = Arc::new(Bus::new());
    let session = SessionId::new();
    let model = kn9t_core::ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&store, &session, ".", &model).unwrap();

    let e1 = mk_msg("first");
    let seq1 = store.append(&session, e1.clone()).unwrap();

    // Step 1: subscribe FIRST (correct order)
    let sub = bus.subscribe(1024);

    // Spawn interleaver: after 10ms, append second durable event and publish it
    let store_clone = store.clone();
    let bus_clone = bus.clone();
    let session_clone = session.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        let e2 = mk_msg("during-window");
        let seq2 = store_clone.append(&session_clone, e2.clone()).unwrap();
        // Echo to bus as server does
        bus_clone.publish(with_seq(&e2, seq2));
    });

    // Step 2-4: build prelude (with bug, this sleeps 80ms BETWEEN queries without holding lock,
    // so interleaver appends in the gap and the event is lost. With atomic fix, the sleep
    // holds the lock, so the interleaver blocks and the event is delivered via live buffer.)
    let prelude = build_attach_prelude(&store, &session.0, 0, &sub);

    handle.join().unwrap();
    // Give the bus a moment to deliver the live event if it was blocked
    std::thread::sleep(Duration::from_millis(20));

    // Clean up env var
    std::env::remove_var("KN9T_SSE_TEST_DELAY_MS");

    let all_prelude = prelude.frames.join("");
    let first_in_prelude = all_prelude.matches("first").count();
    let during_in_prelude = all_prelude.matches("during-window").count();

    // Also drain any live events that arrived after the prelude (the live loop would get them)
    let mut during_in_live = 0;
    while let Some(ev) = sub.try_recv() {
        if sse_frame(&ev).contains("during-window") {
            during_in_live += 1;
        }
    }
    let total_during = during_in_prelude + during_in_live;

    assert_eq!(
        first_in_prelude, 1,
        "first event must appear exactly once in prelude, got {}",
        first_in_prelude
    );
    assert_eq!(total_during, 1, "interleaved event must appear exactly once total (prelude+live) — lost with buggy two-query read, got prelude={}, live={}", during_in_prelude, during_in_live);
    // head_seq is the atomic snapshot's head; with the fix it stays at seq1 (1) because
    // the concurrent append blocks. With the bug it would be seq1+1 (2) but durable misses it.
    // We only check that head_seq is consistent (seq1 or seq1+1) and that no event is lost.
    assert!(
        prelude.head_seq == seq1 || prelude.head_seq == seq1 + 1,
        "head_seq should be {} or {}, got {}",
        seq1,
        seq1 + 1,
        prelude.head_seq
    );
}

#[test]
fn p1_96e7_no_duplicate_when_no_interleaving() {
    // Without interleaving, still no gap/no dup
    std::env::remove_var("KN9T_SSE_TEST_DELAY_MS");
    let (store, _tmp) = temp_store();
    let bus = Arc::new(Bus::new());
    let session = SessionId::new();
    let model = kn9t_core::ModelRef {
        provider: "test".into(),
        id: "m".into(),
    };
    kn9t_store::create_session(&store, &session, ".", &model).unwrap();
    let e1 = mk_msg("only");
    let seq1 = store.append(&session, e1.clone()).unwrap();
    bus.publish(with_seq(&e1, seq1));
    let sub = bus.subscribe(1024);
    // Also publish a transient event during window
    bus.publish(Event::TextDelta {
        msg_id: MsgId::new(),
        idx: 0,
        delta: "TRANSIENT".into(),
    });
    let prelude = build_attach_prelude(&store, &session.0, 0, &sub);
    let all = prelude.frames.join("");
    assert_eq!(all.matches("only").count(), 1);
    assert_eq!(all.matches("TRANSIENT").count(), 1);
}
