//! SSE cancel is intermittent due to BufReader buffering.
//!
//! When BufReader pulls multiple SSE events in one underlying read(), the
//! cancel check (which only happens at read() boundaries) is bypassed for
//! all events after the first in that buffer.
//!
//! This test reproduces the burst case: feed multiple events in one chunk,
//! cancel mid-parse, and observe that parsing continues past the cancel point.

use kn9t_core::Cancel;
use kn9t_provider_core::sse::sse_lines;
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A reader that tracks how many times read() is called.
struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicUsize>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.inner.read(buf)
    }
}

/// Demonstrates BufReader batches multiple SSE events into one read().
///
/// When the network delivers multiple events quickly (or a short response
/// arrives in one TCP segment), BufReader's 8KB default buffer captures them
/// all in one underlying read(). This shows why cancel must be checked at
/// each parsed event, not just at read() boundaries.
#[test]
fn sse_buffers_multiple_events_proves_problem() {
    // Simulate a "burst" response: three events in one contiguous chunk
    let data = b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n";
    let count = Arc::new(AtomicUsize::new(0));
    let reader = CountingReader {
        inner: Cursor::new(data.as_slice()),
        count: count.clone(),
    };

    let mut lines = sse_lines(reader, None);

    // Read all three events
    let e1 = lines.next().unwrap().unwrap();
    let e2 = lines.next().unwrap().unwrap();
    let e3 = lines.next().unwrap().unwrap();

    assert_eq!(e1, b"{\"seq\":1}");
    assert_eq!(e2, b"{\"seq\":2}");
    assert_eq!(e3, b"{\"seq\":3}");

    // BufReader likely called read() only once or twice for this small data.
    let reads = count.load(Ordering::SeqCst);
    eprintln!("Total read() calls for 3 events: {}", reads);
}

/// Without cancel, buffered events are returned even after external cancel signal.
#[test]
fn cancel_not_passed_means_no_check() {
    // Three events that will fit in one BufReader read
    let data = b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n";
    let cancel = Cancel::new();

    // Pass None — no cancel checking
    let mut lines = sse_lines(Cursor::new(data.as_slice()), None);

    let e1 = lines.next().unwrap().unwrap();
    assert_eq!(e1, b"{\"seq\":1}");

    // Cancel externally — but sse_lines has no reference to it
    cancel.cancel();

    // Event 2 is still returned (no cancel check)
    let e2 = lines.next().unwrap().unwrap();
    assert_eq!(e2, b"{\"seq\":2}");
    eprintln!("None cancel: events continue as expected");
}

/// sse_lines with Some(cancel) checks cancel at each parsed event.
#[test]
fn cancel_passed_is_checked_each_event() {
    // Three events that will fit in one BufReader read
    let data = b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n";
    let cancel = Cancel::new();

    let mut lines = sse_lines(Cursor::new(data.as_slice()), Some(cancel.clone()));

    // Read first event
    let e1 = lines.next().unwrap().unwrap();
    assert_eq!(e1, b"{\"seq\":1}");

    // Cancel AFTER first event, BEFORE second
    cancel.cancel();
    assert!(cancel.cancelled());

    // FIX: The second event returns ConnectionAborted because cancel is checked
    // at each iteration, not just at read() boundaries.
    let e2 = lines.next();
    match e2 {
        Some(Err(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::ConnectionAborted);
            eprintln!("FIX VERIFIED: Cancel respected mid-buffer");
        }
        Some(Ok(data)) => {
            panic!(
                "FIX FAILED: Got event 2 after cancel: {:?}",
                String::from_utf8_lossy(&data)
            );
        }
        None => {
            panic!("FIX FAILED: Iterator ended without error");
        }
    }
}

/// The fix is now implemented: sse_lines(r, Some(cancel)) checks cancel in the parse loop.
#[test]
fn fix_implemented_unified_api() {
    // sse_lines(reader, Some(cancel)) checks cancel at each event.
    // sse_lines(reader, None) does not check (for replay/test scenarios).
    // See cancel_passed_is_checked_each_event for verification.
}
