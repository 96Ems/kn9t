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
//!
//! 96E-39: `wait` now accepts a `Cancel` to allow the turn to abort a pending
//! interaction request when the user hits ESC.

use kn9t_core::Cancel;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::Value;

// ── Slot ────────────────────────────────────────────────────────────────────

struct InteractionSlot {
    response: Mutex<Option<Value>>,
    cvar: Condvar,
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
    ///
    /// `session_id`, `plugin`, and `payload` are logged for diagnostics but not
    /// stored in the slot — routing uses only the numeric id.
    pub fn create(
        &self,
        session_id: &str,
        plugin: &str,
        _payload: &Value,
    ) -> (u64, Arc<InteractionSlotHandle>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        eprintln!(
            "[interaction] create id={} session={} plugin={}",
            id, session_id, plugin
        );
        let slot = Arc::new(InteractionSlot {
            response: Mutex::new(None),
            cvar: Condvar::new(),
        });
        self.inner
            .lock()
            .expect("interaction.rs: InteractionRegistry::create lock poisoned")
            .insert(id, slot.clone());
        (id, Arc::new(InteractionSlotHandle { id, slot }))
    }

    /// Block until `id` is resolved (by `POST /ui-respond`) or `cancel` fires.
    ///
    /// 96E-39: Now accepts a `Cancel` to allow the turn to abort a pending
    /// interaction request when the user hits ESC. Returns `None` if cancelled.
    pub fn wait(&self, handle: &InteractionSlotHandle, cancel: &Cancel) -> Option<Value> {
        let mut guard = handle
            .slot
            .response
            .lock()
            .expect("interaction.rs: InteractionRegistry::wait lock poisoned");

        // Poll interval for cancel check. Short enough to be responsive, long
        // enough to avoid busy-looping.
        const POLL_INTERVAL: Duration = Duration::from_millis(100);

        while guard.is_none() {
            // 96E-39: Check cancel at each iteration
            if cancel.cancelled() {
                eprintln!("[interaction] wait id={} cancelled", handle.id);
                // Clean up the pending slot
                self.inner
                    .lock()
                    .expect("interaction.rs: InteractionRegistry::wait cleanup lock poisoned")
                    .remove(&handle.id);
                return None;
            }

            let (new_guard, _timeout) = handle
                .slot
                .cvar
                .wait_timeout(guard, POLL_INTERVAL)
                .expect("interaction.rs: InteractionRegistry::wait cvar poisoned");
            guard = new_guard;
        }

        let v = guard
            .clone()
            .expect("interaction.rs: wait loop must have Some");
        // Clean up after wait so `has_pending` reflects reality.
        self.inner
            .lock()
            .expect("interaction.rs: InteractionRegistry::wait cleanup lock poisoned")
            .remove(&handle.id);
        Some(v)
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

