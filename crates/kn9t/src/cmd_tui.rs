//! kn9t tui: manage TUI settings and configuration.
//!
//! Subcommands:
//!   kn9t tui reset [--force]   — Reset TUI config to defaults
//!
//! The TUI config lives in `~/.kn9t/tui/` as numbered Lua files (00_theme.lua,
//! 10_state.lua, etc.). On first run, defaults are copied there automatically.
//! This command lets you reset to defaults if you break something.

use std::fs;
use std::path::PathBuf;

use crate::bootstrap::kn9t_home_path;

fn tui_dir() -> PathBuf {
    kn9t_home_path().join("tui")
}

/// The built-in TUI files, same as in kn9t-tui.
const DEFAULT_TUI_FILES: &[(&str, &str)] = &[
    (
        "00_theme.lua",
        include_str!("../../kn9t-tui/assets/tui/00_theme.lua"),
    ),
    (
        "10_state.lua",
        include_str!("../../kn9t-tui/assets/tui/10_state.lua"),
    ),
    (
        "20_header.lua",
        include_str!("../../kn9t-tui/assets/tui/20_header.lua"),
    ),
    (
        "30_sidebar_left.lua",
        include_str!("../../kn9t-tui/assets/tui/30_sidebar_left.lua"),
    ),
    (
        "40_sidebar_right.lua",
        include_str!("../../kn9t-tui/assets/tui/40_sidebar_right.lua"),
    ),
    (
        "50_status.lua",
        include_str!("../../kn9t-tui/assets/tui/50_status.lua"),
    ),
    (
        "60_keybinds.lua",
        include_str!("../../kn9t-tui/assets/tui/60_keybinds.lua"),
    ),
    (
        "90_render.lua",
        include_str!("../../kn9t-tui/assets/tui/90_render.lua"),
    ),
];

fn print_help() {
    println!("kn9t tui — TUI configuration management");
    println!();
    println!("Usage:");
    println!("  kn9t tui reset [--force]    Reset TUI config to defaults");
    println!();
    println!("Options:");
    println!("  --force    Overwrite existing files (default: only if empty)");
    println!("  -h, --help Show this help");
    println!();
    println!("Config location: ~/.kn9t/tui/");
    println!("Files: 00_theme.lua, 10_state.lua, 20_header.lua, ..., 90_render.lua");
}

fn reset(force: bool) {
    let dir = tui_dir();

    // Check if directory has .lua files
    let has_lua_files = dir.is_dir()
        && fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("lua"))
            })
            .unwrap_or(false);

    if has_lua_files && !force {
        eprintln!("TUI config already exists at {}", dir.display());
        eprintln!("Use --force to overwrite");
        std::process::exit(1);
    }

    // Create directory if needed
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("Failed to create {}: {}", dir.display(), e);
        std::process::exit(1);
    }

    // Write all files
    for (filename, content) in DEFAULT_TUI_FILES {
        let path = dir.join(filename);
        if let Err(e) = fs::write(&path, content) {
            eprintln!("Failed to write {}: {}", path.display(), e);
            std::process::exit(1);
        }
        println!("  wrote {}", path.display());
    }

    println!();
    println!(
        "TUI config reset to defaults ({} files)",
        DEFAULT_TUI_FILES.len()
    );
}

pub fn run(args: &[String]) {
    if args.is_empty() {
        print_help();
        return;
    }

    match args[0].as_str() {
        "reset" => {
            let force = args.iter().any(|a| a == "--force" || a == "-f");
            if args.iter().any(|a| a == "-h" || a == "--help") {
                println!("kn9t tui reset — reset TUI config to defaults");
                println!();
                println!("Usage: kn9t tui reset [--force]");
                println!();
                println!("Options:");
                println!("  --force, -f    Overwrite existing files");
                return;
            }
            reset(force);
        }
        "-h" | "--help" | "help" => print_help(),
        other => {
            eprintln!("Unknown subcommand: {}", other);
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    }
}
