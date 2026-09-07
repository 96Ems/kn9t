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
    use crate::lua::commands::LuaCommandRegistry;

    fn empty_lua() -> LuaCommandRegistry {
        LuaCommandRegistry::new()
    }

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

    #[test]
    fn activate_shows_all_builtins_with_no_lua_registered() {
        let mut state = SlashState::new();
        state.activate(&empty_lua());
        assert_eq!(state.matches.len(), COMMANDS.len());
    }

    /// A Lua command with no `slash=` must not appear in the dropdown — it's
    /// palette-only, which is the whole point of making `slash` optional.
    #[test]
    fn lua_command_without_slash_is_excluded() {
        let lua = mlua::Lua::new();
        crate::lua::commands::install_command_api(&lua).unwrap();
        lua.load(r#"kn9t.register_command({id = "x", handler = function() end})"#)
            .exec()
            .unwrap();
        let mut reg = LuaCommandRegistry::new();
        crate::lua::commands::drain_pending_commands(&lua, &mut reg).unwrap();

        let mut state = SlashState::new();
        state.activate(&reg);
        assert_eq!(
            state.matches.len(),
            COMMANDS.len(),
            "no slash= means not in the dropdown"
        );
    }

    /// A Lua command WITH `slash=` must appear and be selectable, and its
    /// stored name must have the leading slash already stripped (mirrors
    /// `commands::drain_pending_commands`'s own stripping).
    #[test]
    fn lua_command_with_slash_is_included_and_selectable() {
        let lua = mlua::Lua::new();
        crate::lua::commands::install_command_api(&lua).unwrap();
        lua.load(
            r#"
            kn9t.register_command({
                id = "view_diff", slash = "/view", handler = function() end,
            })
        "#,
        )
        .exec()
        .unwrap();
        let mut reg = LuaCommandRegistry::new();
        crate::lua::commands::drain_pending_commands(&lua, &mut reg).unwrap();

        let mut state = SlashState::new();
        state.activate(&reg);
        assert_eq!(state.matches.len(), COMMANDS.len() + 1);

        state.set_query("view");
        let found = state.selected_command().expect("must match");
        assert_eq!(found.name, "view");
        assert!(found.is_lua);
    }
}
