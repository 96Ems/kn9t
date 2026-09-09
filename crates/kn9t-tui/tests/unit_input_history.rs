use kn9t_tui::input_history::{InputHistory, InputSnapshot};

// Re-expose the private constant so the max-history test can use it.
// The value is 100 (see input_history.rs MAX_HISTORY).
const MAX_HISTORY: usize = 100;

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
