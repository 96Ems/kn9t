//! Unit tests for token_tracker — extracted from src/token_tracker.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::token_tracker::{TokenCounts, TokenTracker};

#[test]
fn test_token_counts_add() {
    let mut counts = TokenCounts::new(10, 20, 5, 3);
    let other = TokenCounts::new(5, 10, 2, 1);

    counts.add(&other);

    assert_eq!(counts.input, 15);
    assert_eq!(counts.output, 30);
    assert_eq!(counts.cache_read, 7);
    assert_eq!(counts.cache_write, 4);
}

#[test]
fn test_token_counts_reset() {
    let mut counts = TokenCounts::new(10, 20, 5, 3);

    counts.reset();

    assert!(counts.is_zero());
}

#[test]
fn test_tracker_session_totals() {
    let mut tracker = TokenTracker::new();

    tracker.record_usage(TokenCounts::new(100, 50, 0, 0), 0.01, false);
    tracker.record_usage(TokenCounts::new(200, 100, 10, 5), 0.02, false);

    assert_eq!(tracker.tokens_in(), 300);
    assert_eq!(tracker.tokens_out(), 150);
    assert_eq!(tracker.cache_read(), 10);
    assert_eq!(tracker.cache_write(), 5);
    assert!((tracker.cost - 0.03).abs() < 0.001);
}

#[test]
fn test_tracker_turn_reset() {
    let mut tracker = TokenTracker::new();

    // First turn.
    tracker.on_turn_started();
    tracker.record_usage(TokenCounts::new(100, 50, 0, 0), 0.01, false);
    tracker.on_turn_ended();

    assert_eq!(tracker.last_turn_input(), 100);
    assert_eq!(tracker.last_turn_output(), 50);

    // Second turn - should reset last_turn.
    tracker.on_turn_started();
    tracker.record_usage(TokenCounts::new(200, 100, 0, 0), 0.02, false);

    assert_eq!(tracker.last_turn_input(), 200);
    assert_eq!(tracker.last_turn_output(), 100);

    // But session totals keep accumulating.
    assert_eq!(tracker.tokens_in(), 300);
    assert_eq!(tracker.tokens_out(), 150);
}

#[test]
fn test_tracker_title_usage_excluded_from_turn() {
    let mut tracker = TokenTracker::new();

    tracker.on_turn_started();
    tracker.record_usage(TokenCounts::new(100, 50, 0, 0), 0.01, false);
    tracker.record_usage(TokenCounts::new(50, 25, 0, 0), 0.005, true); // title

    // Title should be in session totals.
    assert_eq!(tracker.tokens_in(), 150);
    assert_eq!(tracker.tokens_out(), 75);

    // But not in last turn.
    assert_eq!(tracker.last_turn_input(), 100);
    assert_eq!(tracker.last_turn_output(), 50);
}

#[test]
fn test_tracker_reset() {
    let mut tracker = TokenTracker::new();

    tracker.record_usage(TokenCounts::new(100, 50, 10, 5), 1.0, false);
    tracker.reset();

    assert_eq!(tracker.tokens_in(), 0);
    assert_eq!(tracker.tokens_out(), 0);
    assert_eq!(tracker.cost, 0.0);
    assert!(tracker.last_toks_per_sec.is_none());
}
