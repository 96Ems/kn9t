//! Token and usage tracking - extracted from app.rs for better separation of concerns.
//!
//! Tracks cumulative token usage, per-turn stats, and throughput metrics.

use std::time::Instant;

/// Token counts from a single usage event.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenCounts {
    pub input: usize,
    pub output: usize,
    pub cache_read: usize,
    pub cache_write: usize,
}

impl TokenCounts {
    pub fn new(input: usize, output: usize, cache_read: usize, cache_write: usize) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    /// Add another token count to this one.
    pub fn add(&mut self, other: &TokenCounts) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Reset all counts to zero.
    pub fn reset(&mut self) {
        self.input = 0;
        self.output = 0;
        self.cache_read = 0;
        self.cache_write = 0;
    }

    /// Check if all counts are zero.
    pub fn is_zero(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }
}

/// Tracks token usage and throughput for a session.
#[derive(Debug)]
pub struct TokenTracker {
    /// Session totals (cumulative across all turns).
    pub session_totals: TokenCounts,

    /// Last turn stats (reset on first UsageRecorded of each turn).
    pub last_turn: TokenCounts,

    /// Whether to reset last_turn on next UsageRecorded.
    pending_turn_reset: bool,

    /// Throughput tracking.
    turn_start: Option<Instant>,
    turn_output_tokens: usize,

    /// Last calculated throughput (tokens per second).
    pub last_toks_per_sec: Option<f64>,

    /// Cumulative cost in USD.
    pub cost: f64,
}

impl TokenTracker {
    pub fn new() -> Self {
        Self {
            session_totals: TokenCounts::default(),
            last_turn: TokenCounts::default(),
            pending_turn_reset: false,
            turn_start: None,
            turn_output_tokens: 0,
            last_toks_per_sec: None,
            cost: 0.0,
        }
    }

    /// Reset all tracking state (call when switching sessions).
    pub fn reset(&mut self) {
        self.session_totals.reset();
        self.last_turn.reset();
        self.pending_turn_reset = false;
        self.turn_start = None;
        self.turn_output_tokens = 0;
        self.last_toks_per_sec = None;
        self.cost = 0.0;
    }

    /// Called when a new turn starts.
    pub fn on_turn_started(&mut self) {
        // Mark that we need to reset last_turn stats on next UsageRecorded.
        // This keeps the previous turn's stats visible during streaming.
        self.pending_turn_reset = true;

        // Start tracking throughput for this turn.
        self.turn_start = Some(Instant::now());
        self.turn_output_tokens = 0;
    }

    /// Called when a turn ends.
    pub fn on_turn_ended(&mut self) {
        // Calculate tok/s for this turn.
        if let Some(start) = self.turn_start.take() {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.1 && self.turn_output_tokens > 0 {
                self.last_toks_per_sec = Some(self.turn_output_tokens as f64 / elapsed);
            }
        }
    }

    /// Record usage from a provider call.
    ///
    /// # Arguments
    /// * `tokens` - Token counts from this call
    /// * `cost_usd` - Cost in USD for this call
    /// * `is_title` - Whether this is a title generation call (doesn't count toward turn stats)
    pub fn record_usage(&mut self, tokens: TokenCounts, cost_usd: f64, is_title: bool) {
        self.cost += cost_usd;

        // Session totals (always accumulate, including title).
        self.session_totals.add(&tokens);

        // Last turn stats: accumulate non-title calls within a turn.
        // This way we see the combined stats of all provider calls in a ReAct turn.
        if !is_title {
            // Reset on first UsageRecorded of a new turn.
            if self.pending_turn_reset {
                self.last_turn.reset();
                self.pending_turn_reset = false;
            }
            self.last_turn.add(&tokens);

            // Track output tokens for throughput calculation.
            self.turn_output_tokens += tokens.output;
        }
    }

    /// Set cost directly (e.g., when loading session from server).
    pub fn set_cost(&mut self, cost: f64) {
        self.cost = cost;
    }

    /// Get session input tokens.
    pub fn tokens_in(&self) -> usize {
        self.session_totals.input
    }

    /// Get session output tokens.
    pub fn tokens_out(&self) -> usize {
        self.session_totals.output
    }

    /// Get session cache read tokens.
    pub fn cache_read(&self) -> usize {
        self.session_totals.cache_read
    }

    /// Get session cache write tokens.
    pub fn cache_write(&self) -> usize {
        self.session_totals.cache_write
    }

    /// Get last turn input tokens.
    pub fn last_turn_input(&self) -> usize {
        self.last_turn.input
    }

    /// Get last turn output tokens.
    pub fn last_turn_output(&self) -> usize {
        self.last_turn.output
    }

    /// Get last turn cache read tokens.
    pub fn last_turn_cache_read(&self) -> usize {
        self.last_turn.cache_read
    }

    /// Get last turn cache write tokens.
    pub fn last_turn_cache_write(&self) -> usize {
        self.last_turn.cache_write
    }
}

impl Default for TokenTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
