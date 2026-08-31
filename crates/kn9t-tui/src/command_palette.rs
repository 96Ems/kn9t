//! Command palette — Ctrl+P quick-access to all commands.
//!
//! Unlike slash commands (which require typing /), the command palette
//! provides instant access to all actions via fuzzy search.

use crate::slash::fuzzy_match;

/// A command entry in the palette.
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub keybinding: Option<&'static str>,
    pub category: Category,
}

/// Command categories for grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Navigation,
    Session,
    Edit,
    View,
    Tools,
    Settings,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::Navigation => "Navigation",
            Category::Session => "Session",
            Category::Edit => "Edit",
            Category::View => "View",
            Category::Tools => "Tools",
            Category::Settings => "Settings",
        }
    }
}

/// All available commands.
pub const COMMANDS: &[PaletteCommand] = &[
    // Navigation
    PaletteCommand {
        id: "scroll_up",
        label: "Scroll Up",
        description: "Scroll transcript up one line",
        keybinding: Some("k"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "scroll_down",
        label: "Scroll Down",
        description: "Scroll transcript down one line",
        keybinding: Some("j"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "page_up",
        label: "Page Up",
        description: "Scroll up half page",
        keybinding: Some("Ctrl+U"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "page_down",
        label: "Page Down",
        description: "Scroll down half page",
        keybinding: Some("Ctrl+D"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "jump_top",
        label: "Jump to Top",
        description: "Go to start of transcript",
        keybinding: Some("gg"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "jump_bottom",
        label: "Jump to Bottom",
        description: "Go to end of transcript",
        keybinding: Some("G"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "prev_message",
        label: "Previous Message",
        description: "Jump to previous assistant message",
        keybinding: Some("Alt+K"),
        category: Category::Navigation,
    },
    PaletteCommand {
        id: "next_message",
        label: "Next Message",
        description: "Jump to next assistant message",
        keybinding: Some("Alt+J"),
        category: Category::Navigation,
    },
    
    // Session
    PaletteCommand {
        id: "new_session",
        label: "New Session",
        description: "Create a new chat session",
        keybinding: Some("Ctrl+N"),
        category: Category::Session,
    },
    PaletteCommand {
        id: "session_list",
        label: "Session List",
        description: "Open session picker",
        keybinding: None,
        category: Category::Session,
    },
    PaletteCommand {
        id: "abort",
        label: "Abort Turn",
        description: "Cancel the current AI response",
        keybinding: Some("Ctrl+C"),
        category: Category::Session,
    },
    
    // Edit
    PaletteCommand {
        id: "undo",
        label: "Undo",
        description: "Undo last input change",
        keybinding: Some("Ctrl+Z"),
        category: Category::Edit,
    },
    PaletteCommand {
        id: "redo",
        label: "Redo",
        description: "Redo last undone change",
        keybinding: Some("Ctrl+Shift+Z"),
        category: Category::Edit,
    },
    PaletteCommand {
        id: "kill_line",
        label: "Kill Line",
        description: "Cut from cursor to end of line",
        keybinding: Some("Ctrl+K"),
        category: Category::Edit,
    },
    PaletteCommand {
        id: "kill_word",
        label: "Kill Word",
        description: "Cut previous word",
        keybinding: Some("Ctrl+W"),
        category: Category::Edit,
    },
    PaletteCommand {
        id: "yank",
        label: "Yank (Paste)",
        description: "Paste from kill ring",
        keybinding: Some("Ctrl+Y"),
        category: Category::Edit,
    },
    
    // View
    PaletteCommand {
        id: "search",
        label: "Search Transcript",
        description: "Find text in conversation",
        keybinding: Some("Ctrl+F"),
        category: Category::View,
    },
    PaletteCommand {
        id: "toggle_thinking",
        label: "Toggle Thinking Blocks",
        description: "Expand/collapse all thinking sections",
        keybinding: Some("Ctrl+E"),
        category: Category::View,
    },
    PaletteCommand {
        id: "toggle_sidebar",
        label: "Toggle Sidebar",
        description: "Show/hide right sidebar",
        keybinding: None,
        category: Category::View,
    },
    PaletteCommand {
        id: "keybindings",
        label: "Show Keybindings",
        description: "Display all keyboard shortcuts",
        keybinding: None,
        category: Category::View,
    },
    PaletteCommand {
        id: "diff_viewer",
        label: "Open Diff Viewer",
        description: "View file diffs from tool results",
        keybinding: None,
        category: Category::View,
    },
    
    // Tools
    PaletteCommand {
        id: "models",
        label: "Switch Model",
        description: "Open model picker",
        keybinding: Some("Ctrl+M"),
        category: Category::Tools,
    },
    PaletteCommand {
        id: "compact",
        label: "Compact History",
        description: "Compress conversation to save context",
        keybinding: None,
        category: Category::Tools,
    },
    PaletteCommand {
        id: "export",
        label: "Export Conversation",
        description: "Save conversation to file",
        keybinding: None,
        category: Category::Tools,
    },
    
    // Settings
    PaletteCommand {
        id: "theme_toggle",
        label: "Toggle Theme",
        description: "Switch between light and dark theme",
        keybinding: None,
        category: Category::Settings,
    },
    PaletteCommand {
        id: "quit",
        label: "Quit",
        description: "Exit kn9t",
        keybinding: Some("Ctrl+Q"),
        category: Category::Settings,
    },
];

/// Command palette state.
#[derive(Debug, Clone, Default)]
pub struct CommandPalette {
    /// Whether the palette is open.
    pub active: bool,
    /// Current search query.
    pub query: String,
    /// Filtered command indices.
    pub matches: Vec<usize>,
    /// Selected index in matches.
    pub selected: usize,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Open the palette.
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.update_matches();
        self.selected = 0;
    }
    
    /// Close the palette.
    pub fn close(&mut self) {
        self.active = false;
        self.query.clear();
        self.matches.clear();
        self.selected = 0;
    }
    
    /// Update search query.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.update_matches();
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
    
    /// Add a character to the query.
    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.update_matches();
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
    
    /// Remove last character from query.
    pub fn pop_char(&mut self) {
        self.query.pop();
        self.update_matches();
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
    
    /// Get selected command.
    pub fn selected_command(&self) -> Option<&'static PaletteCommand> {
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
            .filter(|(_, cmd)| {
                // Match against label and description
                fuzzy_match(cmd.label, &self.query) || 
                fuzzy_match(cmd.description, &self.query) ||
                fuzzy_match(cmd.id, &self.query)
            })
            .map(|(i, _)| i)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_palette_open_shows_all() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(palette.active);
        assert_eq!(palette.matches.len(), COMMANDS.len());
    }
    
    #[test]
    fn test_palette_filter() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.set_query("search");
        assert!(palette.matches.len() < COMMANDS.len());
        assert!(palette.selected_command().is_some());
    }
    
    #[test]
    fn test_palette_navigation() {
        let mut palette = CommandPalette::new();
        palette.open();
        let initial = palette.selected;
        palette.select_next();
        assert_eq!(palette.selected, initial + 1);
        palette.select_prev();
        assert_eq!(palette.selected, initial);
    }
}
