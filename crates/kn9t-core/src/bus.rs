//! Broadcast bus for transient events: delivers to all subscribers via bounded ring buffers.

use crate::event::{Event, LiveEvent};
use kn9t_macros::safe_expect;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// Type-safe transient event sink: accepts only LiveEvent, never durable Event.
/// Used by providers and tools to emit progress without knowing about bus/store internals.
pub trait EventSink: Send + Sync {
    fn emit(&self, e: LiveEvent);
}

/// Per-subscriber ring buffer: bounded, drops oldest on overflow.
struct Ring {
    queue: Mutex<RingState>,
    cv: Condvar,
    capacity: usize,
}

struct RingState {
    buf: VecDeque<Event>,
    /// True when Bus is dropped, wakes all blocked receivers.
    closed: bool,
}

impl Ring {
    fn push(&self, e: Event) {
        let mut st = safe_expect!(self.queue.lock(), "bus ring poisoned");
        if st.buf.len() == self.capacity {
            st.buf.pop_front(); // drop oldest
        }
        st.buf.push_back(e);
        drop(st);
        self.cv.notify_one();
    }

    fn recv(&self) -> Option<Event> {
        let mut st = safe_expect!(self.queue.lock(), "bus ring poisoned");
        loop {
            if let Some(e) = st.buf.pop_front() {
                return Some(e);
            }
            if st.closed {
                return None;
            }
            st = safe_expect!(self.cv.wait(st), "bus ring poisoned");
        }
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        let mut st = safe_expect!(self.queue.lock(), "bus ring poisoned");
        loop {
            if let Some(e) = st.buf.pop_front() {
                return Some(e);
            }
            if st.closed {
                return None;
            }
            let (new_st, timed_out) = safe_expect!(self
                .cv
                .wait_timeout(st, timeout), "bus ring poisoned");
            st = new_st;
            if timed_out.timed_out() {
                return None;
            }
        }
    }

    fn try_recv(&self) -> Option<Event> {
        safe_expect!(self.queue.lock(), "bus ring poisoned")
            .buf
            .pop_front()
    }

    fn close(&self) {
        let mut st = safe_expect!(self.queue.lock(), "bus ring poisoned");
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
        safe_expect!(self.subs.lock(), "bus mutex poisoned")
            .push(Arc::downgrade(&ring));
        Subscription { ring }
    }

    /// Non-blocking; may drop for slow subs (the ring evicts the oldest).
    pub fn publish(&self, event: Event) {
        let mut subs = safe_expect!(self.subs.lock(), "bus mutex poisoned");
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
///
/// Internal events (`LiveEvent::is_internal`, i.e. `TurnFinishing`) are dropped rather than
/// published: they coordinate the host with itself and no subscriber should observe one. This
/// used to panic instead, which killed the turn thread of any run whose bus was not the
/// server's intercepting `SessionSink`.
impl EventSink for Bus {
    fn emit(&self, e: LiveEvent) {
        if let Some(event) = e.to_observable_event() {
            self.publish(event);
        }
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

