//! Recording bus for test assertions.

use crate::tags::live_event_tag;
use kn9t_core::{EventSink, LiveEvent};
use std::sync::{Arc, Mutex};

/// Recording bus: captures all emitted LiveEvents for test assertions.
#[derive(Clone, Default)]
pub struct RecordingBus {
    pub events: Arc<Mutex<Vec<LiveEvent>>>,
}

impl RecordingBus {
    pub fn new() -> Self {
        RecordingBus {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns the event type names as strings for easy assertions.
    pub fn kinds(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(live_event_tag)
            .collect()
    }

    /// Returns a clone of all recorded events.
    pub fn snapshot(&self) -> Vec<LiveEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Clears all recorded events.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

impl EventSink for RecordingBus {
    fn emit(&self, e: LiveEvent) {
        self.events.lock().unwrap().push(e);
    }
}
