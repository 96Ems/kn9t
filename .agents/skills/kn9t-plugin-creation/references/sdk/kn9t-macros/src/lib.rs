//! Clippy-safe unwrap/expect macros.
//!
//! Use `safe_unwrap!` and `safe_expect!` instead of bare `.unwrap()` / `.expect()`.
//! These macros expand with `#[allow(clippy::unwrap_used/expect_used)]` so clippy
//! passes, while the panic behavior is unchanged.
//!
//! # Why macros instead of traits
//!
//! A `SafeUnwrap` trait requires importing it everywhere and clippy doesn't always
//! see the `#[allow]` inside trait impls. Macros expand inline, so the allow is
//! always visible to clippy.
//!
//! # Zero dependencies
//!
//! This crate has no dependencies, making it safe to use in any crate without
//! violating GI-1 (one workspace dep) or GI-6 (kn9t-tui independence).
//!
//! # Examples
//!
//! ```
//! use kn9t_macros::{safe_unwrap, safe_expect};
//!
//! // Mutex lock — poisoned = fatal
//! # let mutex = std::sync::Mutex::new(42);
//! let guard = safe_unwrap!(mutex.lock());
//!
//! // Option with checked precondition
//! let opt = Some(42);
//! let val = safe_expect!(opt, "checked is_some above");
//! ```

/// Unwrap with clippy lint suppressed.
///
/// Use when panic is acceptable:
/// - Mutex/RwLock `.lock()` — poisoned = another thread panicked
/// - Static format parse — `"127.0.0.1:{}".parse()`
/// - OOM-only failure — `lua.create_table()`
///
/// # Example
///
/// ```
/// use kn9t_macros::safe_unwrap;
/// # let mutex = std::sync::Mutex::new(42);
/// let guard = safe_unwrap!(mutex.lock());
/// ```
#[macro_export]
macro_rules! safe_unwrap {
    ($expr:expr) => {{
        #[allow(clippy::unwrap_used)]
        let __v = $expr.unwrap();
        __v
    }};
}

/// Expect with clippy lint suppressed.
///
/// Use when panic is acceptable and a message documents why:
/// - `"poisoned"` — mutex lock after another thread panicked
/// - `"checked is_some above"` — Option verified by prior condition
///
/// # Example
///
/// ```
/// use kn9t_macros::safe_expect;
/// let opt = Some(42);
/// let val = safe_expect!(opt, "always Some in this context");
/// ```
#[macro_export]
macro_rules! safe_expect {
    ($expr:expr, $msg:expr) => {{
        #[allow(clippy::expect_used)]
        let __v = $expr.expect($msg);
        __v
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn safe_unwrap_option() {
        let opt = Some(42);
        assert_eq!(safe_unwrap!(opt), 42);
    }

    #[test]
    fn safe_unwrap_result() {
        let res: Result<i32, &str> = Ok(42);
        assert_eq!(safe_unwrap!(res), 42);
    }

    #[test]
    fn safe_expect_option() {
        let opt = Some(42);
        assert_eq!(safe_expect!(opt, "test"), 42);
    }

    #[test]
    fn safe_expect_result() {
        let res: Result<i32, &str> = Ok(42);
        assert_eq!(safe_expect!(res, "test"), 42);
    }

    #[test]
    fn safe_unwrap_mutex() {
        let mutex = std::sync::Mutex::new(42);
        let guard = safe_unwrap!(mutex.lock());
        assert_eq!(*guard, 42);
    }
}
