//! Unit tests for the interaction module (extracted from src/interaction.rs).

use kn9t_core::Cancel;
use kn9t_server::interaction::InteractionRegistry;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn registry_blocks_and_resolves_opaque_payload() {
    let reg = Arc::new(InteractionRegistry::new());
    let cancel = Cancel::new();
    let (id, handle) = reg.create(
        "sess-1",
        "my-plugin",
        &json!({"question":"hello"}),
    );
    let reg_c = reg.clone();
    let cancel_c = cancel.clone();
    let h = std::thread::spawn(move || reg_c.wait(&handle, &cancel_c));
    // Not yet resolved
    assert!(reg.has_pending(id));
    // Resolve from another thread (simulates POST /ui-respond)
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(reg.resolve(id, json!({"answer":"world"})));
    let v = h.join().unwrap();
    assert_eq!(v, Some(json!({"answer":"world"})));
    assert!(!reg.has_pending(id));
}

#[test]
fn unknown_id_is_rejected() {
    let reg = InteractionRegistry::new();
    assert!(!reg.resolve(9999, json!({})), "unknown id must be rejected");
}

#[test]
fn opaque_payload_is_forwarded_verbatim() {
    let reg = Arc::new(InteractionRegistry::new());
    let cancel = Cancel::new();
    let payload = json!({"question":"choose","choices":["a","b","c"],"meta":{"x":1}});
    let (id, handle) = reg.create("s", "p", &payload);
    let reg_c = reg.clone();
    let cancel_c = cancel.clone();
    let h = std::thread::spawn(move || reg_c.wait(&handle, &cancel_c));
    reg.resolve(id, json!({"choice":"b"}));
    let v = h.join().unwrap();
    assert_eq!(v, Some(json!({"choice":"b"})));
}

/// Verify that cancel unblocks a waiting interaction.
#[test]
fn cancel_unblocks_wait() {
    let reg = Arc::new(InteractionRegistry::new());
    let cancel = Cancel::new();
    let (_id, handle) = reg.create("s", "p", &json!({}));

    let reg_c = reg.clone();
    let cancel_c = cancel.clone();
    let start = Instant::now();
    let h = std::thread::spawn(move || reg_c.wait(&handle, &cancel_c));

    // Let the wait settle
    std::thread::sleep(Duration::from_millis(50));

    // Fire cancel
    cancel.cancel();

    // Wait should return None quickly
    let result = h.join().unwrap();
    let elapsed = start.elapsed();

    assert!(result.is_none(), "cancelled wait should return None");
    assert!(
        elapsed < Duration::from_millis(500),
        "cancel should unblock quickly, took {:?}",
        elapsed
    );
}
