//! R-CORE-240 — per-turn cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

struct CancelInner {
    flag: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

/// R-CORE-240 — scoped to one turn, created by the ReAct loop at turn start, passed
/// to `Provider::stream` and every `Tool::execute`. Never a bus message.
///
/// `Cancel` is `Send + Sync + Clone` (clones share one flag). It is the one type in
/// core holding an `Arc`; it is never an `Event` payload, so R-CORE-030 is not
/// violated.
#[derive(Clone)]
pub struct Cancel(Arc<CancelInner>);

impl Cancel {
    pub fn new() -> Self {
        Cancel(Arc::new(CancelInner {
            flag: AtomicBool::new(false),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }))
    }

    /// Non-blocking poll.
    pub fn cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Acquire)
    }

    /// Idempotent; wakes waiters.
    pub fn cancel(&self) {
        // Hold the lock across the store so a waiter cannot check the flag and
        // begin waiting in the gap before we notify.
        let _guard = self.0.lock.lock().expect("cancel mutex poisoned");
        self.0.flag.store(true, Ordering::Release);
        self.0.cv.notify_all();
    }

    /// Returns `true` if cancelled (either already, or before `d` elapsed).
    pub fn wait_timeout(&self, d: Duration) -> bool {
        if self.cancelled() {
            return true;
        }
        let guard = self.0.lock.lock().expect("cancel mutex poisoned");
        if self.cancelled() {
            return true;
        }
        let (_guard, _res) = self
            .0
            .cv
            .wait_timeout(guard, d)
            .expect("cancel condvar poisoned");
        self.cancelled()
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_cancel_new_is_not_cancelled() {
        let cancel = Cancel::new();
        assert!(!cancel.cancelled());
    }

    #[test]
    fn test_cancel_after_cancel() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(cancel.cancelled());
    }

    #[test]
    fn test_cancel_is_idempotent() {
        let cancel = Cancel::new();
        cancel.cancel();
        cancel.cancel();
        cancel.cancel();
        assert!(cancel.cancelled());
    }

    #[test]
    fn test_cancel_clones_share_state() {
        let cancel1 = Cancel::new();
        let cancel2 = cancel1.clone();

        assert!(!cancel1.cancelled());
        assert!(!cancel2.cancelled());

        cancel1.cancel();

        assert!(cancel1.cancelled());
        assert!(cancel2.cancelled());
    }

    #[test]
    fn test_cancel_across_threads() {
        let cancel = Cancel::new();
        let cancel_clone = cancel.clone();

        let handle = thread::spawn(move || {
            // Wait a bit then cancel
            thread::sleep(Duration::from_millis(10));
            cancel_clone.cancel();
        });

        // Should eventually become cancelled
        while !cancel.cancelled() {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(cancel.cancelled());

        handle.join().unwrap();
    }

    #[test]
    fn test_wait_timeout_returns_true_if_already_cancelled() {
        let cancel = Cancel::new();
        cancel.cancel();

        let result = cancel.wait_timeout(Duration::from_secs(10));
        assert!(result); // Should return immediately, not wait 10s
    }

    #[test]
    fn test_wait_timeout_returns_false_on_timeout() {
        let cancel = Cancel::new();

        let start = std::time::Instant::now();
        let result = cancel.wait_timeout(Duration::from_millis(10));
        let elapsed = start.elapsed();

        assert!(!result); // Not cancelled
        assert!(elapsed >= Duration::from_millis(10)); // Actually waited
        assert!(elapsed < Duration::from_millis(100)); // Didn't wait too long
    }

    #[test]
    fn test_wait_timeout_wakes_on_cancel() {
        let cancel = Cancel::new();
        let cancel_clone = cancel.clone();

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_clone.cancel();
        });

        let start = std::time::Instant::now();
        let result = cancel.wait_timeout(Duration::from_secs(10));
        let elapsed = start.elapsed();

        assert!(result); // Was cancelled
        assert!(elapsed < Duration::from_secs(1)); // Didn't wait full timeout

        handle.join().unwrap();
    }

    #[test]
    fn test_cancel_default() {
        let cancel = Cancel::default();
        assert!(!cancel.cancelled());
    }
}
