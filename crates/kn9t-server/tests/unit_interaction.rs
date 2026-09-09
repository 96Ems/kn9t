//! Unit tests for the interaction module (extracted from src/interaction.rs).

use kn9t_server::interaction::InteractionRegistry;
use serde_json::json;
use std::sync::Arc;

#[test]
fn registry_blocks_and_resolves_opaque_payload() {
    let reg = Arc::new(InteractionRegistry::new());
    let (id, handle) = reg.create(
        "sess-1",
        "my-plugin",
        &json!({"question":"hello"}),
    );
    let reg_c = reg.clone();
    let h = std::thread::spawn(move || reg_c.wait(&handle));
    // Not yet resolved
    assert!(reg.has_pending(id));
    // Resolve from another thread (simulates POST /ui-respond)
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(reg.resolve(id, json!({"answer":"world"})));
    let v = h.join().unwrap();
    assert_eq!(v, json!({"answer":"world"}));
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
    let payload = json!({"question":"choose","choices":["a","b","c"],"meta":{"x":1}});
    let (id, handle) = reg.create("s", "p", &payload);
    let reg_c = reg.clone();
    let h = std::thread::spawn(move || reg_c.wait(&handle));
    reg.resolve(id, json!({"choice":"b"}));
    let v = h.join().unwrap();
    assert_eq!(v, json!({"choice":"b"}));
}
