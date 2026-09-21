//! Kill ring for Emacs-style text deletion and yank.
//!
//! The kill ring stores deleted text and allows:
//! - Ctrl+K: Kill to end of line
//! - Ctrl+U: Kill to start of line  
//! - Ctrl+W: Kill word backward
//! - Ctrl+Y: Yank (paste most recent kill)
//! - Alt+Y: Yank pop (cycle through kill ring after yank)

use std::collections::VecDeque;

/// Maximum number of entries in the kill ring.
const MAX_RING_SIZE: usize = 10;

/// Kill ring for storing deleted text.
#[derive(Debug)]
pub struct KillRing {
    /// Ring buffer of killed text.
    ring: VecDeque<String>,
    /// Index for yank-pop cycling (only valid immediately after yank).
    yank_index: Option<usize>,
    /// Position where last yank was inserted (for yank-pop replacement).
    last_yank_pos: Option<(usize, usize)>, // (start_byte, len)
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new()
    }
}

impl KillRing {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::new(),
            yank_index: None,
            last_yank_pos: None,
        }
    }

    /// Add text to the kill ring.
    ///
    /// If `append` is true and there's a recent kill, append to it instead
    /// of creating a new entry (useful for consecutive kill-line commands).
    pub fn kill(&mut self, text: String, append: bool) {
        if text.is_empty() {
            return;
        }

        // Reset yank state - new kill breaks yank-pop chain
        self.yank_index = None;
        self.last_yank_pos = None;

        if append && !self.ring.is_empty() {
            // Append to most recent kill
            if let Some(front) = self.ring.front_mut() {
                front.push_str(&text);
            }
        } else {
            // Add new entry
            self.ring.push_front(text);
            if self.ring.len() > MAX_RING_SIZE {
                self.ring.pop_back();
            }
        }
    }

    /// Yank (paste) the most recent kill.
    ///
    /// Returns the text to insert, or None if ring is empty.
    /// Records position for potential yank-pop.
    pub fn yank(&mut self, insert_pos: usize) -> Option<&str> {
        if self.ring.is_empty() {
            return None;
        }

        self.yank_index = Some(0);
        let text = self.ring.front().map(|s| s.as_str())?;
        self.last_yank_pos = Some((insert_pos, text.len()));
        Some(text)
    }

    /// Yank pop: replace last yanked text with previous kill ring entry.
    ///
    /// Only valid immediately after yank or yank-pop.
    /// Returns (text_to_remove_len, text_to_insert), or None if not in yank state.
    pub fn yank_pop(&mut self) -> Option<(usize, &str)> {
        let idx = self.yank_index?;
        let (_pos, len) = self.last_yank_pos?;

        if self.ring.is_empty() {
            return None;
        }

        // Cycle to next entry
        let new_idx = (idx + 1) % self.ring.len();
        self.yank_index = Some(new_idx);

        let text = &self.ring[new_idx];
        // Safe: checked via ? above that last_yank_pos is Some
        self.last_yank_pos = Some((
            self.last_yank_pos.expect("checked Some above").0,
            text.len(),
        ));

        Some((len, text.as_str()))
    }

    /// Reset yank state (called on any non-yank action).
    pub fn reset_yank(&mut self) {
        self.yank_index = None;
        self.last_yank_pos = None;
    }

    /// Check if currently in yank state (yank-pop is valid).
    pub fn in_yank_state(&self) -> bool {
        self.yank_index.is_some()
    }

    /// Get the last yank position for replacement.
    pub fn last_yank_pos(&self) -> Option<(usize, usize)> {
        self.last_yank_pos
    }

    /// Get kill ring size (for debugging).
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Check if kill ring is empty.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}
