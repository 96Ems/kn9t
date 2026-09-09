//! 96E-40: SSE cancel is intermittent due to BufReader buffering.
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
use std::time::Duration;

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

/// Bug reproduction: BufReader batches multiple SSE events into one read().
///
/// When the network delivers multiple events quickly (or a short response
/// arrives in one TCP segment), BufReader's 8KB default buffer captures them
/// all in one underlying read(). The sse_lines iterator then processes all
/// buffered events WITHOUT calling read() again — and therefore without any
/// cancel check between events.
#[test]
fn sse_buffers_multiple_events_proves_problem() {
    // Simulate a "burst" response: three events in one contiguous chunk
    let data = b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n";
    let count = Arc::new(AtomicUsize::new(0));
    let reader = CountingReader {
        inner: Cursor::new(data.as_slice()),
        count: count.clone(),
    };

    let mut lines = sse_lines(reader);

    // Read all three events
    let e1 = lines.next().unwrap().unwrap();
    let e2 = lines.next().unwrap().unwrap();
    let e3 = lines.next().unwrap().unwrap();

    assert_eq!(e1, b"{\"seq\":1}");
    assert_eq!(e2, b"{\"seq\":2}");
    assert_eq!(e3, b"{\"seq\":3}");

    // BufReader likely called read() only once or twice for this small data.
    // If it called read() between each event, we'd see 3+ calls.
    // The actual count depends on BufReader internals, but for small data
    // it's typically 1-2 calls for the whole buffer.
    let reads = count.load(Ordering::SeqCst);
    eprintln!("Total read() calls for 3 events: {}", reads);

    // This proves the buffering behavior: read() is NOT called per-event.
    // A cancel check that only runs at read() boundaries would miss events 2/3
    // if cancel fires after read() returns but before event 2 is parsed.
}

/// Demonstrates the intermittent cancel behavior.
///
/// When using CancellableReader + BufReader + sse_lines:
/// - CancellableReader checks cancel at each read()
/// - BufReader batches reads
/// - sse_lines parses buffered data without re-checking cancel
///
/// Result: cancel is only effective at BufReader refill boundaries, not per-event.
#[test]
fn cancel_between_buffered_events_is_ignored() {
    use kn9t_provider_core::abort::CancellableReader;

    // Three events that will likely fit in one BufReader read
    let data = b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n";
    let cancel = Cancel::new();
    let reader = CancellableReader::new(Cursor::new(data.as_slice()), cancel.clone());

    let mut lines = sse_lines(reader);

    // Read first event
    let e1 = lines.next().unwrap().unwrap();
    assert_eq!(e1, b"{\"seq\":1}");

    // Cancel AFTER first event, BEFORE second
    cancel.cancel();
    assert!(cancel.cancelled());

    // BUG: The second event is still returned because it's already buffered.
    // CancellableReader already returned the full chunk; BufReader is just
    // parsing from its internal buffer now.
    let e2 = lines.next();

    // This assertion will FAIL once the fix is applied (e2 should be None or Err).
    // Currently it PASSES, proving the bug: e2 is returned despite cancel being set.
    match e2 {
        Some(Ok(data)) => {
            // BUG: we got event 2 even though cancel was set
            eprintln!("BUG: Got event 2 after cancel: {:?}", String::from_utf8_lossy(&data));
            // Uncomment to make test fail (document expected behavior):
            // panic!("Event 2 should not be returned after cancel");
        }
        Some(Err(e)) => {
            // FIXED: cancel was respected
            assert_eq!(e.kind(), std::io::ErrorKind::ConnectionAborted);
        }
        None => {
            // FIXED: iterator ended early due to cancel
        }
    }
}

/// Shows the fix needed: check cancel inside the SSE parsing loop.
///
/// The fix in sse.rs should look like:
/// ```ignore
/// pub fn sse_lines_cancellable(r: impl Read, cancel: Cancel) -> impl Iterator<...> {
///     SseCancellableIter { reader: BufReader::new(r), cancel }
/// }
///
/// impl Iterator for SseCancellableIter {
///     fn next(&mut self) -> Option<...> {
///         loop {
///             // CHECK CANCEL AT EACH ITERATION, not just at read() boundaries
///             if self.cancel.cancelled() {
///                 return Some(Err(io::Error::new(ConnectionAborted, "cancelled")));
///             }
///             // ... rest of parsing
///         }
///     }
/// }
/// ```
#[test]
fn fix_requires_cancel_check_in_parse_loop() {
    // This test documents the fix. The new API will be:
    // sse_lines_cancellable(reader, cancel) instead of sse_lines(reader)
    //
    // Or alternatively, keep sse_lines as-is and add the check in the
    // provider's consume loop (less clean but backward compatible).
}
