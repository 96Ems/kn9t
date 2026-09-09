//! Prompt stash for saving/restoring input state.
//!
//! - /stash: Save current input to stash
//! - /unstash: Restore input from stash
//! - Ctrl+S / Ctrl+Shift+S: Keybindings (optional)

/// Stash for saving prompt state.
#[derive(Debug, Default)]
pub struct PromptStash {
    /// Stashed text (None if empty).
    text: Option<String>,
    /// Stashed cursor position (row, col).
    cursor: Option<(usize, usize)>,
}

impl PromptStash {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stash the current prompt state.
    pub fn stash(&mut self, text: &str, cursor_row: usize, cursor_col: usize) {
        if text.is_empty() {
            return;
        }
        self.text = Some(text.to_string());
        self.cursor = Some((cursor_row, cursor_col));
    }

    /// Unstash and return the saved state, clearing the stash.
    /// Returns (text, cursor_row, cursor_col) or None if empty.
    pub fn unstash(&mut self) -> Option<(String, usize, usize)> {
        let text = self.text.take()?;
        let (row, col) = self.cursor.take().unwrap_or((0, 0));
        Some((text, row, col))
    }

    /// Peek at stashed text without consuming it.
    pub fn peek(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Check if stash has content.
    pub fn has_content(&self) -> bool {
        self.text.is_some()
    }

    /// Clear the stash.
    pub fn clear(&mut self) {
        self.text = None;
        self.cursor = None;
    }
}

