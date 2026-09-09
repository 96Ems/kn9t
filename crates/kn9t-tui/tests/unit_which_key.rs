//! Unit tests for which_key — extracted from src/which_key.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::which_key::{get_keybindings, WhichKeyPanel};

#[test]
fn test_keybinding_groups() {
    let groups = get_keybindings(false);
    // Must have at least 3 groups.
    assert!(
        groups.len() >= 3,
        "expected at least 3 groups, got {}",
        groups.len()
    );

    // Every group must have a non-empty name.
    for g in &groups {
        assert!(!g.name.is_empty(), "group name must not be empty");
        assert!(
            !g.entries.is_empty(),
            "group '{}' must have entries",
            g.name
        );
    }

    // Verify specific expected groups exist.
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"Session"), "expected 'Session' group");
    assert!(names.contains(&"Navigation"), "expected 'Navigation' group");
    assert!(names.contains(&"Input"), "expected 'Input' group");

    // Verify specific entries exist in Session group.
    let session = groups.iter().find(|g| g.name == "Session").unwrap();
    let session_keys: Vec<&str> = session.entries.iter().map(|e| e.key.as_str()).collect();
    assert!(
        session_keys.contains(&"Ctrl+N"),
        "Session group must contain Ctrl+N"
    );
    assert!(
        session_keys.contains(&"Ctrl+B"),
        "Session group must contain Ctrl+B"
    );
}

#[test]
fn test_navigation() {
    let groups = get_keybindings(false);
    let mut panel = WhichKeyPanel::new();

    // Initial state.
    assert_eq!(panel.selected_idx, 0);

    // Move down.
    panel.select_next(&groups);
    assert_eq!(panel.selected_idx, 1);

    panel.select_next(&groups);
    assert_eq!(panel.selected_idx, 2);

    // Move up.
    panel.select_prev(&groups);
    assert_eq!(panel.selected_idx, 1);

    panel.select_prev(&groups);
    assert_eq!(panel.selected_idx, 0);

    // Wrap around: prev from 0 goes to last.
    panel.select_prev(&groups);
    assert_eq!(panel.selected_idx, groups.len() - 1);

    // Wrap around: next from last goes to 0.
    panel.select_next(&groups);
    assert_eq!(panel.selected_idx, 0);
}

#[test]
fn test_context_aware() {
    let normal_groups = get_keybindings(false);
    let tool_groups = get_keybindings(true);

    // Tool mode must have different groups.
    let normal_names: Vec<&str> = normal_groups.iter().map(|g| g.name.as_str()).collect();
    let tool_names: Vec<&str> = tool_groups.iter().map(|g| g.name.as_str()).collect();

    // Tool mode must include "Tool Mode" group.
    assert!(
        tool_names.contains(&"Tool Mode"),
        "tool mode must have 'Tool Mode' group, got: {:?}",
        tool_names
    );

    // Normal mode must NOT have "Tool Mode" group.
    assert!(
        !normal_names.contains(&"Tool Mode"),
        "normal mode must not have 'Tool Mode' group"
    );

    // Tool mode must have tool-specific entries.
    let tool_mode_group = tool_groups.iter().find(|g| g.name == "Tool Mode").unwrap();
    let tool_keys: Vec<&str> = tool_mode_group
        .entries
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    assert!(
        tool_keys.contains(&"Esc"),
        "tool mode must have Esc binding"
    );
    assert!(
        tool_keys.contains(&"Up/Down"),
        "tool mode must have Up/Down binding"
    );

    // Normal mode must have Session group (not in tool mode).
    assert!(
        normal_names.contains(&"Session"),
        "normal mode must have 'Session' group"
    );
}
