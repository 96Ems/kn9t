//! Unit tests for identifier types and ULID generation.

use kn9t_core::{ApprovalId, CallId, MsgId, SessionId};

/// Crockford base-32 alphabet — matches the private constant in ids.rs.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

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
    // Validate via the public SessionId::new() surface.
    let id = SessionId::new();
    for c in id.0.chars() {
        assert!(
            CROCKFORD.contains(&(c as u8)),
            "character {} not in Crockford alphabet",
            c
        );
    }
}

#[test]
fn test_ulid_uniqueness() {
    // ULIDs generated in sequence should be unique.
    let id1 = SessionId::new();
    let id2 = SessionId::new();
    let id3 = MsgId::new();

    assert_ne!(id1.0, id2.0, "ULIDs should be unique");
    assert_ne!(id2.0, id3.0, "ULIDs should be unique");
    assert_ne!(id1.0, id3.0, "ULIDs should be unique");
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

    // Can look up using the SessionId.
    assert_eq!(map.get(&key), Some(&"value"));
    // Can iterate and compare using as_str().
    assert!(map.keys().any(|k| k.as_str() == "key"));
}
