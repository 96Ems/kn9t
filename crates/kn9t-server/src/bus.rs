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

use kn9t_core::{Bus, Event, EventSink, LiveEvent, SessionId, Subscription};
use kn9t_store::SqliteStore;

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
        self.map
            .lock()
            .expect("session buses poisoned")
            .remove(session);
    }

    /// Broadcast an event to ALL active sessions (for global events like `PluginDeclared`).
    /// Non-blocking; may drop for slow subscribers.
    pub fn broadcast_all(&self, event: Event) {
        let m = self.map.lock().expect("session buses poisoned");
        for bus in m.values() {
            bus.publish(event.clone());
        }
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
///
/// R-STOR-116: the sink is also where in-flight tool progress is persisted. The loop
/// cannot do it — it owns only trait objects (GI-1) and `Store` has no live-scratch
/// methods — but the sink already sees every `ToolStarted`/`ToolProgress`, and the server
/// is the one component allowed to name `SqliteStore` (GI-1 exception, `state.rs`). So the
/// salvage path is a side effect of publishing, with no new seam.
pub struct SessionSink {
    bus: Arc<Bus>,
    /// `None` in tests that only need the bus.
    store: Option<Arc<SqliteStore>>,
    session: SessionId,
}

impl SessionSink {
    pub fn new(bus: Arc<Bus>) -> Self {
        SessionSink {
            bus,
            store: None,
            session: SessionId(String::new()),
        }
    }

    /// R-STOR-116 — a sink that also salvages tool progress for `session`.
    pub fn with_store(bus: Arc<Bus>, store: Arc<SqliteStore>, session: SessionId) -> Self {
        SessionSink {
            bus,
            store: Some(store),
            session,
        }
    }

    /// Persist what a crash would otherwise lose. Every failure here is swallowed: this is
    /// non-canonical scratch (R-STOR-116), and a write error must never break the turn that
    /// is actually producing the output.
    /// 96E-12: only `LiveEvent` is accepted; durable `MessageAppended` no longer
    /// flows through this sink (it goes via `Store::append` only), so the
    /// `MessageAppended` salvage branch is removed — the store clears live scratch
    /// when the durable tool-result message is appended.
    fn salvage(&self, e: &LiveEvent) {
        let Some(store) = &self.store else { return };
        match e {
            LiveEvent::ToolStarted { call_id, name } => {
                let _ = store.begin_live_tool_call(&self.session, call_id, name);
            }
            LiveEvent::ToolProgress { call_id, note } => {
                let _ = store.append_live_tool_progress(&self.session, call_id, note);
            }
            _ => {}
        }
    }
}

impl EventSink for SessionSink {
    fn emit(&self, e: LiveEvent) {
        self.salvage(&e);
        self.bus.publish(Event::from(e));
    }
}
