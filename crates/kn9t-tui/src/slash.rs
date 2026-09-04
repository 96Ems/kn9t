//! Slash commands with fuzzy matching dropdown.
//!
//! When user types "/" in input, show a dropdown of available commands.
//! Fuzzy match as user types more characters.

/// A slash command definition.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub args: &'static str, // e.g., "<model_id>" or ""
}

/// All available slash commands.
pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "session",
        description: "Switch session",
        args: "",
    },
    SlashCommand {
        name: "models",
        description: "Open model picker",
        args: "",
    },
    SlashCommand {
        name: "model",
        description: "Switch to specific model",
        args: "<model_id>",
    },
    SlashCommand {
        name: "new",
        description: "Create new session",
        args: "",
    },
    SlashCommand {
        name: "help",
        description: "Show help",
        args: "",
    },
    SlashCommand {
        name: "quit",
        description: "Quit application",
        args: "",
    },
    SlashCommand {
        name: "abort",
        description: "Abort current turn",
        args: "",
    },
    SlashCommand {
        name: "compact",
        description: "Compact conversation history",
        args: "",
    },
    SlashCommand {
        name: "export",
        description: "Export conversation to file",
        args: "<path>",
    },
    SlashCommand {
        name: "search",
        description: "Search transcript (Ctrl+F)",
        args: "",
    },
    SlashCommand {
        name: "diff",
        description: "Open diff viewer",
        args: "",
    },
    SlashCommand {
        name: "keys",
        description: "Show keybindings",
        args: "",
    },
    SlashCommand {
        name: "palette",
        description: "Open command palette (Ctrl+P)",
        args: "",
    },
    SlashCommand {
        name: "theme",
        description: "Switch theme",
        args: "",
    },
    SlashCommand {
        name: "stash",
        description: "Save current input to stash",
        args: "",
    },
    SlashCommand {
        name: "pop",
        description: "Restore input from stash (git-style)",
        args: "",
    },
    SlashCommand {
        name: "rename",
        description: "Rename current session",
        args: "<title>",
    },
];

/// State for slash command completion.
#[derive(Debug, Clone, Default)]
pub struct SlashState {
    /// Whether we're in slash command mode.
    pub active: bool,
    /// Current query (text after /).
    pub query: String,
    /// Filtered matches.
    pub matches: Vec<usize>,
    /// Selected index in matches.
    pub selected: usize,
}

impl SlashState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start slash command mode.
    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.update_matches();
        self.selected = 0;
    }

    /// Exit slash command mode.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.matches.clear();
        self.selected = 0;
    }

    /// Update query and re-filter matches.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_lowercase();
        self.update_matches();
        // Keep selected in bounds.
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }

    /// Get currently selected command (if any).
    pub fn selected_command(&self) -> Option<&SlashCommand> {
        self.matches.get(self.selected).map(|&i| &COMMANDS[i])
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            if self.selected == 0 {
                self.selected = self.matches.len() - 1;
            } else {
                self.selected -= 1;
            }
        }
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    fn update_matches(&mut self) {
        self.matches = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| fuzzy_match(cmd.name, &self.query))
            .map(|(i, _)| i)
            .collect();
    }
}

/// Simple fuzzy match — query chars must appear in order in target.
pub fn fuzzy_match(target: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let mut query_chars = query.chars().peekable();
    for ch in target.chars() {
        if let Some(&qch) = query_chars.peek() {
            if ch.to_ascii_lowercase() == qch.to_ascii_lowercase() {
                query_chars.next();
            }
        }
        if query_chars.peek().is_none() {
            return true;
        }
    }

    query_chars.peek().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match() {
        assert!(fuzzy_match("model", ""));
        assert!(fuzzy_match("model", "m"));
        assert!(fuzzy_match("model", "mod"));
        assert!(fuzzy_match("model", "mdl"));
        assert!(fuzzy_match("model", "model"));
        assert!(!fuzzy_match("model", "x"));
        assert!(!fuzzy_match("model", "modelx"));
    }
}
