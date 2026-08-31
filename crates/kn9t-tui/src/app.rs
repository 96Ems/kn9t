//! Application state and main loop.
//!
//! Managers are composed as fields:
//! - `session`    → SessionManager  (session list, id, title, lease, SSE handle, last_seq)
//! - `model_sel`  → ModelSelector   (model list, selected index)
//! - `tokens`     → TokenTracker    (cumulative + per-turn token counts, cost, throughput)
//! - `transcript` → Transcript      (messages, live_delta, scroll)

use std::io;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::execute;
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::client::{spawn_attach_thread, AttachHandle, Client, ClientError};
use crate::config::Config;
use crate::input_history::{InputHistory, InputSnapshot};
use crate::kill_ring::KillRing;
use crate::prompt_history::PromptHistory;
use crate::prompt_stash::PromptStash;
use crate::search::SearchState;
use crate::thinking::ThinkingState;
use crate::event::{Event, EventLoop, TickControl};
use crate::keybind::{Action, Keybinds};
use crate::message_handler::Transcript;
use crate::model_selector::ModelSelector;
use crate::session_manager::SessionManager;
use crate::slash::{fuzzy_match, SlashState};
use crate::token_tracker::{TokenCounts, TokenTracker};
use crate::ui::layout::LayoutState;
use crate::ui::render::render;
use crate::which_key::WhichKeyPanel;
use crate::wire::SseFrame;

// Re-export types from extracted modules for backward compatibility.
// The canonical definitions are in the submodules.
pub use crate::message_handler::{Message, ToolCard};
pub use crate::model_selector::ModelEntry;
pub use crate::session_manager::SessionEntry;

/// Tool info for sidebar.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub name: String,
    pub enabled: bool,
}

/// Overlay state.
#[derive(Debug, Clone)]
pub enum Overlay {
    Approval { tool: String, args: String, selected: usize },
    Help,
    WhichKey,
    CommandPalette,
    ModelSelect { selected: usize, filter: String },
    SessionSelect { selected: usize, filter: String },
}

/// Current screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Welcome,
    Chat,
}

/// Pending action from welcome screen (legacy — kept for compat, no longer queued).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum WelcomeAction {
    Select,
    NewSession,
}

/// Hit area for tool card click detection.
#[derive(Debug, Clone)]
pub struct ToolHitArea {
    pub call_id: String,
    pub header_y: u16,              // Y position of header line
    pub content_y_start: u16,       // Y start of content area (tabs + output/input)
    pub content_y_end: u16,         // Y end of content area
    pub progress_tab_x: (u16, u16), // X range for Progress tab
    pub output_tab_x: (u16, u16),   // X range for Output tab
    pub input_tab_x: (u16, u16),    // X range for Input tab
}

/// Main app state.
pub struct App {
    pub config: Config,
    pub client: Option<Client>,
    pub layout: LayoutState,
    pub screen: Screen,

    // Global attach handle (keeps server alive).
    attach_handle: Option<AttachHandle>,

    // ── Composed managers ──────────────────────────────────────────────────
    /// Session lifecycle: list, id, title, lease, SSE handle, last_seq.
    pub session: SessionManager,
    /// Model list and selection.
    pub model_sel: ModelSelector,
    /// Token/cost tracking (cumulative + per-turn + throughput).
    pub tokens: TokenTracker,
    /// Transcript: messages, live_delta, scroll.
    pub transcript: Transcript,

    // Right sidebar tool toggles (UI-local, not owned by any manager).
    pub tools: Vec<ToolEntry>,

    // Input.
    pub input: String,
    pub cursor_row: usize,
    pub cursor_col: usize,
    
    // Undo/redo history for input.
    input_history: InputHistory,
    
    // Kill ring for Emacs-style delete/yank.
    kill_ring: KillRing,
    
    // Prompt history for Up/Down navigation.
    prompt_history: PromptHistory,
    
    // Prompt stash for /stash and /unstash commands.
    prompt_stash: PromptStash,

    // Pending images (base64-encoded, to be sent with next prompt).
    pub staged_images: Vec<String>,

    // State.
    pub streaming: bool,
    pub aborting: bool,  // True when abort requested, waiting for TurnEnded
    pub spinner_frame: usize,
    pub phrase_idx: usize,
    pub overlay: Option<Overlay>,
    pub active_approval_id: Option<u64>,
    pub slash: SlashState,
    pub quit: bool,

    // Tool mode state (UI-local).
    pub tool_mode: bool,
    pub focused_tool: Option<String>,  // call_id of focused tool
    pub tool_hit_areas: Vec<ToolHitArea>,  // Click detection areas from last render
    
    // Thinking block collapse state (UI-local).
    pub thinking_state: ThinkingState,

    // Search state (None = search bar closed).
    pub search_state: Option<SearchState>,

    // Which-key panel state.
    pub which_key_panel: WhichKeyPanel,

    // Command palette state.
    pub command_palette: crate::command_palette::CommandPalette,

    // Diff viewer (separate from overlay - needs &mut for mouse hit tracking).
    pub diff_viewer: Option<crate::diff_viewer::DiffViewer>,

    // Keybinds.
    keybinds: Keybinds,
    tick_ctl: TickControl,
    term_width: u16,
}

impl App {
    pub fn new(config: Config, tick_ctl: TickControl) -> Self {
        let keybinds = Keybinds::new(&config.keybinds);
        let layout = LayoutState {
            right_enabled: config.right_sidebar,
            ..Default::default()
        };

        Self {
            config,
            client: None,
            layout,
            screen: Screen::Welcome,
            attach_handle: None,
            session: SessionManager::new(),
            model_sel: ModelSelector::new(),
            tokens: TokenTracker::new(),
            transcript: Transcript::new(),
            tools: Vec::new(),
            input: String::new(),
            cursor_row: 0,
            cursor_col: 0,
            input_history: InputHistory::new(),
            kill_ring: KillRing::new(),
            prompt_history: PromptHistory::new(),
            prompt_stash: PromptStash::new(),
            staged_images: Vec::new(),
            streaming: false,
            aborting: false,
            spinner_frame: 0,
            phrase_idx: 0,
            overlay: None,
            active_approval_id: None,
            slash: SlashState::new(),
            quit: false,
            tool_mode: false,
            focused_tool: None,
            tool_hit_areas: Vec::new(),
            thinking_state: ThinkingState::new(),
            search_state: None,
            which_key_panel: WhichKeyPanel::new(),
            command_palette: crate::command_palette::CommandPalette::new(),
            diff_viewer: None,
            keybinds,
            tick_ctl,
            term_width: 80,
        }
    }

    /// Connect to server and load session list + models for welcome screen.
    pub fn connect(&mut self) -> Result<(), ClientError> {
        let client = Client::new(&self.config.base_url, self.config.token.as_deref());

        // Load session list for welcome screen.
        self.session.load_sessions(&client)?;

        // Load available models (auto-discovered from connected providers).
        let _ = self.model_sel.load_models(&client);

        // Load tools from server (GET /tools — Phase 4, discovered plugins, not hardcoded).
        self.refresh_tools(&client);

        // Start global attach thread to keep server alive.
        self.attach_handle = Some(spawn_attach_thread(
            self.config.base_url.clone(),
            self.config.token.clone(),
        ));

        self.client = Some(client);
        Ok(())
    }

    /// Refresh tools sidebar from server (GET /tools — Phase 4).
    pub fn refresh_tools(&mut self, client: &Client) {
        match client.get_tools() {
            Ok(names) => {
                self.tools = names.into_iter().map(|n| ToolEntry { name: n, enabled: true }).collect();
                crate::log!("TOOLS: refreshed {} tools", self.tools.len());
            }
            Err(e) => {
                crate::log!("TOOLS: refresh failed: {:?}", e);
            }
        }
    }

    /// Get current model display name.
    pub fn current_model_name(&self) -> String {
        self.model_sel.current_model_name()
    }

    /// Reset all session-specific state. Call before switching sessions.
    /// IMPORTANT: cleanup uses the OLD session_id (still in self.session.state),
    /// then enter_session() sets the new one. Do not reorder.
    fn reset_session_state(&mut self) {
        // Stop SSE stream and release lease (uses OLD session_id).
        self.session.reset_state(self.client.as_ref(), &self.tick_ctl);

        // Clear transcript.
        self.transcript.clear();

        // Clear metrics.
        self.tokens.reset();

        // Reset streaming state.
        self.streaming = false;
        self.tick_ctl.set_streaming(false);

        // Clear any pending UI state.
        self.overlay = None;
        self.active_approval_id = None;

        // Clear tool mode state.
        self.tool_mode = false;
        self.focused_tool = None;
        self.tool_hit_areas.clear();
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tool mode helpers
    // ═══════════════════════════════════════════════════════════════════════

    /// Find all tool call_ids in order (for navigation).
    pub fn tool_ids(&self) -> Vec<String> {
        self.transcript.messages().iter()
            .flat_map(|m| m.tools.iter())
            .map(|t| t.call_id.clone())
            .collect()
    }

    /// Get mutable reference to tool by call_id.
    pub fn tool_mut(&mut self, call_id: &str) -> Option<&mut ToolCard> {
        self.transcript.messages_mut().iter_mut()
            .flat_map(|m| m.tools.iter_mut())
            .find(|t| t.call_id == call_id)
    }

    /// Toggle expand/collapse for a tool.
    pub fn toggle_tool_expand(&mut self, call_id: &str) {
        if let Some(tool) = self.tool_mut(call_id) {
            tool.expanded = !tool.expanded;
            if tool.expanded {
                tool.scroll_offset = 0;  // Reset scroll on expand
            }
        }
    }

    /// Switch tab for a tool.
    pub fn switch_tool_tab(&mut self, call_id: &str, tab: crate::message_handler::ToolTab) {
        if let Some(tool) = self.tool_mut(call_id) {
            tool.active_tab = tab;
            tool.scroll_offset = 0;  // Reset scroll on tab switch
        }
    }

    /// Cycle through tool tabs (Progress <-> Output <-> Input).
    pub fn cycle_tool_tab(&mut self, call_id: &str, forward: bool) {
        use crate::message_handler::ToolTab;
        if let Some(tool) = self.tool_mut(call_id) {
            tool.active_tab = if forward {
                match tool.active_tab {
                    ToolTab::Progress => ToolTab::Output,
                    ToolTab::Output => ToolTab::Input,
                    ToolTab::Input => ToolTab::Progress,
                }
            } else {
                match tool.active_tab {
                    ToolTab::Progress => ToolTab::Input,
                    ToolTab::Output => ToolTab::Progress,
                    ToolTab::Input => ToolTab::Output,
                }
            };
            tool.scroll_offset = 0;  // Reset scroll on tab switch
        }
    }

    /// Navigate to next/prev tool in tool mode.
    pub fn navigate_tool(&mut self, forward: bool) {
        let ids = self.tool_ids();
        if ids.is_empty() { return; }

        let current_idx = self.focused_tool.as_ref()
            .and_then(|id| ids.iter().position(|i| i == id));

        let new_idx = match (current_idx, forward) {
            (Some(idx), true) => (idx + 1).min(ids.len() - 1),
            (Some(idx), false) => idx.saturating_sub(1),
            (None, true) => 0,
            (None, false) => ids.len() - 1,
        };

        self.focused_tool = Some(ids[new_idx].clone());
    }

    /// Scroll within focused tool's output (includes progress_lines + output).
    pub fn scroll_tool_output(&mut self, delta: isize) {
        if let Some(ref call_id) = self.focused_tool.clone() {
            if let Some(tool) = self.tool_mut(&call_id) {
                // Count total lines: progress_lines + output
                let mut line_count = tool.progress_lines.len();
                if let Some(ref output) = tool.output {
                    if !output.is_empty() {
                        if line_count > 0 {
                            line_count += 1; // separator
                        }
                        line_count += output.lines().count();
                    }
                }
                let visible_lines = 20;  // Must match render constant
                let max_scroll = line_count.saturating_sub(visible_lines);
                tool.scroll_offset = (tool.scroll_offset as isize + delta)
                    .max(0)
                    .min(max_scroll as isize) as usize;
            }
        }
    }

    /// Enter tool mode, focusing the first tool if none focused.
    pub fn enter_tool_mode(&mut self) {
        self.tool_mode = true;
        if self.focused_tool.is_none() {
            let ids = self.tool_ids();
            if let Some(first) = ids.first() {
                self.focused_tool = Some(first.clone());
            }
        }
    }

    /// Exit tool mode.
    pub fn exit_tool_mode(&mut self) {
        self.tool_mode = false;
        self.focused_tool = None;
    }

    /// Enter a session (from welcome screen or session list).
    pub fn enter_session(&mut self, session_id: &str, tx: Sender<Event>) -> Result<(), ClientError> {
        let client = self.client.as_ref().ok_or(ClientError::Http("not connected".into()))?;

        self.session.set_session_id(session_id.to_string());
        // Get title from sessions list (if available).
        let title = self.session.sessions.iter()
            .find(|s| s.id == session_id)
            .map(|s| s.name.clone());
        self.session.set_session_title(title);

        // Try to acquire lease.
        let lease_result = self.session.acquire_lease(client, session_id)?;
        if let Some(ref holder) = lease_result {
            // Send initial model to session (so server uses our selected model).
            if let Some(model) = self.model_sel.current_model() {
                crate::log!("enter_session: setting initial model {}:{}", model.provider, model.id);
                match client.set_model(session_id, holder, &model.provider, &model.id) {
                    Ok(_) => crate::log!("enter_session: set_model OK"),
                    Err(e) => crate::log!("enter_session: set_model error {:?}", e),
                }
            }
        }

        // Load session transcript.
        crate::log!("enter_session: loading transcript for {}", session_id);
        match client.get_session(session_id) {
            Ok(detail) => {
                crate::log!("Loading session: {} messages", detail.transcript.len());
                self.tokens.set_cost(detail.cost_usd);
                // Store session cwd from meta for /diff fix (Phase 4 — use session cwd, not env::current_dir).
                if let Some(cwd) = detail.meta.get("cwd").and_then(|v| v.as_str()) {
                    self.session.state.cwd = Some(cwd.to_string());
                }

                // Use TranscriptParser from message_handler to parse the transcript.
                // Filter out silent messages (e.g., AGENTS.md injection).
                let transcript_values: Vec<serde_json::Value> = detail.transcript.iter()
                    .filter(|msg| !msg.silent)
                    .map(|msg| {
                        serde_json::json!({
                            "role": msg.role,
                            "content": msg.content,
                        })
                    })
                    .collect();
                let messages = crate::message_handler::TranscriptParser::parse(&transcript_values);

                // Log summary.
                let tool_count: usize = messages.iter().map(|m| m.tools.len()).sum();
                crate::log!("Loaded {} messages with {} total tools, head_seq={}", messages.len(), tool_count, detail.head_seq);

                for msg in messages {
                    self.transcript.push(msg);
                }

                // Set last_seq from snapshot to avoid replaying already-loaded messages.
                self.session.state.last_seq = detail.head_seq;
            }
            Err(e) => {
                crate::log!("enter_session: failed to load transcript: {:?}", e);
            }
        }

        // Start SSE stream for this session, starting from last_seq.
        crate::log!("enter_session: starting SSE for {} from_seq={}", session_id, self.session.state.last_seq);
        self.session.start_sse(&self.config, session_id, self.session.state.last_seq, tx);

        // Switch to chat screen.
        self.screen = Screen::Chat;

        // Mark this session as active in the list.
        self.session.mark_active(session_id);

        Ok(())
    }

    /// Create a new session and enter it.
    pub fn create_new_session(&mut self, tx: Sender<Event>) -> Result<(), ClientError> {
        crate::log!("create_new_session: starting...");
        let client = self.client.as_ref().ok_or(ClientError::Http("not connected".into()))?;

        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".into());
        crate::log!("create_new_session: calling server create_session cwd={}", cwd);
        let session_id = self.session.create_session(client, &cwd)?;
        crate::log!("create_new_session: got session_id={}", &session_id);

        self.enter_session(&session_id, tx)
    }

    /// Main event loop.
    pub fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        event_loop: &EventLoop,
    ) -> io::Result<()> {
        let tx = event_loop.sender();

        loop {
            // Render with CSI 2026 synchronized update (flicker-free).
            // Begin synchronized update - terminal buffers all output.
            let _ = execute!(terminal.backend_mut(), BeginSynchronizedUpdate);
            terminal.draw(|f| {
                self.term_width = f.area().width;
                render(f, self);
            })?;
            // End synchronized update - terminal flushes buffer atomically.
            let _ = execute!(terminal.backend_mut(), EndSynchronizedUpdate);

            if self.quit {
                break;
            }

            // Block on next event.
            let Some(event) = event_loop.recv() else {
                break;
            };

            // Handle event.
            match event {
                Event::Key(key) => self.handle_key(key, &tx),
                Event::Mouse(mouse) => self.handle_mouse(mouse),
                Event::Resize(_, _) => {} // Handled by ratatui
                Event::Paste(text) => {
                    if text.is_empty() {
                        // Empty paste = probably an image, try arboard.
                        self.paste_image_from_clipboard();
                    } else {
                        self.handle_paste(&text);
                    }
                }
                Event::Sse(session_id, frame) => {
                    // Only process events for the active session.
                    if session_id == self.session.state.session_id {
                        self.handle_sse(frame);
                    } else {
                        crate::log!("SSE: ignoring event from old session {} (current={})",
                            &session_id[..8.min(session_id.len())],
                            &self.session.state.session_id[..8.min(self.session.state.session_id.len())]);
                    }
                }
                Event::Tick => {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                    if self.spinner_frame % 25 == 0 {
                        self.phrase_idx = self.phrase_idx.wrapping_add(1);
                    }
                }
                Event::SseError(session_id, e) => {
                    // Phase 4 fix: R-TUI-230 — reconnect from last_seq instead of lying.
                    if session_id == self.session.state.session_id {
                        crate::log!("SSE error: {} — reconnecting from seq {}", e, self.session.state.last_seq);
                        // Show transient reconnection message in transcript (deduplicate).
                        if !self.transcript.messages().iter().rev().take(1).any(|m| m.role == "system" && m.content.contains("reconnecting")) {
                            self.transcript.push(Message::new("system", format!("Connection interrupted, reconnecting... ({})", e)));
                        }
                        // Reconnect: stop old handle (already dead) and start new from last_seq.
                        if !session_id.is_empty() {
                            self.session.start_sse(&self.config, &session_id, self.session.state.last_seq, tx.clone());
                        }
                    }
                }
            }

            // Drain any additional events — handle ALL event types, not just SSE/Tick.
            for ev in event_loop.drain() {
                match ev {
                    Event::Key(key) => self.handle_key(key, &tx),
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    Event::Resize(_, _) => {}
                    Event::Paste(text) => {
                        if text.is_empty() {
                            self.paste_image_from_clipboard();
                        } else {
                            self.handle_paste(&text);
                        }
                    }
                    Event::Sse(session_id, frame) => {
                        if session_id == self.session.state.session_id {
                            self.handle_sse(frame);
                        }
                    }
                    Event::Tick => self.spinner_frame = self.spinner_frame.wrapping_add(1),
                    Event::SseError(session_id, e) => {
                        if session_id == self.session.state.session_id {
                            crate::log!("SSE error: {} — reconnecting from seq {}", e, self.session.state.last_seq);
                            if !self.transcript.messages().iter().rev().take(1).any(|m| m.role == "system" && m.content.contains("reconnecting")) {
                                self.transcript.push(Message::new("system", format!("Connection interrupted, reconnecting... ({})", e)));
                            }
                            if !session_id.is_empty() {
                                self.session.start_sse(&self.config, &session_id, self.session.state.last_seq, tx.clone());
                            }
                        }
                    }
                }
            }
        }

        // Release lease on exit (uses current session_id).
        if let Some(client) = &self.client {
            self.session.release_lease(client);
        }

        // Stop global attach (allows server idle-exit).
        if let Some(ref handle) = self.attach_handle {
            handle.stop();
        }

        Ok(())
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent, tx: &Sender<Event>) {
        crate::log!("KEY: {:?} mods={:?} screen={:?} overlay={:?} diff={:?} slash={}", 
            key.code, key.modifiers, self.screen, self.overlay.is_some(), self.diff_viewer.is_some(), self.slash.active);
        
        // Diff viewer handling (separate from overlay).
        if self.diff_viewer.is_some() {
            crate::log!("  -> diff_viewer handler");
            self.handle_overlay_key(key, tx);
            return;
        }
        
        // Overlay handling.
        if self.overlay.is_some() {
            crate::log!("  -> overlay handler");
            self.handle_overlay_key(key, tx);
            return;
        }

        // Search mode handling (intercepts keys when search bar is open).
        if self.search_state.is_some() {
            crate::log!("  -> search handler");
            self.handle_search_key(key);
            return;
        }

        // Screen-specific handling.
        match self.screen {
            Screen::Welcome => {
                crate::log!("  -> welcome handler");
                self.handle_welcome_key(key, tx);
                return;
            }
            Screen::Chat => {}
        }

        // Slash command mode handling.
        if self.slash.active {
            match key.code {
                KeyCode::Esc => {
                    self.slash.deactivate();
                    // Clear the "/" from input.
                    if self.input.starts_with('/') {
                        self.input.clear();
                        self.cursor_col = 0;
                    }
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Execute selected command.
                    if let Some(cmd) = self.slash.selected_command() {
                        self.execute_slash_command(cmd.name, tx);
                    }
                    self.slash.deactivate();
                    self.input.clear();
                    self.cursor_col = 0;
                    return;
                }
                KeyCode::Up => {
                    self.slash.select_prev();
                    return;
                }
                KeyCode::Down => {
                    self.slash.select_next();
                    return;
                }
                KeyCode::Backspace => {
                    if self.input.len() <= 1 {
                        // Just "/" left, deactivate.
                        self.slash.deactivate();
                        self.input.clear();
                        self.cursor_col = 0;
                    } else {
                        // Remove last char and update query.
                        self.input.pop();
                        self.cursor_col = self.cursor_col.saturating_sub(1);
                        let query = self.input.trim_start_matches('/');
                        self.slash.set_query(query);
                    }
                    return;
                }
                KeyCode::Char(c) => {
                    self.input.push(c);
                    self.cursor_col += 1;
                    let query = self.input.trim_start_matches('/');
                    self.slash.set_query(query);
                    return;
                }
                _ => return,
            }
        }

        // Check for modifier+Enter for newline.
        if key.code == KeyCode::Enter {
            let has_modifier = key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT);
            if has_modifier {
                self.insert_newline();
                return;
            }
        }

        // Tool mode handling (keyboard navigation for tools).
        if self.tool_mode {
            match key.code {
                KeyCode::Esc => {
                    self.exit_tool_mode();
                    return;
                }
                KeyCode::Up => {
                    self.navigate_tool(false);
                    return;
                }
                KeyCode::Down => {
                    self.navigate_tool(true);
                    return;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(ref id) = self.focused_tool.clone() {
                        self.toggle_tool_expand(&id);
                    }
                    return;
                }
                KeyCode::Left => {
                    // Cycle tabs: Progress <- Output <- Input (wrap)
                    if let Some(ref id) = self.focused_tool.clone() {
                        self.cycle_tool_tab(&id, false);
                    }
                    return;
                }
                KeyCode::Right => {
                    // Cycle tabs: Progress -> Output -> Input (wrap)
                    if let Some(ref id) = self.focused_tool.clone() {
                        self.cycle_tool_tab(&id, true);
                    }
                    return;
                }
                KeyCode::PageUp => {
                    self.scroll_tool_output(-10);
                    return;
                }
                KeyCode::PageDown => {
                    self.scroll_tool_output(10);
                    return;
                }
                _ => {
                    // Let other keys (like Ctrl+T to exit) fall through to keybind matching
                }
            }
        }

        // Match keybind.
        if let Some(action) = self.keybinds.match_key(key) {
            crate::log!("  -> keybind action: {:?}", action);
            self.execute_action(action, tx);
            return;
        }

        // Input handling.
        crate::log!("  -> input handler");
        
        // Reset kill ring yank state on any non-yank input
        self.kill_ring.reset_yank();
        
        match key.code {
            KeyCode::Char(c) => {
                // Handle special control characters that terminals send.
                // Ctrl+Backspace often sends ^H (0x08) or DEL (0x7f).
                if c == '\x08' || c == '\x7f' {
                    crate::log!("  -> Ctrl+Backspace detected (0x{:02x}), killing word", c as u8);
                    self.kill_word_backward();
                    return;
                }
                
                crate::log!("  -> inserting char: {:?}", c);
                // Reset prompt history navigation when typing
                self.prompt_history.reset();
                // Record state for undo (coalesces rapid typing)
                self.record_input_change();
                self.input.insert(self.cursor_pos(), c);
                self.cursor_col += 1;
                
                // Activate slash mode if typing "/" at start.
                if c == '/' && self.input == "/" {
                    self.slash.activate();
                }
            }
            KeyCode::Backspace => {
                let pos = self.cursor_pos();
                if pos > 0 {
                    // Record state for undo (coalesces rapid deletes)
                    self.record_input_change();
                    // Find the start of the previous character (UTF-8 safe).
                    let prev_char_start = self.input[..pos]
                        .char_indices()
                        .next_back()
                        .map(|(idx, _)| idx)
                        .unwrap_or(0);
                    self.input.remove(prev_char_start);
                    if self.cursor_col > 0 {
                        self.cursor_col -= 1;
                    } else if self.cursor_row > 0 {
                        self.cursor_row -= 1;
                        self.cursor_col = self.current_line_len();
                    }
                }
            }
            KeyCode::Delete => {
                let pos = self.cursor_pos();
                if pos < self.input.len() {
                    // Record state for undo (coalesces rapid deletes)
                    self.record_input_change();
                    // pos should already be at a char boundary from cursor_pos().
                    // But ensure we remove the full character.
                    let char_len = self.input[pos..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                    for _ in 0..char_len {
                        if pos < self.input.len() {
                            self.input.remove(pos);
                        }
                    }
                }
            }
            KeyCode::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.current_line_len();
                }
            }
            KeyCode::Right => {
                if self.cursor_col < self.current_line_len() {
                    self.cursor_col += 1;
                } else if self.cursor_row < self.input.lines().count().saturating_sub(1) {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                }
            }
            KeyCode::Up => {
                if self.cursor_row > 0 {
                    // Move up within multi-line input
                    self.cursor_row -= 1;
                    self.cursor_col = self.cursor_col.min(self.current_line_len());
                } else {
                    // On first line: navigate prompt history
                    let current = self.input.clone();
                    if let Some(prev) = self.prompt_history.prev(&current, self.cursor_row) {
                        self.input = prev.to_string();
                        self.cursor_row = 0;
                        self.cursor_col = self.input.lines().next().map(|l| l.chars().count()).unwrap_or(0);
                    }
                }
            }
            KeyCode::Down => {
                let total_lines = self.input.lines().count().max(1);
                if self.cursor_row < total_lines.saturating_sub(1) {
                    // Move down within multi-line input
                    self.cursor_row += 1;
                    self.cursor_col = self.cursor_col.min(self.current_line_len());
                } else {
                    // On last line: navigate prompt history forward
                    if let Some(next) = self.prompt_history.next(self.cursor_row, total_lines) {
                        self.input = next;
                        self.cursor_row = 0;
                        self.cursor_col = self.input.lines().next().map(|l| l.chars().count()).unwrap_or(0);
                    }
                }
            }
            KeyCode::Home => self.cursor_col = 0,
            KeyCode::End => self.cursor_col = self.current_line_len(),
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, key: crossterm::event::KeyEvent, tx: &Sender<Event>) {
        // Handle diff viewer separately (not in Overlay enum)
        if let Some(ref mut viewer) = self.diff_viewer {
            if viewer.commenting {
                match key.code {
                    KeyCode::Esc => {
                        viewer.cancel_comment();
                    }
                    KeyCode::Enter => {
                        viewer.add_comment();
                    }
                    KeyCode::Backspace => {
                        viewer.comment_input.pop();
                    }
                    KeyCode::Char(c) => {
                        viewer.comment_input.push(c);
                    }
                    _ => {}
                }
                return;
            }
            
            match key.code {
                KeyCode::Esc => {
                    // On close, append comments to input if any
                    if viewer.has_comments() {
                        let comments = viewer.format_comments();
                        if !self.input.is_empty() && !self.input.ends_with('\n') {
                            self.input.push('\n');
                        }
                        self.input.push_str(&comments);
                        self.cursor_col = self.input.chars().count();
                    }
                    self.diff_viewer = None;
                }
                KeyCode::Char(']') => {
                    viewer.next_hunk();
                    viewer.cursor_line = 0;
                }
                KeyCode::Char('[') => {
                    viewer.prev_hunk();
                    viewer.cursor_line = 0;
                }
                KeyCode::Char('u') => {
                    viewer.toggle_split_mode();
                }
                KeyCode::Char('f') => {
                    viewer.toggle_fullscreen();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    viewer.cursor_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    viewer.cursor_up();
                }
                KeyCode::Char('c') | KeyCode::Enter => {
                    viewer.start_comment();
                }
                KeyCode::Char('n') => {
                    viewer.next_file();
                }
                KeyCode::Char('p') => {
                    viewer.prev_file();
                }
                KeyCode::Char('b') => {
                    viewer.toggle_file_tree();
                }
                KeyCode::PageDown => {
                    viewer.scroll_down(10);
                }
                KeyCode::PageUp => {
                    viewer.scroll_up(10);
                }
                _ => {}
            }
            return;
        }
        
        match &mut self.overlay {
            Some(Overlay::Approval { selected, .. }) => {
                match key.code {
                    KeyCode::Left => *selected = selected.saturating_sub(1),
                    KeyCode::Right => *selected = (*selected + 1).min(2),
                    KeyCode::Char('y') => {
                        self.respond_approval("allow");
                        self.overlay = None;
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.respond_approval("deny");
                        self.overlay = None;
                    }
                    KeyCode::Char('a') => {
                        self.respond_approval("always");
                        self.overlay = None;
                    }
                    KeyCode::Enter => {
                        let decision = match *selected {
                            0 => "allow",
                            1 => "always",
                            _ => "deny",
                        };
                        self.respond_approval(decision);
                        self.overlay = None;
                    }
                    _ => {}
                }
            }
            Some(Overlay::Help) => {
                // Any key closes help.
                self.overlay = None;
            }
            Some(Overlay::WhichKey) => {
                use crossterm::event::KeyCode;
                let groups = crate::which_key::get_keybindings(self.tool_mode);
                crate::log!("WHICH-KEY: key={:?} selected_idx={}", key.code, self.which_key_panel.selected_idx);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.which_key_panel.select_prev(&groups);
                        crate::log!("WHICH-KEY: after prev, selected_idx={}", self.which_key_panel.selected_idx);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.which_key_panel.select_next(&groups);
                        crate::log!("WHICH-KEY: after next, selected_idx={}", self.which_key_panel.selected_idx);
                    }
                    _ => {}
                }
            }
            Some(Overlay::ModelSelect { ref mut selected, ref mut filter }) => {
                crate::log!("  ModelSelect overlay: key={:?} selected={} filter={}", key.code, *selected, filter);

                // Get filtered models count for bounds checking.
                // Filter matches on display name OR provider.
                let filtered: Vec<usize> = self.model_sel.models().iter()
                    .enumerate()
                    .filter(|(_, m)| {
                        filter.is_empty()
                        || fuzzy_match(&m.display_name(), filter)
                        || fuzzy_match(&m.provider, filter)
                    })
                    .map(|(i, _)| i)
                    .collect();

                match key.code {
                    KeyCode::Esc => {
                        crate::log!("  ModelSelect: ESC pressed, closing overlay");
                        self.overlay = None;
                    }
                    KeyCode::Up => {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if !filtered.is_empty() {
                            *selected = filtered.len() - 1;
                        }
                        crate::log!("  ModelSelect: UP -> selected={}", *selected);
                    }
                    KeyCode::Down => {
                        if *selected < filtered.len().saturating_sub(1) {
                            *selected += 1;
                        } else {
                            *selected = 0;
                        }
                        crate::log!("  ModelSelect: DOWN -> selected={}", *selected);
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        *selected = 0; // Reset selection on filter change.
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *selected = 0; // Reset selection on filter change.
                    }
                    KeyCode::Enter => {
                        crate::log!("  ModelSelect: ENTER pressed, selected={}", *selected);
                        // Switch to selected model from filtered list.
                        if let Some(&model_idx) = filtered.get(*selected) {
                            self.model_sel.set_selected(model_idx);
                            if let Some(model) = self.model_sel.get_model(model_idx) {
                                let provider = model.provider.clone();
                                let id = model.id.clone();
                                crate::log!("  ModelSelect: model={}:{}", provider, id);
                                let session_id = self.session.state.session_id.clone();
                                let lease = self.session.state.lease.clone();
                                crate::log!("  ModelSelect: client={} lease={} session_id={}",
                                    self.client.is_some(), lease.is_some(), &session_id);
                                if let Some(client) = &self.client {
                                    // Persist selection to server preferences.
                                    let _ = client.set_pref("last_model", &id);
                                    // Send model change to current session (requires lease).
                                    if let Some(ref holder) = lease {
                                        if !session_id.is_empty() {
                                            crate::log!("MODEL CHANGE: session={} provider={} id={}", &session_id, &provider, &id);
                                            match client.set_model(&session_id, holder, &provider, &id) {
                                                Ok(_) => crate::log!("MODEL CHANGE: success"),
                                                Err(e) => crate::log!("MODEL CHANGE: error {:?}", e),
                                            }
                                        } else {
                                            crate::log!("MODEL CHANGE: no session_id");
                                        }
                                    } else {
                                        crate::log!("MODEL CHANGE: no lease");
                                    }
                                }
                            }
                        } else {
                            crate::log!("  ModelSelect: selected out of range");
                        }
                        self.overlay = None;
                    }
                    _ => {
                        crate::log!("  ModelSelect: unhandled key {:?}", key.code);
                    }
                }
            }
            Some(Overlay::SessionSelect { ref mut selected, ref mut filter }) => {
                crate::log!("  SessionSelect overlay: key={:?} selected={} filter={}", key.code, *selected, filter);

                // Get filtered sessions for bounds checking.
                // Selection index 0 = "New session", 1+ = filtered sessions.
                let filtered: Vec<usize> = self.session.sessions.iter()
                    .enumerate()
                    .filter(|(_, s)| fuzzy_match(&s.name, filter) || fuzzy_match(&s.id, filter))
                    .map(|(i, _)| i)
                    .collect();
                let total_selectable = 1 + filtered.len(); // "New session" + filtered sessions

                match key.code {
                    KeyCode::Esc => {
                        crate::log!("  SessionSelect: ESC pressed, closing overlay");
                        self.overlay = None;
                    }
                    KeyCode::Up => {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if total_selectable > 0 {
                            *selected = total_selectable - 1;
                        }
                    }
                    KeyCode::Down => {
                        if *selected < total_selectable.saturating_sub(1) {
                            *selected += 1;
                        } else {
                            *selected = 0;
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        *selected = 0; // Reset selection on filter change.
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *selected = 0; // Reset selection on filter change.
                    }
                    KeyCode::Delete => {
                        // Delete selected session (not "New session" which is index 0).
                        if *selected > 0 {
                            if let Some(&session_idx) = filtered.get(*selected - 1) {
                                let session_id = self.session.sessions[session_idx].id.clone();
                                crate::log!("  SessionSelect: DELETE session {}", session_id);
                                if let Some(client) = &self.client {
                                    if client.delete_session(&session_id).is_ok() {
                                        // Remove from local list.
                                        self.session.sessions.remove(session_idx);
                                        // Adjust selection if needed.
                                        if *selected >= total_selectable {
                                            *selected = total_selectable.saturating_sub(2);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        crate::log!("  SessionSelect: ENTER pressed, selected={}", *selected);
                        let is_new = *selected == 0;
                        let target_idx = if !is_new { filtered.get(*selected - 1).copied() } else { None };
                        self.overlay = None;
                        if is_new {
                            crate::log!("  SessionSelect: creating new session");
                            self.reset_session_state();
                            if let Err(e) = self.create_new_session(tx.clone()) {
                                self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                            }
                        } else if let Some(session_idx) = target_idx {
                            if session_idx < self.session.sessions.len() {
                                let new_session = self.session.sessions[session_idx].id.clone();
                                crate::log!("SESSION SWITCH: -> {}", &new_session[..8.min(new_session.len())]);
                                self.reset_session_state();
                                if let Err(e) = self.enter_session(&new_session, tx.clone()) {
                                    self.transcript.push(Message::new("error", format!("Failed to switch session: {}", e)));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(Overlay::CommandPalette) => {
                match key.code {
                    KeyCode::Esc => {
                        self.command_palette.close();
                        self.overlay = None;
                    }
                    KeyCode::Up => {
                        self.command_palette.select_prev();
                    }
                    KeyCode::Down => {
                        self.command_palette.select_next();
                    }
                    KeyCode::Backspace => {
                        self.command_palette.pop_char();
                    }
                    KeyCode::Char(c) => {
                        self.command_palette.push_char(c);
                    }
                    KeyCode::Enter => {
                        if let Some(cmd) = self.command_palette.selected_command() {
                            let cmd_id = cmd.id;
                            self.command_palette.close();
                            self.overlay = None;
                            self.execute_palette_command(cmd_id, tx);
                        }
                    }
                    _ => {}
                }
            }
            None => {}
        }
    }

    fn handle_welcome_key(&mut self, key: crossterm::event::KeyEvent, tx: &Sender<Event>) {
        // Handle diff viewer first (separate from overlay).
        if self.diff_viewer.is_some() {
            self.handle_overlay_key(key, tx);
            return;
        }
        
        // Handle overlay (same logic as chat screen).
        if self.overlay.is_some() {
            self.handle_overlay_key(key, tx);
            return;
        }
        
        // Check Ctrl+C/Q for quit, Ctrl+V for paste.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('q') => {
                    self.quit = true;
                    return;
                }
                KeyCode::Char('p') => {
                    self.command_palette.open();
                    self.overlay = Some(Overlay::CommandPalette);
                    return;
                }
                KeyCode::Char('v') => {
                    // Bracketed paste handles text; this is fallback for images only.
                    crate::log!("WELCOME: Ctrl+V detected, trying image paste");
                    self.paste_image_from_clipboard();
                    return;
                }
                _ => {}
            }
        }
        
        // Slash command handling (same as chat).
        if self.slash.active {
            match key.code {
                KeyCode::Esc => {
                    self.slash.deactivate();
                    self.input.clear();
                    self.cursor_col = 0;
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    if let Some(cmd) = self.slash.selected_command() {
                        self.execute_slash_command(cmd.name, tx);
                    }
                    self.slash.deactivate();
                    self.input.clear();
                    self.cursor_col = 0;
                    return;
                }
                KeyCode::Up => {
                    self.slash.select_prev();
                    return;
                }
                KeyCode::Down => {
                    self.slash.select_next();
                    return;
                }
                KeyCode::Backspace => {
                    if self.input.len() <= 1 {
                        self.slash.deactivate();
                        self.input.clear();
                        self.cursor_col = 0;
                    } else {
                        self.input.pop();
                        self.cursor_col = self.cursor_col.saturating_sub(1);
                        let query = self.input.trim_start_matches('/');
                        self.slash.set_query(query);
                    }
                    return;
                }
                KeyCode::Char(c) => {
                    self.input.push(c);
                    self.cursor_col += 1;
                    let query = self.input.trim_start_matches('/');
                    self.slash.set_query(query);
                    return;
                }
                _ => return,
            }
        }
        
        // Check keybinds for word navigation and model cycling actions.
        if let Some(action) = self.keybinds.match_key(key) {
            match action {
                Action::WordLeft => {
                    self.word_left();
                    return;
                }
                Action::WordRight => {
                    self.word_right();
                    return;
                }
                Action::DeleteWordLeft | Action::KillWord => {
                    self.kill_word_backward();
                    return;
                }
                Action::DeleteWordRight => {
                    self.delete_word_forward();
                    return;
                }
                Action::CycleModelNext => {
                    self.cycle_model_next();
                    return;
                }
                Action::CycleModelPrev => {
                    self.cycle_model_prev();
                    return;
                }
                // Ignore other actions on welcome screen
                _ => {}
            }
        }
        
        // Input handling.
        match key.code {
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.quit = true;
                } else {
                    self.input.clear();
                    self.cursor_col = 0;
                }
            }
            KeyCode::Left if !self.input.is_empty() => {
                // Cursor movement in input (by character, not byte).
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            KeyCode::Right if !self.input.is_empty() => {
                let char_count = self.input.chars().count();
                if self.cursor_col < char_count {
                    self.cursor_col += 1;
                }
            }
            KeyCode::Backspace => {
                if self.cursor_col > 0 {
                    // Convert char index to byte index.
                    let byte_idx = self.input.char_indices()
                        .nth(self.cursor_col - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.input.remove(byte_idx);
                    self.cursor_col -= 1;
                }
            }
            KeyCode::Char('/') if self.input.is_empty() => {
                // Start slash command.
                self.input.push('/');
                self.cursor_col = 1;
                self.slash.activate();
            }
            KeyCode::Char(c) if c == '\x08' || c == '\x7f' => {
                // Ctrl+Backspace sends ^H (0x08) or DEL (0x7f) - delete word backward.
                crate::log!("WELCOME: Ctrl+Backspace detected (0x{:02x})", c as u8);
                self.kill_word_backward();
            }
            KeyCode::Char(c) => {
                // Convert char index to byte index for insert.
                let byte_idx = self.input.char_indices()
                    .nth(self.cursor_col)
                    .map(|(i, _)| i)
                    .unwrap_or(self.input.len());
                self.input.insert(byte_idx, c);
                self.cursor_col += 1;
            }
            KeyCode::Enter => {
                if self.input.is_empty() {
                    // No input - create new empty session immediately (Phase 4.4c: no queued buffers).
                    crate::log!("ENTER: creating new session (empty input)");
                    if let Err(e) = self.create_new_session(tx.clone()) {
                        self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                    }
                } else {
                    // Has input - create session and send message immediately.
                    let msg = self.input.clone();
                    crate::log!("ENTER: creating session with message: {}", &msg);
                    self.input.clear();
                    self.cursor_col = 0;
                    match self.create_new_session(tx.clone()) {
                        Ok(()) => {
                            crate::log!("Sending first message: {}", &msg);
                            let images = std::mem::take(&mut self.staged_images);
                            let image_count = images.len();
                            self.transcript.push(Message::with_images("user", &msg, image_count));
                            let session_id = self.session.state.session_id.clone();
                            let lease = self.session.state.lease.clone().unwrap_or_default();
                            if let Some(client) = &self.client {
                                match client.prompt(&session_id, &lease, &msg, images) {
                                    Ok(_) => {
                                        crate::log!("prompt OK");
                                        self.streaming = true;
                                        self.tick_ctl.set_streaming(true);
                                    }
                                    Err(e) => {
                                        crate::log!("prompt FAILED: {:?}", e);
                                        self.transcript.push(Message::new("error", format!("Failed to send message: {}", e)));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn execute_action(&mut self, action: Action, tx: &Sender<Event>) {
        match action {
            Action::Quit => self.quit = true,
            Action::Abort => {
                if self.streaming && !self.aborting {
                    let session_id = self.session.state.session_id.clone();
                    let lease = self.session.state.lease.clone();
                    if let (Some(client), Some(holder)) = (&self.client, lease) {
                        let _ = client.abort(&session_id, &holder);
                        // Mark as aborting for visual feedback. streaming stays true until TurnEnded.
                        self.aborting = true;
                    }
                }
            }
            Action::Help => {
                self.command_palette.open();
                self.overlay = Some(Overlay::CommandPalette);
            }
            Action::Send => self.send_prompt(),
            Action::ScrollUp => self.transcript.scroll_up(3),
            Action::ScrollDown => self.transcript.scroll_down(3),
            Action::ScrollTop => self.transcript.scroll_top(),
            Action::ScrollBottom => self.transcript.scroll_bottom(),
            Action::PrevMessage => {
                if self.transcript.message_count() > 0 {
                    self.transcript.scroll_up(10);
                }
            }
            Action::NextMessage => {
                if self.transcript.message_count() > 0 {
                    self.transcript.scroll_down(10);
                }
            }
            Action::ToggleLeft => {
                // Left sidebar removed - sessions accessed via /session command.
                // Open session picker instead.
                self.overlay = Some(Overlay::SessionSelect { selected: 0, filter: String::new() });
            }
            Action::NewSession => {
                // Phase 4.4c: create immediately (no queued buffer).
                self.reset_session_state();
                if let Err(e) = self.create_new_session(tx.clone()) {
                    self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                }
            }
            Action::ToolMode => {
                if self.tool_mode {
                    self.exit_tool_mode();
                } else {
                    self.enter_tool_mode();
                }
            }
            Action::Paste => {
                // Bracketed paste handles text; this is fallback for images only.
                crate::log!("ACTION: Paste triggered, trying image paste");
                self.paste_image_from_clipboard();
            }
            Action::Undo => {
                self.undo_input();
            }
            Action::Redo => {
                self.redo_input();
            }
            Action::KillToEnd => {
                self.kill_to_end();
            }
            Action::KillToStart => {
                self.kill_to_start();
            }
            Action::KillWord => {
                self.kill_word_backward();
            }
            Action::Yank => {
                self.yank();
            }
            Action::YankPop => {
                self.yank_pop();
            }
            Action::ToggleThinking => {
                self.toggle_all_thinking();
            }
            Action::OpenSearch => {
                self.open_search();
            }
            // Word navigation
            Action::WordLeft => {
                self.word_left();
            }
            Action::WordRight => {
                self.word_right();
            }
            Action::DeleteWordLeft => {
                self.kill_word_backward();
            }
            Action::DeleteWordRight => {
                self.delete_word_forward();
            }
            // Semantic navigation (jump between user/assistant messages)
            Action::PrevUserMessage => {
                self.jump_to_prev_user_message();
            }
            Action::NextUserMessage => {
                self.jump_to_next_user_message();
            }
            // Model cycling (quick switch without picker)
            Action::CycleModelNext => {
                self.cycle_model_next();
            }
            Action::CycleModelPrev => {
                self.cycle_model_prev();
            }
            // Search mode actions are handled in handle_search_key, not here.
            Action::CloseSearch | Action::NextMatch | Action::PrevMatch
            | Action::ToggleRegex | Action::ToggleCase => {}
            _ => {}
        }
    }

    /// Open the search bar.
    fn open_search(&mut self) {
        if self.search_state.is_none() {
            let mut state = SearchState::new();
            state.search(self.transcript.messages());
            self.search_state = Some(state);
        }
    }

    /// Handle key events when the search bar is open.
    fn handle_search_key(&mut self, key: crossterm::event::KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            // Close search.
            KeyCode::Esc => {
                self.search_state = None;
                return;
            }
            // Ctrl+F again: close search (toggle).
            KeyCode::Char('f') if ctrl => {
                self.search_state = None;
                return;
            }
            // Next match: Enter (no shift).
            KeyCode::Enter if !shift => {
                if let Some(ref mut s) = self.search_state {
                    s.next_match();
                    // Scroll transcript to show the current match.
                    if let Some(m) = s.current_match() {
                        let msg_idx = m.msg_idx;
                        // Approximate scroll: jump to message position.
                        // We scroll to put the matching message near the top.
                        // The transcript scroll is in lines-from-bottom; we use a
                        // heuristic of scrolling to a large value then back down.
                        let total = self.transcript.messages().len();
                        let lines_per_msg = 4usize; // rough estimate
                        let lines_from_bottom = (total.saturating_sub(msg_idx + 1)) * lines_per_msg;
                        self.transcript.set_scroll(lines_from_bottom);
                    }
                }
                return;
            }
            // Previous match: Shift+Enter.
            KeyCode::Enter if shift => {
                if let Some(ref mut s) = self.search_state {
                    s.prev_match();
                    if let Some(m) = s.current_match() {
                        let msg_idx = m.msg_idx;
                        let total = self.transcript.messages().len();
                        let lines_per_msg = 4usize;
                        let lines_from_bottom = (total.saturating_sub(msg_idx + 1)) * lines_per_msg;
                        self.transcript.set_scroll(lines_from_bottom);
                    }
                }
                return;
            }
            // Toggle regex: Alt+R.
            KeyCode::Char('r') if alt => {
                if let Some(ref mut s) = self.search_state {
                    s.regex_mode = !s.regex_mode;
                    s.search(self.transcript.messages());
                }
                return;
            }
            // Toggle case: Alt+C.
            KeyCode::Char('c') if alt => {
                if let Some(ref mut s) = self.search_state {
                    s.case_sensitive = !s.case_sensitive;
                    s.search(self.transcript.messages());
                }
                return;
            }
            // Backspace: delete char before cursor in query.
            KeyCode::Backspace => {
                if let Some(ref mut s) = self.search_state {
                    s.delete_before_cursor();
                    s.search(self.transcript.messages());
                }
                return;
            }
            // Typing: insert char into query.
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(ref mut s) = self.search_state {
                    s.insert_char(c);
                    s.search(self.transcript.messages());
                }
                return;
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        // Handle diff viewer mouse events first (separate from overlay)
        if let Some(ref mut viewer) = self.diff_viewer {
            if !viewer.commenting {
                match mouse.kind {
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        // Click on file tree or diff line
                        viewer.handle_click_at(mouse.column, mouse.row);
                        return;
                    }
                    MouseEventKind::ScrollUp => {
                        viewer.handle_scroll(-3);
                        return;
                    }
                    MouseEventKind::ScrollDown => {
                        viewer.handle_scroll(3);
                        return;
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                        // Right click to add comment
                        if viewer.select_by_click(mouse.row) {
                            viewer.start_comment();
                        }
                        return;
                    }
                    _ => {}
                }
            }
            return;
        }
        
        match mouse.kind {
            MouseEventKind::Moved => {
                // Right sidebar stays expanded — no hover behavior needed.
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                self.handle_click(mouse.column, mouse.row);
            }
            MouseEventKind::ScrollUp => {
                // Check if scrolling over a tool card output area
                if let Some(call_id) = self.find_tool_at_y(mouse.row) {
                    // Scroll within tool output
                    if let Some(tool) = self.tool_mut(&call_id) {
                        if tool.expanded {
                            tool.scroll_offset = tool.scroll_offset.saturating_sub(3);
                            return;
                        }
                    }
                }
                // Default: scroll transcript
                self.transcript.scroll_up(3);
            }
            MouseEventKind::ScrollDown => {
                // Check if scrolling over a tool card output area
                if let Some(call_id) = self.find_tool_at_y(mouse.row) {
                    // Scroll within tool output
                    let call_id_clone = call_id.clone();
                    if let Some(tool) = self.tool_mut(&call_id_clone) {
                        if tool.expanded {
                            // Count total lines: progress_lines + output
                            let mut line_count = tool.progress_lines.len();
                            if let Some(ref output) = tool.output {
                                if !output.is_empty() {
                                    if line_count > 0 { line_count += 1; }
                                    line_count += output.lines().count();
                                }
                            }
                            if line_count > 0 {
                                let visible_lines = 20;
                                let max_scroll = line_count.saturating_sub(visible_lines);
                                tool.scroll_offset = (tool.scroll_offset + 3).min(max_scroll);
                                return;
                            }
                        }
                    }
                }
                // Default: scroll transcript
                self.transcript.scroll_down(3);
            }
            _ => {}
        }
    }

    /// Find tool call_id if mouse Y is within a tool's content area.
    fn find_tool_at_y(&self, y: u16) -> Option<String> {
        for hit in &self.tool_hit_areas {
            if y >= hit.content_y_start && y < hit.content_y_end {
                return Some(hit.call_id.clone());
            }
        }
        None
    }
    
    fn handle_click(&mut self, x: u16, y: u16) {
        // Right sidebar: tools are now server-driven (GET /tools), not togglable locally.
        // The previous dead `enabled` toggle (F9) has been removed — clicks here are no-ops
        // (future: per-tool enable could be a server endpoint, but not a lying local flip).
        let right_start = self.term_width.saturating_sub(24); // RIGHT_EXPANDED width
        if x >= right_start {
            // Sidebar click — refresh tools from server instead of toggling dead state.
            if let Some(names) = self.client.as_ref().and_then(|c| c.get_tools().ok()) {
                self.tools = names.into_iter().map(|n| ToolEntry { name: n, enabled: true }).collect();
            }
            return;
        }
        
        // Check tool card clicks
        for hit in &self.tool_hit_areas.clone() {
            // Click on header line - toggle expand/collapse
            if y == hit.header_y {
                self.toggle_tool_expand(&hit.call_id);
                return;
            }

            // Click on tabs (only if expanded)
            let is_expanded = self.transcript.messages().iter()
                .flat_map(|m| m.tools.iter())
                .find(|t| t.call_id == hit.call_id)
                .map(|t| t.expanded)
                .unwrap_or(false);
            if is_expanded && y == hit.content_y_start {
                // Tab row - check which tab
                if x >= hit.progress_tab_x.0 && x < hit.progress_tab_x.1 {
                    self.switch_tool_tab(&hit.call_id, crate::message_handler::ToolTab::Progress);
                    return;
                } else if x >= hit.output_tab_x.0 && x < hit.output_tab_x.1 {
                    self.switch_tool_tab(&hit.call_id, crate::message_handler::ToolTab::Output);
                    return;
                } else if x >= hit.input_tab_x.0 && x < hit.input_tab_x.1 {
                    self.switch_tool_tab(&hit.call_id, crate::message_handler::ToolTab::Input);
                    return;
                }
            }
        }
    }

    fn handle_paste(&mut self, text: &str) {
        // Record state for undo (paste is a boundary - don't coalesce)
        self.record_input_boundary();
        // Insert at cursor.
        let pos = self.cursor_pos();
        self.input.insert_str(pos, text);
        self.cursor_col += text.lines().last().map(|l| l.len()).unwrap_or(0);
        self.cursor_row += text.lines().count().saturating_sub(1);
    }

    /// Paste image from system clipboard (fallback when bracketed paste is empty).
    fn paste_image_from_clipboard(&mut self) {
        crate::log!("PASTE IMAGE: starting, current staged_images.len={}", self.staged_images.len());
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                crate::log!("PASTE IMAGE: Clipboard init error: {:?}", e);
                return;
            }
        };

        match clipboard.get_image() {
            Ok(image) => {
                let width = image.width;
                let height = image.height;
                crate::log!("PASTE IMAGE: got image {}x{}", width, height);
                if let Some(base64) = self.encode_image_as_base64_png(&image) {
                    self.staged_images.push(base64);
                    let img_num = self.staged_images.len();
                    crate::log!("PASTE IMAGE: encoded, staged_images.len={}", img_num);
                    
                    // Insert [imgN: WxH PNG] marker at cursor position.
                    let marker = format!("[img{}: {}x{} PNG]", img_num, width, height);
                    let pos = self.cursor_pos();
                    self.input.insert_str(pos, &marker);
                    self.cursor_col += marker.len();
                }
            }
            Err(e) => {
                crate::log!("PASTE IMAGE: no image in clipboard: {:?}", e);
            }
        }
    }

    /// Encode arboard ImageData as base64 PNG (data URI format).
    fn encode_image_as_base64_png(&self, image: &arboard::ImageData) -> Option<String> {
        use std::io::Cursor;

        // arboard gives us RGBA bytes.
        let width = image.width as u32;
        let height = image.height as u32;
        let rgba = &image.bytes;

        // Encode to PNG in memory.
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(Cursor::new(&mut png_data), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(rgba).ok()?;
        }

        // Base64 encode with data URI prefix.
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
        Some(format!("data:image/png;base64,{}", b64))
    }

    fn handle_sse(&mut self, frame: SseFrame) {
        if let Some(seq) = frame.seq() {
            self.session.state.last_seq = seq;
        }

        match frame {
            SseFrame::TurnStarted { .. } => {
                self.streaming = true;
                self.tick_ctl.set_streaming(true);
                self.transcript.take_delta(); // clear live_delta
                // Delegate turn-start accounting to TokenTracker.
                // This marks turn-reset as pending so last_turn stats stay visible during streaming.
                self.tokens.on_turn_started();
            }
            SseFrame::TurnEnded { stop, .. } => {
                self.streaming = false;
                self.aborting = false;
                self.tick_ctl.set_streaming(false);
                // Finalize throughput calculation.
                self.tokens.on_turn_ended();
                if stop.to_ascii_lowercase().contains("abort") {
                    let partial = self.transcript.take_delta();
                    if !partial.is_empty() {
                        // Keep the partial streaming text so resend doesn't lose it (user noted it disappears)
                        let preview: String = partial.chars().take(400).collect();
                        self.transcript.push(Message::new("system", format!("Aborted — kept partial ({} chars): {}", partial.len(), preview)));
                        // Also put partial back into input for easy resend? Keep as system note only; input stays as user left it.
                    } else {
                        self.transcript.push(Message::new("system", "Aborted"));
                    }
                }
            }
            SseFrame::TextDelta { delta, .. } => {
                self.transcript.append_delta(&delta);
            }
            SseFrame::MessageAppended { msg, .. } => {
                // Skip user messages - we already added them locally in send_prompt().
                // Skip silent messages (e.g., AGENTS.md injection).
                if msg.role == "user" || msg.silent {
                    return;
                }

                let (text, tool_calls, tool_results, _) = Self::extract_message_content(&msg.content);

                // Apply tool results to existing tool cards.
                for (call_id, output, is_error) in &tool_results {
                    self.transcript.update_tool(call_id, |tool| {
                        tool.output = Some(output.clone());
                        if *is_error {
                            tool.status = "error".into();
                        }
                    });
                }

                // Use live_delta if we were streaming, otherwise use message content.
                let final_content = if !self.transcript.live_delta().is_empty() {
                    self.transcript.take_delta()
                } else {
                    text
                };

                // Create tool cards from tool calls.
                let tools: Vec<ToolCard> = tool_calls.iter().map(|(id, name, args)| {
                    ToolCard {
                        call_id: id.clone(),
                        name: name.clone(),
                        args: args.clone(),
                        status: "pending".into(),
                        output: None,
                        progress_lines: Vec::new(),
                        expanded: false,
                        active_tab: crate::message_handler::ToolTab::Input,
                        scroll_offset: 0,
                    }
                }).collect();

                // Add assistant message if there's content or tools.
                if !final_content.is_empty() || !tools.is_empty() {
                    self.transcript.push(Message::new(&msg.role, final_content).with_tools(tools));
                }
            }
            SseFrame::UsageRecorded { tokens, cost_usd, usage_kind, .. } => {
                // Delegate all token accounting to TokenTracker.
                // It handles: session totals, last-turn accumulation, turn-reset flag,
                // title filtering, and throughput token counting.
                let counts = TokenCounts::new(
                    tokens.input as usize,
                    tokens.output as usize,
                    tokens.cache_read as usize,
                    tokens.cache_write as usize,
                );
                self.tokens.record_usage(counts, cost_usd, usage_kind == "title");
            }
            SseFrame::ToolStarted { call_id, .. } => {
                // Find the tool card (created by MessageAppended) and mark it running.
                self.transcript.update_tool(&call_id, |tool| {
                    tool.status = "running".into();
                    tool.expanded = true;  // Auto-expand running tools
                    tool.active_tab = crate::message_handler::ToolTab::Progress;
                });
            }
            SseFrame::ToolArgsDelta { .. } => {
                // Ignored by TUI — args come complete in MessageAppended.
            }
            SseFrame::ToolProgress { call_id, note, .. } => {
                // Accumulate progress lines and update status.
                self.transcript.update_tool(&call_id, |tool| {
                    tool.progress_lines.push(note.clone());
                    tool.status = format!("running: {}", note);
                });
            }
            SseFrame::ToolFinished { call_id, is_error, .. } => {
                self.transcript.update_tool(&call_id, |tool| {
                    tool.status = if is_error { "error".into() } else { "done".into() };
                    tool.active_tab = crate::message_handler::ToolTab::Output;
                    tool.expanded = false;  // Auto-collapse when done
                    tool.scroll_offset = 0;
                });
            }
            SseFrame::ApprovalRequest { id, tool, args, .. } => {
                self.active_approval_id = Some(id);
                self.overlay = Some(Overlay::Approval {
                    tool,
                    args: serde_json::to_string(&args).unwrap_or_default(),
                    selected: 0,
                });
            }
            SseFrame::Error { message } => {
                self.transcript.push(Message::new("error", message));
            }
            SseFrame::TitleChanged { title } => {
                // Update the current session title.
                self.session.set_session_title(Some(title.clone()));
                // Also update in the sessions list for sidebar display.
                let session_id = self.session.state.session_id.clone();
                if let Some(s) = self.session.sessions.iter_mut().find(|s| s.id == session_id) {
                    s.name = title;
                }
            }
            SseFrame::PluginNotification { plugin, message } => {
                self.transcript.push(Message::new(&plugin, message));
            }
            SseFrame::HookFailed { plugin, hook, reason } => {
                self.transcript.push(Message::new("system", format!("Hook failed {}:{} — {}", plugin, hook, reason)));
            }
            SseFrame::ThinkingDelta { delta, .. } => {
                // Phase 4: previously ignored (only seq recorded). Now treat as streamed thinking.
                self.transcript.append_delta(&delta);
                // Also update thinking collapse state: ensure thinking blocks are expanded while streaming?
            }
            SseFrame::ModelChanged { model, .. } => {
                // Phase 4: previously ignored. Update model selector if known and show notification.
                let name = format!("{}:{}", model.provider, model.id);
                crate::log!("SSE ModelChanged: {}", name);
                // Try to select in ModelSelector if present.
                if let Some(idx) = self.model_sel.models().iter().position(|m| m.provider == model.provider && m.id == model.id) {
                    self.model_sel.set_selected(idx);
                }
                self.transcript.push(Message::new("system", format!("Model changed to {}", name)));
            }
            SseFrame::Compacted { replaced, summary, .. } => {
                // Phase 4: previously ignored. Reflect compaction in transcript.
                crate::log!("SSE Compacted: replaced {}..{} summary {}", replaced.start, replaced.end, summary.id);
                let (text, _, _, _) = Self::extract_message_content(&summary.content);
                let summary_text = if text.is_empty() { "Conversation compacted.".to_string() } else { text };
                self.transcript.push(Message::new("system", format!("Compacted {}..{}: {}", replaced.start, replaced.end, summary_text.clone())));
                // Also push summary as assistant message so transcript reflects new state.
                self.transcript.push(Message::new(&summary.role, summary_text));
            }
        }
    }

    /// Extract text, tool calls, tool results, and image count from wire content blocks.
    /// Returns (text, tool_calls, tool_results, image_count)
    fn extract_message_content(content: &[crate::wire::WireContent]) 
        -> (String, Vec<(String, String, String)>, Vec<(String, String, bool)>, usize) 
    {
        use crate::wire::WireContent;
        
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut tool_results = Vec::new();
        let mut image_count = 0;
        
        for c in content {
            match c {
                WireContent::Text { text } => text_parts.push(text.as_str()),
                WireContent::Thinking { text } => text_parts.push(text.as_str()),
                WireContent::ToolCall { id, name, args_json } => {
                    tool_calls.push((id.clone(), name.clone(), args_json.clone()));
                }
                WireContent::ToolResult { id, content: result_content, is_error } => {
                    // Extract text from tool result content.
                    let output: String = result_content.iter().filter_map(|rc| {
                        match rc {
                            WireContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        }
                    }).collect::<Vec<_>>().join("\n");
                    tool_results.push((id.clone(), output, *is_error));
                }
                WireContent::Image { .. } => {
                    image_count += 1;
                }
            }
        }
        
        (text_parts.join("\n"), tool_calls, tool_results, image_count)
    }

    /// Condense <system-reminder> messages to a summary line.
    /// Keeps the source attribute but removes the full content.
    fn condense_system_reminder(text: &str) -> String {
        // Check for <system-reminder source="...">
        if let Some(start) = text.find("<system-reminder") {
            if let Some(source_start) = text[start..].find("source=\"") {
                let source_begin = start + source_start + 8; // skip 'source="'
                if let Some(source_end) = text[source_begin..].find('"') {
                    let source = &text[source_begin..source_begin + source_end];
                    return format!("ℹ {}", source);
                }
            }
            // Fallback: found tag but couldn't parse source
            return "ℹ system-reminder injected".to_string();
        }
        // Not a system-reminder, return as-is
        text.to_string()
    }

    fn send_prompt(&mut self) {
        // Allow sending with just images (no text required).
        if self.input.trim().is_empty() && self.staged_images.is_empty() {
            return;
        }
        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();
        if let (Some(client), Some(holder)) = (&self.client, lease) {
            let text = std::mem::take(&mut self.input);
            let images = std::mem::take(&mut self.staged_images);
            let image_count = images.len();
            self.cursor_row = 0;
            self.cursor_col = 0;
            
            // Add to prompt history
            self.prompt_history.add(text.clone());
            
            // Clear undo/redo history for new prompt
            self.input_history.clear();

            // Add user message locally with image count (don't wait for SSE).
            self.transcript.push(Message::with_images("user", &text, image_count));

            // Send to server.
            let _ = client.prompt(&session_id, &holder, &text, images);
        }
    }

    fn respond_approval(&mut self, decision: &str) {
        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();
        if let (Some(client), Some(holder), Some(id)) = (&self.client, lease, self.active_approval_id) {
            let _ = client.approve(&session_id, &holder, id, decision);
            self.active_approval_id = None;
        }
    }

    fn insert_newline(&mut self) {
        // Record state for undo (newline is a boundary)
        self.record_input_boundary();
        let pos = self.cursor_pos();
        self.input.insert(pos, '\n');
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// Convert (row, col) cursor position to byte index, handling UTF-8.
    fn cursor_pos(&self) -> usize {
        let mut pos = 0;
        for (i, line) in self.input.lines().enumerate() {
            if i == self.cursor_row {
                // Convert char col to byte offset within line.
                let byte_col = line.char_indices()
                    .nth(self.cursor_col)
                    .map(|(idx, _)| idx)
                    .unwrap_or(line.len());
                return pos + byte_col;
            }
            pos += line.len() + 1; // +1 for newline
        }
        self.input.len()
    }

    /// Get current line length in characters (not bytes).
    fn current_line_len(&self) -> usize {
        self.input.lines().nth(self.cursor_row).map(|l| l.chars().count()).unwrap_or(0)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // UNDO/REDO
    // ═══════════════════════════════════════════════════════════════════════

    /// Create a snapshot of current input state.
    fn input_snapshot(&self) -> InputSnapshot {
        InputSnapshot::new(self.input.clone(), self.cursor_row, self.cursor_col)
    }

    /// Restore input state from a snapshot.
    fn restore_snapshot(&mut self, snapshot: InputSnapshot) {
        self.input = snapshot.text;
        self.cursor_row = snapshot.cursor_row;
        self.cursor_col = snapshot.cursor_col;
        // Clamp cursor to valid range
        let line_count = self.input.lines().count().max(1);
        self.cursor_row = self.cursor_row.min(line_count - 1);
        self.cursor_col = self.cursor_col.min(self.current_line_len());
    }

    /// Record current state before a change (for character typing).
    fn record_input_change(&mut self) {
        let snapshot = self.input_snapshot();
        self.input_history.record(snapshot);
    }

    /// Record current state before a significant change (paste, delete word, etc.).
    fn record_input_boundary(&mut self) {
        let snapshot = self.input_snapshot();
        self.input_history.record_boundary(snapshot);
    }

    /// Undo the last input change.
    fn undo_input(&mut self) {
        let current = self.input_snapshot();
        if let Some(prev) = self.input_history.undo(current) {
            self.restore_snapshot(prev);
            crate::log!("UNDO: restored to {} chars", self.input.len());
        }
    }

    /// Redo the last undone change.
    fn redo_input(&mut self) {
        let current = self.input_snapshot();
        if let Some(next) = self.input_history.redo(current) {
            self.restore_snapshot(next);
            crate::log!("REDO: restored to {} chars", self.input.len());
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // KILL RING
    // ═══════════════════════════════════════════════════════════════════════

    /// Kill (cut) text from cursor to end of line.
    fn kill_to_end(&mut self) {
        self.kill_ring.reset_yank();
        let pos = self.cursor_pos();
        let line_end = self.line_end_pos();
        
        if pos >= line_end {
            // At end of line, kill the newline
            if pos < self.input.len() {
                self.record_input_boundary();
                let killed = self.input[pos..pos+1].to_string();
                self.input.remove(pos);
                self.kill_ring.kill(killed, false);
            }
        } else {
            // Kill to end of line
            self.record_input_boundary();
            let killed: String = self.input[pos..line_end].to_string();
            self.input.replace_range(pos..line_end, "");
            self.kill_ring.kill(killed, false);
        }
    }

    /// Kill (cut) text from start of line to cursor.
    fn kill_to_start(&mut self) {
        self.kill_ring.reset_yank();
        let pos = self.cursor_pos();
        let line_start = self.line_start_pos();
        
        if pos > line_start {
            self.record_input_boundary();
            let killed: String = self.input[line_start..pos].to_string();
            self.input.replace_range(line_start..pos, "");
            self.cursor_col = 0;
            self.kill_ring.kill(killed, false);
        }
    }

    /// Kill (cut) the word before cursor.
    fn kill_word_backward(&mut self) {
        self.kill_ring.reset_yank();
        let pos = self.cursor_pos();
        if pos == 0 {
            return;
        }

        // Find word boundary (skip whitespace, then non-whitespace)
        let before = &self.input[..pos];
        let trimmed = before.trim_end();
        let word_start = trimmed.rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        
        let word_start = if trimmed.len() < before.len() {
            // There was trailing whitespace
            trimmed.len().saturating_sub(trimmed.len() - word_start)
        } else {
            word_start
        };

        // Calculate how many chars we're removing
        let chars_removed = before[word_start..].chars().count();
        
        self.record_input_boundary();
        let killed: String = self.input[word_start..pos].to_string();
        self.input.replace_range(word_start..pos, "");
        self.cursor_col = self.cursor_col.saturating_sub(chars_removed);
        self.kill_ring.kill(killed, false);
    }

    /// Yank (paste) from kill ring.
    fn yank(&mut self) {
        let pos = self.cursor_pos();
        // Clone the text first to avoid borrow issues
        let text = match self.kill_ring.yank(pos) {
            Some(t) => t.to_string(),
            None => return,
        };
        
        self.record_input_boundary();
        let chars_added = text.chars().count();
        self.input.insert_str(pos, &text);
        self.cursor_col += chars_added;
        crate::log!("YANK: inserted {} chars", chars_added);
    }

    /// Yank pop - cycle through kill ring after yank.
    fn yank_pop(&mut self) {
        if !self.kill_ring.in_yank_state() {
            return;
        }

        // Get yank_pop result and last_yank_pos separately to avoid borrow issues
        let yank_result = self.kill_ring.yank_pop().map(|(len, text)| (len, text.to_string()));
        
        if let Some((remove_len, new_text)) = yank_result {
            // Re-query last_yank_pos after the mutable borrow is released
            if let Some((start_pos, _)) = self.kill_ring.last_yank_pos() {
                let end_pos = start_pos + remove_len;
                if end_pos <= self.input.len() {
                    self.record_input_boundary();
                    self.input.replace_range(start_pos..end_pos, &new_text);
                    let chars_diff = new_text.chars().count() as isize - remove_len as isize;
                    self.cursor_col = (self.cursor_col as isize + chars_diff).max(0) as usize;
                    crate::log!("YANK-POP: replaced {} bytes with {} chars", remove_len, new_text.len());
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Word Navigation (Ctrl+Left/Right, Ctrl+Delete)
    // ═══════════════════════════════════════════════════════════════════════
    
    /// Move cursor to previous word boundary.
    fn word_left(&mut self) {
        use crate::word_segmenter::{prev_word_boundary, byte_to_char_offset};
        
        let byte_pos = self.cursor_pos();
        if byte_pos == 0 {
            return;
        }
        
        let new_byte_pos = prev_word_boundary(&self.input, byte_pos);
        self.cursor_col = byte_to_char_offset(&self.input, new_byte_pos);
    }
    
    /// Move cursor to next word boundary.
    fn word_right(&mut self) {
        use crate::word_segmenter::{next_word_boundary, byte_to_char_offset};
        
        let byte_pos = self.cursor_pos();
        if byte_pos >= self.input.len() {
            return;
        }
        
        let new_byte_pos = next_word_boundary(&self.input, byte_pos);
        self.cursor_col = byte_to_char_offset(&self.input, new_byte_pos);
    }
    
    /// Delete word forward (Ctrl+Delete).
    fn delete_word_forward(&mut self) {
        use crate::word_segmenter::next_word_boundary;
        
        let pos = self.cursor_pos();
        if pos >= self.input.len() {
            return;
        }
        
        let word_end = next_word_boundary(&self.input, pos);
        if word_end > pos {
            self.record_input_boundary();
            let killed: String = self.input[pos..word_end].to_string();
            self.input.replace_range(pos..word_end, "");
            self.kill_ring.kill(killed, false);
        }
    }
    
    // ═══════════════════════════════════════════════════════════════════════
    // Semantic Navigation (Ctrl+Up/Down to jump user/assistant messages)
    // ═══════════════════════════════════════════════════════════════════════
    
    /// Jump to previous user message in transcript.
    fn jump_to_prev_user_message(&mut self) {
        let messages = self.transcript.messages();
        if messages.is_empty() {
            return;
        }
        
        // Find user message indices
        let user_indices: Vec<usize> = messages.iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i)
            .collect();
        
        if user_indices.is_empty() {
            return;
        }
        
        // Estimate current view position based on scroll
        // (scroll is lines-from-bottom, higher = earlier messages)
        let current_scroll = self.transcript.scroll();
        let total_messages = messages.len();
        let lines_per_msg = 4usize; // rough estimate
        
        // Convert scroll to approximate message index (from end)
        let msgs_from_bottom = current_scroll / lines_per_msg;
        let approx_visible_idx = total_messages.saturating_sub(msgs_from_bottom + 1);
        
        // Find previous user message before current position
        let target_idx = user_indices.iter()
            .rev()
            .find(|&&i| i < approx_visible_idx)
            .or_else(|| user_indices.last())
            .copied();
        
        if let Some(idx) = target_idx {
            // Scroll to show this message
            let lines_from_bottom = (total_messages.saturating_sub(idx + 1)) * lines_per_msg;
            self.transcript.set_scroll(lines_from_bottom);
        }
    }
    
    /// Jump to next user message in transcript.
    fn jump_to_next_user_message(&mut self) {
        let messages = self.transcript.messages();
        if messages.is_empty() {
            return;
        }
        
        // Find user message indices
        let user_indices: Vec<usize> = messages.iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i)
            .collect();
        
        if user_indices.is_empty() {
            return;
        }
        
        // Estimate current view position
        let current_scroll = self.transcript.scroll();
        let total_messages = messages.len();
        let lines_per_msg = 4usize;
        
        let msgs_from_bottom = current_scroll / lines_per_msg;
        let approx_visible_idx = total_messages.saturating_sub(msgs_from_bottom + 1);
        
        // Find next user message after current position
        let target_idx = user_indices.iter()
            .find(|&&i| i > approx_visible_idx)
            .or_else(|| user_indices.first())
            .copied();
        
        if let Some(idx) = target_idx {
            let lines_from_bottom = (total_messages.saturating_sub(idx + 1)) * lines_per_msg;
            self.transcript.set_scroll(lines_from_bottom);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Model Cycling (F2 / Shift+F2)
    // ═══════════════════════════════════════════════════════════════════════
    
    /// Cycle to next model and apply immediately.
    fn cycle_model_next(&mut self) {
        if self.model_sel.model_count() == 0 {
            self.transcript.push(Message::new("system", "No models available"));
            return;
        }
        
        self.model_sel.select_next();
        self.apply_current_model();
    }
    
    /// Cycle to previous model and apply immediately.
    fn cycle_model_prev(&mut self) {
        if self.model_sel.model_count() == 0 {
            self.transcript.push(Message::new("system", "No models available"));
            return;
        }
        
        self.model_sel.select_prev();
        self.apply_current_model();
    }
    
    /// Apply the currently selected model to the session.
    fn apply_current_model(&mut self) {
        let Some(model) = self.model_sel.current_model() else {
            return;
        };
        
        let provider = model.provider.clone();
        let id = model.id.clone();
        let display_name = model.display_name();
        
        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();
        
        if let Some(client) = &self.client {
            // Persist selection to server preferences.
            let _ = client.set_pref("last_model", &id);
            
            // Send model change to current session (requires lease).
            if let Some(ref holder) = lease {
                if !session_id.is_empty() {
                    crate::log!("MODEL CYCLE: session={} provider={} id={}", &session_id, &provider, &id);
                    match client.set_model(&session_id, holder, &provider, &id) {
                        Ok(_) => {
                            crate::log!("MODEL CYCLE: success");
                            self.transcript.push(Message::new("system", &format!("Model: {}", display_name)));
                        }
                        Err(e) => {
                            crate::log!("MODEL CYCLE: error {:?}", e);
                            self.transcript.push(Message::new("system", &format!("Failed to set model: {:?}", e)));
                        }
                    }
                } else {
                    // No session yet - just show the selection.
                    self.transcript.push(Message::new("system", &format!("Model: {} (will apply on next message)", display_name)));
                }
            } else {
                // No lease - just show the selection.
                self.transcript.push(Message::new("system", &format!("Model: {} (will apply on next message)", display_name)));
            }
        } else {
            self.transcript.push(Message::new("system", &format!("Model: {} (not connected)", display_name)));
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Prompt Stash (/stash and /unstash commands)
    // ═══════════════════════════════════════════════════════════════════════
    
    /// Stash current prompt state.
    fn stash_prompt(&mut self) {
        if self.input.is_empty() {
            self.transcript.push(Message::new("system", "Nothing to stash (input is empty)"));
            return;
        }
        self.prompt_stash.stash(&self.input, self.cursor_row, self.cursor_col);
        let len = self.input.len();
        self.input.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.transcript.push(Message::new("system", &format!("Stashed {} chars", len)));
    }
    
    /// Restore prompt from stash.
    fn unstash_prompt(&mut self) {
        if let Some((text, row, col)) = self.prompt_stash.unstash() {
            // If there's current input, swap it
            if !self.input.is_empty() {
                let current = self.input.clone();
                let cur_row = self.cursor_row;
                let cur_col = self.cursor_col;
                self.prompt_stash.stash(&current, cur_row, cur_col);
                self.transcript.push(Message::new("system", "Swapped stash with current input"));
            }
            self.input = text;
            self.cursor_row = row;
            self.cursor_col = col.min(self.current_line_len());
        } else {
            self.transcript.push(Message::new("system", "Stash is empty"));
        }
    }

    /// Toggle collapse state for all thinking blocks.
    fn toggle_all_thinking(&mut self) {
        // Count thinking blocks in current transcript
        let mut count = 0;
        for msg in self.transcript.messages() {
            if msg.role == "assistant" {
                let segments = crate::thinking::parse_content(&msg.content);
                for seg in segments {
                    if matches!(seg, crate::thinking::ContentSegment::Thinking { .. }) {
                        count += 1;
                    }
                }
            }
        }
        
        // Also count in live delta
        let segments = crate::thinking::parse_content(self.transcript.live_delta());
        for seg in segments {
            if matches!(seg, crate::thinking::ContentSegment::Thinking { .. }) {
                count += 1;
            }
        }
        
        // Toggle: if any expanded, collapse all. Otherwise expand all.
        let any_expanded = (0..count).any(|i| !self.thinking_state.is_collapsed(i));
        if any_expanded {
            self.thinking_state.collapse_all(count);
            crate::log!("THINKING: collapsed all {} blocks", count);
        } else {
            self.thinking_state.expand_all();
            crate::log!("THINKING: expanded all {} blocks", count);
        }
    }

    /// Get byte position of current line start.
    fn line_start_pos(&self) -> usize {
        let mut pos = 0;
        for (i, line) in self.input.lines().enumerate() {
            if i == self.cursor_row {
                return pos;
            }
            pos += line.len() + 1;
        }
        0
    }

    /// Get byte position of current line end.
    fn line_end_pos(&self) -> usize {
        let mut pos = 0;
        for (i, line) in self.input.lines().enumerate() {
            if i == self.cursor_row {
                return pos + line.len();
            }
            pos += line.len() + 1;
        }
        self.input.len()
    }
    
    fn execute_slash_command(&mut self, cmd: &str, tx: &Sender<Event>) {
        match cmd {
            "session" => {
                self.overlay = Some(Overlay::SessionSelect { selected: 0, filter: String::new() });
            }
            "models" | "model" => {
                self.overlay = Some(Overlay::ModelSelect { selected: self.model_sel.selected(), filter: String::new() });
            }
            "new" => {
                self.reset_session_state();
                if let Err(e) = self.create_new_session(tx.clone()) {
                    self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                }
            }
            "clear" => {
                // Disabled - users never want to clear conversation.
            }
            "help" => {
                self.which_key_panel = WhichKeyPanel::new();
                self.overlay = Some(Overlay::WhichKey);
            }
            "quit" => {
                self.quit = true;
            }
            "abort" => {
                if self.streaming && !self.aborting {
                    let session_id = self.session.state.session_id.clone();
                    let lease = self.session.state.lease.clone();
                    if let (Some(client), Some(holder)) = (&self.client, lease) {
                        let _ = client.abort(&session_id, &holder);
                        self.aborting = true;
                    }
                }
            }
            "compact" => {
                // Phase 4: POST /session/{id}/compact (lease required, engine at exec.rs:139).
                let sid = self.session.state.session_id.clone();
                let lease = self.session.state.lease.clone();
                if sid.is_empty() {
                    self.transcript.push(Message::new("system", "No active session to compact."));
                } else if let (Some(client), Some(holder)) = (&self.client, lease) {
                    match client.compact_session(&sid, &holder) {
                        Ok(seq) => self.transcript.push(Message::new("system", format!("Compacted (seq={})", seq))),
                        Err(e) => self.transcript.push(Message::new("system", format!("Compact failed: {}", e))),
                    }
                } else {
                    self.transcript.push(Message::new("system", "No lease — cannot compact (acquire lease first)."));
                }
            }
            "export" => {
                // Phase 4: GET /session/{id}/export (replaces placeholder).
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript.push(Message::new("system", "No active session to export."));
                } else if let Some(client) = &self.client {
                    // Support `/export <path>` args in raw input.
                    let raw = self.input.clone();
                    let arg_path = raw.trim_start_matches('/').trim_start_matches(cmd).trim();
                    match client.export_session(&sid) {
                        Ok(val) => {
                            let out_path = if arg_path.is_empty() {
                                format!("/tmp/kn9t-export-{}.json", &sid[..8.min(sid.len())])
                            } else {
                                arg_path.to_string()
                            };
                            let json = serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".into());
                            match std::fs::write(&out_path, json) {
                                Ok(_) => self.transcript.push(Message::new("system", format!("Exported to {}", out_path))),
                                Err(e) => self.transcript.push(Message::new("system", format!("Export write failed: {}", e))),
                            }
                        }
                        Err(e) => self.transcript.push(Message::new("system", format!("Export failed: {}", e))),
                    }
                }
            }
            "search" => {
                self.open_search();
            }
            "diff" => {
                // Run git diff and open diff viewer
                self.open_git_diff();
            }
            "keys" => {
                self.which_key_panel = WhichKeyPanel::new();
                self.overlay = Some(Overlay::WhichKey);
            }
            "palette" => {
                self.command_palette.open();
                self.overlay = Some(Overlay::CommandPalette);
            }
            "theme" => {
                // TODO: theme selector overlay
                self.transcript.push(Message::new("system",
                    "Theme selector is planned for a future release. \
                     Configure theme in ~/.kn9t/config.toml"));
            }
            "stash" => {
                self.stash_prompt();
            }
            "pop" | "unstash" | "stashpop" => {
                self.unstash_prompt();
            }
            "rename" => {
                // Parse `/rename <title>` args from raw input before it was cleared.
                let raw = self.input.clone();
                let title = raw.trim_start_matches('/').trim_start_matches("rename").trim();
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript.push(Message::new("system", "No active session to rename."));
                } else if title.is_empty() {
                    self.transcript.push(Message::new("system", "Usage: /rename <new title>"));
                } else if let Some(client) = &self.client {
                    match client.rename_session(&sid, title) {
                        Ok(_) => {
                            self.session.set_session_title(Some(title.to_string()));
                            if let Some(s) = self.session.sessions.iter_mut().find(|s| s.id == sid) {
                                s.name = title.to_string();
                            }
                            self.transcript.push(Message::new("system", format!("Renamed to '{}'", title)));
                        }
                        Err(e) => self.transcript.push(Message::new("system", format!("Rename failed: {}", e))),
                    }
                }
            }
            _ => {}
        }
    }

    fn execute_palette_command(&mut self, cmd_id: &str, tx: &Sender<Event>) {
        match cmd_id {
            // Navigation
            "scroll_up" => self.transcript.scroll_up(3),
            "scroll_down" => self.transcript.scroll_down(3),
            "page_up" => self.transcript.scroll_up(20),
            "page_down" => self.transcript.scroll_down(20),
            "jump_top" => self.transcript.scroll_top(),
            "jump_bottom" => self.transcript.scroll_bottom(),
            "prev_message" => self.transcript.scroll_up(10),
            "next_message" => self.transcript.scroll_down(10),
            
            // Session
            "new_session" => {
                self.reset_session_state();
                if let Err(e) = self.create_new_session(tx.clone()) {
                    self.transcript.push(Message::new("error", format!("Failed to create session: {}", e)));
                }
            }
            "session_list" => {
                self.overlay = Some(Overlay::SessionSelect { selected: 0, filter: String::new() });
            }
            "abort" => {
                if self.streaming && !self.aborting {
                    let session_id = self.session.state.session_id.clone();
                    let lease = self.session.state.lease.clone();
                    if let (Some(client), Some(holder)) = (&self.client, lease) {
                        let _ = client.abort(&session_id, &holder);
                        self.aborting = true;
                    }
                }
            }
            
            // Edit - these require cursor context, handled via keybinds
            "undo" | "redo" | "kill_line" | "kill_word" | "yank" => {
                // These require cursor context; user should use keybinds instead
            }
            
            // View
            "search" => {
                self.open_search();
            }
            "toggle_thinking" => {
                // Toggle all thinking blocks - expand if any collapsed, collapse if all expanded
                // For now, just expand all since we don't track count here
                self.thinking_state.expand_all();
            }
            "keybindings" => {
                self.which_key_panel = WhichKeyPanel::new();
                self.overlay = Some(Overlay::WhichKey);
            }
            "diff_viewer" => {
                self.open_git_diff();
            }
            
            // Tools
            "models" => {
                self.overlay = Some(Overlay::ModelSelect { selected: self.model_sel.selected(), filter: String::new() });
            }
            "compact" => {
                let sid = self.session.state.session_id.clone();
                let lease = self.session.state.lease.clone();
                if sid.is_empty() {
                    self.transcript.push(Message::new("system", "No active session to compact."));
                } else if let (Some(client), Some(holder)) = (&self.client, lease) {
                    match client.compact_session(&sid, &holder) {
                        Ok(seq) => self.transcript.push(Message::new("system", format!("Compacted (seq={})", seq))),
                        Err(e) => self.transcript.push(Message::new("system", format!("Compact failed: {}", e))),
                    }
                } else {
                    self.transcript.push(Message::new("system", "No lease — cannot compact."));
                }
            }
            "export" => {
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript.push(Message::new("system", "No active session to export."));
                } else if let Some(client) = &self.client {
                    match client.export_session(&sid) {
                        Ok(val) => {
                            let out_path = format!("/tmp/kn9t-export-{}.json", &sid[..8.min(sid.len())]);
                            let json = serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".into());
                            match std::fs::write(&out_path, json) {
                                Ok(_) => self.transcript.push(Message::new("system", format!("Exported to {}", out_path))),
                                Err(e) => self.transcript.push(Message::new("system", format!("Export write failed: {}", e))),
                            }
                        }
                        Err(e) => self.transcript.push(Message::new("system", format!("Export failed: {}", e))),
                    }
                }
            }
            
            // Settings
            "theme_toggle" => {
                // TODO: implement theme toggle
                self.transcript.push(Message::new("system",
                    "Theme toggle is planned for a future release."));
            }
            "quit" => {
                self.quit = true;
            }
            
            _ => {}
        }
    }

    /// Open diff viewer with current git diff.
    fn open_git_diff(&mut self) {
        use std::process::Command;
        
        // Phase 4 fix: use the session's cwd from snapshot (not env::current_dir which is the TUI process cwd).
        let cwd = self.session.state.cwd.as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        
        // Run git diff
        let output = Command::new("git")
            .args(["diff", "--no-color"])
            .current_dir(&cwd)
            .output();
        
        match output {
            Ok(out) if out.status.success() => {
                let diff_text = String::from_utf8_lossy(&out.stdout);
                if diff_text.trim().is_empty() {
                    // No changes, try staged
                    let staged = Command::new("git")
                        .args(["diff", "--cached", "--no-color"])
                        .current_dir(&cwd)
                        .output();
                    
                    match staged {
                        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                            let diff_text = String::from_utf8_lossy(&out.stdout);
                            let files = crate::diff_viewer::parse_unified_diff(&diff_text);
                            if files.is_empty() {
                                self.transcript.push(Message::new("system", "No changes to display."));
                            } else {
                                let viewer = crate::diff_viewer::DiffViewer::new(files);
                                self.diff_viewer = Some(viewer);
                            }
                        }
                        _ => {
                            self.transcript.push(Message::new("system", "No uncommitted changes."));
                        }
                    }
                } else {
                    let files = crate::diff_viewer::parse_unified_diff(&diff_text);
                    if files.is_empty() {
                        self.transcript.push(Message::new("system", "No changes to display."));
                    } else {
                        let viewer = crate::diff_viewer::DiffViewer::new(files);
                        self.diff_viewer = Some(viewer);
                    }
                }
            }
            Ok(_) => {
                self.transcript.push(Message::new("system", "Not a git repository or git command failed."));
            }
            Err(e) => {
                self.transcript.push(Message::new("system", &format!("Failed to run git: {}", e)));
            }
        }
    }
}
