//! Prompt history for navigating previous prompts with Up/Down.
//!
//! - Up (on first line): previous prompt
//! - Down (on last line): next prompt
//! - Typing filters history by prefix
//! - Current input is stashed when navigating

use std::env;
use std::fs;
use std::path::PathBuf;

/// Maximum number of prompts to store.
const MAX_HISTORY: usize = 500;

/// Prompt history manager.
#[derive(Debug)]
pub struct PromptHistory {
    /// History of prompts (oldest first).
    history: Vec<String>,
    /// Current position in history (None = editing new prompt).
    position: Option<usize>,
    /// Stashed input when navigating history.
    stashed: Option<String>,
    /// Prefix filter (what user typed before pressing Up).
    prefix: String,
    /// Path to persist history.
    path: PathBuf,
    /// Whether history has been modified.
    dirty: bool,
}

impl PromptHistory {
    /// Create a new prompt history, loading from disk if available.
    pub fn new() -> Self {
        let path = Self::history_path();
        let history = Self::load_from_disk(&path).unwrap_or_default();

        Self {
            history,
            position: None,
            stashed: None,
            prefix: String::new(),
            path,
            dirty: false,
        }
    }

    /// Get the history file path.
    fn history_path() -> PathBuf {
        let mut path = Self::home_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push(".kn9t");
        path.push("prompt_history.json");
        path
    }

    /// Get home directory (cross-platform).
    fn home_dir() -> Option<PathBuf> {
        env::var("HOME")
            .or_else(|_| env::var("USERPROFILE"))
            .ok()
            .map(PathBuf::from)
    }

    /// Load history from disk.
    fn load_from_disk(path: &PathBuf) -> Option<Vec<String>> {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save history to disk.
    pub fn save(&self) {
        if !self.dirty {
            return;
        }

        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(json) = serde_json::to_string_pretty(&self.history) {
            let _ = fs::write(&self.path, json);
        }
    }

    /// Add a prompt to history.
    ///
    /// Deduplicates consecutive identical entries.
    pub fn add(&mut self, prompt: String) {
        if prompt.trim().is_empty() {
            return;
        }

        // Don't add if same as last entry
        if self.history.last().map(|s| s.as_str()) == Some(prompt.as_str()) {
            return;
        }

        self.history.push(prompt);

        // Trim to max size
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }

        self.dirty = true;
        self.reset();
    }

    /// Navigate to previous prompt in history.
    ///
    /// - `current_input`: current text in input box
    /// - `cursor_row`: current cursor row (0-indexed)
    ///
    /// Returns the text to display, or None if at beginning of history.
    pub fn prev(&mut self, current_input: &str, cursor_row: usize) -> Option<&str> {
        // Only navigate when cursor is on first line
        if cursor_row > 0 {
            return None;
        }

        // Filter history by prefix (what user has typed)
        let matches: Vec<usize> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, h)| h.starts_with(&self.prefix))
            .map(|(i, _)| i)
            .collect();

        if matches.is_empty() {
            return None;
        }

        // First navigation: stash current input and set prefix
        if self.position.is_none() {
            self.stashed = Some(current_input.to_string());
            self.prefix = current_input.to_string();
            // Re-filter with new prefix
            let matches: Vec<usize> = self
                .history
                .iter()
                .enumerate()
                .filter(|(_, h)| h.starts_with(&self.prefix))
                .map(|(i, _)| i)
                .collect();
            if matches.is_empty() {
                self.stashed = None;
                return None;
            }
            // Start at most recent match
            let idx = *matches.last().unwrap();
            self.position = Some(idx);
            return Some(&self.history[idx]);
        }

        // Already navigating: go to previous match
        let current_pos = self.position.unwrap();
        let prev_match = matches.iter().rev().find(|&&i| i < current_pos).copied();

        if let Some(idx) = prev_match {
            self.position = Some(idx);
            Some(&self.history[idx])
        } else {
            None // At beginning of history
        }
    }

    /// Navigate to next prompt in history.
    ///
    /// - `cursor_row`: current cursor row (0-indexed)
    /// - `total_lines`: total lines in input
    ///
    /// Returns the text to display, or the stashed input if at end.
    pub fn next(&mut self, cursor_row: usize, total_lines: usize) -> Option<String> {
        // Only navigate when cursor is on last line
        if cursor_row < total_lines.saturating_sub(1) {
            return None;
        }

        // Not navigating
        if self.position.is_none() {
            return None;
        }

        // Filter history by prefix
        let matches: Vec<usize> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, h)| h.starts_with(&self.prefix))
            .map(|(i, _)| i)
            .collect();

        let current_pos = self.position.unwrap();
        let next_match = matches.iter().find(|&&i| i > current_pos).copied();

        if let Some(idx) = next_match {
            self.position = Some(idx);
            Some(self.history[idx].clone())
        } else {
            // At end of history, return stashed input
            let stashed = self.stashed.take();
            self.reset();
            stashed
        }
    }

    /// Reset navigation state.
    pub fn reset(&mut self) {
        self.position = None;
        self.stashed = None;
        self.prefix.clear();
    }

    /// Check if currently navigating history.
    pub fn is_navigating(&self) -> bool {
        self.position.is_some()
    }

    /// Get history length.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// Check if history is empty.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }
}

impl Default for PromptHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PromptHistory {
    fn drop(&mut self) {
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_history() -> PromptHistory {
        PromptHistory {
            history: vec![
                "first prompt".into(),
                "second prompt".into(),
                "fix bug".into(),
                "fix test".into(),
            ],
            position: None,
            stashed: None,
            prefix: String::new(),
            path: PathBuf::from("/tmp/test_history.json"),
            dirty: false,
        }
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
}
