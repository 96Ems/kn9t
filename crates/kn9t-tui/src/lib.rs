//! kn9t-tui — terminal UI client for the kn9t agent server.
//!
//! R-TUI-010: NO kn9t-* workspace dependencies. HTTP + SSE only.
//! R-TUI-020: Pure event-driven architecture (block on recv, zero polling).
//! R-TUI-030: 3-column layout with collapsible sidebars.

pub mod app;
pub mod client;
pub mod command_palette;
pub mod config;
pub mod diff_viewer;
pub mod input_history;
pub mod kill_ring;
pub mod prompt_history;
pub mod prompt_stash;
pub mod event;
pub mod keybind;
pub mod log;
pub mod markdown;
pub mod message_handler;
pub mod model_selector;
pub mod search;
pub mod session_manager;
pub mod slash;
pub mod syntax;
pub mod theme;
pub mod thinking;
pub mod token_tracker;
pub mod ui;
pub mod hyperlinks;
pub mod latex;
pub mod which_key;
pub mod widgets;
pub mod wire;
pub mod word_segmenter;
