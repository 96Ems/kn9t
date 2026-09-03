//! Event system — R-TUI-020.
//!
//! Pure event-driven: block on recv(), zero CPU when idle.
//! Three sources: keyboard/mouse, SSE, tick (streaming only).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent, KeyEvent, KeyEventKind, MouseEvent};

use crate::wire::SseFrame;

/// Unified event — all sources funnel here.
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Paste(String),
    /// SSE event tagged with session ID to filter stale events.
    Sse(String, SseFrame),
    Tick,
    /// SSE error tagged with session ID to filter errors from old sessions.
    SseError(String, String), // (session_id, error_message)
}

/// Event loop — owns the channel, blocks on recv().
pub struct EventLoop {
    rx: Receiver<Event>,
    tx: Sender<Event>,
}

impl EventLoop {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { rx, tx }
    }

    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Block until next event. Zero CPU when idle.
    pub fn recv(&self) -> Option<Event> {
        self.rx.recv().ok()
    }

    /// Drain all pending events without blocking.
    pub fn drain(&self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(ev) => events.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        events
    }
}

/// Spawn crossterm input thread.
pub fn spawn_input_thread(tx: Sender<Event>) {
    thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    let mapped = match ev {
                        // Bracketed paste event - works on all platforms with patched crossterm.
                        // See: https://github.com/crossterm-rs/crossterm/pull/1030
                        CtEvent::Paste(s) => {
                            crate::log!("PASTE EVENT: len={}", s.len());
                            Event::Paste(s)
                        }
                        // Only handle key press, not release/repeat.
                        CtEvent::Key(k) if k.kind == KeyEventKind::Press => Event::Key(k),
                        CtEvent::Key(_) => continue,
                        CtEvent::Mouse(m) => Event::Mouse(m),
                        CtEvent::Resize(w, h) => Event::Resize(w, h),
                        _ => continue,
                    };
                    if tx.send(mapped).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

/// Tick control handle.
pub struct TickControl {
    streaming: Arc<AtomicBool>,
}

impl TickControl {
    pub fn set_streaming(&self, val: bool) {
        self.streaming.store(val, Ordering::Relaxed);
    }
    #[cfg(test)]
    pub fn dummy() -> Self { Self { streaming: Arc::new(AtomicBool::new(false)) } }
}

/// Spawn tick thread — only sends when streaming.
pub fn spawn_tick_thread(tx: Sender<Event>, interval: Duration) -> TickControl {
    let streaming = Arc::new(AtomicBool::new(false));
    let flag = streaming.clone();

    thread::spawn(move || {
        loop {
            thread::sleep(interval);
            if flag.load(Ordering::Relaxed) {
                if tx.send(Event::Tick).is_err() {
                    break;
                }
            }
        }
    });

    TickControl { streaming }
}
