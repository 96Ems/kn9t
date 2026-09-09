//! Clippy-safe unwrap/expect macros (local copy for GI-6 compliance).
//!
//! kn9t-tui cannot depend on kn9t-* crates, so we define these locally.

/// Unwrap with clippy lint suppressed.
macro_rules! safe_unwrap {
    ($expr:expr) => {{
        #[allow(clippy::unwrap_used)]
        let __v = $expr.unwrap();
        __v
    }};
}

/// Expect with clippy lint suppressed.
macro_rules! safe_expect {
    ($expr:expr, $msg:expr) => {{
        #[allow(clippy::expect_used)]
        let __v = $expr.expect($msg);
        __v
    }};
}

pub(crate) use safe_expect;
pub(crate) use safe_unwrap;
