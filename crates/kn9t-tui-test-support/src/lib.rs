//! Test utilities for kn9t-tui.
//!
//! This crate provides test helpers without violating GI-6 (no kn9t-core dependency).
//! All fixtures use kn9t-tui's wire types directly.

#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

pub mod frames;

pub use frames::*;
