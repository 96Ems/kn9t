//! Per-turn cancellation: atomic flag with waiter support for graceful shutdown.

use kn9t_macros::safe_expect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

struct CancelInner {
    flag: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

/// Per-turn cancellation token: created at turn start, passed to provider and all tools.
/// Clones share one flag; used to gracefully abort streaming and tool execution.
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

    /// Sets the cancel flag and wakes all waiters (idempotent).
    pub fn cancel(&self) {
        // Lock held across store/notify to prevent race condition on flag check.
        let _guard = safe_expect!(self.0.lock.lock(), "cancel mutex poisoned");
        self.0.flag.store(true, Ordering::Release);
        self.0.cv.notify_all();
    }

    /// Waits for cancel flag or timeout. Loops to handle spurious wakeups from Condvar.
    pub fn wait_timeout(&self, d: Duration) -> bool {
        if self.cancelled() {
            return true;
        }
        let deadline = std::time::Instant::now() + d;
        let mut guard = safe_expect!(self.0.lock.lock(), "cancel mutex poisoned");
        loop {
            if self.cancelled() {
                return true;
            }
            let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
                Some(r) if !r.is_zero() => r,
                _ => return self.cancelled(),
            };
            let (g, _res) = safe_expect!(
                self.0.cv.wait_timeout(guard, remaining),
                "cancel condvar poisoned"
            );
            guard = g;
        }
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}
