//! Command palette — Ctrl+P quick-access to all commands.
//!
//! Unlike slash commands (which require typing /), the command palette
//! provides instant access to all actions via fuzzy search.
//!
//! Entries come from two sources: the built-in `COMMANDS` (compile-time,
//! `'static`) and `kn9t.register_command(...)` (runtime, owned `String`s).
//! `PaletteEntry` normalizes both into one thing the palette can search,
//! display and select without caring which source a given command came from.

use crate::slash::fuzzy_match;

/// A built-in command entry in the palette.
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

/// One searchable/selectable palette row, from either source.
///
/// Built fresh on `open()`/query changes from `COMMANDS` plus whatever
/// `kn9t.register_command` currently has registered — see
/// `CommandPalette::refresh` and its `lua_commands` parameter. This is an
/// owned snapshot (not `&'static` for the Lua case) since a Lua string cannot
/// be `'static`.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    pub keybinding: Option<String>,
    /// Grouping label for display. Built-ins use `Category::label()`; Lua
    /// commands use whatever string `category=` gave, or "Lua" if omitted.
    pub category: String,
    /// Whether this entry came from `kn9t.register_command`, so the executor
    /// knows to call the Lua handler instead of matching `id` against the
    /// built-in Rust `match`.
    pub is_lua: bool,
}

impl From<&PaletteCommand> for PaletteEntry {
    fn from(cmd: &PaletteCommand) -> Self {
        Self {
            id: cmd.id.to_string(),
            label: cmd.label.to_string(),
            description: cmd.description.to_string(),
            keybinding: cmd.keybinding.map(|s| s.to_string()),
            category: cmd.category.label().to_string(),
            is_lua: false,
        }
    }
}

impl From<&crate::lua::commands::LuaCommand> for PaletteEntry {
    fn from(cmd: &crate::lua::commands::LuaCommand) -> Self {
        Self {
            id: cmd.id.clone(),
            label: cmd.label.clone(),
            description: cmd.description.clone(),
            keybinding: None,
            category: cmd.category.clone(),
            is_lua: true,
        }
    }
}

/// All available built-in commands.
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
        id: "keybindings",
        label: "Show Keybindings",
        description: "Display all keyboard shortcuts",
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
    PaletteCommand {
        id: "tools_manager",
        label: "Manage Tools",
        description: "Enable/disable tools for this session",
        keybinding: Some("Ctrl+T"),
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
    /// All entries as of the last `refresh` (built-ins + Lua-registered),
    /// before filtering. `matches` indexes into THIS, not into `COMMANDS`.
    entries: Vec<PaletteEntry>,
    /// Filtered indices into `entries`.
    pub matches: Vec<usize>,
    /// Selected index in matches.
    pub selected: usize,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the palette, snapshotting `lua_commands` alongside the built-ins.
    ///
    /// A snapshot rather than a live reference: the palette can stay open
    /// across a hot-reload without needing `&App` on every keystroke, and a
    /// reload mid-search simply won't be reflected until the palette is
    /// reopened — an acceptable staleness window for an interactive picker.
    pub fn open(&mut self, lua_commands: &crate::lua::commands::LuaCommandRegistry) {
        self.active = true;
        self.query.clear();
        self.entries = COMMANDS
            .iter()
            .map(PaletteEntry::from)
            .chain(lua_commands.iter().map(PaletteEntry::from))
            .collect();
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
    pub fn selected_command(&self) -> Option<&PaletteEntry> {
        self.matches.get(self.selected).map(|&i| &self.entries[i])
    }

    /// All current entries, for the renderer.
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
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
        self.matches = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                // Match against label and description
                fuzzy_match(&cmd.label, &self.query)
                    || fuzzy_match(&cmd.description, &self.query)
                    || fuzzy_match(&cmd.id, &self.query)
            })
            .map(|(i, _)| i)
            .collect();
    }
}

