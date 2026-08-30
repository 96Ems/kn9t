//! R-CORE-040, R-CORE-045 — identifier newtypes and a dependency-free ULID.
//!
//! ULID (not UUID) is used for `SessionId`/`MsgId` because lexical order equals
//! creation order, which the store relies on (R-CORE-045).

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::cell::Cell;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// R-CORE-040
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);
/// R-CORE-040
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MsgId(pub String);
/// R-CORE-040 — the provider's tool-call id, stored verbatim, never regenerated.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);
/// R-CORE-040 — process-local monotonic approval id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(pub u64);

impl SessionId {
    /// R-CORE-045 — fresh ULID.
    pub fn new() -> Self {
        SessionId(ulid())
    }

    /// Create from a string (for testing).
    #[cfg(test)]
    pub fn new_test(s: &str) -> Self {
        SessionId(s.to_string())
    }

    /// Get as string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl MsgId {
    /// R-CORE-045 — fresh ULID.
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

/// splitmix64, seeded once per thread from wall-clock nanos + a global counter.
/// Not cryptographic — IDs only need uniqueness, and timestamp ordering carries
/// monotonicity (R-CORE-045).
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

/// Canonical 26-char Crockford-base32 ULID: 48-bit ms timestamp + 80 random bits.
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
    String::from_utf8(out.to_vec()).expect("crockford bytes are valid ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_new_is_26_chars() {
        let id = SessionId::new();
        assert_eq!(id.0.len(), 26);
    }

    #[test]
    fn test_msg_id_new_is_26_chars() {
        let id = MsgId::new();
        assert_eq!(id.0.len(), 26);
    }

    #[test]
    fn test_ulid_is_crockford_base32() {
        let id = ulid();
        for c in id.chars() {
            assert!(
                CROCKFORD.contains(&(c as u8)),
                "character {} not in Crockford alphabet",
                c
            );
        }
    }

    #[test]
    fn test_ulid_uniqueness() {
        // ULIDs generated in sequence should be unique
        let id1 = ulid();
        let id2 = ulid();
        let id3 = ulid();

        assert_ne!(id1, id2, "ULIDs should be unique");
        assert_ne!(id2, id3, "ULIDs should be unique");
        assert_ne!(id1, id3, "ULIDs should be unique");
    }

    #[test]
    fn test_session_id_deref() {
        let id = SessionId("test-session".into());
        let s: &str = &id;
        assert_eq!(s, "test-session");
    }

    #[test]
    fn test_session_id_as_ref() {
        let id = SessionId("test-session".into());
        let s: &str = id.as_ref();
        assert_eq!(s, "test-session");
    }

    #[test]
    fn test_session_id_as_str() {
        let id = SessionId("test-session".into());
        assert_eq!(id.as_str(), "test-session");
    }

    #[test]
    fn test_call_id_deref() {
        let id = CallId("call-123".into());
        let s: &str = &id;
        assert_eq!(s, "call-123");
    }

    #[test]
    fn test_msg_id_deref() {
        let id = MsgId("msg-456".into());
        let s: &str = &id;
        assert_eq!(s, "msg-456");
    }

    #[test]
    fn test_session_id_debug() {
        let id = SessionId("test".into());
        let debug = format!("{:?}", id);
        assert!(debug.contains("SessionId"));
        assert!(debug.contains("test"));
    }

    #[test]
    fn test_approval_id_debug() {
        let id = ApprovalId(42);
        let debug = format!("{:?}", id);
        assert!(debug.contains("ApprovalId"));
        assert!(debug.contains("42"));
    }

    #[test]
    fn test_session_id_eq() {
        let id1 = SessionId("same".into());
        let id2 = SessionId("same".into());
        let id3 = SessionId("different".into());

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_session_id_hash() {
        use std::collections::HashSet;
        
        let mut set = HashSet::new();
        set.insert(SessionId("session-1".into()));
        set.insert(SessionId("session-2".into()));
        set.insert(SessionId("session-1".into())); // duplicate

        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_session_id_borrow_str() {
        use std::collections::HashMap;
        
        let mut map = HashMap::new();
        let key = SessionId("key".into());
        map.insert(key.clone(), "value");

        // Can look up using the SessionId
        assert_eq!(map.get(&key), Some(&"value"));
        // Can iterate and compare using as_str()
        assert!(map.keys().any(|k| k.as_str() == "key"));
    }
}
