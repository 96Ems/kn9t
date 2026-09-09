//! Unit tests for search — extracted from src/search.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::message_handler::Message;
use kn9t_tui::search::SearchState;

fn make_messages(contents: &[&str]) -> Vec<Message> {
    contents
        .iter()
        .map(|c| Message::new("assistant", *c))
        .collect()
}

#[test]
fn test_search_basic() {
    let mut state = SearchState::new();
    state.query = "hello".to_string();
    state.cursor_pos = 5;
    let msgs = make_messages(&["say hello world", "goodbye", "hello again"]);
    state.search(&msgs);
    assert_eq!(state.matches.len(), 2);
    assert_eq!(state.matches[0].msg_idx, 0);
    assert_eq!(state.matches[1].msg_idx, 2);
}

#[test]
fn test_search_case_insensitive() {
    let mut state = SearchState::new();
    // Default: case_sensitive = false
    assert!(!state.case_sensitive);
    state.query = "HELLO".to_string();
    let msgs = make_messages(&["say hello world", "HELLO there", "no match"]);
    state.search(&msgs);
    // Should find both "hello" and "HELLO"
    assert_eq!(state.matches.len(), 2);
}

#[test]
fn test_search_regex() {
    let mut state = SearchState::new();
    state.regex_mode = true;
    state.query = r"\d+".to_string();
    let msgs = make_messages(&["abc 123 def", "no digits here", "456 and 789"]);
    state.search(&msgs);
    // "123" in msg 0, "456" and "789" in msg 2
    assert_eq!(state.matches.len(), 3);
    assert_eq!(state.matches[0].msg_idx, 0);
    assert_eq!(state.matches[1].msg_idx, 2);
    assert_eq!(state.matches[2].msg_idx, 2);
}

#[test]
fn test_search_navigation() {
    let mut state = SearchState::new();
    state.query = "x".to_string();
    let msgs = make_messages(&["x one", "x two", "x three"]);
    state.search(&msgs);
    assert_eq!(state.matches.len(), 3);
    assert_eq!(state.current_match_idx, 0);

    state.next_match();
    assert_eq!(state.current_match_idx, 1);

    state.next_match();
    assert_eq!(state.current_match_idx, 2);

    // Wraps around forward.
    state.next_match();
    assert_eq!(state.current_match_idx, 0);

    // Wraps around backward.
    state.prev_match();
    assert_eq!(state.current_match_idx, 2);

    state.prev_match();
    assert_eq!(state.current_match_idx, 1);
}
