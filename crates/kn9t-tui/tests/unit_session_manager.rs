//! Unit tests for session_manager — extracted from src/session_manager.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::session_manager::{session_matches, SessionEntry, SessionManager, SessionState};

fn entry(id: &str, name: &str) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        name: name.into(),
        running: false,
        created_at: None,
    }
}

/// 96E-19 — id filtering must be substring, not fuzzy: every long random id
/// contains most letters somewhere, so fuzzy matching made the picker useless.
#[test]
fn session_filter_matches_id_by_substring_only() {
    let e = entry("01M1GXZDENG6JTD9C5NRK1CZXX", "hello");
    assert!(session_matches(&e, "01M1GXZ"), "prefix matches");
    assert!(
        !session_matches(&e, "01M1GVZZ"),
        "foreign suffix must NOT fuzzy-match"
    );
    // Fuzzy still applies to names.
    let e2 = entry("abcdef", "review commit");
    assert!(session_matches(&e2, "rvw"));
    assert!(session_matches(&e2, ""));
}

#[test]
fn test_session_state_reset() {
    let mut state = SessionState {
        session_id: "test-123".into(),
        session_title: Some("My Session".into()),
        lease: Some("holder-456".into()),
        last_seq: 42,
        cwd: Some("/tmp".into()),
    };

    state.reset();

    assert!(state.session_id.is_empty());
    assert!(state.session_title.is_none());
    assert!(state.lease.is_none());
    assert_eq!(state.last_seq, 0);
}

#[test]
fn test_session_manager_new() {
    let manager = SessionManager::new();

    assert!(manager.sessions.is_empty());
    assert_eq!(manager.selected, 0);
    assert!(manager.state.session_id.is_empty());
}

#[test]
fn test_mark_active() {
    let mut manager = SessionManager::new();
    manager.sessions = vec![
        SessionEntry {
            id: "session-1".into(),
            name: "First".into(),
            running: false,
            created_at: None,
        },
        SessionEntry {
            id: "session-2".into(),
            name: "Second".into(),
            running: false,
            created_at: None,
        },
        SessionEntry {
            id: "session-3".into(),
            name: "Third".into(),
            running: false,
            created_at: None,
        },
    ];

    manager.mark_active("session-2");

    assert!(!manager.sessions[0].running);
    assert!(manager.sessions[1].running);
    assert!(!manager.sessions[2].running);
    assert_eq!(manager.selected, 1);
}
