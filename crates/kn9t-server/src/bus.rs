//! In-process per-session publish/subscribe for live (transient + durable-echo)
//! events (DESIGN §12, §5.1). Each session has one `kn9t_core::Bus`; SSE attachers
//! subscribe to it, and the ReAct loop / route handlers publish to it.
//!
//! The bus carries the SSE stream: transient deltas AND durable-event echoes. The
//! authoritative durable truth is in the store (R-CORE-225); the bus is the wire
//! for observers (§12.4). Losing a transient event to a slow subscriber is
//! permitted (§5.1) — the core `Bus` ring drops the oldest on overflow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kn9t_core::{Bus, Event, EventSink, Subscription};

/// Registry of per-session buses, created lazily on first subscribe/publish.
pub struct SessionBuses {
    map: Mutex<HashMap<String, Arc<Bus>>>,
}

impl SessionBuses {
    pub fn new() -> Self {
        SessionBuses {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// The bus for `session`, creating it if absent.
    pub fn bus_for(&self, session: &str) -> Arc<Bus> {
        let mut m = self.map.lock().expect("session buses poisoned");
        m.entry(session.to_owned())
            .or_insert_with(|| Arc::new(Bus::new()))
            .clone()
    }

    /// Subscribe to `session`'s live stream with a bounded ring of `capacity`.
    pub fn subscribe(&self, session: &str, capacity: usize) -> Subscription {
        self.bus_for(session).subscribe(capacity)
    }

    /// Publish an event on `session`'s bus (non-blocking; may drop for slow subs).
    pub fn publish(&self, session: &str, event: Event) {
        self.bus_for(session).publish(event);
    }

    /// Drop a session's bus (on delete). Existing subscriptions detach as the bus
    /// is dropped (their rings close and `recv` returns `None`).
    pub fn drop_session(&self, session: &str) {
        self.map.lock().expect("session buses poisoned").remove(session);
    }
}

impl Default for SessionBuses {
    fn default() -> Self {
        Self::new()
    }
}

/// An [`EventSink`] bound to one session, so the ReAct loop (which takes an
/// `Arc<dyn EventSink>`) publishes onto that session's bus. Cloning the
/// `Arc<Bus>` up front keeps `emit` lock-free at the registry level.
pub struct SessionSink {
    bus: Arc<Bus>,
}

impl SessionSink {
    pub fn new(bus: Arc<Bus>) -> Self {
        SessionSink { bus }
    }
}

impl EventSink for SessionSink {
    fn emit(&self, e: Event) {
        self.bus.emit(e);
    }
}
