//! Session management - extracted from app.rs for better separation of concerns.
//!
//! Handles session lifecycle: list, create, switch, enter, and state reset.

use std::sync::mpsc::Sender;

use crate::client::{spawn_sse_thread, Client, ClientError, SseHandle};
use crate::config::Config;
use crate::event::{Event, TickControl};

/// Session info for sidebar display.
///
/// `Default` so a caller that only cares about identity (tests, and any construction site
/// that predates the parentage fields) does not have to spell out four `None`s.
#[derive(Debug, Clone, Default)]
pub struct SessionEntry {
    pub id: String,
    pub name: String,
    pub running: bool,
    /// ISO 8601 timestamp for date grouping (e.g., "2026-08-28T10:30:00").
    pub created_at: Option<String>,
    /// the session this one was forked from, `None` for a root.
    pub parent_id: Option<String>,
    /// Why the fork happened: `"fork" | "rewind" | "subagent" | "tree"`.
    pub fork_reason: Option<String>,
    /// The parent seq this session branched at.
    pub origin_seq: Option<u64>,
    /// Head seq, needed by `/undo` to compute `head_seq - n` without a round-trip.
    pub head_seq: u64,
}

/// session filter used by the picker (render + key handler MUST agree).
/// Names match fuzzily (subsequence); IDs match by case-insensitive SUBSTRING —
/// fuzzy-matching random ULID-style ids matches nearly everything (every long id
/// contains most letters somewhere), which made the session filter useless.
pub fn session_matches(s: &SessionEntry, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    crate::slash::fuzzy_match(&s.name, filter)
        || s.id
            .to_ascii_lowercase()
            .contains(&filter.to_ascii_lowercase())
}

/// Session state that can be reset when switching sessions.
#[derive(Debug, Default)]
pub struct SessionState {
    pub session_id: String,
    pub session_title: Option<String>,
    pub lease: Option<String>,
    pub last_seq: u64,
    /// Working directory for the current session (from server snapshot, used for /diff cwd fix).
    pub cwd: Option<String>,
}

impl SessionState {
    /// Reset all session-specific state.
    pub fn reset(&mut self) {
        self.session_id.clear();
        self.session_title = None;
        self.lease = None;
        self.last_seq = 0;
        self.cwd = None;
    }
}

/// Manages session list and session lifecycle.
pub struct SessionManager {
    /// List of sessions for sidebar.
    pub sessions: Vec<SessionEntry>,
    /// Currently selected session index.
    pub selected: usize,
    /// Current session state.
    pub state: SessionState,
    /// Active SSE handle (to stop on switch).
    sse_handle: Option<SseHandle>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            state: SessionState::default(),
            sse_handle: None,
        }
    }

    /// Load session list from server.
    pub fn load_sessions(&mut self, client: &Client) -> Result<(), ClientError> {
        let sessions = client.list_sessions()?;
        self.sessions = sessions
            .iter()
            .map(|s| SessionEntry {
                id: s.id.clone(),
                name: s
                    .name
                    .clone()
                    .unwrap_or_else(|| s.id[..8.min(s.id.len())].to_string()),
                running: false,
                created_at: s.created_at.clone(),
                parent_id: s.origin_session.clone(),
                fork_reason: s.fork_reason.clone(),
                origin_seq: s.origin_seq,
                head_seq: s.head_seq,
            })
            .collect();
        Ok(())
    }

    /// Create a new session and return its ID.
    pub fn create_session(&mut self, client: &Client, cwd: &str) -> Result<String, ClientError> {
        let session_id = client.create_session(cwd, None)?;

        // Add to session list at the front.
        // created_at will be None until we refresh from server.
        self.sessions.insert(
            0,
            SessionEntry {
                id: session_id.clone(),
                name: "New session".into(),
                running: false,
                created_at: None,
                parent_id: None,
                fork_reason: None,
                origin_seq: None,
                head_seq: 0,
            },
        );

        Ok(session_id)
    }

    /// fork `origin_id` and return the new session id.
    ///
    /// Mirrors `create_session` but hits `POST /session/{id}/fork`. `/fork` and `/undo` are
    /// the same server call with a different `reason` and `origin_seq`; the log is
    /// append-only, so "drop the last message" is a fork that stops short of it, never a
    /// mutation (R-STOR-120).
    ///
    /// The entry is inserted locally so the sidebar shows the branch immediately, with the
    /// parentage the caller already knows — the next `load_sessions` refresh confirms it.
    pub fn fork_into(
        &mut self,
        client: &Client,
        origin_id: &str,
        origin_seq: Option<u64>,
        reason: &str,
    ) -> Result<String, ClientError> {
        let new_id = client.fork_session(origin_id, origin_seq, reason)?;
        let name = match reason {
            "rewind" => "Rewind".to_string(),
            _ => "Fork".to_string(),
        };
        self.sessions.insert(
            0,
            SessionEntry {
                id: new_id.clone(),
                name,
                running: false,
                created_at: None,
                parent_id: Some(origin_id.to_string()),
                fork_reason: Some(reason.to_string()),
                origin_seq,
                head_seq: origin_seq.unwrap_or(0),
            },
        );
        Ok(new_id)
    }

    /// Head seq of a session as last listed. Used by `/undo` to derive `head_seq - n`.
    pub fn head_seq_of(&self, session_id: &str) -> Option<u64> {
        self.sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.head_seq)
    }

    /// Stop SSE stream for current session.
    pub fn stop_sse(&mut self) {
        if let Some(ref handle) = self.sse_handle {
            handle.stop();
        }
        self.sse_handle = None;
    }

    /// Start SSE stream for a session.
    ///
    /// Passes the current lease holder (if any) so the stream owns the lease
    /// server-side and keeps it warm while connected (prevents idle-lease-loss →
    /// silent 409 on the next prompt).
    pub fn start_sse(
        &mut self,
        config: &Config,
        session_id: &str,
        from_seq: u64,
        tx: Sender<Event>,
    ) {
        let lease = self.state.lease.clone();
        let handle = spawn_sse_thread(
            config.base_url.clone(),
            config.token.clone(),
            session_id.to_string(),
            from_seq,
            lease,
            tx,
        );
        self.sse_handle = Some(handle);
    }

    /// Release lease for current session.
    pub fn release_lease(&mut self, client: &Client) {
        if let Some(holder) = &self.state.lease {
            let _ = client.release_lease(&self.state.session_id, holder);
        }
        self.state.lease = None;
    }

    /// Acquire lease for a session.
    pub fn acquire_lease(
        &mut self,
        client: &Client,
        session_id: &str,
    ) -> Result<Option<String>, ClientError> {
        match client.acquire_lease(session_id, false) {
            Ok(holder) => {
                self.state.lease = Some(holder.clone());
                Ok(Some(holder))
            }
            Err(ClientError::SessionBusy) => {
                self.state.lease = None;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Mark a session as active in the list.
    pub fn mark_active(&mut self, session_id: &str) {
        for (i, s) in self.sessions.iter_mut().enumerate() {
            s.running = s.id == session_id;
            if s.running {
                self.selected = i;
            }
        }
    }

    /// Reset all session state (call before switching sessions).
    pub fn reset_state(&mut self, client: Option<&Client>, tick_ctl: &TickControl) {
        // Stop SSE stream.
        self.stop_sse();

        // Release lease.
        if let Some(client) = client {
            self.release_lease(client);
        }

        // Reset session state.
        self.state.reset();

        // Reset streaming state.
        tick_ctl.set_streaming(false);
    }

    /// Check if we have a valid lease.
    pub fn has_lease(&self) -> bool {
        self.state.lease.is_some()
    }

    /// Get the current session ID.
    pub fn current_session_id(&self) -> &str {
        &self.state.session_id
    }

    /// Set the current session ID.
    pub fn set_session_id(&mut self, id: String) {
        self.state.session_id = id;
    }

    /// Get the current session title.
    pub fn session_title(&self) -> Option<&str> {
        self.state.session_title.as_deref()
    }

    /// Set the current session title.
    pub fn set_session_title(&mut self, title: Option<String>) {
        self.state.session_title = title;
    }

    /// Get session by index.
    pub fn get_session(&self, idx: usize) -> Option<&SessionEntry> {
        self.sessions.get(idx)
    }

    /// Get session count.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
