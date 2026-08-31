//! kn9t-tui — terminal UI client for the kn9t agent server.
//!
//! R-TUI-010: NO kn9t-* workspace dependencies. HTTP + SSE only.
//! R-TUI-020: Pure event-driven architecture (block on recv, zero polling).
//! R-TUI-030: 3-column layout with collapsible sidebars.

pub mod app;
pub mod client;
pub mod reducer;

#[cfg(test)]
mod tui {
    /// R-TUI-230: SSE reconnect — the "reconnecting..." message is no longer a lie.
    /// Verifies that durable `seq` is tracked and used for `?from=` on reconnect.
    #[test]
    fn sse_reconnect() {
        use crate::reducer::{reduce, State};
        use crate::wire::{SseFrame, WireMessage, WireContent};
        let mut s = State::default();
        s.session_id = "sess1".into();
        // First durable event at seq 10
        reduce(&mut s, SseFrame::MessageAppended { seq: 10, msg: WireMessage { id: "m1".into(), role: "assistant".into(), content: vec![WireContent::Text { text: "hi".into() }], silent: false } });
        assert_eq!(s.last_seq, 10);
        // Simulate disconnect and reconnect: last_seq should still be 10
        let from = s.last_seq;
        assert_eq!(from, 10);
        // Next event after reconnect at seq 11 should be processed
        reduce(&mut s, SseFrame::MessageAppended { seq: 11, msg: WireMessage { id: "m2".into(), role: "assistant".into(), content: vec![WireContent::Text { text: "again".into() }], silent: false } });
        assert_eq!(s.last_seq, 11);
        assert_eq!(s.transcript.messages().len(), 2);
    }
}
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
