//! Unit tests for word_segmenter — extracted from src/word_segmenter.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::word_segmenter::{
    byte_to_char_offset, char_to_byte_offset, next_word_boundary, prev_word_boundary,
};

#[test]
fn test_prev_word_simple() {
    let text = "hello world";
    // From end (11), should go to start of "world" (6)
    assert_eq!(prev_word_boundary(text, 11), 6);
    // From "world" (6), should go to start of "hello" (0)
    assert_eq!(prev_word_boundary(text, 6), 0);
}

#[test]
fn test_next_word_simple() {
    let text = "hello world";
    // From start, should go to end of "hello" (5)
    assert_eq!(next_word_boundary(text, 0), 5);
    // From "hello" end, should skip space and go to end of "world" (11)
    assert_eq!(next_word_boundary(text, 5), 11);
}

#[test]
fn test_multiple_spaces() {
    let text = "hello   world";
    // From end, should go to start of "world" (8)
    assert_eq!(prev_word_boundary(text, 13), 8);
    // From after hello (5), should go to end after spaces (8)
    assert_eq!(next_word_boundary(text, 5), 13);
}

#[test]
fn test_emoji() {
    let text = "hello 🚀 world";
    // 🚀 is 4 bytes. "hello " = 6, "🚀" = 4, " world" = 6
    let rocket_start = 6;
    let world_start = 11; // 6 + 4 + 1

    // From end, should go to "world"
    let end = text.len();
    let prev = prev_word_boundary(text, end);
    assert_eq!(prev, world_start);

    // ...and stepping back again lands on the emoji, not inside it.
    let prev = prev_word_boundary(text, prev);
    assert_eq!(prev, rocket_start);
}

#[test]
fn test_cjk() {
    let text = "hello 你好 world";
    // "hello " = 6, "你好" = 6 bytes (3 each), " world" = 6
    let nihao_start = 6;
    let world_start = 13; // 6 + 6 + 1

    // Each CJK character is a word boundary
    let prev = prev_word_boundary(text, text.len());
    assert_eq!(prev, world_start);

    // Walking further back must stop on a char boundary within the CJK run,
    // never mid-codepoint, and reach the run's start.
    let mut cur = prev;
    while cur > nihao_start {
        cur = prev_word_boundary(text, cur);
        assert!(text.is_char_boundary(cur), "cut mid-codepoint at {cur}");
    }
    assert_eq!(cur, nihao_start);
}

#[test]
fn test_boundary_at_start() {
    let text = "hello";
    assert_eq!(prev_word_boundary(text, 0), 0);
    assert_eq!(prev_word_boundary(text, 1), 0);
}

#[test]
fn test_boundary_at_end() {
    let text = "hello";
    assert_eq!(next_word_boundary(text, 5), 5);
    assert_eq!(next_word_boundary(text, 4), 5);
}

#[test]
fn test_byte_char_conversion() {
    let text = "hello 🚀 world";
    // "hello " = 6 chars, "🚀" = 1 char, " world" = 6 chars = 13 chars total
    // "hello " = 6 bytes, "🚀" = 4 bytes, " world" = 6 bytes = 16 bytes total

    assert_eq!(byte_to_char_offset(text, 6), 6); // "hello "
    assert_eq!(byte_to_char_offset(text, 10), 7); // "hello 🚀"
    assert_eq!(char_to_byte_offset(text, 6), 6); // "hello "
    assert_eq!(char_to_byte_offset(text, 7), 10); // "hello 🚀"
}
