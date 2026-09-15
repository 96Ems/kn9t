//! Identifier newtypes and ULID generation.
//! ULID maintains lexical ordering = creation ordering for store consistency.

use kn9t_macros::safe_expect;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::cell::Cell;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Session identifier: ULID for lexical ordering.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);
/// Message identifier: ULID for lexical ordering.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MsgId(pub String);
/// Tool call ID from provider: stored verbatim, never regenerated.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);
/// Process-local monotonic approval ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(pub u64);

impl SessionId {
    /// Generates a fresh ULID.
    pub fn new() -> Self {
        SessionId(ulid())
    }

    /// Get as string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl MsgId {
    /// Generates a fresh ULID.
    pub fn new() -> Self {
        MsgId(ulid())
    }

    /// Get as string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl CallId {
    /// Get as string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Deref implementations for ergonomic string access without cloning.

impl Deref for SessionId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for MsgId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for CallId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// AsRef<str> for passing to functions expecting &str.

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MsgId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CallId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Borrow<str> for HashMap lookups without cloning.

impl Borrow<str> for SessionId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MsgId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CallId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

// Debug implementations for better error messages.

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({})", &self.0)
    }
}

impl fmt::Debug for MsgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MsgId({})", &self.0)
    }
}

impl fmt::Debug for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CallId({})", &self.0)
    }
}

impl fmt::Debug for ApprovalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApprovalId({})", self.0)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}
impl Default for MsgId {
    fn default() -> Self {
        Self::new()
    }
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

thread_local! {
    static SEED: Cell<u64> = const { Cell::new(0) };
}

/// Splitmix64 seeded from wall-clock nanos + global counter: fast and unique, not cryptographic.
fn next_rand() -> u64 {
    SEED.with(|s| {
        let mut x = s.get();
        if x == 0 {
            static CTR: AtomicU64 = AtomicU64::new(0);
            let c = CTR.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            x = nanos ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
            if x == 0 {
                x = 0x0123_4567_89AB_CDEF;
            }
        }
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        s.set(x);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    })
}

/// Generates canonical 26-char Crockford-base32 ULID: 48-bit ms timestamp + 80 random bits.
fn ulid() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        & 0xFFFF_FFFF_FFFF; // 48 bits

    let r1 = next_rand();
    let r2 = next_rand();
    let rand80: u128 = ((r1 as u128) << 16) | ((r2 as u128) & 0xFFFF);

    let value: u128 = ((ms as u128) << 80) | rand80;

    let mut out = [0u8; 26];
    for (i, slot) in out.iter_mut().enumerate() {
        let shift = 125 - i * 5;
        *slot = CROCKFORD[((value >> shift) & 0x1f) as usize];
    }
    // Safe: every byte is from the ASCII CROCKFORD table.
    safe_expect!(String::from_utf8(out.to_vec()), "crockford bytes are valid ascii")
}

