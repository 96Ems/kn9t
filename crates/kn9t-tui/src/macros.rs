//! Clippy-safe expect macro (local copy for GI-6 compliance).
//!
//! kn9t-tui cannot depend on kn9t-* crates, so we define it locally.

/// Expect with clippy lint suppressed.
macro_rules! safe_expect {
    ($expr:expr, $msg:expr) => {{
        #[allow(clippy::expect_used)]
        let __v = $expr.expect($msg);
        __v
    }};
}
