//! Error helper constructors for StoreErr.
//!
//! Provides typed constructors to reduce `format!()` overhead and improve
//! error consistency. The underlying StoreErr(String) type is preserved
//! for spec compliance (R-CORE-135).

use kn9t_core::StoreErr;

/// Error kind for categorized store errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrKind {
    /// Lock poisoned (Mutex panic recovery).
    LockPoisoned,
    /// Transaction failed to begin.
    BeginFailed,
    /// Transaction failed to commit.
    CommitFailed,
    /// Session not found.
    SessionNotFound,
    /// Query failed.
    QueryFailed,
    /// Insert failed.
    InsertFailed,
    /// Update failed.
    UpdateFailed,
    /// Serialization failed.
    SerializeFailed,
    /// Deserialization failed.
    DeserializeFailed,
    /// Transient event rejected (no seq).
    TransientRejected,
    /// Blob not found.
    BlobNotFound,
    /// Invalid state.
    InvalidState,
}

impl StoreErrKind {
    /// Get the error prefix for this kind.
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::LockPoisoned => "lock poisoned",
            Self::BeginFailed => "begin",
            Self::CommitFailed => "commit",
            Self::SessionNotFound => "session not found",
            Self::QueryFailed => "query",
            Self::InsertFailed => "insert",
            Self::UpdateFailed => "update",
            Self::SerializeFailed => "serialize",
            Self::DeserializeFailed => "deserialize",
            Self::TransientRejected => "transient event rejected",
            Self::BlobNotFound => "blob not found",
            Self::InvalidState => "invalid state",
        }
    }
}

/// Create a StoreErr from kind and optional details.
pub fn store_err(kind: StoreErrKind, details: Option<&str>) -> StoreErr {
    match details {
        Some(d) => StoreErr(format!("{}: {}", kind.prefix(), d)),
        None => StoreErr(kind.prefix().to_string()),
    }
}

/// Create a StoreErr from kind and a rusqlite error.
pub fn store_err_sql(kind: StoreErrKind, err: &rusqlite::Error) -> StoreErr {
    StoreErr(format!("{}: {}", kind.prefix(), err))
}

/// Create a StoreErr from kind and a serde_json error.
pub fn store_err_json(kind: StoreErrKind, err: &serde_json::Error) -> StoreErr {
    StoreErr(format!("{}: {}", kind.prefix(), err))
}

/// Extension trait for Result to add context to StoreErr.
pub trait StoreResultExt<T> {
    /// Add context to an error.
    fn context(self, ctx: &str) -> Result<T, StoreErr>;
}

impl<T, E: std::fmt::Display> StoreResultExt<T> for Result<T, E> {
    fn context(self, ctx: &str) -> Result<T, StoreErr> {
        self.map_err(|e| StoreErr(format!("{}: {}", ctx, e)))
    }
}

/// Extension trait for rusqlite Results.
pub trait SqliteResultExt<T> {
    /// Convert rusqlite error to StoreErr with context.
    fn store_err(self, kind: StoreErrKind) -> Result<T, StoreErr>;
}

impl<T> SqliteResultExt<T> for Result<T, rusqlite::Error> {
    fn store_err(self, kind: StoreErrKind) -> Result<T, StoreErr> {
        self.map_err(|e| store_err_sql(kind, &e))
    }
}

/// Extension trait for serde_json Results.
pub trait JsonResultExt<T> {
    /// Convert serde_json error to StoreErr with context.
    fn store_err(self, kind: StoreErrKind) -> Result<T, StoreErr>;
}

impl<T> JsonResultExt<T> for Result<T, serde_json::Error> {
    fn store_err(self, kind: StoreErrKind) -> Result<T, StoreErr> {
        self.map_err(|e| store_err_json(kind, &e))
    }
}

/// Macro for creating StoreErr with lock poisoned error.
#[macro_export]
macro_rules! lock_err {
    () => {
        StoreErr("lock poisoned".into())
    };
}

/// Macro for quick StoreErr creation with format-like syntax.
#[macro_export]
macro_rules! store_err {
    ($msg:literal) => {
        kn9t_core::StoreErr($msg.into())
    };
    ($fmt:literal, $($arg:tt)*) => {
        kn9t_core::StoreErr(format!($fmt, $($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let err = store_err!("simple error");
        assert_eq!(err.0, "simple error");

        let session = "session-123";
        let err = store_err!("session {} not found", session);
        assert_eq!(err.0, "session session-123 not found");
    }
}
