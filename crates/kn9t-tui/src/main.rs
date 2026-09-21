//! kn9t-tui entry point.

use std::io::{self, stdout};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use kn9t_tui::app::App;
use kn9t_tui::config::Config;
use kn9t_tui::event::{spawn_input_thread, spawn_tick_thread, EventLoop};

/// Handle CLI flags that exit without starting the TUI.
///
/// Returns `Some(exit_code)` when the process should stop here.
fn handle_cli_args() -> Option<i32> {
    use kn9t_tui::lua::default_config::{builtin_source, export_config, ExportOutcome};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |name: &str| args.iter().any(|a| a == name);

    if has("-h") || has("--help") {
        println!(
            "kn9t-tui — terminal UI for kn9t\n\
             \n\
             USAGE:\n\
             \x20   kn9t-tui [OPTIONS]\n\
             \n\
             OPTIONS:\n\
             \x20   -h, --help           Show this help\n\
             \x20   --print-config       Print the built-in tui.lua to stdout\n\
             \x20   --export-config      Write the built-in tui.lua to ~/.kn9t/tui.lua\n\
             \x20   --force              Allow --export-config to overwrite\n\
             \n\
             The UI is defined in Lua and embedded in this binary, so no files\n\
             are required. To customise it, run --export-config and edit the\n\
             result; changes hot-reload. Anything you leave out falls back to\n\
             the built-in defaults."
        );
        return Some(0);
    }

    if has("--print-config") {
        print!("{}", builtin_source());
        return Some(0);
    }

    if has("--export-config") {
        let Some(path) = kn9t_tui::lua::default_config_path() else {
            eprintln!("could not determine config path (no HOME/USERPROFILE)");
            return Some(1);
        };
        return match export_config(&path, has("--force")) {
            ExportOutcome::Written => {
                println!("Wrote {}", path.display());
                println!("Edit it and kn9t-tui will hot-reload the changes.");
                Some(0)
            }
            ExportOutcome::Exists => {
                eprintln!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
                Some(1)
            }
            ExportOutcome::Failed(e) => {
                eprintln!("export failed: {e}");
                Some(1)
            }
        };
    }

    None
}

fn main() -> io::Result<()> {
    // CLI flags are handled before any terminal setup.
    if let Some(code) = handle_cli_args() {
        std::process::exit(code);
    }

    // Initialize debug log.
    kn9t_tui::log::init("kn9t-tui.log");
    kn9t_tui::log!("=== kn9t-tui starting ===");

    // Load config.
    let config = Config::load();

    // Terminal setup.
    enable_raw_mode()?;
    let mut stdout = stdout();

    // Enable keyboard enhancements to get modifiers on special keys (Enter, Esc, etc.).
    // This is required for Shift+Enter, Ctrl+Enter to work properly.
    // Not all terminals support this — we try and ignore failure.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    );

    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Event loop.
    let event_loop = EventLoop::new();

    // Spawn input thread (keyboard + mouse).
    spawn_input_thread(event_loop.sender());

    // Spawn tick thread (spinner animation, only active during streaming).
    // 100ms = 10 FPS for spinner (sufficient for smooth animation).
    // Combined with the even/odd frame skip in app.rs, effective rate is ~5 FPS.
    // This is enough for the spinner while minimizing CPU usage.
    let tick_ctl = spawn_tick_thread(event_loop.sender(), std::time::Duration::from_millis(100));

    // Create app.
    let mut app = App::new(config, tick_ctl);

    // Initialize Lua runtime with hot-reload watcher.
    app.init_lua(event_loop.sender());

    // Connect to server and load session list for welcome screen.
    if let Err(e) = app.connect() {
        cleanup_terminal(&mut terminal)?;
        eprintln!("Failed to connect: {}", e);
        return Ok(());
    }

    // Main loop — blocks on recv(), zero CPU when idle.
    // SSE thread is spawned when user selects a session from welcome screen.
    let result = app.run(&mut terminal, &event_loop);

    // Cleanup.
    cleanup_terminal(&mut terminal)?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    Ok(())
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    // Pop keyboard enhancements (ignore error if terminal doesn't support it).
    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
