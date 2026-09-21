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
