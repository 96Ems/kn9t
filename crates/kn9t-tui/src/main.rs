//! kn9t-tui entry point.

use std::io::{self, stdout};

use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use kn9t_tui::app::App;
use kn9t_tui::config::Config;
use kn9t_tui::event::{spawn_input_thread, spawn_tick_thread, EventLoop};

fn main() -> io::Result<()> {
    // Initialize debug log.
    kn9t_tui::log::init("kn9t-tui.log");
    kn9t_tui::log!("=== kn9t-tui starting ===");
    
    // Load config.
    let config = Config::load();

    // Terminal setup.
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Event loop.
    let event_loop = EventLoop::new();
    
    // Spawn input thread (keyboard + mouse).
    spawn_input_thread(event_loop.sender());
    
    // Spawn tick thread (spinner animation, only active during streaming).
    let tick_ctl = spawn_tick_thread(event_loop.sender(), std::time::Duration::from_millis(80));

    // Create app.
    let mut app = App::new(config, tick_ctl);

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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
