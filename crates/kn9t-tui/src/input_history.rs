//! Input history for undo/redo functionality.
//!
//! Tracks snapshots of input state (text + cursor position) and allows
//! undoing/redoing changes. Coalesces rapid keystrokes to avoid cluttering
//! the history with single-character changes.

use std::time::{Duration, Instant};

/// Maximum number of undo states to keep.
const MAX_HISTORY: usize = 100;

/// Time window for coalescing rapid keystrokes (300ms).
const COALESCE_WINDOW: Duration = Duration::from_millis(300);

/// A snapshot of input state.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSnapshot {
    pub text: String,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

impl InputSnapshot {
    pub fn new(text: String, cursor_row: usize, cursor_col: usize) -> Self {
        Self { text, cursor_row, cursor_col }
    }
}

/// Manages undo/redo history for input.
#[derive(Debug)]
pub struct InputHistory {
    /// Stack of past states (for undo).
    undo_stack: Vec<InputSnapshot>,
    /// Stack of future states (for redo).
    redo_stack: Vec<InputSnapshot>,
    /// Last snapshot time (for coalescing).
    last_snapshot_time: Option<Instant>,
    /// Whether next change should be coalesced.
    coalescing: bool,
}

impl Default for InputHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl InputHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot_time: None,
            coalescing: false,
        }
    }

    /// Record a snapshot before a change.
    /// 
    /// Call this BEFORE modifying the input. If called within the coalesce
    /// window of the previous snapshot, the previous snapshot is updated
    /// instead of adding a new one.
    pub fn record(&mut self, snapshot: InputSnapshot) {
        let now = Instant::now();
        
        // Check if we should coalesce with previous snapshot
        let should_coalesce = self.coalescing
            && self.last_snapshot_time
                .map(|t| now.duration_since(t) < COALESCE_WINDOW)
                .unwrap_or(false);
        
        if should_coalesce && !self.undo_stack.is_empty() {
            // Don't add new snapshot, just update timing
            self.last_snapshot_time = Some(now);
        } else {
            // Add new snapshot
            if self.undo_stack.len() >= MAX_HISTORY {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(snapshot);
            self.last_snapshot_time = Some(now);
        }
        
        // Clear redo stack on new change
        self.redo_stack.clear();
        self.coalescing = true;
    }

    /// Record a snapshot and mark it as a boundary (non-coalescing).
    /// 
    /// Use this for significant changes like paste, delete word, etc.
    pub fn record_boundary(&mut self, snapshot: InputSnapshot) {
        self.coalescing = false;
        self.record(snapshot);
        self.coalescing = false;
    }

    /// Stop coalescing - next record will start a new undo group.
    pub fn break_coalesce(&mut self) {
        self.coalescing = false;
    }

    /// Undo: restore previous state.
    /// 
    /// Returns the state to restore to, or None if nothing to undo.
    /// The current state should be passed in to save for redo.
    pub fn undo(&mut self, current: InputSnapshot) -> Option<InputSnapshot> {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(current);
            self.coalescing = false;
            Some(prev)
        } else {
            None
        }
    }

    /// Redo: restore next state.
    /// 
    /// Returns the state to restore to, or None if nothing to redo.
    /// The current state should be passed in to save for undo.
    pub fn redo(&mut self, current: InputSnapshot) -> Option<InputSnapshot> {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(current);
            self.coalescing = false;
            Some(next)
        } else {
            None
        }
    }

    /// Check if undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Check if redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Clear all history (e.g., on new session).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_snapshot_time = None;
        self.coalescing = false;
    }

    /// Get undo stack size (for debugging/display).
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Get redo stack size (for debugging/display).
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_undo_redo() {
        let mut history = InputHistory::new();
        
        // Record state before typing "a"
        history.record_boundary(InputSnapshot::new("".into(), 0, 0));
        
        // Record state before typing "b"
        history.record_boundary(InputSnapshot::new("a".into(), 0, 1));
        
        // Current state is "ab"
        let current = InputSnapshot::new("ab".into(), 0, 2);
        
        // Undo should restore "a"
        let prev = history.undo(current.clone()).unwrap();
        assert_eq!(prev.text, "a");
        assert_eq!(prev.cursor_col, 1);
        
        // Undo again should restore ""
        let prev2 = history.undo(prev).unwrap();
        assert_eq!(prev2.text, "");
        
        // Redo should restore "a"
        let next = history.redo(prev2).unwrap();
        assert_eq!(next.text, "a");
    }

    #[test]
    fn test_coalescing() {
        let mut history = InputHistory::new();
        
        // Initial state
        history.record(InputSnapshot::new("".into(), 0, 0));
        
        // Rapid typing should coalesce
        history.record(InputSnapshot::new("a".into(), 0, 1));
        history.record(InputSnapshot::new("ab".into(), 0, 2));
        history.record(InputSnapshot::new("abc".into(), 0, 3));
        
        // Should only have one undo state due to coalescing
        assert_eq!(history.undo_depth(), 1);
    }

    #[test]
    fn test_boundary_breaks_coalesce() {
        let mut history = InputHistory::new();
        
        // Initial state
        history.record(InputSnapshot::new("".into(), 0, 0));
        history.record(InputSnapshot::new("a".into(), 0, 1));
        
        // Boundary should not coalesce
        history.record_boundary(InputSnapshot::new("ab".into(), 0, 2));
        
        // Should have separate states
        assert!(history.undo_depth() >= 2);
    }

    #[test]
    fn test_redo_cleared_on_new_change() {
        let mut history = InputHistory::new();
        
        history.record_boundary(InputSnapshot::new("".into(), 0, 0));
        history.record_boundary(InputSnapshot::new("a".into(), 0, 1));
        
        // Undo
        let current = InputSnapshot::new("ab".into(), 0, 2);
        let _ = history.undo(current);
        
        assert!(history.can_redo());
        
        // New change should clear redo
        history.record(InputSnapshot::new("x".into(), 0, 1));
        
        assert!(!history.can_redo());
    }

    #[test]
    fn test_max_history() {
        let mut history = InputHistory::new();
        
        // Add more than MAX_HISTORY entries
        for i in 0..MAX_HISTORY + 10 {
            history.record_boundary(InputSnapshot::new(format!("{}", i), 0, i));
        }
        
        // Should be capped at MAX_HISTORY
        assert!(history.undo_depth() <= MAX_HISTORY);
    }
}
