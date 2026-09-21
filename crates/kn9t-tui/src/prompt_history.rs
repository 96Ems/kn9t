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
            // Start at most recent match — safe: matches not empty checked above
            let idx = *matches
                .last()
                .expect("matches non-empty after is_empty check");
            self.position = Some(idx);
            return Some(&self.history[idx]);
        }

        // Already navigating: go to previous match — safe: checked position.is_none() above
        let current_pos = self.position.expect("position Some after is_none check");
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
        self.position?;

        // Filter history by prefix
        let matches: Vec<usize> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, h)| h.starts_with(&self.prefix))
            .map(|(i, _)| i)
            .collect();

        // Safe: early return via `?` above guarantees position is Some
        let current_pos = self.position.expect("position Some after ? early return");
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

impl PromptHistory {
    /// Construct a pre-populated history for integration tests (bypasses disk I/O).
    #[doc(hidden)]
    pub fn for_test(history: Vec<String>) -> Self {
        Self {
            history,
            position: None,
            stashed: None,
            prefix: String::new(),
            path: std::path::PathBuf::from("/tmp/test_history.json"),
            dirty: false,
        }
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
