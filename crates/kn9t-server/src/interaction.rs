//! 96E-28 — generic client→host interaction primitive.
//!
//! Generalization of `PolicyRegistry`'s `id → slot(Mutex<Option<T>>, Condvar)` pattern
//! for opaque JSON payloads. The host does NOT interpret the payload — it is the
//! plugin's own shape, forwarded verbatim to the client and back.
//!
//! This is a **one-shot modal request/response**, structurally identical to the
//! approval flow, but exposed as a generic primitive so any plugin can build its
//! own `ask_user`-shaped tool (or anything else) without host special-casing.
//!
//! Registry lives in `ServerState` alongside `ApprovalRegistry`; the route
//! `POST /ui-respond {id, payload}` resolves a pending slot, and the
//! `HostApi` op `interaction_request {session, payload}` blocks the plugin's
//! worker thread until the client responds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::Value;

// ── Slot ────────────────────────────────────────────────────────────────────

struct InteractionSlot {
    response: Mutex<Option<Value>>,
    cvar: Condvar,
    /// Debug meta: which session/plugin created this interaction.
    ///
    /// Captured for diagnostics only — never read for routing, which goes by the
    /// slot id alone. Kept because an orphaned interaction is otherwise
    /// impossible to attribute from a core dump or a debugger.
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    plugin: String,
    /// The original request payload (for diagnostics, not used for routing).
    #[allow(dead_code)]
    request_payload: Value,
}

// ── Registry ────────────────────────────────────────────────────────────────

pub struct InteractionRegistry {
    inner: Mutex<HashMap<u64, Arc<InteractionSlot>>>,
    next_id: AtomicU64,
}

impl InteractionRegistry {
    pub fn new() -> Self {
        InteractionRegistry {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a new pending interaction. Returns the allocated `id` and the
    /// slot that the caller will `wait` on.
    pub fn create(
        &self,
        session_id: String,
        plugin: String,
        payload: Value,
    ) -> (u64, Arc<InteractionSlotHandle>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let slot = Arc::new(InteractionSlot {
            response: Mutex::new(None),
            cvar: Condvar::new(),
            session_id,
            plugin,
            request_payload: payload,
        });
        self.inner
            .lock()
            .expect("interaction.rs: InteractionRegistry::create lock poisoned")
            .insert(id, slot.clone());
        (id, Arc::new(InteractionSlotHandle { id, slot }))
    }

    /// Block until `id` is resolved (by `POST /ui-respond`). The caller must hold
    /// the `InteractionSlotHandle` returned by `create`.
    pub fn wait(&self, handle: &InteractionSlotHandle) -> Value {
        let mut guard = handle
            .slot
            .response
            .lock()
            .expect("interaction.rs: InteractionRegistry::wait lock poisoned");
        while guard.is_none() {
            guard = handle
                .slot
                .cvar
                .wait(guard)
                .expect("interaction.rs: InteractionRegistry::wait cvar poisoned");
        }
        let v = guard
            .clone()
            .expect("interaction.rs: wait loop must have Some");
        // Clean up after wait so `has_pending` reflects reality.
        self.inner
            .lock()
            .expect("interaction.rs: InteractionRegistry::wait cleanup lock poisoned")
            .remove(&handle.id);
        v
    }

    /// Resolve `id` with `response`, waking any waiter. Returns `true` if a pending
    /// slot existed (validated request ID), `false` if unknown — callers must reject
    /// undeclared/unknown IDs (96E-28 acceptance: responses to unknown IDs are rejected).
    pub fn resolve(&self, id: u64, response: Value) -> bool {
        let slot = {
            let map = self
                .inner
                .lock()
                .expect("interaction.rs: InteractionRegistry::resolve lock poisoned");
            map.get(&id).cloned()
        };
        if let Some(slot) = slot {
            let mut guard = slot
                .response
                .lock()
                .expect("interaction.rs: InteractionRegistry::resolve response lock poisoned");
            *guard = Some(response);
            slot.cvar.notify_all();
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub fn has_pending(&self, id: u64) -> bool {
        self.inner
            .lock()
            .expect("interaction.rs: has_pending lock poisoned")
            .contains_key(&id)
    }

    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .expect("interaction.rs: pending_count lock poisoned")
            .len()
    }
}

/// Handle returned by `create` — bundles the id with the slot so `wait` does not
/// need a second map lookup. `Drop` does NOT auto-remove; removal happens in `wait`
/// or explicitly via `resolve`.
pub struct InteractionSlotHandle {
    pub id: u64,
    slot: Arc<InteractionSlot>,
}

impl InteractionSlotHandle {
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Default for InteractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_blocks_and_resolves_opaque_payload() {
        let reg = Arc::new(InteractionRegistry::new());
        let (id, handle) = reg.create(
            "sess-1".into(),
            "my-plugin".into(),
            json!({"question":"hello"}),
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
        let (id, handle) = reg.create("s".into(), "p".into(), payload.clone());
        let reg_c = reg.clone();
        let h = std::thread::spawn(move || reg_c.wait(&handle));
        reg.resolve(id, json!({"choice":"b"}));
        let v = h.join().unwrap();
        assert_eq!(v, json!({"choice":"b"}));
    }
}
