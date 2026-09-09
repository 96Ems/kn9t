use kn9t_tui::prompt_history::PromptHistory;

fn test_history() -> PromptHistory {
    PromptHistory::for_test(vec![
        "first prompt".into(),
        "second prompt".into(),
        "fix bug".into(),
        "fix test".into(),
    ])
}

#[test]
fn test_prev_navigation() {
    let mut h = test_history();

    // Navigate from empty input
    let p1 = h.prev("", 0).unwrap();
    assert_eq!(p1, "fix test");

    // Navigate further back
    let p2 = h.prev("", 0).unwrap();
    assert_eq!(p2, "fix bug");
}

#[test]
fn test_prefix_filter() {
    let mut h = test_history();

    // Navigate with "fix" prefix
    let p1 = h.prev("fix", 0).unwrap();
    assert_eq!(p1, "fix test");

    let p2 = h.prev("fix", 0).unwrap();
    assert_eq!(p2, "fix bug");

    // No more "fix" matches
    assert!(h.prev("fix", 0).is_none());
}

#[test]
fn test_next_returns_stashed() {
    let mut h = test_history();

    // Navigate back with empty prefix (matches all)
    let r1 = h.prev("", 0); // stashes "", goes to "fix test" (last)
    assert_eq!(r1, Some("fix test"));

    let r2 = h.prev("", 0); // goes to "fix bug"
    assert_eq!(r2, Some("fix bug"));

    // Navigate forward once -> "fix test"
    let r3 = h.next(0, 1);
    assert_eq!(r3, Some("fix test".into()));

    // Navigate forward again should return stashed (empty string)
    let r4 = h.next(0, 1);
    assert_eq!(r4, Some("".into()));
}

#[test]
fn test_add_deduplicates() {
    let mut h = test_history();
    h.add("fix test".into()); // Same as last
    assert_eq!(h.len(), 4); // No change

    h.add("new prompt".into());
    assert_eq!(h.len(), 5);
}

#[test]
fn test_cursor_position_check() {
    let mut h = test_history();

    // Not on first line, should not navigate
    assert!(h.prev("", 1).is_none());

    // On first line, should navigate
    assert!(h.prev("", 0).is_some());
}

#[test]
fn test_empty_prompt_ignored() {
    let mut h = test_history();
    h.add("".into());
    h.add("   ".into());
    assert_eq!(h.len(), 4);
}
