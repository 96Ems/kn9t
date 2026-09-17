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
///
/// **This is the shipped default.** A fresh install renders these, seeded into
/// `~/.kn9t/tui/`; there is no separate single-file fallback, so what the tests boot
/// (`LuaRuntime::load_builtin`) and what a user sees are the same source. Two copies would
/// drift, and the drift would be invisible — a test can be green against a default nobody
/// renders. (It was, until PLAN §P7 L1: the catalogue and `default_tui.lua` disagreed and
/// the test asserted the file that was not shipped.)
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

/// The whole built-in UI as one source string, for `--print-config`.
///
/// Concatenated with a delimiter naming each file, because the UI is a catalogue rather
/// than a single file: printing eight files run together with no marker would read as one
/// malformed script. For inspection only — the loader runs the files one at a time
/// (`LuaRuntime::load_builtin`), so a syntax error is attributed to its own file.
pub fn builtin_source() -> String {
    let mut out = String::from(
        "-- The built-in kn9t UI, concatenated for inspection.\n\
         -- The loader runs these as separate files, in this order:\n",
    );
    for (name, _) in DEFAULT_TUI_FILES {
        out.push_str(&format!("--   {name}\n"));
    }
    out.push_str("--\n-- To customise it, edit ~/.kn9t/tui/*.lua and re-run; changes hot-reload.\n\n");
    for (name, content) in DEFAULT_TUI_FILES {
        out.push_str(&format!(
            "\n-- ═══════════════════════════════════════════════════════════════\n\
             -- {name}\n\
             -- ═══════════════════════════════════════════════════════════════\n\n"
        ));
        out.push_str(content);
    }
    out
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

/// Print the whole built-in UI to `path`, for inspection.
///
/// The UI is a catalogue (`assets/tui/*.lua`), and this is a *rendering* of it, not the
/// thing the loader runs — `--print-config` exists to read the default, not to edit it.
/// Refuses to clobber an existing file unless `force`.
pub fn export_config(path: &Path, force: bool) -> ExportOutcome {
    if path.exists() && !force {
        return ExportOutcome::Exists;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ExportOutcome::Failed(format!("could not create {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, builtin_source()) {
        Ok(()) => ExportOutcome::Written,
        Err(e) => ExportOutcome::Failed(format!("could not write {}: {e}", path.display())),
    }
}

