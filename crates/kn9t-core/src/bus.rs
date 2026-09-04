//! R-CORE-220 .. R-CORE-230 — the broadcast bus (transient events only) and the
//! `EventSink` trait.

use crate::event::{Event, LiveEvent};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// R-CORE-230 — the transient-event sink used by `assemble()` (PCORE) and tools
/// (TOOL) to emit deltas/progress without knowing about the bus or store. Durable
/// events MUST NOT be emitted through an `EventSink`.
/// 96E-12: type-safe — accepts only `LiveEvent` (transient), never durable `Event`.
pub trait EventSink: Send + Sync {
    fn emit(&self, e: LiveEvent);
}

/// Shared per-subscriber ring buffer. Bounded; when full, `push` drops the oldest
/// so the newest is retained (R-CORE-220).
struct Ring {
    queue: Mutex<RingState>,
    cv: Condvar,
    capacity: usize,
}

struct RingState {
    buf: VecDeque<Event>,
    /// Set when the owning `Bus` is dropped so blocked receivers wake and return
    /// `None`.
    closed: bool,
}

impl Ring {
    fn push(&self, e: Event) {
        let mut st = self.queue.lock().expect("bus ring poisoned");
        if st.buf.len() == self.capacity {
            st.buf.pop_front(); // drop oldest
        }
        st.buf.push_back(e);
        drop(st);
        self.cv.notify_one();
    }

    fn recv(&self) -> Option<Event> {
        let mut st = self.queue.lock().expect("bus ring poisoned");
        loop {
            if let Some(e) = st.buf.pop_front() {
                return Some(e);
            }
            if st.closed {
                return None;
            }
            st = self.cv.wait(st).expect("bus ring poisoned");
        }
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        let mut st = self.queue.lock().expect("bus ring poisoned");
        loop {
            if let Some(e) = st.buf.pop_front() {
                return Some(e);
            }
            if st.closed {
                return None;
            }
            let (new_st, timed_out) = self
                .cv
                .wait_timeout(st, timeout)
                .expect("bus ring poisoned");
            st = new_st;
            if timed_out.timed_out() {
                return None;
            }
        }
    }

    fn try_recv(&self) -> Option<Event> {
        self.queue
            .lock()
            .expect("bus ring poisoned")
            .buf
            .pop_front()
    }

    fn close(&self) {
        let mut st = self.queue.lock().expect("bus ring poisoned");
        st.closed = true;
        drop(st);
        self.cv.notify_all();
    }
}

/// R-CORE-220 — a broadcast bus for **transient** events only.
///
/// - publishing NEVER blocks the publisher (Principle 3);
/// - each subscriber has a **bounded** queue; when full, the oldest transient event
///   is dropped (§5.1 self-healing covers the loss);
/// - it carries no reply channel — value-returning work is a trait call.
///
/// R-CORE-225: the bus is NOT the persistence path. Durable events reach disk via
/// `Store::append`, which assigns `seq` and commits before the event is published
/// to the bus for observers.
pub struct Bus {
    subs: Mutex<Vec<Weak<Ring>>>,
}

/// R-CORE-220 — one subscriber's receiving end. Dropping it detaches the subscriber
/// from the bus.
pub struct Subscription {
    ring: Arc<Ring>,
}

impl Bus {
    pub fn new() -> Self {
        Bus {
            subs: Mutex::new(Vec::new()),
        }
    }

    pub fn subscribe(&self, capacity: usize) -> Subscription {
        let capacity = capacity.max(1);
        let ring = Arc::new(Ring {
            queue: Mutex::new(RingState {
                buf: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            cv: Condvar::new(),
            capacity,
        });
        self.subs
            .lock()
            .expect("bus mutex poisoned")
            .push(Arc::downgrade(&ring));
        Subscription { ring }
    }

    /// Non-blocking; may drop for slow subs (the ring evicts the oldest).
    pub fn publish(&self, event: Event) {
        let mut subs = self.subs.lock().expect("bus mutex poisoned");
        subs.retain(|weak| match weak.upgrade() {
            Some(ring) => {
                ring.push(event.clone());
                true
            }
            None => false, // subscriber gone; prune
        });
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        if let Ok(subs) = self.subs.lock() {
            for weak in subs.iter() {
                if let Some(ring) = weak.upgrade() {
                    ring.close();
                }
            }
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

/// R-CORE-230 — a `Bus` is an `EventSink`; its `emit` delegates to `publish` via conversion.
impl EventSink for Bus {
    fn emit(&self, e: LiveEvent) {
        self.publish(Event::from(e));
    }
}

impl Subscription {
    /// Blocks; `None` when the bus is dropped.
    pub fn recv(&self) -> Option<Event> {
        self.ring.recv()
    }

    /// Non-blocking.
    pub fn try_recv(&self) -> Option<Event> {
        self.ring.try_recv()
    }

    /// Blocks up to `timeout`. Returns `None` on timeout or bus closed.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.ring.recv_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::LiveEvent;
    use std::thread;

    fn test_event() -> Event {
        Event::Error {
            message: "test".into(),
        }
    }

    fn test_live_event() -> LiveEvent {
        LiveEvent::Error {
            message: "test".into(),
        }
    }

    #[test]
    fn test_bus_new() {
        let bus = Bus::new();
        // Should not panic
        drop(bus);
    }

    #[test]
    fn test_bus_subscribe() {
        let bus = Bus::new();
        let _sub = bus.subscribe(10);
        // Subscription created successfully
    }

    #[test]
    fn test_bus_publish_to_subscriber() {
        let bus = Bus::new();
        let sub = bus.subscribe(10);

        bus.publish(test_event());

        let event = sub.try_recv();
        assert!(event.is_some());
    }

    #[test]
    fn test_bus_try_recv_empty() {
        let bus = Bus::new();
        let sub = bus.subscribe(10);

        let event = sub.try_recv();
        assert!(event.is_none());
    }

    #[test]
    fn test_bus_multiple_subscribers() {
        let bus = Bus::new();
        let sub1 = bus.subscribe(10);
        let sub2 = bus.subscribe(10);

        bus.publish(test_event());

        assert!(sub1.try_recv().is_some());
        assert!(sub2.try_recv().is_some());
    }

    #[test]
    fn test_bus_dropped_subscriber_pruned() {
        let bus = Bus::new();
        let sub1 = bus.subscribe(10);
        {
            let _sub2 = bus.subscribe(10);
            // sub2 dropped here
        }

        bus.publish(test_event());

        // sub1 should still work
        assert!(sub1.try_recv().is_some());
    }

    #[test]
    fn test_bus_capacity_drops_oldest() {
        let bus = Bus::new();
        let sub = bus.subscribe(2); // Capacity of 2

        bus.publish(Event::Error {
            message: "first".into(),
        });
        bus.publish(Event::Error {
            message: "second".into(),
        });
        bus.publish(Event::Error {
            message: "third".into(),
        }); // Should drop "first"

        // Should receive "second" and "third", not "first"
        let e1 = sub.try_recv().unwrap();
        let e2 = sub.try_recv().unwrap();
        let e3 = sub.try_recv();

        match e1 {
            Event::Error { message } => assert_eq!(message, "second"),
            _ => panic!("expected Error event"),
        }
        match e2 {
            Event::Error { message } => assert_eq!(message, "third"),
            _ => panic!("expected Error event"),
        }
        assert!(e3.is_none());
    }

    #[test]
    fn test_bus_recv_timeout_returns_none_on_timeout() {
        let bus = Bus::new();
        let sub = bus.subscribe(10);

        let result = sub.recv_timeout(Duration::from_millis(10));

        // Should return None because no event was published
        assert!(result.is_none());
    }

    #[test]
    fn test_bus_recv_timeout_returns_event() {
        let bus = Bus::new();
        let sub = bus.subscribe(10);

        bus.publish(test_event());

        let result = sub.recv_timeout(Duration::from_secs(1));
        assert!(result.is_some());
    }

    #[test]
    fn test_bus_recv_blocks_until_event() {
        let bus = Arc::new(Bus::new());
        let sub = bus.subscribe(10);
        let bus_clone = bus.clone();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            bus_clone.publish(test_event());
        });

        let start = std::time::Instant::now();
        let result = sub.recv();
        let elapsed = start.elapsed();

        assert!(result.is_some());
        assert!(elapsed >= Duration::from_millis(20));

        handle.join().unwrap();
    }

    #[test]
    fn test_bus_drop_closes_subscriptions() {
        let sub;
        {
            let bus = Bus::new();
            sub = bus.subscribe(10);
            // bus dropped here
        }

        // recv should return None because bus is closed
        let result = sub.try_recv();
        assert!(result.is_none());

        // recv with timeout should also return None
        let result = sub.recv_timeout(Duration::from_millis(10));
        assert!(result.is_none());
    }

    #[test]
    fn test_event_sink_trait() {
        let bus = Bus::new();
        let sub = bus.subscribe(10);

        // Use the EventSink trait — 96E-12: must be LiveEvent, not Event
        let sink: &dyn EventSink = &bus;
        sink.emit(test_live_event());

        assert!(sub.try_recv().is_some());
    }

    #[test]
    fn test_event_sink_cannot_accept_durable() {
        // Compile-time guarantee: EventSink::emit takes LiveEvent, so the following
        // would not compile after 96E-12:
        //   let sink: &dyn EventSink = &Bus::new();
        //   sink.emit(Event::MessageAppended { seq: 0, msg: ... });
        // This test documents the type safety by asserting that LiveEvent does not
        // have durable variants and that Event::MessageAppended cannot be used as LiveEvent.
        // We verify at runtime that LiveEvent has no `seq` field and that a durable
        // Event is distinguishable.
        let live = LiveEvent::Error {
            message: "x".into(),
        };
        let event: Event = live.clone().into();
        assert!(
            event.seq().is_none(),
            "LiveEvent must be transient (no seq)"
        );
        let durable = Event::MessageAppended {
            seq: 1,
            msg: crate::Message {
                id: crate::MsgId::new(),
                role: crate::Role::User,
                content: vec![],
                silent: false,
            },
        };
        assert!(durable.seq().is_some(), "durable must have seq");
        // The type system prevents `sink.emit(durable)` — this would be a compile error:
        // `expected LiveEvent, found Event`
    }

    #[test]
    fn test_bus_default() {
        let bus = Bus::default();
        let sub = bus.subscribe(10);
        bus.publish(test_event());
        assert!(sub.try_recv().is_some());
    }
}
