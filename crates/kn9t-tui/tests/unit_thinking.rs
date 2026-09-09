//! Unit tests for thinking — extracted from src/thinking.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::thinking::{parse_content, ContentSegment, ThinkingState};

#[test]
fn test_parse_no_thinking() {
    let segments = parse_content("Hello world");
    assert_eq!(segments.len(), 1);
    assert!(matches!(&segments[0], ContentSegment::Text(t) if t == "Hello world"));
}

#[test]
fn test_parse_simple_thinking() {
    let content = "Before <thinking>I'm thinking</thinking> After";
    let segments = parse_content(content);
    assert_eq!(segments.len(), 3);
    assert!(matches!(&segments[0], ContentSegment::Text(t) if t == "Before "));
    assert!(
        matches!(&segments[1], ContentSegment::Thinking { tag, content } 
        if tag == "thinking" && content == "I'm thinking")
    );
    assert!(matches!(&segments[2], ContentSegment::Text(t) if t == " After"));
}

#[test]
fn test_parse_ant_thinking() {
    let content = "<antThinking>reasoning here</antThinking>";
    let segments = parse_content(content);
    assert_eq!(segments.len(), 1);
    assert!(matches!(&segments[0], ContentSegment::Thinking { tag, .. } 
        if tag == "antThinking"));
}

#[test]
fn test_parse_multiple_blocks() {
    let content = "A <thinking>first</thinking> B <reasoning>second</reasoning> C";
    let segments = parse_content(content);
    assert_eq!(segments.len(), 5);
}

#[test]
fn test_parse_unclosed_tag() {
    let content = "Text <thinking>unclosed";
    let segments = parse_content(content);
    // Should treat unclosed tag as text
    assert_eq!(segments.len(), 1);
}

#[test]
fn test_thinking_state() {
    let mut state = ThinkingState::new();
    assert!(!state.is_collapsed(0));
    state.toggle(0);
    assert!(state.is_collapsed(0));
    state.toggle(0);
    assert!(!state.is_collapsed(0));
}

#[test]
fn test_collapse_all() {
    let mut state = ThinkingState::new();
    state.collapse_all(3);
    assert!(state.is_collapsed(0));
    assert!(state.is_collapsed(1));
    assert!(state.is_collapsed(2));
    assert!(!state.is_collapsed(3));
}
