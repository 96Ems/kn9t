//! R-PCORE-060 — retry wrapper for pre-stream errors only.

use kn9t_core::{Chunk, ProvErr};
use std::time::Duration;

/// Exponential-backoff config.
#[derive(Clone, Copy)]
pub struct Backoff {
    pub initial_ms: u64,
    pub factor:     f64,
    pub max_ms:     u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { initial_ms: 500, factor: 2.0, max_ms: 10_000 }
    }
}

/// R-PCORE-060 — retry `attempt` up to `max` times on connect/5xx/429 errors.
/// Once the first chunk has been yielded, any failure is propagated as a hard
/// mid-stream error and never retried.
pub fn with_retry<F>(
    max: u32,
    backoff: Backoff,
    attempt: F,
) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>
where
    F: Fn() -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>,
{
    let mut delay_ms = backoff.initial_ms;
    let mut last_err = None;

    for n in 0..=max {
        match attempt() {
            Ok(iter) => {
                // Wrap iterator: once a chunk is consumed, mid-stream errors are fatal.
                return Ok(Box::new(OnceStartedIter { inner: iter, started: false }));
            }
            Err(e) => {
                if n == max {
                    last_err = Some(e);
                    break;
                }
                if !is_retryable(&e) {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(delay_ms));
                delay_ms = (delay_ms as f64 * backoff.factor) as u64;
                if delay_ms > backoff.max_ms {
                    delay_ms = backoff.max_ms;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(ProvErr::Connect("max retries exceeded".into())))
}

fn is_retryable(e: &ProvErr) -> bool {
    matches!(e,
        ProvErr::Connect(_)
        | ProvErr::Http { status: 429, .. }
        | ProvErr::Http { status: 500..=599, .. }
    )
}

/// Iterator that wraps an inner iterator. Once the first chunk is consumed,
/// mid-stream errors are passed through as-is (no retry).
struct OnceStartedIter<I: Iterator<Item = Result<Chunk, ProvErr>>> {
    inner:   I,
    started: bool,
}

impl<I: Iterator<Item = Result<Chunk, ProvErr>> + Send> Iterator for OnceStartedIter<I> {
    type Item = Result<Chunk, ProvErr>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next();
        if item.is_some() {
            self.started = true;
        }
        item
    }
}
