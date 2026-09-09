//! The built-in Lua UI, embedded in the executable.
//!
//! kn9t ships as a single self-contained binary: the default UI lives in
//! `assets/tui/*.lua` and is compiled in, so a bare executable with no
//! config directory still has a complete interface.
//!
//! Load order at startup:
//!   1. If `~/.kn9t/tui/` is empty or missing, copy default files there.
//!   2. Load all `~/.kn9t/tui/*.lua` files in alphabetical order.
//!
//! The user can reset to defaults anytime via the "Reset TUI config" command.

use std::path::Path;

/// The built-in UI files, compiled into the binary.
/// Each tuple is (filename, content).
pub const DEFAULT_TUI_FILES: &[(&str, &str)] = &[
    (
        "00_theme.lua",
        include_str!("../../assets/tui/00_theme.lua"),
    ),
    (
        "10_state.lua",
        include_str!("../../assets/tui/10_state.lua"),
    ),
    (
        "20_header.lua",
        include_str!("../../assets/tui/20_header.lua"),
    ),
    (
        "30_sidebar_left.lua",
        include_str!("../../assets/tui/30_sidebar_left.lua"),
    ),
    (
        "40_sidebar_right.lua",
        include_str!("../../assets/tui/40_sidebar_right.lua"),
    ),
    (
        "50_status.lua",
        include_str!("../../assets/tui/50_status.lua"),
    ),
    (
        "60_keybinds.lua",
        include_str!("../../assets/tui/60_keybinds.lua"),
    ),
    (
        "90_render.lua",
        include_str!("../../assets/tui/90_render.lua"),
    ),
];

/// Legacy single-file config for backwards compatibility.
pub const DEFAULT_TUI_LUA: &str = include_str!("../../assets/default_tui.lua");

/// Result of an explicit export request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    /// Written to disk.
    Written,
    /// Refused: a file is already there and `force` was not set.
    Exists,
    /// Failed with a message.
    Failed(String),
}

/// Check if the tui/ directory is empty or missing.
pub fn tui_dir_is_empty(dir: &Path) -> bool {
    if !dir.exists() {
        return true;
    }
    match std::fs::read_dir(dir) {
        Ok(mut entries) => !entries.any(|e| {
            e.ok()
                .map(|e| e.path().extension().and_then(|s| s.to_str()) == Some("lua"))
                .unwrap_or(false)
        }),
        Err(_) => true,
    }
}

/// Copy all default TUI files to the given directory.
///
/// Creates the directory if it doesn't exist. Overwrites existing files
/// when `force` is true.
pub fn export_tui_dir(dir: &Path, force: bool) -> ExportOutcome {
    if !force && !tui_dir_is_empty(dir) {
        return ExportOutcome::Exists;
    }

    if let Err(e) = std::fs::create_dir_all(dir) {
        return ExportOutcome::Failed(format!("could not create {}: {e}", dir.display()));
    }

    for (filename, content) in DEFAULT_TUI_FILES {
        let path = dir.join(filename);
        if let Err(e) = std::fs::write(&path, content) {
            return ExportOutcome::Failed(format!("could not write {}: {e}", path.display()));
        }
    }

    ExportOutcome::Written
}

/// Ensure the tui/ directory has config files, copying defaults if empty.
///
/// Called at startup. Does nothing if the directory already has .lua files.
pub fn ensure_tui_config(dir: &Path) -> Result<(), String> {
    if tui_dir_is_empty(dir) {
        match export_tui_dir(dir, false) {
            ExportOutcome::Written => {
                crate::log!("Initialized {} with default TUI config", dir.display());
                Ok(())
            }
            ExportOutcome::Exists => Ok(()), // shouldn't happen but ok
            ExportOutcome::Failed(e) => Err(e),
        }
    } else {
        Ok(())
    }
}

/// Write the built-in config to `path` so the user can edit it.
///
/// Refuses to clobber an existing file unless `force`, because an edited
/// config is worth more than our default.
pub fn export_config(path: &Path, force: bool) -> ExportOutcome {
    if path.exists() && !force {
        return ExportOutcome::Exists;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ExportOutcome::Failed(format!("could not create {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, DEFAULT_TUI_LUA) {
        Ok(()) => ExportOutcome::Written,
        Err(e) => ExportOutcome::Failed(format!("could not write {}: {e}", path.display())),
    }
}

