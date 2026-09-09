//! Unit tests extracted from src/err.rs.
#![allow(clippy::unwrap_used)]

use kn9t_store::err::{store_err, StoreErrKind, StoreResultExt};

#[test]
fn test_store_err_kind_prefix() {
    assert_eq!(StoreErrKind::LockPoisoned.prefix(), "lock poisoned");
    assert_eq!(StoreErrKind::SessionNotFound.prefix(), "session not found");
    assert_eq!(StoreErrKind::QueryFailed.prefix(), "query");
}

#[test]
fn test_store_err_creation() {
    let err = store_err(StoreErrKind::SessionNotFound, Some("session-123"));
    assert_eq!(err.0, "session not found: session-123");

    let err = store_err(StoreErrKind::LockPoisoned, None);
    assert_eq!(err.0, "lock poisoned");
}

#[test]
fn test_store_result_ext_context() {
    let result: Result<(), &str> = Err("underlying error");
    let err = result.context("operation failed").unwrap_err();
    assert_eq!(err.0, "operation failed: underlying error");
}

#[test]
fn test_store_err_macro() {
    let err = kn9t_store::store_err!("simple error");
    assert_eq!(err.0, "simple error");

    let session = "session-123";
    let err = kn9t_store::store_err!("session {} not found", session);
    assert_eq!(err.0, "session session-123 not found");
}
