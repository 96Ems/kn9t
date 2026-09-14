//! Slash commands with fuzzy matching dropdown.
//!
//! When user types "/" in input, show a dropdown of available commands.
//! Fuzzy match as user types more characters.
//!
//! Entries come from two sources, same as the command palette: the built-in
//! `COMMANDS` (compile-time) and `kn9t.register_command({slash="...", ...})`
//! (runtime). `SlashEntry` normalizes both.

/// A built-in slash command definition.
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
    SlashCommand {
        name: "queue",
        description: "Queue message for next turn",
        args: "<message>",
    },
    SlashCommand {
        name: "q",
        description: "Queue message (alias for /queue)",
        args: "<message>",
    },
];

/// One searchable/selectable slash-dropdown row, from either source.
#[derive(Debug, Clone)]
pub struct SlashEntry {
    pub name: String,
    pub description: String,
    pub args: String,
    /// Whether this came from `kn9t.register_command({slash=...})`, so the
    /// executor knows to call the Lua handler for it.
    pub is_lua: bool,
    /// The `LuaCommandRegistry` id to look up when `is_lua` is true. Distinct
    /// from `name` (the slash text) because a Lua command's `id` and its
    /// `slash=` string need not match (e.g. id="view_diff", slash="/view").
    /// Empty for built-ins, which are looked up by `name` in `COMMANDS`
    /// instead.
    pub lua_id: String,
}

impl From<&SlashCommand> for SlashEntry {
    fn from(cmd: &SlashCommand) -> Self {
        Self {
            name: cmd.name.to_string(),
            description: cmd.description.to_string(),
            args: cmd.args.to_string(),
            is_lua: false,
            lua_id: String::new(),
        }
    }
}

impl From<&crate::lua::commands::LuaCommand> for SlashEntry {
    fn from(cmd: &crate::lua::commands::LuaCommand) -> Self {
        Self {
            name: cmd.slash.clone().unwrap_or_default(),
            description: cmd.description.clone(),
            args: String::new(),
            is_lua: true,
            lua_id: cmd.id.clone(),
        }
    }
}

/// State for slash command completion.
#[derive(Debug, Clone, Default)]
pub struct SlashState {
    /// Whether we're in slash command mode.
    pub active: bool,
    /// Current query (text after /).
    pub query: String,
    /// All entries as of the last `activate` (built-ins + Lua-registered ones
    /// that declared a `slash=`). `matches` indexes into THIS, not `COMMANDS`.
    entries: Vec<SlashEntry>,
    /// Filtered indices into `entries`.
    pub matches: Vec<usize>,
    /// Selected index in matches.
    pub selected: usize,
}

impl SlashState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start slash command mode, snapshotting `lua_commands` (only the ones
    /// that declared `slash=`) alongside the built-ins. See
    /// `CommandPalette::open` for why this is a snapshot, not a live borrow.
    pub fn activate(&mut self, lua_commands: &crate::lua::commands::LuaCommandRegistry) {
        self.active = true;
        self.query.clear();
        self.entries = COMMANDS
            .iter()
            .map(SlashEntry::from)
            .chain(
                lua_commands
                    .iter()
                    .filter(|c| c.slash.is_some())
                    .map(SlashEntry::from),
            )
            .collect();
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
    pub fn selected_command(&self) -> Option<&SlashEntry> {
        self.matches.get(self.selected).map(|&i| &self.entries[i])
    }

    /// All current entries, for the renderer.
    pub fn entries(&self) -> &[SlashEntry] {
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
            .filter(|(_, cmd)| fuzzy_match(&cmd.name, &self.query))
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
            if ch.eq_ignore_ascii_case(&qch) {
                query_chars.next();
            }
        }
        if query_chars.peek().is_none() {
            return true;
        }
    }

    query_chars.peek().is_none()
}

