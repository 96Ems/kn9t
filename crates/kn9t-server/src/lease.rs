//! R-SRV-060 — the single-writer lease (DESIGN §12.6).
//!
//! Many observers, one writer. Exactly one client holds a session's write lease
//! and may `prompt`/`steer`/`abort`/`approve`/set `model`; others get 409. The
//! lease releases on explicit `DELETE /lease`, on client disconnect (the SSE
//! stream owning the lease ends), or after an idle timeout (default 5 min,
//! SPEC-OPEN §12.6). `?takeover=1` steals it.
//!
//! A lease is identified by an opaque holder token the acquiring client keeps and
//! presents on every write. This lets the server tell "the current holder" from a
//! stale former holder after a takeover — the former holder's writes then 409.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// SPEC-OPEN §12.6 lease idle timeout (interim 5 min).
pub const DEFAULT_LEASE_IDLE: Duration = Duration::from_secs(5 * 60);

struct Lease {
    holder: String,
    last_active: Instant,
}

pub struct LeaseMap {
    leases: Mutex<HashMap<String, Lease>>,
    idle_timeout: Duration,
    counter: Mutex<u64>,
}

/// Result of an acquire attempt.
pub enum AcquireResult {
    /// Lease granted; the opaque holder token to present on writes.
    Granted(String),
    /// Held by another (and no takeover requested): 409.
    Busy,
}

impl LeaseMap {
    pub fn new(idle_timeout: Duration) -> Self {
        LeaseMap {
            leases: Mutex::new(HashMap::new()),
            idle_timeout,
            counter: Mutex::new(0),
        }
    }

    fn mint_holder(&self) -> String {
        let mut c = self.counter.lock().expect("lease counter poisoned");
        *c += 1;
        format!("lease-{}-{}", *c, Instant::now().elapsed().as_nanos())
    }

    /// R-SRV-060 — acquire `session`'s lease. If free (or the current holder has
    /// gone idle past the timeout) grant it; if held and `takeover`, steal it;
    /// otherwise `Busy`.
    pub fn acquire(&self, session: &str, takeover: bool) -> AcquireResult {
        let mut m = self.leases.lock().expect("lease map poisoned");
        let now = Instant::now();
        let occupied = match m.get(session) {
            Some(l) => now.duration_since(l.last_active) < self.idle_timeout,
            None => false,
        };
        if occupied && !takeover {
            return AcquireResult::Busy;
        }
        let holder = self.mint_holder();
        m.insert(
            session.to_owned(),
            Lease {
                holder: holder.clone(),
                last_active: now,
            },
        );
        AcquireResult::Granted(holder)
    }

    /// True if `holder` currently holds `session`'s lease (and it is not idle-expired).
    /// Refreshes `last_active` on success (writing keeps the lease warm).
    pub fn holds(&self, session: &str, holder: &str) -> bool {
        let mut m = self.leases.lock().expect("lease map poisoned");
        let now = Instant::now();
        match m.get_mut(session) {
            Some(l) if l.holder == holder => {
                if now.duration_since(l.last_active) >= self.idle_timeout {
                    // Idle-expired: treat as not held and drop it.
                    m.remove(session);
                    false
                } else {
                    l.last_active = now;
                    true
                }
            }
            _ => false,
        }
    }

    /// Keep `session`'s lease warm from a *live connection* rather than a write.
    ///
    /// DESIGN §12.6 / this module's header: a lease releases "on client disconnect
    /// (the SSE stream owning the lease ends)" — i.e. an attached client must never
    /// lose its lease to the idle timer while its SSE stream is alive. `holds()`
    /// only refreshes on a successful *write*, so a client that reads for >5 min
    /// without writing would idle-expire even though it is plainly still connected;
    /// the next prompt then 409s. The owning SSE stream calls this on every
    /// heartbeat to prove liveness.
    ///
    /// Unlike `holds()`, this refreshes even if the lease has *just* crossed the
    /// idle threshold: the live connection is authoritative proof the holder is
    /// present. Returns true if the holder still owns the lease (and was refreshed).
    pub fn touch(&self, session: &str, holder: &str) -> bool {
        let mut m = self.leases.lock().expect("lease map poisoned");
        match m.get_mut(session) {
            Some(l) if l.holder == holder => {
                l.last_active = Instant::now();
                true
            }
            _ => false,
        }
    }

    /// Release `session`'s lease only if `holder` currently holds it.
    /// Returns true if a release occurred.
    pub fn release(&self, session: &str, holder: &str) -> bool {
        let mut m = self.leases.lock().expect("lease map poisoned");
        match m.get(session) {
            Some(l) if l.holder == holder => {
                m.remove(session);
                true
            }
            _ => false,
        }
    }

    /// Force-release regardless of holder (client disconnect owning the lease).
    pub fn force_release(&self, session: &str, holder: &str) {
        let mut m = self.leases.lock().expect("lease map poisoned");
        if let Some(l) = m.get(session) {
            if l.holder == holder {
                m.remove(session);
            }
        }
    }

    /// True if any (non-idle) lease is currently held (used by idle-exit accounting
    /// indirectly; leases alone do not keep the server alive, attached clients do).
    pub fn any_active(&self) -> bool {
        let m = self.leases.lock().expect("lease map poisoned");
        let now = Instant::now();
        m.values()
            .any(|l| now.duration_since(l.last_active) < self.idle_timeout)
    }
}
