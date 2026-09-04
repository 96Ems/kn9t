//! Word segmentation for navigation (CJK/Emoji aware).
//!
//! Provides word-boundary detection for Ctrl+Left/Right navigation
//! and Ctrl+Backspace/Delete word deletion.

use unicode_segmentation::UnicodeSegmentation;

/// Find the start of the previous word from a given byte position.
/// Returns byte offset (not char offset).
pub fn prev_word_boundary(text: &str, byte_pos: usize) -> usize {
    if byte_pos == 0 {
        return 0;
    }

    let before = &text[..byte_pos];
    let word_bounds: Vec<_> = before.split_word_bound_indices().collect();

    // Find last non-whitespace word boundary
    let mut pos = 0;
    let mut found_word = false;

    for (idx, segment) in word_bounds.iter().rev() {
        let is_whitespace = segment.chars().all(|c| c.is_whitespace());

        if found_word && is_whitespace {
            // We've passed through a word and hit whitespace before it
            pos = idx + segment.len();
            break;
        }

        if !is_whitespace {
            found_word = true;
            pos = *idx;
        }
    }

    if !found_word {
        // All whitespace before cursor
        0
    } else {
        pos
    }
}

/// Find the end of the next word from a given byte position.
/// Returns byte offset (not char offset).
pub fn next_word_boundary(text: &str, byte_pos: usize) -> usize {
    if byte_pos >= text.len() {
        return text.len();
    }

    let after = &text[byte_pos..];
    let word_bounds: Vec<_> = after.split_word_bound_indices().collect();

    let mut pos = byte_pos;
    let mut found_non_ws = false;

    for (idx, segment) in &word_bounds {
        let is_whitespace = segment.chars().all(|c| c.is_whitespace());

        if !is_whitespace {
            found_non_ws = true;
        }

        if found_non_ws && is_whitespace {
            // We've passed through a word and hit whitespace
            pos = byte_pos + idx;
            break;
        }

        // Keep advancing
        pos = byte_pos + idx + segment.len();
    }

    pos.min(text.len())
}

/// Convert byte offset to char count.
pub fn byte_to_char_offset(text: &str, byte_pos: usize) -> usize {
    text[..byte_pos.min(text.len())].chars().count()
}

/// Convert char count to byte offset.
pub fn char_to_byte_offset(text: &str, char_pos: usize) -> usize {
    text.char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
