//! Application state and main loop.
//!
//! Managers are composed as fields: `session` (SessionManager), `model_sel` (ModelSelector),
//! `tokens` (TokenTracker), `transcript` (Transcript).

use std::io;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::client::{spawn_attach_thread, AttachHandle, Client, ClientError};
use crate::config::Config;
use crate::event::{Event, EventLoop, TickControl};
use crate::input_history::{InputHistory, InputSnapshot};
use crate::keybind::{Action, Keybinds};
use crate::kill_ring::KillRing;
use crate::message_handler::Transcript;
use crate::model_selector::ModelSelector;
use crate::prompt_history::PromptHistory;
use crate::prompt_stash::PromptStash;
use crate::search::SearchState;
use crate::session_manager::SessionManager;
use crate::slash::{fuzzy_match, SlashState};
use crate::token_tracker::TokenTracker;
use crate::ui::render::render;
use crate::which_key::WhichKeyPanel;
use crate::wire::SseFrame;

// Re-export types from extracted modules for backward compatibility.
// The canonical definitions are in the submodules.
pub use crate::message_handler::{Message, ToolCard};
pub use crate::model_selector::ModelEntry;
pub use crate::session_manager::SessionEntry;

/// Tool info for sidebar and tools manager overlay.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub plugin: Option<String>,
    pub enabled: bool,
}

/// Overlay state.
#[derive(Debug, Clone)]
pub enum Overlay {
    Approval {
        tool: String,
        args: String,
        selected: usize,
    },
    /// generic interaction — payload is plugin's opaque shape, rendered generically.
    Interaction {
        id: u64,
        plugin: String,
        state: InteractionState,
    },
    Help,
    WhichKey,
    CommandPalette,
    ModelSelect {
        selected: usize,
        filter: String,
    },
    SessionSelect {
        selected: usize,
        filter: String,
    },
    /// Tools manager: enable/disable tools per session, grouped by plugin.
    ToolsManager {
        selected: usize,
        filter: String,
    },
    /// the session tree, drawn from the same `build_forest` the sidebar uses.
    /// `selected` indexes the flattened (depth-first) node list, not `sessions`, so
    /// moving down the view follows what is on screen rather than list order.
    SessionTree {
        selected: usize,
    },
}

/// Option for choice/multi questions.
#[derive(Debug, Clone)]
pub struct QuestionOption {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
}

/// State for different question types in interactions.
#[derive(Debug, Clone)]
pub enum InteractionState {
    /// Free-form text input.
    Text {
        question: String,
        header: Option<String>,
        placeholder: Option<String>,
        input: String,
    },
    /// Single choice selection.
    Choice {
        question: String,
        header: Option<String>,
        options: Vec<QuestionOption>,
        selected: usize,
        allow_custom: bool,
        custom_input: String,
        in_custom_mode: bool,
    },
    /// Multiple choice selection.
    Multi {
        question: String,
        header: Option<String>,
        options: Vec<QuestionOption>,
        cursor: usize,
        selected: Vec<bool>,
    },
    /// Yes/No confirmation.
    Confirm {
        question: String,
        header: Option<String>,
        selected: bool, // true = Yes, false = No
    },
    /// Fallback for unknown payload shapes.
    Generic { payload: String, input: String },
}

impl InteractionState {
    /// Parse a JSON payload into an InteractionState.
    pub fn from_payload(payload_str: &str) -> Self {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(payload_str) else {
            return Self::Generic {
                payload: payload_str.to_string(),
                input: String::new(),
            };
        };

        let obj = match v.as_object() {
            Some(o) => o,
            None => {
                return Self::Generic {
                    payload: payload_str.to_string(),
                    input: String::new(),
                }
            }
        };

        let question = obj
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let header = obj.get("header").and_then(|v| v.as_str()).map(String::from);
        let qtype = obj.get("type").and_then(|v| v.as_str()).unwrap_or("text");

        match qtype {
            "text" => Self::Text {
                question,
                header,
                placeholder: obj
                    .get("placeholder")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                input: obj
                    .get("default")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            },
            "choice" => {
                let options = Self::parse_options(obj.get("options"));
                Self::Choice {
                    question,
                    header,
                    options,
                    selected: 0,
                    allow_custom: obj
                        .get("allow_custom")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    custom_input: String::new(),
                    in_custom_mode: false,
                }
            }
            "multi" => {
                let options = Self::parse_options(obj.get("options"));
                let count = options.len();
                Self::Multi {
                    question,
                    header,
                    options,
                    cursor: 0,
                    selected: vec![false; count],
                }
            }
            "confirm" => Self::Confirm {
                question,
                header,
                selected: obj.get("default").and_then(|v| v.as_bool()).unwrap_or(true),
            },
            // Unknown `type`: the plugin (kn9t-ask-user) normalizes the legacy
            // `{question, choices}` shape, so a second decoder here would be the duplication
            // AGENTS.md §10 forbids.
            _ => {
                if !question.is_empty() {
                    Self::Text {
                        question,
                        header,
                        placeholder: obj
                            .get("placeholder")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        input: String::new(),
                    }
                } else {
                    Self::Generic {
                        payload: payload_str.to_string(),
                        input: String::new(),
                    }
                }
            }
        }
    }

    fn parse_options(val: Option<&serde_json::Value>) -> Vec<QuestionOption> {
        let Some(arr) = val.and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|item| {
                if let Some(s) = item.as_str() {
                    Some(QuestionOption {
                        label: s.to_string(),
                        value: s.to_string(),
                        description: None,
                    })
                } else if let Some(obj) = item.as_object() {
                    let label = obj.get("label").and_then(|v| v.as_str())?.to_string();
                    let value = obj
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| label.clone());
                    let description = obj
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    Some(QuestionOption {
                        label,
                        value,
                        description,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the response value for this interaction state.
    pub fn response_value(&self) -> serde_json::Value {
        match self {
            Self::Text { input, .. } => serde_json::json!({"value": input}),
            Self::Choice {
                options,
                selected,
                in_custom_mode,
                custom_input,
                allow_custom,
                ..
            } => {
                if *in_custom_mode && *allow_custom {
                    serde_json::json!({"value": custom_input})
                } else if let Some(opt) = options.get(*selected) {
                    serde_json::json!({"value": opt.value})
                } else {
                    serde_json::json!({"value": null})
                }
            }
            Self::Multi {
                options, selected, ..
            } => {
                let values: Vec<&str> = options
                    .iter()
                    .zip(selected.iter())
                    .filter(|(_, &sel)| sel)
                    .map(|(opt, _)| opt.value.as_str())
                    .collect();
                serde_json::json!({"value": values})
            }
            Self::Confirm { selected, .. } => {
                serde_json::json!({"value": *selected})
            }
            Self::Generic { input, .. } => serde_json::from_str::<serde_json::Value>(input)
                .map(|v| serde_json::json!({"value": v}))
                .unwrap_or_else(|_| serde_json::json!({"value": input})),
        }
    }
}

/// Current screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Welcome,
    Chat,
}

/// A prompt queued for later sending (during plugins loading or streaming).
#[derive(Debug, Clone)]
pub struct QueuedPrompt {
    pub text: String,
    pub images: Vec<String>,
}

/// Hit area for tool card click detection.
#[derive(Debug, Clone)]
pub struct ToolHitArea {
    pub call_id: String,
    /// Call ids this area owns when it is a turn's group header, empty otherwise. A click
    /// resolves both the same way, and a header is just an area naming several calls (PLAN §P7 D11).
    pub group_calls: Vec<String>,
    pub header_y: u16,              // Y position of header line
    pub content_y_start: u16,       // Y start of content area (tabs + output/input)
    pub content_y_end: u16,         // Y end of content area
    pub x_start: u16,               // X start of card (for scroll detection)
    pub x_end: u16,                 // X end of card (for scroll detection)
    pub progress_tab_x: (u16, u16), // X range for Progress tab
    pub output_tab_x: (u16, u16),   // X range for Output tab
    pub input_tab_x: (u16, u16),    // X range for Input tab
}

impl ToolHitArea {
    /// An area that toggles every call in a turn.
    pub fn group(calls: Vec<String>, y: u16, x_start: u16, x_end: u16) -> Self {
        Self {
            call_id: String::new(),
            group_calls: calls,
            header_y: y,
            content_y_start: y,
            content_y_end: y + 1,
            x_start,
            x_end,
            progress_tab_x: (0, 0),
            output_tab_x: (0, 0),
            input_tab_x: (0, 0),
        }
    }
}

/// Hit area for a reasoning card header, toggled on click.
#[derive(Debug, Clone)]
pub struct ThinkingHitArea {
    /// Index of the message in the transcript.
    pub msg_idx: usize,
    /// Index of the card within that message.
    pub card_idx: usize,
    /// Screen Y of the header line.
    pub y: u16,
    pub x_start: u16,
    pub x_end: u16,
}

/// Hit area for one explorer row, so a click selects it and a directory toggles (D4).
#[derive(Debug, Clone)]
pub struct ExplorerHit {
    /// Path relative to the index root — the key the row is looked up by.
    pub path: String,
    pub is_dir: bool,
    /// Screen Y of the row.
    pub y: u16,
    pub x_start: u16,
    pub x_end: u16,
}

/// Main app state.
pub struct App {
    pub config: Config,
    pub client: Option<Client>,
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
    /// The workspace file index — one walk feeding the explorer tree, the file viewer and
    /// the `@` dropdown (PLAN §P7 L2 / D6). Refreshed at most once per session cwd.
    pub file_index: crate::file_index::FileIndex,
    /// `@path` mention completion (PLAN §P7 L2 / D19).
    pub mention: crate::mention::MentionState,
    /// The file explorer column (PLAN §P7 L2 / D3), a native view over `file_index`.
    pub explorer: crate::explorer::ExplorerState,
    /// The read-only file viewer (PLAN §P7 L2 / D4/D5), opened from the explorer.
    pub viewer: Option<crate::viewer::ViewerState>,
    /// Row hit areas recorded while drawing the explorer, for click-to-select.
    pub explorer_hit_areas: Vec<ExplorerHit>,
    /// Rect the explorer was drawn in, for mouse hit-testing.
    pub explorer_area: Option<(u16, u16, u16, u16)>,
    /// Rect the viewer was drawn in, for mouse scrolling.
    pub viewer_area: Option<(u16, u16, u16, u16)>,
    /// Live watch over the workspace root; flags a rebuild when a file appears, changes or is
    /// removed. `None` when the platform watcher is unavailable (the index is then a snapshot,
    /// as it was before). See `crate::workspace_watch`.
    workspace_watcher: Option<crate::workspace_watch::WorkspaceWatcher>,
    /// Root the watcher was started for, recorded even when `spawn` failed so a broken watcher
    /// is not retried on every turn.
    workspace_watch_root: Option<std::path::PathBuf>,

    /// Tools available this session, as reported by `GET /tools`.
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

    // Steering buffer — injected into the current/next turn.
    // When streaming: added as steer messages to the current turn.
    // When plugins loading: sent as first message(s) when ready.
    pub steering: Vec<QueuedPrompt>,

    // Queue buffer — sent sequentially, one per turn.
    // Each message triggers a new turn after the previous one completes.
    pub queue: std::collections::VecDeque<QueuedPrompt>,

    // State.
    pub streaming: bool,
    pub aborting: bool,     // True when abort requested, waiting for TurnEnded
    pub turn_phase: String, // idle|thinking|streaming|tool|retrying|failed|aborted — syncs server state machine
    pub turn_status_msg: String, // detail from TurnStatus
    pub spinner_frame: usize,
    pub phrase_idx: usize,
    pub overlay: Option<Overlay>,
    pub active_approval_id: Option<u64>,
    /// active generic interaction id (if any) — analogous to approval but opaque.
    pub active_interaction_id: Option<u64>,
    pub slash: SlashState,
    pub quit: bool,
    pub theme_mode: String, // "light" or "dark"

    // Tool mode state (UI-local).
    pub tool_mode: bool,
    pub focused_tool: Option<String>,     // call_id of focused tool
    pub tool_hit_areas: Vec<ToolHitArea>, // Click detection areas from last render
    pub thinking_hit_areas: Vec<ThinkingHitArea>, // Reasoning headers, same idea

    // Scrollbar hit area for transcript (x, y_start, y_end, total_lines, visible_lines).
    pub scrollbar_area: Option<(u16, u16, u16, usize, usize)>,
    // Scrollbar drag state: if Some, we're dragging from this Y position.
    scrollbar_dragging: bool,

    // Search state (None = search bar closed).
    pub search_state: Option<SearchState>,

    // Which-key panel state.
    pub which_key_panel: WhichKeyPanel,

    // Command palette state.
    pub command_palette: crate::command_palette::CommandPalette,

    // Structured UI directives (session-scoped, transport only).
    pub ui_directives: Vec<(String, String, String, serde_json::Value)>,
    // collapsible subagent sub-entries
    pub subagents: Vec<crate::reducer::SubagentEntry>,
    pub attached_subagent: Option<(String, Vec<crate::wire::TranscriptMessage>)>,

    // Keybinds.
    keybinds: Keybinds,
    tick_ctl: TickControl,
    term_width: u16,

    /// Transcript rect as actually drawn last frame, so hit-testing follows the real geometry
    /// even when Lua places it.
    pub transcript_area: Option<ratatui::layout::Rect>,
    /// Input width as drawn last frame; feeds `input_height_for`, keeping the published
    /// `ctx.input_height` in step with the layout.
    pub input_width: Option<u16>,
    /// `(id, rect)` for every `id="..."` widget in last frame's `render_ui()`, in paint order.
    /// Floats are recorded separately (below) and hit-tested first.
    pub lua_click_areas: Vec<(String, ratatui::layout::Rect)>,
    /// As `lua_click_areas`, for `kn9t.register_panel` floats: they paint on top, so they
    /// hit-test first.
    pub lua_panel_click_areas: Vec<(String, ratatui::layout::Rect)>,
    /// `(plugin, id, rect)` for widgets inside a plugin view, recorded by `render_plugin_views`.
    /// Separate from `lua_click_areas` because registries are keyed by `(plugin, id)`.
    pub plugin_click_areas: Vec<(String, String, ratatui::layout::Rect)>,
    /// `(plugin, rect)` for each plugin view drawn last frame; clicking inside focuses it.
    pub plugin_view_areas: Vec<(String, ratatui::layout::Rect)>,
    /// Plugin view currently receiving keys. Focus is required so a plugin cannot swallow
    /// global keys just by being visible.
    pub focused_plugin: Option<String>,

    /// Lua key handlers, refreshed on every config (re)load.
    pub lua_keymaps: crate::lua::keymap::KeymapRegistry,
    /// Lua click handlers, keyed by widget `id`. See `lua::click`.
    pub lua_clicks: crate::lua::click::ClickRegistry,
    /// Commands registered via `kn9t.register_command`. See `lua::commands`.
    pub lua_commands: crate::lua::commands::LuaCommandRegistry,

    // Render cache for transcript (avoids re-parsing markdown on every frame).
    pub render_cache: crate::render_cache::RenderCache,

    // Lua customization runtime (optional, initialized if config file exists).
    pub lua_runtime: Option<std::sync::Arc<crate::lua::LuaRuntime>>,
    // Watcher handle kept alive to continue hot-reload.
    #[allow(dead_code)]
    lua_watcher: Option<crate::lua::WatcherHandle>,
    /// Second watcher, only used when the config is a `tui/` directory
    /// rather than a single `tui.lua`. Kept alive the same way.
    #[allow(dead_code)]
    lua_dir_watcher: Option<crate::lua::WatcherHandle>,

    // Dynamic panel registry for Lua-registered panels.
    pub lua_panels: crate::lua::panels::PanelRegistry,
    // Input states for Lua Input widgets (id -> current value).
    pub lua_input_states: std::collections::HashMap<String, String>,

    /// True when server plugins are fully loaded. While false, session creation
    /// and prompts are blocked with a user-visible message.
    pub plugins_ready: bool,
}

impl App {
    pub fn new(config: Config, tick_ctl: TickControl) -> Self {
        let keybinds = Keybinds::new(&config.keybinds);
        let theme_mode = config.theme.get_mode().to_string();

        Self {
            config,
            client: None,
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
            steering: Vec::new(),
            queue: std::collections::VecDeque::new(),
            ui_directives: Vec::new(),
            subagents: Vec::new(),
            attached_subagent: None,
            streaming: false,
            aborting: false,
            turn_phase: "idle".into(),
            turn_status_msg: String::new(),
            spinner_frame: 0,
            phrase_idx: 0,
            overlay: None,
            active_approval_id: None,
            active_interaction_id: None,
            slash: SlashState::new(),
            file_index: crate::file_index::FileIndex::new(),
            mention: crate::mention::MentionState::new(),
            explorer: crate::explorer::ExplorerState::new(),
            viewer: None,
            explorer_hit_areas: Vec::new(),
            explorer_area: None,
            viewer_area: None,
            workspace_watcher: None,
            workspace_watch_root: None,
            quit: false,
            theme_mode,
            tool_mode: false,
            focused_tool: None,
            tool_hit_areas: Vec::new(),
            thinking_hit_areas: Vec::new(),
            scrollbar_area: None,
            scrollbar_dragging: false,
            search_state: None,
            which_key_panel: WhichKeyPanel::new(),
            command_palette: crate::command_palette::CommandPalette::new(),
            keybinds,
            tick_ctl,
            term_width: 80,
            transcript_area: None,
            input_width: None,
            lua_click_areas: Vec::new(),
            lua_panel_click_areas: Vec::new(),
            plugin_click_areas: Vec::new(),
            plugin_view_areas: Vec::new(),
            focused_plugin: None,
            lua_keymaps: crate::lua::keymap::KeymapRegistry::new(),
            lua_clicks: crate::lua::click::ClickRegistry::new(),
            lua_commands: crate::lua::commands::LuaCommandRegistry::new(),
            render_cache: crate::render_cache::RenderCache::new(),
            lua_runtime: None,
            lua_watcher: None,
            lua_dir_watcher: None,
            lua_panels: crate::lua::panels::PanelRegistry::new(),
            lua_input_states: std::collections::HashMap::new(),
            plugins_ready: false,
        }
    }

    /// Initialize the Lua runtime and start hot-reload watcher.
    ///
    /// Should be called after creating App but before the main loop.
    pub fn init_lua(&mut self, event_tx: std::sync::mpsc::Sender<Event>) {
        use std::sync::Arc;

        let Some(config_path) = crate::lua::default_config_path() else {
            crate::log!("Lua: could not determine config path");
            return;
        };
        let config_dir = crate::lua::default_config_dir();

        // Ensure the tui/ directory has config files, copying defaults if empty.
        // This makes the config immediately editable by the user.
        if let Some(ref dir) = config_dir {
            if let Err(e) = crate::lua::default_config::ensure_tui_config(dir) {
                crate::log!("Lua: failed to initialize tui config: {}", e);
            }
        }

        // Ship a working UI with zero setup: the built-in Lua below is the
        // baseline, and the user file (if any) only overrides parts of it.

        let runtime = match crate::lua::LuaRuntime::new() {
            Ok(rt) => Arc::new(rt),
            Err(e) => {
                crate::log!("Lua: failed to create runtime: {}", e);
                return;
            }
        };

        // Publish the palette and native-view list before the config
        // runs, so it can reference `kn9t.theme.user` at load time.
        runtime.install_environment(&self.config.theme);

        // Load the user's config from `~/.kn9t/tui/`. The directory was
        // populated with defaults by ensure_tui_config above if empty.
        if let Some(dir) = &config_dir {
            runtime.load_dir(dir);
        }

        // Process any panels registered during load
        runtime.process_panels(&mut self.lua_panels);

        // Apply keymaps declared at load time.
        runtime.drain_keymaps(&mut self.lua_keymaps);
        // Apply click handlers declared at load time.
        runtime.drain_clicks(&mut self.lua_clicks);
        // Apply palette/slash commands declared at load time.
        runtime.drain_commands(&mut self.lua_commands);

        // Start watcher(s) for hot-reload: the single file always (in case a
        // user switches to it later), plus the directory when one is in use.
        let watcher = crate::lua::spawn_watcher(config_path, runtime.clone(), event_tx.clone());
        if watcher.is_none() {
            crate::log!("Lua: failed to start config file watcher");
        }
        self.lua_watcher = watcher;

        if let Some(dir) = config_dir {
            self.lua_dir_watcher = crate::lua::spawn_dir_watcher(dir, runtime.clone(), event_tx);
        }

        self.lua_runtime = Some(runtime);

        crate::log!("Lua: runtime initialized");
    }

    /// Process Lua panel updates (called on each frame or event).
    pub fn process_lua_panels(&mut self) {
        if let Some(ref runtime) = self.lua_runtime {
            let before = self.lua_panels.len();
            runtime.process_panels(&mut self.lua_panels);
            let after = self.lua_panels.len();
            if before != after {
                crate::log!(
                    "Lua panels: {} -> {} (visible: {})",
                    before,
                    after,
                    self.lua_panels.visible().count()
                );
            }

            // Same cadence for keymaps, so `kn9t.map` picks up on hot-reload.
            let applied = runtime.drain_keymaps(&mut self.lua_keymaps);
            if applied > 0 {
                crate::log!(
                    "Lua keymaps: {} applied ({} bound)",
                    applied,
                    self.lua_keymaps.len()
                );
            }

            // Same cadence for click handlers, so `kn9t.on_click` picks up on
            // hot-reload without needing a restart.
            let click_applied = runtime.drain_clicks(&mut self.lua_clicks);
            if click_applied > 0 {
                crate::log!(
                    "Lua click handlers: {} applied ({} bound)",
                    click_applied,
                    self.lua_clicks.len()
                );
            }

            // Same cadence for palette/slash commands.
            let cmd_applied = runtime.drain_commands(&mut self.lua_commands);
            if cmd_applied > 0 {
                crate::log!(
                    "Lua commands: {} applied ({} bound)",
                    cmd_applied,
                    self.lua_commands.len()
                );
            }
        }
    }

    /// Get the last Lua error, if any (for status line display).
    pub fn lua_error(&self) -> Option<String> {
        self.lua_runtime.as_ref().and_then(|rt| rt.last_error())
    }

    /// Connect and load the session list + models for the welcome screen. Non-blocking: if
    /// plugins are still loading, `plugins_ready = false` and the main loop polls.
    pub fn connect(&mut self) -> Result<(), ClientError> {
        let client = Client::new(&self.config.base_url, self.config.token.as_deref());

        // Check if plugins are ready (non-blocking)
        self.plugins_ready = client.check_plugins_ready();

        // Load session list for welcome screen (works even during loading).
        self.session.load_sessions(&client)?;

        // Load available models (auto-discovered from connected providers).
        let _ = self.model_sel.load_models(&client);

        // Load tools from server - may be empty/partial if plugins still loading.
        self.refresh_tools(&client);

        // Start global attach thread to keep server alive.
        self.attach_handle = Some(spawn_attach_thread(
            self.config.base_url.clone(),
            self.config.token.clone(),
        ));

        self.client = Some(client);
        Ok(())
    }

    /// Poll server to check if plugins are ready. Called periodically from main loop.
    /// Returns true if state changed (for forcing redraw).
    pub fn poll_plugins_ready(&mut self, tx: &Sender<Event>) -> bool {
        if self.plugins_ready {
            return false;
        }
        let is_ready = self
            .client
            .as_ref()
            .map(|c| c.check_plugins_ready())
            .unwrap_or(false);

        if is_ready {
            self.plugins_ready = true;
            // Refresh tools now that all plugins are loaded
            if let Some(client) = self.client.take() {
                self.refresh_tools(&client);
                self.client = Some(client);
            }
            crate::log!("plugins ready, refreshed tools");

            // Process any steering/queue that was waiting for plugins.
            self.process_pending_on_ready(tx);
            return true; // State changed, force redraw
        }
        false
    }

    /// Process steering and queue buffers when plugins become ready.
    /// Sends all steering as a single fused message, then triggers the first queue item.
    fn process_pending_on_ready(&mut self, tx: &Sender<Event>) {
        // Nothing to do if both buffers are empty.
        if self.steering.is_empty() && self.queue.is_empty() {
            return;
        }

        // Need a session. If none, create one.
        if self.session.state.session_id.is_empty() {
            if let Err(e) = self.create_new_session(tx.clone()) {
                crate::log!("PENDING: failed to create session: {:?}", e);
                return;
            }
        }

        // Send all steering messages fused into one prompt.
        if !self.steering.is_empty() {
            let steering = std::mem::take(&mut self.steering);
            let fused_text: String = steering
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let fused_images: Vec<String> = steering.into_iter().flat_map(|p| p.images).collect();

            let session_id = self.session.state.session_id.clone();
            let lease = self.session.state.lease.clone();
            if let (Some(client), Some(holder)) = (&self.client, lease) {
                let image_count = fused_images.len();
                self.transcript
                    .push(Message::with_images("user", &fused_text, image_count));
                match client.prompt(&session_id, &holder, &fused_text, fused_images) {
                    Ok(_) => {
                        crate::log!("PENDING: sent {} steering messages", image_count);
                        self.streaming = true;
                        self.tick_ctl.set_streaming(true);
                    }
                    Err(e) => {
                        crate::log!("PENDING: failed to send steering: {:?}", e);
                        self.transcript
                            .push(Message::new("error", format!("Failed to send: {}", e)));
                    }
                }
            }
            return; // Queue will be processed on TurnEnded.
        }

        // No steering, but queue has items — send the first one.
        self.process_next_queue_item();
    }

    /// Refresh tools sidebar from server (GET /tools?session= — Phase 4).
    pub fn refresh_tools(&mut self, client: &Client) {
        let session_id = if self.session.state.session_id.is_empty() {
            None
        } else {
            Some(self.session.state.session_id.as_str())
        };
        match client.get_tools(session_id) {
            Ok(entries) => {
                self.tools = entries;
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
        self.session
            .reset_state(self.client.as_ref(), &self.tick_ctl);

        // Clear transcript.
        self.transcript.clear();

        // Clear metrics.
        self.tokens.reset();

        // Reset streaming state.
        self.streaming = false;
        self.aborting = false;
        self.turn_phase = "idle".into();
        self.turn_status_msg.clear();
        self.tick_ctl.set_streaming(false);

        // Clear any pending UI state.
        self.overlay = None;
        self.active_approval_id = None;
        self.active_interaction_id = None;

        // Clear steering and queue (don't carry over to new session).
        self.steering.clear();
        self.queue.clear();
        self.ui_directives.clear();
        self.subagents.clear();
        self.attached_subagent = None;

        // Clear tool mode state.
        self.tool_mode = false;
        self.focused_tool = None;
        self.tool_hit_areas.clear();
        self.thinking_hit_areas.clear();

        // Clear render cache (new session = new content).
        self.render_cache.clear();
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tool mode helpers
    // ═══════════════════════════════════════════════════════════════════════

    /// Find all tool call_ids in order (for navigation).
    pub fn tool_ids(&self) -> Vec<String> {
        self.transcript
            .messages()
            .iter()
            .flat_map(|m| m.tools.iter())
            .map(|t| t.call_id.clone())
            .collect()
    }

    /// Get mutable reference to tool by call_id.
    pub fn tool_mut(&mut self, call_id: &str) -> Option<&mut ToolCard> {
        self.transcript
            .messages_mut()
            .iter_mut()
            .flat_map(|m| m.tools.iter_mut())
            .find(|t| t.call_id == call_id)
    }

    /// Toggle expand/collapse for a tool.
    pub fn toggle_tool_expand(&mut self, call_id: &str) {
        if let Some(tool) = self.tool_mut(call_id) {
            tool.expanded = !tool.expanded;
            if tool.expanded {
                tool.scroll_offset = 0; // Reset scroll on expand
            }
        }
    }

    /// Expand or collapse every card of a turn at once (PLAN §P7 D11). Openness is derived
    /// from the owned cards, so collapsing one by hand cannot leave the header claiming otherwise.
    pub fn toggle_tool_group(&mut self, calls: &[String]) {
        let any_open = calls.iter().any(|id| {
            self.transcript
                .messages()
                .iter()
                .flat_map(|m| m.tools.iter())
                .find(|t| &t.call_id == id)
                .map(|t| t.expanded)
                .unwrap_or(false)
        });
        let open = !any_open;
        for id in calls {
            if let Some(tool) = self.tool_mut(id) {
                tool.expanded = open;
                if open {
                    tool.scroll_offset = 0;
                }
            }
        }
    }

    /// toggle collapse for a subagent sub-entry.
    pub fn toggle_subagent(&mut self, call_id: &str) {
        if let Some(entry) = self.subagents.iter_mut().find(|e| e.call_id == call_id) {
            entry.collapsed = !entry.collapsed;
        }
    }

    /// attach — fetch subagent's full transcript via session_read (host_api).
    pub fn attach_subagent(&mut self, call_id: &str) {
        // If already attached to this one, detach.
        if self
            .attached_subagent
            .as_ref()
            .map(|(id, _)| id == call_id)
            .unwrap_or(false)
        {
            self.attached_subagent = None;
            return;
        }
        // Find the subagent's own session id; without it there is nothing to fetch.
        let session_opt = self
            .subagents
            .iter()
            .find(|e| e.call_id == call_id)
            .and_then(|e| e.session_id.clone());
        if let (Some(client), Some(sess)) = (&self.client, session_opt) {
            // Try GET /session/{id} (transcript fetch)
            if let Ok(detail) = client.get_session(&sess) {
                self.attached_subagent = Some((call_id.to_string(), detail.transcript));
                return;
            }
        }
        // Fallback: attach with empty transcript (still shows the sub-entry header)
        self.attached_subagent = Some((call_id.to_string(), vec![]));
    }

    /// Switch tab for a tool.
    pub fn switch_tool_tab(&mut self, call_id: &str, tab: crate::message_handler::ToolTab) {
        if let Some(tool) = self.tool_mut(call_id) {
            tool.active_tab = tab;
            tool.scroll_offset = 0; // Reset scroll on tab switch
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
            tool.scroll_offset = 0; // Reset scroll on tab switch
        }
    }

    /// Navigate to next/prev tool in tool mode.
    pub fn navigate_tool(&mut self, forward: bool) {
        let ids = self.tool_ids();
        if ids.is_empty() {
            return;
        }

        let current_idx = self
            .focused_tool
            .as_ref()
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
            if let Some(tool) = self.tool_mut(call_id) {
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
                // Use a reasonable default for scroll calculation
                // The actual visible lines depends on terminal height (handled in render)
                let visible_lines = 50; // Max visible when terminal is tall
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

    /// Switch to an existing session by id. Shared by the session-select overlay and
    /// `kn9t.action("switch_session")`, so a Lua-driven list cannot drift from the built-in path.
    pub fn switch_to_session(&mut self, session_id: &str, tx: Sender<Event>) {
        crate::log!(
            "SESSION SWITCH: -> {}",
            &session_id[..8.min(session_id.len())]
        );
        self.reset_session_state();
        if let Err(e) = self.enter_session(session_id, tx) {
            self.transcript.push(Message::new(
                "error",
                format!("Failed to switch session: {}", e),
            ));
        }
    }

    pub fn enter_session(
        &mut self,
        session_id: &str,
        tx: Sender<Event>,
    ) -> Result<(), ClientError> {
        let client = self
            .client
            .as_ref()
            .ok_or(ClientError::Http("not connected".into()))?;

        self.session.set_session_id(session_id.to_string());
        // Get title from sessions list (if available).
        let title = self
            .session
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.name.clone());
        self.session.set_session_title(title);

        // Try to acquire lease.
        let lease_result = self.session.acquire_lease(client, session_id)?;
        if let Some(ref holder) = lease_result {
            // Send initial model to session (so server uses our selected model).
            if let Some(model) = self.model_sel.current_model() {
                crate::log!(
                    "enter_session: setting initial model {}:{}",
                    model.provider,
                    model.id
                );
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
                let transcript_values: Vec<serde_json::Value> = detail
                    .transcript
                    .iter()
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
                crate::log!(
                    "Loaded {} messages with {} total tools, head_seq={}",
                    messages.len(),
                    tool_count,
                    detail.head_seq
                );

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
        crate::log!(
            "enter_session: starting SSE for {} from_seq={}",
            session_id,
            self.session.state.last_seq
        );
        self.session
            .start_sse(&self.config, session_id, self.session.state.last_seq, tx);

        // Switch to chat screen.
        self.screen = Screen::Chat;

        // Mark this session as active in the list.
        self.session.mark_active(session_id);

        // If we have pending steering/queue and agent is idle, send now.
        if !self.streaming && self.has_pending_messages() {
            self.process_next_queue_item();
        }

        Ok(())
    }

    /// `/fork [origin_seq]` and `/undo [n]` are one `POST /session/{id}/fork` differing only in
    /// `reason`/`origin_seq`; the log is append-only, so "remove the last message" is a branch that
    /// stops short of it (`reason: rewind`). The switch is deliberately silent.
    fn handle_fork_command(&mut self, cmd: &str, args: &str, tx: Sender<Event>) {
        let sid = self.session.state.session_id.clone();
        if sid.is_empty() {
            self.transcript
                .push(Message::new("system", "No active session."));
            return;
        }
        // `last_seq` tracks the head this client has actually seen, which is what the user
        // is looking at — more truthful here than the possibly staler value in the list.
        let head = self
            .session
            .head_seq_of(&sid)
            .unwrap_or(0)
            .max(self.session.state.last_seq);

        let (reason, origin_seq, note) = match crate::session_tree::plan_fork(cmd, args, head) {
            Ok(plan) => (plan.reason, plan.origin_seq, plan.note),
            Err(msg) => {
                self.transcript.push(Message::new("system", msg));
                return;
            }
        };

        let Some(client) = self.client.as_ref() else {
            self.transcript
                .push(Message::new("error", "Not connected."));
            return;
        };
        let new_id = match self.session.fork_into(client, &sid, origin_seq, reason) {
            Ok(id) => id,
            Err(e) => {
                self.transcript
                    .push(Message::new("error", format!("/{cmd} failed: {e}")));
                return;
            }
        };

        // Same path as `/new` and the session picker: reset, then enter. The new session
        // replays its own (shorter) transcript, so the screen ends up showing the rewound
        // history rather than the one we were on.
        self.switch_to_session(&new_id, tx);
        self.transcript.push(Message::new("system", note));
    }

    pub fn create_new_session(&mut self, tx: Sender<Event>) -> Result<(), ClientError> {
        if !self.plugins_ready {
            return Err(ClientError::ServerLoading);
        }

        crate::log!("create_new_session: starting...");
        let client = self
            .client
            .as_ref()
            .ok_or(ClientError::Http("not connected".into()))?;

        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".into());
        crate::log!(
            "create_new_session: calling server create_session cwd={}",
            cwd
        );
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

        let mut needs_redraw = true;

        loop {
            // Only render when needed (skip redundant redraws on Tick when not streaming)
            if needs_redraw {
                // Render with CSI 2026 synchronized update (flicker-free).
                let _ = execute!(terminal.backend_mut(), BeginSynchronizedUpdate);
                terminal.draw(|f| {
                    self.term_width = f.area().width;
                    render(f, self);
                })?;
                let _ = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
            }

            if self.quit {
                break;
            }

            // Block on next event.
            let Some(event) = event_loop.recv() else {
                break;
            };

            // Assume we need to redraw, unless it's just a tick with nothing happening
            needs_redraw = true;

            // Handle event.
            match event {
                Event::Key(key) => self.handle_key(key, &tx),
                Event::Mouse(mouse) => self.handle_mouse(mouse, &tx),
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
                        crate::log!(
                            "SSE: ignoring event from old session {} (current={})",
                            &session_id[..8.min(session_id.len())],
                            &self.session.state.session_id
                                [..8.min(self.session.state.session_id.len())]
                        );
                        needs_redraw = false; // Stale event, no need to redraw
                    }
                }
                Event::Tick => {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                    if self.spinner_frame.is_multiple_of(12) {
                        self.phrase_idx = self.phrase_idx.wrapping_add(1);
                    }
                    // Poll for plugins ready (every ~500ms = every 5 ticks at 100ms)
                    let mut plugins_just_ready = false;
                    if !self.plugins_ready && self.spinner_frame.is_multiple_of(5) {
                        plugins_just_ready = self.poll_plugins_ready(&tx);
                    }
                    // Process Lua panel commands (show/hide/toggle/register)
                    self.process_lua_panels();
                    // Redraw while streaming, Lua panels are visible, or plugin-ready changed.
                    needs_redraw = self.streaming
                        || !self.lua_panels.is_empty()
                        || !self.plugins_ready
                        || plugins_just_ready;
                }
                Event::SseError(session_id, e) => {
                    // Phase 4 fix: R-TUI-230 — reconnect from last_seq instead of lying.
                    if session_id == self.session.state.session_id {
                        crate::log!(
                            "SSE error: {} — reconnecting from seq {}",
                            e,
                            self.session.state.last_seq
                        );
                        // Show transient reconnection message in transcript (deduplicate).
                        if !self
                            .transcript
                            .messages()
                            .iter()
                            .rev()
                            .take(1)
                            .any(|m| m.role == "system" && m.content.contains("reconnecting"))
                        {
                            self.transcript.push(Message::new(
                                "system",
                                format!("Connection interrupted, reconnecting... ({})", e),
                            ));
                        }
                        // Reconnect: stop old handle (already dead) and start new from last_seq.
                        if !session_id.is_empty() {
                            self.session.start_sse(
                                &self.config,
                                &session_id,
                                self.session.state.last_seq,
                                tx.clone(),
                            );
                        }
                    }
                }
            }

            // Drain any additional events — handle ALL event types, not just SSE/Tick.
            for ev in event_loop.drain() {
                match ev {
                    Event::Key(key) => self.handle_key(key, &tx),
                    Event::Mouse(mouse) => self.handle_mouse(mouse, &tx),
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
                            crate::log!(
                                "SSE error: {} — reconnecting from seq {}",
                                e,
                                self.session.state.last_seq
                            );
                            if !self
                                .transcript
                                .messages()
                                .iter()
                                .rev()
                                .take(1)
                                .any(|m| m.role == "system" && m.content.contains("reconnecting"))
                            {
                                self.transcript.push(Message::new(
                                    "system",
                                    format!("Connection interrupted, reconnecting... ({})", e),
                                ));
                            }
                            if !session_id.is_empty() {
                                self.session.start_sse(
                                    &self.config,
                                    &session_id,
                                    self.session.state.last_seq,
                                    tx.clone(),
                                );
                            }
                        }
                    }
                }
            }

            // One place, once per turn: the index, the explorer tree and the `@` dropdown all
            // read the same walk (PLAN §P7 L2 / D6). Doing it here rather than in the render
            // path keeps "the input/index changed" and "the views changed" together. A live
            // rebuild must also repaint, since an idle `Tick` turn skips the redraw.
            needs_redraw |= self.sync_index_views();
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
        crate::log!(
            "KEY: {:?} mods={:?} screen={:?} overlay={:?} plugin={:?} slash={}",
            key.code,
            key.modifiers,
            self.screen,
            self.overlay.is_some(),
            self.focused_plugin.as_deref(),
            self.slash.active
        );

        // Overlay handling.
        if self.overlay.is_some() {
            crate::log!("  -> overlay handler");
            self.handle_overlay_key(key, tx);
            return;
        }

        // A plugin-rendered interaction has no overlay, so Esc cancels it here.
        if self.active_interaction_id.is_some() && key.code == KeyCode::Esc {
            self.respond_interaction(serde_json::json!({"cancelled": true}));
            return;
        }

        // Explorer owns the keyboard while focused (PLAN §P7 L2 / D3), after overlays; F1/F3
        // fall through to the keybind matcher, arrows/Enter never reach the prompt.
        if self.explorer.is_focused() && self.handle_explorer_key(key) {
            return;
        }

        // The viewer owns the keyboard while focused (D4: `c` comments on a line/range).
        // F3 is not consumed here, so it still reaches the keybind matcher and closes it.
        if self.viewer.as_ref().is_some_and(|v| v.focused) && self.handle_viewer_key(key) {
            return;
        }

        // The `@` mention list gets the keys that mean something to it (arrows, Enter, Esc)
        // and lets everything else through, so typing narrows the query.
        if self.handle_mention_key(key) {
            return;
        }

        // A focused plugin view gets first refusal, but only for keys it binds (Esc always
        // blurs, so it cannot trap focus). After overlays, before global keybinds.
        if let Some(plugin) = self.focused_plugin.clone() {
            if let Some(runtime) = self.lua_runtime.clone() {
                if key.code == KeyCode::Esc {
                    crate::log!("  -> blur plugin '{}'", plugin);
                    self.focused_plugin = None;
                    runtime.invalidate_ui();
                    return;
                }
                if let Some(key_str) = crate::keybind::key_event_to_string(key) {
                    if runtime.plugin_has_key(&plugin, &key_str) {
                        let consumed = runtime.dispatch_plugin_key(&plugin, &key_str);
                        self.apply_plugin_effects(&runtime, tx);
                        if consumed {
                            crate::log!("  -> plugin '{}' consumed {}", plugin, key_str);
                            return;
                        }
                    }
                }
                // Printable input; exact `on_key` bindings matched first.
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    if let KeyCode::Char(ch) = key.code {
                        if runtime.plugin_has_text(&plugin) {
                            let consumed = runtime.dispatch_plugin_text(&plugin, &ch.to_string());
                            self.apply_plugin_effects(&runtime, tx);
                            if consumed {
                                crate::log!("  -> plugin '{}' consumed text '{}'", plugin, ch);
                                return;
                            }
                        }
                    }
                }
            }
        }

        // Search mode handling (intercepts keys when search bar is open).
        if self.search_state.is_some() {
            crate::log!("  -> search handler");
            self.handle_search_key(key);
            return;
        }

        // attached subagent view — Esc closes it.
        if self.attached_subagent.is_some() && key.code == KeyCode::Esc {
            self.attached_subagent = None;
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
            if self.handle_slash_key(key, tx) {
                return;
            }
        }

        // Check for modifier+Enter.
        // Shift+Enter = Queue (add to queue buffer, reliable cross-platform).
        // Alt+Enter = insert newline.
        if key.code == KeyCode::Enter {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+Enter: queue message
                self.queue_prompt();
                return;
            }
            if key.modifiers.contains(KeyModifiers::ALT) {
                // Alt+Enter: insert newline
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
                        self.toggle_tool_expand(id);
                    }
                    return;
                }
                KeyCode::Left => {
                    // Cycle tabs: Progress <- Output <- Input (wrap)
                    if let Some(ref id) = self.focused_tool.clone() {
                        self.cycle_tool_tab(id, false);
                    }
                    return;
                }
                KeyCode::Right => {
                    // Cycle tabs: Progress -> Output -> Input (wrap)
                    if let Some(ref id) = self.focused_tool.clone() {
                        self.cycle_tool_tab(id, true);
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
                KeyCode::Char('a') => {
                    if let Some(id) = self.focused_tool.clone() {
                        self.attach_subagent(&id);
                    }
                    return;
                }
                KeyCode::Char('e') => {
                    if let Some(id) = self.focused_tool.clone() {
                        self.toggle_subagent(&id);
                    }
                    return;
                }
                _ => {
                    // Let other keys (like Ctrl+T to exit) fall through to keybind matching
                }
            }
        }

        // Lua keymaps get first refusal, so a user binding can override any
        // built-in action. A handler returning `false` falls through to Rust.
        if !self.lua_keymaps.is_empty() {
            if let Some(key_str) = crate::keybind::key_event_to_string(key) {
                if self.lua_keymaps.has(&key_str) {
                    let consumed = self
                        .lua_runtime
                        .as_ref()
                        .is_some_and(|rt| rt.dispatch_keymap(&self.lua_keymaps, &key_str));
                    crate::log!("  -> lua keymap '{}' consumed={}", key_str, consumed);

                    // Run any built-in actions the handler asked for via kn9t.action().
                    self.run_queued_lua_actions(tx);

                    if consumed {
                        return;
                    }
                }
            }
        }

        // Match keybind.
        if let Some(action) = self.keybinds.match_key(key) {
            crate::log!("  -> keybind action: {:?}", action);
            self.execute_action(action, None, tx);
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
                    crate::log!(
                        "  -> Ctrl+Backspace detected (0x{:02x}), killing word",
                        c as u8
                    );
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
                    self.slash.activate(&self.lua_commands);
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
                    let char_len = self.input[pos..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
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
                        self.cursor_col = self
                            .input
                            .lines()
                            .next()
                            .map(|l| l.chars().count())
                            .unwrap_or(0);
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
                        self.cursor_col = self
                            .input
                            .lines()
                            .next()
                            .map(|l| l.chars().count())
                            .unwrap_or(0);
                    }
                }
            }
            KeyCode::Home => self.cursor_col = 0,
            KeyCode::End => self.cursor_col = self.current_line_len(),
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, key: crossterm::event::KeyEvent, tx: &Sender<Event>) {
        match &mut self.overlay {
            Some(Overlay::Approval { selected, .. }) => match key.code {
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
            },
            Some(Overlay::Interaction { ref mut state, .. }) => {
                match key.code {
                    KeyCode::Esc => {
                        // Check if we're in custom mode for Choice - exit custom mode first
                        if let InteractionState::Choice {
                            ref mut in_custom_mode,
                            ..
                        } = state
                        {
                            if *in_custom_mode {
                                *in_custom_mode = false;
                                return;
                            }
                        }
                        // Cancel: respond with empty/cancel payload so plugin unblocks.
                        self.respond_interaction(serde_json::json!({"cancelled": true}));
                    }
                    KeyCode::Enter => {
                        // For Choice in custom mode, submit the custom input
                        if let InteractionState::Choice {
                            in_custom_mode,
                            allow_custom,
                            ..
                        } = state
                        {
                            if *in_custom_mode && *allow_custom {
                                let response = state.response_value();
                                self.respond_interaction(response);
                                return;
                            }
                        }
                        // For Multi, submit current selections
                        // For others, submit current value
                        let response = state.response_value();
                        self.respond_interaction(response);
                    }
                    KeyCode::Up => match state {
                        InteractionState::Choice {
                            selected,
                            options,
                            in_custom_mode,
                            allow_custom,
                            ..
                        } => {
                            if !*in_custom_mode {
                                let max = options.len() + if *allow_custom { 1 } else { 0 };
                                *selected = selected.saturating_sub(1).min(max.saturating_sub(1));
                            }
                        }
                        InteractionState::Multi {
                            cursor, options, ..
                        } => {
                            *cursor = cursor
                                .saturating_sub(1)
                                .min(options.len().saturating_sub(1));
                        }
                        InteractionState::Confirm { selected, .. } => {
                            *selected = true;
                        }
                        _ => {}
                    },
                    KeyCode::Down => match state {
                        InteractionState::Choice {
                            selected,
                            options,
                            in_custom_mode,
                            allow_custom,
                            ..
                        } => {
                            if !*in_custom_mode {
                                let max = options.len() + if *allow_custom { 1 } else { 0 };
                                *selected = (*selected + 1).min(max.saturating_sub(1));
                            }
                        }
                        InteractionState::Multi {
                            cursor, options, ..
                        } => {
                            *cursor = (*cursor + 1).min(options.len().saturating_sub(1));
                        }
                        InteractionState::Confirm { selected, .. } => {
                            *selected = false;
                        }
                        _ => {}
                    },
                    KeyCode::Left | KeyCode::Right => {
                        if let InteractionState::Confirm { selected, .. } = state {
                            *selected = !*selected;
                        }
                    }
                    KeyCode::Char(' ') => match state {
                        InteractionState::Multi {
                            cursor, selected, ..
                        } => {
                            if let Some(sel) = selected.get_mut(*cursor) {
                                *sel = !*sel;
                            }
                        }
                        InteractionState::Choice {
                            selected,
                            options,
                            allow_custom,
                            in_custom_mode,
                            custom_input,
                            ..
                        } => {
                            if *in_custom_mode {
                                custom_input.push(' ');
                            } else if *allow_custom && *selected == options.len() {
                                *in_custom_mode = true;
                            }
                        }
                        InteractionState::Text { input, .. } => {
                            input.push(' ');
                        }
                        InteractionState::Generic { input, .. } => {
                            input.push(' ');
                        }
                        _ => {}
                    },
                    KeyCode::Tab => {
                        // Tab cycles through options for choice/multi
                        match state {
                            InteractionState::Choice {
                                selected,
                                options,
                                allow_custom,
                                in_custom_mode,
                                ..
                            } => {
                                if !*in_custom_mode {
                                    let max = options.len() + if *allow_custom { 1 } else { 0 };
                                    *selected = (*selected + 1) % max;
                                }
                            }
                            InteractionState::Multi {
                                cursor, options, ..
                            } => {
                                *cursor = (*cursor + 1) % options.len().max(1);
                            }
                            _ => {}
                        }
                    }
                    KeyCode::Backspace => match state {
                        InteractionState::Text { input, .. } => {
                            input.pop();
                        }
                        InteractionState::Choice {
                            custom_input,
                            in_custom_mode,
                            ..
                        } if *in_custom_mode => {
                            custom_input.pop();
                        }
                        InteractionState::Generic { input, .. } => {
                            input.pop();
                        }
                        _ => {}
                    },
                    KeyCode::Char(c) => match state {
                        InteractionState::Text { input, .. } => {
                            input.push(c);
                        }
                        InteractionState::Choice {
                            custom_input,
                            in_custom_mode,
                            ..
                        } if *in_custom_mode => {
                            custom_input.push(c);
                        }
                        InteractionState::Generic { input, .. } => {
                            input.push(c);
                        }
                        _ => {}
                    },
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
                crate::log!(
                    "WHICH-KEY: key={:?} selected_idx={}",
                    key.code,
                    self.which_key_panel.selected_idx
                );
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.which_key_panel.select_prev(&groups);
                        crate::log!(
                            "WHICH-KEY: after prev, selected_idx={}",
                            self.which_key_panel.selected_idx
                        );
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.which_key_panel.select_next(&groups);
                        crate::log!(
                            "WHICH-KEY: after next, selected_idx={}",
                            self.which_key_panel.selected_idx
                        );
                    }
                    _ => {}
                }
            }
            Some(Overlay::ModelSelect {
                ref mut selected,
                ref mut filter,
            }) => {
                crate::log!(
                    "  ModelSelect overlay: key={:?} selected={} filter={}",
                    key.code,
                    *selected,
                    filter
                );

                // Get filtered models count for bounds checking.
                // Filter matches on display name OR provider.
                let filtered: Vec<usize> = self
                    .model_sel
                    .models()
                    .iter()
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
                                crate::log!(
                                    "  ModelSelect: client={} lease={} session_id={}",
                                    self.client.is_some(),
                                    lease.is_some(),
                                    &session_id
                                );
                                if let Some(client) = &self.client {
                                    // Persist selection to server preferences.
                                    let _ = client.set_pref("last_model", &id);
                                    // Send model change to current session (requires lease).
                                    if let Some(ref holder) = lease {
                                        if !session_id.is_empty() {
                                            crate::log!(
                                                "MODEL CHANGE: session={} provider={} id={}",
                                                &session_id,
                                                &provider,
                                                &id
                                            );
                                            match client.set_model(
                                                &session_id,
                                                holder,
                                                &provider,
                                                &id,
                                            ) {
                                                Ok(_) => crate::log!("MODEL CHANGE: success"),
                                                Err(e) => {
                                                    crate::log!("MODEL CHANGE: error {:?}", e)
                                                }
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
            Some(Overlay::SessionSelect {
                ref mut selected,
                ref mut filter,
            }) => {
                crate::log!(
                    "  SessionSelect overlay: key={:?} selected={} filter={}",
                    key.code,
                    *selected,
                    filter
                );

                // Bounds-check against the renderer's list order via the shared `picker_order`
                // (the two must not compute it independently). No "New session"
                // row, so index 0 is the first match.
                let filtered: Vec<usize> =
                    crate::session_tree::picker_order(&self.session.sessions, filter)
                        .into_iter()
                        .map(|(idx, _)| idx)
                        .collect();
                let total_selectable = filtered.len();

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
                        if *selected + 1 < total_selectable {
                            *selected += 1;
                        } else {
                            *selected = 0;
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        // as soon as a filter is typed, highlight the first match
                        // so Enter opens it rather than creating something.
                        *selected = 0;
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *selected = 0;
                    }
                    KeyCode::Delete => {
                        // Delete the selected session.
                        if let Some(&session_idx) = filtered.get(*selected) {
                            let session_id = self.session.sessions[session_idx].id.clone();
                            crate::log!("  SessionSelect: DELETE session {}", session_id);
                            if let Some(client) = &self.client {
                                if client.delete_session(&session_id).is_ok() {
                                    // Remove from local list.
                                    self.session.sessions.remove(session_idx);
                                    // The list just got one shorter; keep the selection on
                                    // the row that is now last instead of past the end.
                                    if *selected > total_selectable.saturating_sub(2) {
                                        *selected = total_selectable.saturating_sub(2);
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        crate::log!("  SessionSelect: ENTER pressed, selected={}", *selected);
                        let target = filtered.get(*selected).copied();
                        self.overlay = None;
                        if let Some(session_idx) = target {
                            if session_idx < self.session.sessions.len() {
                                let new_session = self.session.sessions[session_idx].id.clone();
                                self.switch_to_session(&new_session, tx.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(Overlay::CommandPalette) => match key.code {
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
                        let cmd_id = cmd.id.clone();
                        let is_lua = cmd.is_lua;
                        self.command_palette.close();
                        self.overlay = None;
                        if is_lua {
                            self.run_lua_command(&cmd_id, "", tx);
                        } else {
                            self.execute_palette_command(&cmd_id, tx);
                        }
                    }
                }
                _ => {}
            },
            Some(Overlay::ToolsManager {
                ref mut selected,
                ref mut filter,
            }) => {
                // Tools are grouped by plugin; flat list for selection.
                // Filter matches on tool name or plugin name.
                let filtered: Vec<usize> = self
                    .tools
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| {
                        filter.is_empty()
                            || fuzzy_match(&t.name, filter)
                            || t.plugin
                                .as_ref()
                                .map(|p| fuzzy_match(p, filter))
                                .unwrap_or(false)
                    })
                    .map(|(i, _)| i)
                    .collect();

                match key.code {
                    KeyCode::Esc => {
                        self.overlay = None;
                    }
                    KeyCode::Up => {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if !filtered.is_empty() {
                            *selected = filtered.len() - 1;
                        }
                    }
                    KeyCode::Down => {
                        if *selected < filtered.len().saturating_sub(1) {
                            *selected += 1;
                        } else {
                            *selected = 0;
                        }
                    }
                    KeyCode::Backspace => {
                        filter.pop();
                        *selected = 0;
                    }
                    KeyCode::Char(' ') | KeyCode::Enter => {
                        // Toggle selected tool's enabled state.
                        if let Some(&tool_idx) = filtered.get(*selected) {
                            self.tools[tool_idx].enabled = !self.tools[tool_idx].enabled;
                            // Sync to server.
                            self.sync_tools_to_server();
                        }
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        *selected = 0;
                    }
                    _ => {}
                }
            }
            // the tree overlay. Selection walks the *rendered* (depth-first)
            // order, so Down always moves to the row below rather than to whatever comes
            // next in list order.
            Some(Overlay::SessionTree { ref mut selected }) => {
                let nodes = crate::session_tree::build_forest(&self.session.sessions).flatten();
                match key.code {
                    KeyCode::Esc => {
                        self.overlay = None;
                    }
                    KeyCode::Up => {
                        if *selected > 0 {
                            *selected -= 1;
                        } else if !nodes.is_empty() {
                            *selected = nodes.len() - 1;
                        }
                    }
                    KeyCode::Down => {
                        if *selected + 1 < nodes.len() {
                            *selected += 1;
                        } else {
                            *selected = 0;
                        }
                    }
                    KeyCode::Enter => {
                        // Same transparent switch as `/fork` and the picker.
                        let target = nodes
                            .get(*selected)
                            .and_then(|n| self.session.sessions.get(n.idx))
                            .map(|s| s.id.clone());
                        self.overlay = None;
                        if let Some(id) = target {
                            if id != self.session.state.session_id {
                                self.switch_to_session(&id, tx.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            None => {}
        }
    }

    fn handle_welcome_key(&mut self, key: crossterm::event::KeyEvent, tx: &Sender<Event>) {
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
                    self.command_palette.open(&self.lua_commands);
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

        // Slash command handling (shared with chat).
        if self.slash.active {
            if self.handle_slash_key(key, tx) {
                return;
            }
        }

        // Check keybinds for word navigation, model cycling, and queue actions.
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
                // Queue action works on welcome screen too.
                Action::Queue => {
                    self.queue_prompt();
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
                    let byte_idx = self
                        .input
                        .char_indices()
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
                self.slash.activate(&self.lua_commands);
            }
            KeyCode::Char(c) if c == '\x08' || c == '\x7f' => {
                // Ctrl+Backspace sends ^H (0x08) or DEL (0x7f) - delete word backward.
                crate::log!("WELCOME: Ctrl+Backspace detected (0x{:02x})", c as u8);
                self.kill_word_backward();
            }
            KeyCode::Char(c) => {
                // Convert char index to byte index for insert.
                let byte_idx = self
                    .input
                    .char_indices()
                    .nth(self.cursor_col)
                    .map(|(i, _)| i)
                    .unwrap_or(self.input.len());
                self.input.insert(byte_idx, c);
                self.cursor_col += 1;
            }
            KeyCode::Enter => {
                // Handle Enter on welcome screen.

                // First: check for slash commands (they work even during plugin loading).
                if self.input.starts_with('/') {
                    if let Some(handled) = self.try_execute_slash_input(tx) {
                        if handled {
                            self.input.clear();
                            self.cursor_col = 0;
                            return;
                        }
                    }
                }

                if self.input.is_empty() && self.staged_images.is_empty() {
                    // Empty input — create empty session if plugins ready.
                    if self.plugins_ready {
                        crate::log!("ENTER: creating new session (empty input)");
                        if let Err(e) = self.create_new_session(tx.clone()) {
                            self.transcript.push(Message::new(
                                "error",
                                format!("Failed to create session: {}", e),
                            ));
                        }
                    }
                    // If plugins not ready, do nothing — user can type something first.
                } else if !self.plugins_ready {
                    // Has input but plugins not ready — add to steering.
                    crate::log!("WELCOME ENTER: plugins loading, buffering to steering");
                    let text = std::mem::take(&mut self.input);
                    let images = std::mem::take(&mut self.staged_images);
                    self.cursor_col = 0;
                    self.prompt_history.add(text.clone());
                    self.steering.push(QueuedPrompt { text, images });
                } else {
                    // Has input AND plugins ready — create session and send.
                    let msg = std::mem::take(&mut self.input);
                    let images = std::mem::take(&mut self.staged_images);
                    self.cursor_col = 0;
                    self.prompt_history.add(msg.clone());
                    crate::log!("WELCOME ENTER: creating session and sending: {}", &msg);

                    match self.create_new_session(tx.clone()) {
                        Ok(()) => {
                            let image_count = images.len();
                            self.transcript
                                .push(Message::with_images("user", &msg, image_count));
                            let session_id = self.session.state.session_id.clone();
                            let lease = self.session.state.lease.clone().unwrap_or_default();
                            if let Some(client) = &self.client {
                                match client.prompt(&session_id, &lease, &msg, images) {
                                    Ok(_) => {
                                        crate::log!("WELCOME ENTER: prompt sent OK");
                                        self.streaming = true;
                                        self.tick_ctl.set_streaming(true);
                                    }
                                    Err(e) => {
                                        crate::log!("WELCOME ENTER: prompt failed: {:?}", e);
                                        self.transcript.push(Message::new(
                                            "error",
                                            format!("Failed to send: {}", e),
                                        ));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Session creation failed — put the message back in steering
                            // so it can be sent when ready.
                            self.steering.push(QueuedPrompt { text: msg, images });
                            self.transcript.push(Message::new(
                                "error",
                                format!("Failed to create session: {}", e),
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Drain and run every `kn9t.action(...)` queued by a Lua entry point (keymap, click,
    /// command) since the last drain. Factored out so the copies cannot drift.
    fn run_queued_lua_actions(&mut self, tx: &Sender<Event>) {
        let queued = self
            .lua_runtime
            .as_ref()
            .map(|rt| rt.drain_lua_actions())
            .unwrap_or_default();
        for (action, arg) in queued {
            crate::log!("  -> lua action: {:?} arg={:?}", action, arg);
            self.execute_action(action, arg.as_deref(), tx);
        }
    }

    /// Apply side effects queued by a plugin view's handlers. Deliberately narrower than
    /// `run_queued_lua_actions` — a plugin nudges the host, it does not drive it. Always
    /// invalidates the UI cache, since the render fingerprint cannot see inside a plugin's Lua.
    fn apply_plugin_effects(
        &mut self,
        runtime: &std::sync::Arc<crate::lua::LuaRuntime>,
        _tx: &Sender<Event>,
    ) {
        use crate::lua::plugin_ui::PluginEffect;
        for effect in runtime.drain_plugin_effects() {
            match effect {
                PluginEffect::InsertInput { text, .. } => {
                    if !self.input.is_empty() && !self.input.ends_with('\n') {
                        self.input.push('\n');
                    }
                    self.input.push_str(&text);
                    self.cursor_col = self.input.chars().count();
                }
                PluginEffect::NotifyPlugin {
                    plugin,
                    event,
                    data,
                } => {
                    let session_id = self.session.state.session_id.clone();
                    if !session_id.is_empty() {
                        if let Some(client) = &self.client {
                            let client_base = client.base_url_clone();
                            let token = client.token_clone();
                            std::thread::spawn(move || {
                                let c = crate::client::Client::new(&client_base, token.as_deref());
                                let _ = c.notify_plugin(&plugin, &session_id, &event, &data);
                            });
                        }
                    }
                }
                PluginEffect::Respond { payload, .. } => {
                    // The view owns the answer; the host owns the transport.
                    self.respond_interaction(payload);
                }
            }
        }
        runtime.invalidate_ui();
    }

    /// Run a `kn9t.register_command` handler by id, then any `kn9t.action(...)` it queued — a
    /// command handler is just another Lua entry point.
    fn run_lua_command(&mut self, id: &str, args: &str, tx: &Sender<Event>) {
        let Some(runtime) = self.lua_runtime.clone() else {
            return;
        };
        let Some(cmd) = self.lua_commands.get(id) else {
            crate::log!("run_lua_command: unknown id '{}'", id);
            return;
        };
        runtime.run_lua_command(cmd, args);
        self.run_queued_lua_actions(tx);
    }

    /// Run a built-in action. `arg` carries the payload for the few that need one (currently
    /// just `SwitchSession`); passing it separately keeps `Action` `Copy`.
    fn execute_action(&mut self, action: Action, arg: Option<&str>, tx: &Sender<Event>) {
        match action {
            Action::Quit => self.quit = true,
            Action::Abort => {
                // Escape cancels the last pending message and restores it to input,
                // otherwise it aborts the streaming turn.
                if self.has_pending_messages() {
                    if let Some(text) = self.cancel_last_pending() {
                        // Restore to input for editing.
                        self.input = text;
                        self.cursor_col = self.input.chars().count();
                        self.cursor_row = 0;
                    }
                } else if self.streaming && !self.aborting {
                    let session_id = self.session.state.session_id.clone();
                    let lease = self.session.state.lease.clone();
                    if let (Some(client), Some(holder)) = (&self.client, lease) {
                        let _ = client.abort(&session_id, &holder);
                        // Mark as aborting for visual feedback; streaming stays true until TurnEnded.
                        self.aborting = true;
                    }
                }
            }
            Action::Help => {
                self.command_palette.open(&self.lua_commands);
                self.overlay = Some(Overlay::CommandPalette);
            }
            Action::Send => self.send_prompt(tx),
            Action::Queue => {
                // Ctrl+Enter always queues (for sequential execution).
                self.queue_prompt();
            }
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
            Action::SessionPicker => {
                self.overlay = Some(Overlay::SessionSelect {
                    selected: 0,
                    filter: String::new(),
                });
            }
            Action::NewSession => {
                // Phase 4.4c: create immediately (no queued buffer).
                self.reset_session_state();
                if let Err(e) = self.create_new_session(tx.clone()) {
                    self.transcript.push(Message::new(
                        "error",
                        format!("Failed to create session: {}", e),
                    ));
                }
            }
            Action::SwitchSession => {
                if let Some(id) = arg {
                    self.switch_to_session(id, tx.clone());
                } else {
                    crate::log!("switch_session action fired with no id");
                }
            }
            Action::ToolMode => {
                if self.tool_mode {
                    self.exit_tool_mode();
                } else {
                    self.enter_tool_mode();
                }
            }
            Action::ToggleExplorer => {
                // Opening also focuses: "show me the files" is the same intent as "let me walk
                // the files". Closing releases the keyboard with it.
                if self.explorer.toggle() {
                    self.explorer.sync(&self.file_index);
                }
                self.invalidate_lua_ui();
            }
            Action::CloseViewer => {
                if self.viewer.take().is_some() {
                    self.invalidate_lua_ui();
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

            // Overlays, so a config can bind them directly instead of only
            // reaching them through the palette.
            Action::OpenModels => {
                self.overlay = Some(Overlay::ModelSelect {
                    selected: self.model_sel.selected(),
                    filter: String::new(),
                });
            }
            Action::OpenTools => {
                self.overlay = Some(Overlay::ToolsManager {
                    selected: 0,
                    filter: String::new(),
                });
            }
            Action::OpenPalette => {
                self.command_palette.open(&self.lua_commands);
                self.overlay = Some(Overlay::CommandPalette);
            }
            Action::RefreshTools => {
                if let Some(client) = self.client.take() {
                    self.refresh_tools(&client);
                    self.client = Some(client);
                }
            }

            // Focus a plugin view by name, or blur when given nothing. Only
            // accepts a plugin that actually has a registered UI, so a typo in
            // a config leaves focus alone instead of black-holing every key.
            Action::FocusPlugin => {
                let Some(runtime) = self.lua_runtime.clone() else {
                    return;
                };
                match arg.filter(|a| !a.is_empty()) {
                    None => self.focused_plugin = None,
                    Some(name) => {
                        if runtime.plugin_view_names().iter().any(|p| p == name) {
                            self.focused_plugin = Some(name.to_string());
                        } else {
                            crate::log!("focus_plugin: no view registered for '{}'", name);
                        }
                    }
                }
                runtime.invalidate_ui();
            }

            // Search mode actions are handled in handle_search_key, not here.
            Action::CloseSearch
            | Action::NextMatch
            | Action::PrevMatch
            | Action::ToggleRegex
            | Action::ToggleCase => {}
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
            }
            // Ctrl+F again: close search (toggle).
            KeyCode::Char('f') if ctrl => {
                self.search_state = None;
            }
            // Next match: Enter (no shift).
            KeyCode::Enter if !shift => {
                if let Some(ref mut s) = self.search_state {
                    s.next_match();
                    // Scroll transcript to show the current match.
                    if let Some(m) = s.current_match() {
                        let msg_idx = m.msg_idx;
                        // Approximate: a heuristic from-bottom value puts the match near the top.
                        let total = self.transcript.messages().len();
                        let lines_per_msg = 4usize; // rough estimate
                        let lines_from_bottom = (total.saturating_sub(msg_idx + 1)) * lines_per_msg;
                        self.transcript.set_scroll(lines_from_bottom);
                    }
                }
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
            }
            // Toggle regex: Alt+R.
            KeyCode::Char('r') if alt => {
                if let Some(ref mut s) = self.search_state {
                    s.regex_mode = !s.regex_mode;
                    s.search(self.transcript.messages());
                }
            }
            // Toggle case: Alt+C.
            KeyCode::Char('c') if alt => {
                if let Some(ref mut s) = self.search_state {
                    s.case_sensitive = !s.case_sensitive;
                    s.search(self.transcript.messages());
                }
            }
            // Backspace: delete char before cursor in query.
            KeyCode::Backspace => {
                if let Some(ref mut s) = self.search_state {
                    s.delete_before_cursor();
                    s.search(self.transcript.messages());
                }
            }
            // Typing: insert char into query.
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(ref mut s) = self.search_state {
                    s.insert_char(c);
                    s.search(self.transcript.messages());
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent, tx: &Sender<Event>) {
        match mouse.kind {
            MouseEventKind::Moved => {
                // Handle scrollbar drag
                if self.scrollbar_dragging {
                    self.handle_scrollbar_drag(mouse.row);
                }
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                // Stop scrollbar dragging
                self.scrollbar_dragging = false;
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // The explorer owns clicks inside its column (D4): select, then toggle a
                // directory or open a file in the viewer.
                if self.handle_explorer_click(mouse.column, mouse.row) {
                    return;
                }
                // Clicking a line in the viewer puts the cursor on it (D4).
                if self.handle_viewer_click(mouse.column, mouse.row) {
                    self.explorer.blur();
                    return;
                }
                // A click outside the column releases its keyboard focus.
                self.explorer.blur();
                // Check if clicking on scrollbar
                if self.handle_scrollbar_click(mouse.column, mouse.row) {
                    return;
                }
                self.handle_click(mouse.column, mouse.row, "left", tx);
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                // Handle scrollbar drag
                if self.scrollbar_dragging {
                    self.handle_scrollbar_drag(mouse.row);
                }
            }
            MouseEventKind::ScrollUp => {
                // The viewer scrolls under the wheel without taking the keyboard.
                if self.scroll_viewer_at(mouse.column, mouse.row, true) {
                    return;
                }
                // Check if scrolling over a tool card (both X and Y must be inside card)
                if let Some(call_id) = self.find_tool_at(mouse.column, mouse.row) {
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
                // The viewer scrolls under the wheel without taking the keyboard.
                if self.scroll_viewer_at(mouse.column, mouse.row, false) {
                    return;
                }
                // Check if scrolling over a tool card (both X and Y must be inside card)
                if let Some(call_id) = self.find_tool_at(mouse.column, mouse.row) {
                    // Scroll within tool output
                    let call_id_clone = call_id.clone();
                    if let Some(tool) = self.tool_mut(&call_id_clone) {
                        if tool.expanded {
                            // Count total lines: progress_lines + output
                            let mut line_count = tool.progress_lines.len();
                            if let Some(ref output) = tool.output {
                                if !output.is_empty() {
                                    if line_count > 0 {
                                        line_count += 1;
                                    }
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

    /// Handle a click in the explorer column: any click focuses it, a row click selects and
    /// does the D4 thing (directory toggles, file opens in the viewer).
    fn handle_explorer_click(&mut self, x: u16, y: u16) -> bool {
        let Some((ax, ay, aw, ah)) = self.explorer_area else {
            return false;
        };
        if x < ax || x >= ax + aw || y < ay || y >= ay + ah {
            return false;
        }
        self.explorer.focus();

        let hit = self
            .explorer_hit_areas
            .iter()
            .find(|h| h.y == y && x >= h.x_start && x < h.x_end)
            .cloned();
        if let Some(hit) = hit {
            if let Some(i) = self.explorer.rows().iter().position(|r| r.path == hit.path) {
                self.explorer.select(i);
            }
            if hit.is_dir {
                let _ = self.explorer.activate(&self.file_index);
            } else {
                self.open_viewer(&hit.path);
            }
        }
        true
    }

    /// Focus the viewer and put the cursor on the clicked line. Returns true when consumed.
    fn handle_viewer_click(&mut self, x: u16, y: u16) -> bool {
        let Some((vx, vy, vw, vh)) = self.viewer_area else {
            return false;
        };
        if x < vx || x >= vx + vw || y < vy || y >= vy + vh {
            return false;
        }
        let Some(viewer) = self.viewer.as_mut() else {
            return false;
        };
        viewer.focus();
        // The panel draws a one-cell border; the gutter shifts columns, not rows.
        let inner_y = vy + 1;
        if y >= inner_y {
            viewer.set_cursor(viewer.scroll + (y - inner_y) as usize);
        }
        true
    }

    /// Scroll the file viewer when the wheel is over it. Returns true when consumed.
    fn scroll_viewer_at(&mut self, x: u16, y: u16, up: bool) -> bool {
        let Some((vx, vy, vw, vh)) = self.viewer_area else {
            return false;
        };
        if x < vx || x >= vx + vw || y < vy || y >= vy + vh {
            return false;
        }
        let Some(viewer) = self.viewer.as_mut() else {
            return false;
        };
        if up {
            viewer.scroll_up(3);
        } else {
            viewer.scroll_down(3);
        }
        true
    }

    /// Find tool call_id if mouse Y is within a tool's content area.
    /// Find tool at mouse position (checks both X and Y).
    fn find_tool_at(&self, x: u16, y: u16) -> Option<String> {
        for hit in &self.tool_hit_areas {
            // Check if mouse is within the card bounds (both X and Y)
            if y >= hit.content_y_start
                && y < hit.content_y_end
                && x >= hit.x_start
                && x < hit.x_end
            {
                return Some(hit.call_id.clone());
            }
        }
        None
    }

    /// Handle scrollbar click - returns true if click was on scrollbar.
    fn handle_scrollbar_click(&mut self, x: u16, y: u16) -> bool {
        if let Some((sb_x, y_start, y_end, _total, _visible)) = self.scrollbar_area {
            // Check if click is on scrollbar column (or 1 pixel to the left for easier clicking)
            if x >= sb_x.saturating_sub(1) && x <= sb_x && y >= y_start && y < y_end {
                self.scrollbar_dragging = true;
                self.handle_scrollbar_drag(y);
                return true;
            }
        }
        false
    }

    /// Handle scrollbar drag - scroll to position based on Y coordinate.
    fn handle_scrollbar_drag(&mut self, y: u16) {
        if let Some((_, y_start, y_end, total, visible)) = self.scrollbar_area {
            let scrollbar_height = (y_end - y_start) as usize;
            if scrollbar_height == 0 || total <= visible {
                return;
            }

            // Calculate relative position (0.0 to 1.0)
            let relative_y = if y <= y_start {
                0.0
            } else if y >= y_end {
                1.0
            } else {
                (y - y_start) as f64 / scrollbar_height as f64
            };

            // Convert to scroll position (inverted: top = max scroll, bottom = 0)
            let max_scroll = total.saturating_sub(visible);
            let new_scroll = ((1.0 - relative_y) * max_scroll as f64).round() as usize;

            // Set scroll position directly
            self.transcript.set_scroll(new_scroll.min(max_scroll));
        }
    }

    /// Whether row `y` is outside the transcript as actually drawn last frame.
    ///
    /// Returns false when geometry is unknown, so clicks are never wrongly swallowed.
    #[doc(hidden)]
    pub fn is_outside_transcript(&self, y: u16) -> bool {
        self.transcript_area
            .is_some_and(|r| y < r.y || y >= r.y + r.height)
    }

    fn handle_click(&mut self, x: u16, y: u16, button: &str, tx: &Sender<Event>) {
        // Lua gets first refusal, floats before the base layout (topmost wins, false falls
        // through, as in `dispatch_keymap`). Checked before the transcript-only early return,
        // since a Lua widget can sit anywhere on screen.
        if let Some(runtime) = self.lua_runtime.clone() {
            for (id, rect) in self.lua_panel_click_areas.clone() {
                if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
                {
                    let local_x = x - rect.x;
                    let local_y = y - rect.y;
                    if runtime.dispatch_click(&self.lua_clicks, &id, local_x, local_y, button) {
                        self.run_queued_lua_actions(tx);
                        return;
                    }
                }
            }

            // Plugin views are painted into a slot of the base tree, so they are on top;
            // clicking one focuses it, which makes its `on_key` bindings live.
            let plugin_hit = self
                .plugin_view_areas
                .iter()
                .find(|(_, rect)| {
                    x >= rect.x
                        && x < rect.x + rect.width
                        && y >= rect.y
                        && y < rect.y + rect.height
                })
                .map(|(plugin, _)| plugin.clone());
            if let Some(plugin) = plugin_hit {
                if self.focused_plugin.as_deref() != Some(plugin.as_str()) {
                    self.focused_plugin = Some(plugin.clone());
                    runtime.invalidate_ui();
                }
            }
            for (plugin, id, rect) in self.plugin_click_areas.clone() {
                if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
                {
                    let local_x = x - rect.x;
                    let local_y = y - rect.y;
                    if runtime.dispatch_plugin_click(&plugin, &id, local_x, local_y, button) {
                        self.apply_plugin_effects(&runtime, tx);
                        return;
                    }
                }
            }

            for (id, rect) in self.lua_click_areas.clone() {
                if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
                {
                    let local_x = x - rect.x;
                    let local_y = y - rect.y;
                    if runtime.dispatch_click(&self.lua_clicks, &id, local_x, local_y, button) {
                        self.run_queued_lua_actions(tx);
                        return;
                    }
                }
            }
        }

        // Tool cards live in the transcript: ignore clicks outside it so a Lua
        // panel placed over the transcript doesn't trigger stale hit areas.
        if self.is_outside_transcript(y) {
            return;
        }

        // Reasoning cards precede the answer and the calls they led to, so they get first
        // refusal on the row.
        for hit in &self.thinking_hit_areas.clone() {
            if y == hit.y && x >= hit.x_start && x < hit.x_end {
                self.toggle_thinking_at(hit.msg_idx, hit.card_idx);
                return;
            }
        }

        // Check tool card clicks. A group header owns several calls and is checked
        // first: it is drawn above the cards it toggles, so it must win the row.
        for hit in &self.tool_hit_areas.clone() {
            if !hit.group_calls.is_empty() {
                if y == hit.header_y && x >= hit.x_start && x < hit.x_end {
                    self.toggle_tool_group(&hit.group_calls);
                    return;
                }
                continue;
            }

            // Click on header line - toggle expand/collapse
            if y == hit.header_y {
                self.toggle_tool_expand(&hit.call_id);
                return;
            }

            // Click on tabs (only if expanded)
            let is_expanded = self
                .transcript
                .messages()
                .iter()
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
        crate::log!(
            "PASTE IMAGE: starting, current staged_images.len={}",
            self.staged_images.len()
        );
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
        // If we receive a user message, clear matching item from steering buffer
        // and add to transcript (since reducer ignores user messages to avoid duplicates,
        // but for steered messages we haven't added them to transcript yet).
        if let SseFrame::MessageAppended { ref msg, .. } = frame {
            if msg.role == "user" && !self.steering.is_empty() {
                // Extract text from content blocks.
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        crate::wire::WireContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                // Remove the first steering item whose text matches (FIFO order).
                if let Some(idx) = self
                    .steering
                    .iter()
                    .position(|p| text.contains(&p.text) || p.text.contains(&text))
                {
                    let prompt = self.steering.remove(idx);
                    crate::log!(
                        "STEER CONFIRMED: removed '{}' from steering buffer",
                        &prompt.text[..prompt.text.len().min(30)]
                    );
                    // Now add to transcript since reducer won't
                    self.transcript.push(Message::with_images(
                        "user",
                        &prompt.text,
                        prompt.images.len(),
                    ));
                }
            }
        }

        // Pure reducer owns the state machine — App is a thin shell that syncs
        // tick_ctl/aborting on top (fix 4.5: live path now delegates to reducer).
        let needs_tick_sync = matches!(
            frame,
            SseFrame::TurnStarted { .. } | SseFrame::TurnEnded { .. } | SseFrame::TurnStatus { .. }
        );
        let was_streaming = self.streaming;
        // Bridge App fields into pure State, delegate, then copy back.
        let mut st = crate::reducer::State {
            streaming: self.streaming,
            turn_phase: self.turn_phase.clone(),
            turn_status_msg: self.turn_status_msg.clone(),
            last_seq: self.session.state.last_seq,
            transcript: std::mem::take(&mut self.transcript),
            tokens: std::mem::take(&mut self.tokens),
            active_approval_id: self.active_approval_id,
            active_interaction_id: self.active_interaction_id,
            overlay: self.overlay.clone(),
            session_id: self.session.state.session_id.clone(),
            session_title: self.session.session_title().map(|s| s.to_string()),
            sessions: self.session.sessions.clone(),
            model_sel: self.model_sel.clone(),
            ui_directives: std::mem::take(&mut self.ui_directives),
            subagents: std::mem::take(&mut self.subagents),
            attached_subagent: self.attached_subagent.clone(),
            tools_need_refresh: false,
            plugin_lua_pending: Vec::new(),
        };
        crate::reducer::reduce(&mut st, frame);
        // Copy back
        self.streaming = st.streaming;
        self.turn_phase = st.turn_phase;
        self.turn_status_msg = st.turn_status_msg;
        self.session.state.last_seq = st.last_seq;
        self.transcript = st.transcript;
        self.tokens = st.tokens;
        self.active_approval_id = st.active_approval_id;
        self.active_interaction_id = st.active_interaction_id;
        self.overlay = st.overlay;
        self.ui_directives = st.ui_directives;
        self.subagents = st.subagents;
        self.attached_subagent = st.attached_subagent;
        self.session.state.session_id = st.session_id;
        self.session.set_session_title(st.session_title);
        self.session.sessions = st.sessions;
        self.model_sel = st.model_sel;

        // A plugin that ships its own Lua UI renders its interaction; do not also
        // open the generic overlay, and give that view the keyboard.
        if let Some(Overlay::Interaction { plugin, .. }) = self.overlay.clone() {
            if let Some(runtime) = self.lua_runtime.clone() {
                if runtime.plugin_view_names().iter().any(|p| p == &plugin) {
                    self.overlay = None;
                    self.focused_plugin = Some(plugin);
                    runtime.invalidate_ui();
                }
            }
        }
        // Aborting is App-only (not in State): clear on terminal phases
        if matches!(self.turn_phase.as_str(), "aborted" | "idle" | "failed") {
            self.aborting = false;
        }
        if needs_tick_sync || was_streaming != self.streaming {
            self.tick_ctl.set_streaming(self.streaming);
        }

        // Apply plugin Lua UI operations. Deferred to here because the reducer
        // is pure over `State` and cannot touch the Lua runtime.
        if let Some(ref runtime) = self.lua_runtime {
            for op in st.plugin_lua_pending {
                runtime.apply_plugin_lua_op(&op);
            }
        }

        // If turn just ended, process any pending steering/queue.
        if was_streaming && !self.streaming {
            self.process_next_queue_item();
        }

        // R-PLUG2-110: refresh tools if a plugin re-declared (done last to avoid borrow issues)
        if st.tools_need_refresh {
            if let Some(client) = self.client.as_ref() {
                let session_id = if self.session.state.session_id.is_empty() {
                    None
                } else {
                    Some(self.session.state.session_id.as_str())
                };
                if let Ok(entries) = client.get_tools(session_id) {
                    self.tools = entries;
                    crate::log!(
                        "TOOLS: hot-refreshed {} tools after plugin declare",
                        self.tools.len()
                    );
                }
            }
        }
    }

    /// Bring the file-index views in step with the session (PLAN §P7 L2), once per event-loop
    /// turn rather than from the render path. Each step no-ops unless its input changed.
    ///
    /// Returns true when the index was rebuilt, so the caller can force a redraw: an idle turn
    /// receives only `Tick`s, whose handler deliberately skips the redraw, and a live tree
    /// change would otherwise be applied to state that is not painted until the next key.
    fn sync_index_views(&mut self) -> bool {
        // Root is where the user launched the TUI, not the session cwd: the viewer/explorer read
        // local files, and the session cwd may name a path this machine cannot see.
        let Ok(root) = std::env::current_dir() else {
            return false;
        };
        self.ensure_workspace_watcher(&root);

        // A watcher hit means the walk is stale: rebuild in place, keeping frecency. Otherwise
        // `refresh` still covers a root that changed under us and no-ops the rest of the time.
        let changed = if self
            .workspace_watcher
            .as_ref()
            .is_some_and(|w| w.take_changed())
        {
            self.file_index.rebuild(&root);
            true
        } else {
            self.file_index.refresh(&root)
        };

        self.explorer.sync(&self.file_index);
        self.mention
            .sync(&self.file_index, &self.input, self.cursor_col);
        changed
    }

    /// Start the workspace watcher once, on the first turn, and re-point it if the root moves.
    fn ensure_workspace_watcher(&mut self, root: &std::path::Path) {
        if self.workspace_watch_root.as_deref() == Some(root) {
            return;
        }
        self.workspace_watch_root = Some(root.to_path_buf());
        self.workspace_watcher = crate::workspace_watch::spawn(root);
        if self.workspace_watcher.is_none() {
            crate::log!("workspace watch: unavailable; the explorer tree is a snapshot");
        }
    }

    /// Force the Lua layout to rebuild next frame, for host state `render_ui` reads that has no
    /// fingerprint field (viewer open, explorer toggle).
    fn invalidate_lua_ui(&self) {
        if let Some(rt) = self.lua_runtime.as_ref() {
            rt.invalidate_ui();
        }
    }

    /// Keys for the focused explorer: arrows navigate, Left collapses or goes to the parent,
    /// Right/Enter expands or opens in the viewer (D4), `m` inserts the mention, Esc releases the
    /// keyboard without closing the column.
    fn handle_explorer_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                self.explorer.select_prev();
                true
            }
            KeyCode::Down => {
                self.explorer.select_next();
                true
            }
            KeyCode::Home => {
                self.explorer.select_first();
                true
            }
            KeyCode::End => {
                self.explorer.select_last();
                true
            }
            KeyCode::Left => {
                self.explorer.collapse_or_parent(&self.file_index);
                true
            }
            KeyCode::Right | KeyCode::Enter => {
                if let crate::explorer::Activate::File(path) =
                    self.explorer.activate(&self.file_index)
                {
                    self.open_viewer(&path);
                }
                true
            }
            KeyCode::Char('m') => {
                self.mention_selected_file();
                true
            }
            KeyCode::Esc => {
                // Same as F1 on a focused explorer: close it. (It stays visible by default, so
                // reopening is one key away.)
                self.explorer.close();
                self.invalidate_lua_ui();
                true
            }
            _ => false,
        }
    }

    /// Open `rel` (relative to the workspace root) in the read-only viewer (D4/D5).
    fn open_viewer(&mut self, rel: &str) {
        let Some(root) = self.file_index.root().map(std::path::Path::to_path_buf) else {
            return;
        };
        match crate::viewer::ViewerState::open(&root, rel) {
            Ok(viewer) => {
                crate::log!("viewer: {} ({} lines)", viewer.path(), viewer.line_count());
                self.viewer = Some(viewer);
                // The viewer takes the keyboard from the explorer: the user opened a file to
                // read/comment on it, not to keep walking the tree.
                self.explorer.blur();
                self.invalidate_lua_ui();
            }
            Err(e) => {
                crate::log!("viewer: {e}");
                self.transcript.push_system(format!("viewer: {e}"));
            }
        }
    }

    /// Insert text at the input cursor.
    fn insert_into_input(&mut self, text: &str) {
        let pos = self.cursor_pos();
        self.input.insert_str(pos, text);
        self.cursor_col += text.chars().count();
    }

    /// Insert the selected file as an `@path` mention at the cursor (D4 / D19).
    fn mention_selected_file(&mut self) {
        let Some(path) = self.explorer.selected_file().map(str::to_string) else {
            return;
        };
        let inserted = format!("@{path}");
        self.insert_into_input(&inserted);
        self.explorer.blur();
    }

    /// Keys for the focused viewer: `j`/`k` move the line cursor, `v` opens a range, `c` drops
    /// the `@path:lines` reference into the prompt so a comment can follow it (D4).
    fn handle_viewer_key(&mut self, key: KeyEvent) -> bool {
        if self.viewer.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Char('c') => {
                let reference = self
                    .viewer
                    .as_ref()
                    .map(|v| v.reference())
                    .unwrap_or_default();
                if !reference.is_empty() {
                    self.insert_into_input(&reference);
                }
                if let Some(v) = self.viewer.as_mut() {
                    v.blur();
                }
                true
            }
            KeyCode::Esc => {
                if let Some(v) = self.viewer.as_mut() {
                    v.blur();
                }
                true
            }
            KeyCode::Char('v') => {
                if let Some(v) = self.viewer.as_mut() {
                    v.toggle_selection();
                }
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(v) = self.viewer.as_mut() {
                    v.move_cursor(-1);
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(v) = self.viewer.as_mut() {
                    v.move_cursor(1);
                }
                true
            }
            KeyCode::PageUp => {
                if let Some(v) = self.viewer.as_mut() {
                    v.move_cursor(-10);
                }
                true
            }
            KeyCode::PageDown => {
                if let Some(v) = self.viewer.as_mut() {
                    v.move_cursor(10);
                }
                true
            }
            KeyCode::Home => {
                if let Some(v) = self.viewer.as_mut() {
                    v.set_cursor(0);
                }
                true
            }
            KeyCode::End => {
                if let Some(v) = self.viewer.as_mut() {
                    v.set_cursor(usize::MAX);
                }
                true
            }
            _ => false,
        }
    }

    /// Keys while the mention dropdown is open. Unlike the slash menu, it does not swallow
    /// typing: the query narrows on ordinary characters, so only meaningful keys are taken.
    fn handle_mention_key(&mut self, key: KeyEvent) -> bool {
        if !self.mention.active {
            return false;
        }
        match key.code {
            KeyCode::Up => {
                self.mention.select_prev();
                true
            }
            KeyCode::Down => {
                self.mention.select_next();
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                match self.mention.selected_path().map(str::to_string) {
                    Some(path) => {
                        self.file_index.record_open(&path);
                        self.cursor_col = self.mention.apply(&mut self.input, &path);
                    }
                    None => self.mention.deactivate(),
                }
                true
            }
            KeyCode::Esc => {
                self.mention.deactivate();
                true
            }
            _ => false,
        }
    }

    /// Key handling while the slash dropdown is active.
    fn handle_slash_key(&mut self, key: KeyEvent, tx: &Sender<Event>) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.slash.deactivate();
                if self.input.starts_with('/') {
                    self.input.clear();
                    self.cursor_col = 0;
                }
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                // If command expects args: prefill and let user type.
                // If no args expected: execute directly.
                if let Some(cmd) = self.slash.selected_command() {
                    let cmd_name = cmd.name.clone();
                    let expects_args = !cmd.args.is_empty();
                    let is_lua = cmd.is_lua;
                    let lua_id = cmd.lua_id.clone();

                    self.slash.deactivate();

                    if expects_args {
                        // Prefill input with `/command ` and let user type args.
                        self.input = format!("/{} ", cmd_name);
                        self.cursor_col = self.input.chars().count();
                    } else {
                        // Execute directly (no args needed).
                        if is_lua {
                            self.run_lua_command(&lua_id, "", tx);
                        } else {
                            self.execute_slash_command_with_args(&cmd_name, "", tx);
                        }
                        self.input.clear();
                        self.cursor_col = 0;
                    }
                }
                true
            }
            KeyCode::Up => {
                self.slash.select_prev();
                true
            }
            KeyCode::Down => {
                self.slash.select_next();
                true
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
                true
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.cursor_col += 1;
                let query = self.input.trim_start_matches('/');
                self.slash.set_query(query);
                true
            }
            _ => true, // Consume all other keys while in slash mode
        }
    }

    fn send_prompt(&mut self, tx: &Sender<Event>) {
        // Allow sending with just images (no text required).
        if self.input.trim().is_empty() && self.staged_images.is_empty() {
            return;
        }

        // Parse slash commands first: `/command args`
        if self.input.starts_with('/') {
            if let Some(handled) = self.try_execute_slash_input(tx) {
                if handled {
                    self.input.clear();
                    self.cursor_row = 0;
                    self.cursor_col = 0;
                    return;
                }
            }
        }

        let text = std::mem::take(&mut self.input);
        let images = std::mem::take(&mut self.staged_images);
        self.cursor_row = 0;
        self.cursor_col = 0;

        // Add to prompt history
        self.prompt_history.add(text.clone());

        // Clear undo/redo history for new prompt
        self.input_history.clear();

        // If plugins not ready, buffer to steering (sent when ready).
        if !self.plugins_ready {
            crate::log!("STEERING: plugins loading, buffering message");
            self.steering.push(QueuedPrompt { text, images });
            return;
        }

        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();

        // If agent is currently streaming: STEER immediately (inject into current turn).
        if self.streaming {
            if let (Some(client), Some(holder)) = (&self.client, lease) {
                crate::log!("STEER: injecting message into current turn");
                // Don't add to transcript yet — keep in steering buffer as "pending".
                // It will appear muted in the "── Steering ──" section until confirmed.
                // Call /steer to inject into the agent's current turn
                match client.steer(&session_id, &holder, &text) {
                    Ok(seq) => {
                        crate::log!("STEER: success seq={}", seq);
                        // Keep in steering buffer — it's now "pending injection".
                        // The server will send a UserAppended event when it's actually
                        // added to the context, and we'll remove it from steering then.
                        self.steering.push(QueuedPrompt { text, images });
                    }
                    Err(e) => {
                        // Steer failed (turn may have just ended) — queue for next turn
                        crate::log!("STEER: failed {:?}, queueing for next turn", e);
                        self.queue.push_back(QueuedPrompt { text, images });
                    }
                }
            }
            return;
        }

        // Normal case: agent is idle, send prompt directly.
        if let (Some(client), Some(holder)) = (&self.client, lease) {
            let image_count = images.len();
            self.transcript
                .push(Message::with_images("user", &text, image_count));
            match client.prompt(&session_id, &holder, &text, images) {
                Ok(_) => {
                    // Mark as streaming immediately so queue doesn't try to send.
                    self.streaming = true;
                    self.tick_ctl.set_streaming(true);
                }
                Err(e) => {
                    crate::log!("PROMPT: failed {:?}", e);
                    self.transcript
                        .push(Message::new("error", format!("Failed to send: {}", e)));
                }
            }
        }
    }

    /// Try to execute input as a slash command. Returns Some(true) if handled,
    /// Some(false) if it looks like a command but wasn't found, None if not a command.
    fn try_execute_slash_input(&mut self, tx: &Sender<Event>) -> Option<bool> {
        let input = self.input.trim();
        if !input.starts_with('/') {
            return None;
        }

        // Parse: `/command args` or `/command`
        let without_slash = &input[1..];
        let (cmd_name, args) = match without_slash.find(' ') {
            Some(idx) => (&without_slash[..idx], without_slash[idx + 1..].trim()),
            None => (without_slash, ""),
        };
        let cmd_name = cmd_name.to_string();
        let args = args.to_string();

        crate::log!(
            "SLASH: parsing '{}' -> cmd='{}' args='{}'",
            input,
            cmd_name,
            args
        );

        // Check built-in commands
        let is_builtin = crate::slash::COMMANDS.iter().any(|c| c.name == cmd_name);

        // Check Lua commands
        let lua_cmd_id = self
            .lua_commands
            .iter()
            .find(|c| {
                c.slash
                    .as_ref()
                    .map(|s| s.trim_start_matches('/'))
                    .unwrap_or("")
                    == cmd_name
            })
            .map(|c| c.id.clone());

        if is_builtin {
            // Execute built-in command with args
            self.execute_slash_command_with_args(&cmd_name, &args, tx);
            return Some(true);
        }

        if let Some(cmd_id) = lua_cmd_id {
            self.run_lua_command(&cmd_id, &args, tx);
            return Some(true);
        }

        // Not a recognized command — let it be sent as a normal message
        None
    }

    /// Execute a built-in slash command with parsed args.
    fn execute_slash_command_with_args(&mut self, cmd: &str, args: &str, tx: &Sender<Event>) {
        crate::log!("SLASH EXEC: cmd='{}' args='{}'", cmd, args);
        match cmd {
            "queue" | "q" => {
                if args.is_empty() {
                    self.transcript
                        .push(Message::new("system", "Usage: /queue <message>"));
                } else {
                    self.queue.push_back(QueuedPrompt {
                        text: args.to_string(),
                        images: Vec::new(),
                    });
                    crate::log!(
                        "QUEUE: message queued via /queue ({} total)",
                        self.queue.len()
                    );
                }
            }
            "session" => {
                self.overlay = Some(Overlay::SessionSelect {
                    selected: 0,
                    filter: String::new(),
                });
            }
            "models" | "model" => {
                if !args.is_empty() {
                    // Direct model switch: /model <id>
                    // TODO: implement direct model selection
                }
                self.overlay = Some(Overlay::ModelSelect {
                    selected: self.model_sel.selected(),
                    filter: String::new(),
                });
            }
            "new" => {
                self.reset_session_state();
                if let Err(e) = self.create_new_session(tx.clone()) {
                    self.transcript.push(Message::new(
                        "error",
                        format!("Failed to create session: {}", e),
                    ));
                }
            }
            "fork" | "undo" => {
                self.handle_fork_command(cmd, args, tx.clone());
            }
            "tree" => {
                if self.session.session_count() == 0 {
                    self.transcript
                        .push(Message::new("system", "No sessions to show."));
                } else {
                    self.overlay = Some(Overlay::SessionTree { selected: 0 });
                }
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
                let sid = self.session.state.session_id.clone();
                let lease = self.session.state.lease.clone();
                if sid.is_empty() {
                    self.transcript
                        .push(Message::new("system", "No active session to compact."));
                } else if let (Some(client), Some(holder)) = (&self.client, lease) {
                    match client.compact_session(&sid, &holder) {
                        Ok(()) => self
                            .transcript
                            .push(Message::new("system", "Compaction started...")),
                        Err(e) => self
                            .transcript
                            .push(Message::new("system", format!("Compact failed: {}", e))),
                    }
                }
            }
            "export" => {
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript
                        .push(Message::new("system", "No active session to export."));
                } else if let Some(client) = &self.client {
                    match client.export_session(&sid) {
                        Ok(val) => {
                            let out_path = if args.is_empty() {
                                format!("{}.json", &sid[..8.min(sid.len())])
                            } else {
                                args.to_string()
                            };
                            if let Err(e) = std::fs::write(
                                &out_path,
                                serde_json::to_string_pretty(&val).unwrap_or_default(),
                            ) {
                                self.transcript
                                    .push(Message::new("system", format!("Write failed: {}", e)));
                            } else {
                                self.transcript.push(Message::new(
                                    "system",
                                    format!("Exported to {}", out_path),
                                ));
                            }
                        }
                        Err(e) => self
                            .transcript
                            .push(Message::new("system", format!("Export failed: {}", e))),
                    }
                }
            }
            "search" => {
                self.open_search();
            }
            "keys" => {
                self.which_key_panel = WhichKeyPanel::new();
                self.overlay = Some(Overlay::WhichKey);
            }
            "palette" => {
                self.command_palette.open(&self.lua_commands);
                self.overlay = Some(Overlay::CommandPalette);
            }
            "stash" => {
                self.stash_prompt();
            }
            "pop" | "unstash" | "stashpop" => {
                self.unstash_prompt();
            }
            "rename" => {
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript
                        .push(Message::new("system", "No active session to rename."));
                } else if args.is_empty() {
                    self.transcript
                        .push(Message::new("system", "Usage: /rename <new title>"));
                } else if let Some(client) = &self.client {
                    match client.rename_session(&sid, args) {
                        Ok(_) => {
                            self.session.set_session_title(Some(args.to_string()));
                            if let Some(s) = self.session.sessions.iter_mut().find(|s| s.id == sid)
                            {
                                s.name = args.to_string();
                            }
                            self.transcript
                                .push(Message::new("system", format!("Renamed to '{}'", args)));
                        }
                        Err(e) => self
                            .transcript
                            .push(Message::new("system", format!("Rename failed: {}", e))),
                    }
                }
            }
            _ => {
                // Unknown command — show error
                self.transcript
                    .push(Message::new("system", format!("Unknown command: /{}", cmd)));
            }
        }
    }

    /// Queue a message (Ctrl+Enter key).
    /// Adds to the queue buffer — will be sent one-per-turn when agent goes idle.
    fn queue_prompt(&mut self) {
        if self.input.trim().is_empty() && self.staged_images.is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.input);
        let images = std::mem::take(&mut self.staged_images);
        self.cursor_row = 0;
        self.cursor_col = 0;

        // Add to prompt history
        self.prompt_history.add(text.clone());

        // Clear undo/redo history
        self.input_history.clear();

        // Add to queue buffer
        self.queue.push_back(QueuedPrompt { text, images });
        crate::log!("QUEUE: message queued ({} total)", self.queue.len());
    }

    /// Process the next queued item after turn ends.
    /// Called when streaming ends (TurnEnded).
    fn process_next_queue_item(&mut self) {
        // Safety: don't send if already streaming (409 conflict).
        if self.streaming {
            crate::log!("QUEUE: skipping, still streaming");
            return;
        }

        // Pop the next queued message.
        let Some(prompt) = self.queue.pop_front() else {
            return;
        };

        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();

        if let (Some(client), Some(holder)) = (&self.client, lease) {
            let image_count = prompt.images.len();
            crate::log!(
                "QUEUE: sending queued message ({} remaining)",
                self.queue.len()
            );

            // Send to server first, only add to transcript on success
            match client.prompt(&session_id, &holder, &prompt.text, prompt.images.clone()) {
                Ok(_) => {
                    crate::log!("QUEUE: prompt sent successfully");
                    self.transcript
                        .push(Message::with_images("user", &prompt.text, image_count));
                    self.streaming = true;
                    self.tick_ctl.set_streaming(true);
                }
                Err(crate::client::ClientError::TurnRunning) => {
                    // 409 = turn still running, just re-queue silently and wait for TurnEnded.
                    crate::log!("QUEUE: turn still running, will retry on TurnEnded");
                    self.queue.push_front(prompt);
                }
                Err(e) => {
                    crate::log!("QUEUE: prompt failed: {:?}", e);
                    // Real error - put it back and show error to user.
                    self.queue.push_front(prompt);
                    self.transcript.push(Message::new(
                        "error",
                        format!("Failed to send queued message (will retry): {}", e),
                    ));
                }
            }
        }
    }
    /// Cancel the last item from steering (if any), then queue.
    /// Returns the cancelled prompt text to restore to input, or None if both empty.
    fn cancel_last_pending(&mut self) -> Option<String> {
        // Priority: steering first, then queue.
        if let Some(prompt) = self.steering.pop() {
            crate::log!("CANCEL: removed last steering message");
            return Some(prompt.text);
        }
        if let Some(prompt) = self.queue.pop_back() {
            crate::log!("CANCEL: removed last queued message");
            return Some(prompt.text);
        }
        None
    }

    /// Check if there are any pending messages (steering or queue).
    pub fn has_pending_messages(&self) -> bool {
        !self.steering.is_empty() || !self.queue.is_empty()
    }

    fn respond_approval(&mut self, decision: &str) {
        let session_id = self.session.state.session_id.clone();
        let lease = self.session.state.lease.clone();
        if let (Some(client), Some(holder), Some(id)) =
            (&self.client, lease, self.active_approval_id)
        {
            let _ = client.approve(&session_id, &holder, id, decision);
            self.active_approval_id = None;
        }
    }

    /// respond to a generic interaction with an opaque payload.
    fn respond_interaction(&mut self, payload: serde_json::Value) {
        if let (Some(client), Some(id)) = (&self.client, self.active_interaction_id) {
            let _ = client.ui_respond(id, payload);
            self.active_interaction_id = None;
            self.overlay = None;
            // A plugin-owned interaction view is done; release its keyboard.
            self.focused_plugin = None;
        }
    }

    /// Sync the current tools enabled/disabled state to the server.
    fn sync_tools_to_server(&mut self) {
        let session_id = self.session.state.session_id.clone();
        if session_id.is_empty() {
            return;
        }
        let lease = self.session.state.lease.clone();
        let Some(holder) = lease else { return };
        let Some(client) = &self.client else { return };

        // Collect disabled tool names.
        let disabled: Vec<String> = self
            .tools
            .iter()
            .filter(|t| !t.enabled)
            .map(|t| t.name.clone())
            .collect();

        crate::log!("TOOLS: syncing disabled={:?} to server", disabled);
        match client.set_tools(&session_id, &holder, &disabled) {
            Ok(reenabled) => {
                if !reenabled.is_empty() {
                    crate::log!("TOOLS: reenabled={:?}", reenabled);
                }
            }
            Err(e) => {
                crate::log!("TOOLS: sync failed: {:?}", e);
            }
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
                let byte_col = line
                    .char_indices()
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
        self.input
            .lines()
            .nth(self.cursor_row)
            .map(|l| l.chars().count())
            .unwrap_or(0)
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
                let killed = self.input[pos..pos + 1].to_string();
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
        let word_start = trimmed
            .rfind(|c: char| c.is_whitespace())
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
        let yank_result = self
            .kill_ring
            .yank_pop()
            .map(|(len, text)| (len, text.to_string()));

        if let Some((remove_len, new_text)) = yank_result {
            // Re-query last_yank_pos after the mutable borrow is released
            if let Some((start_pos, _)) = self.kill_ring.last_yank_pos() {
                let end_pos = start_pos + remove_len;
                if end_pos <= self.input.len() {
                    self.record_input_boundary();
                    self.input.replace_range(start_pos..end_pos, &new_text);
                    let chars_diff = new_text.chars().count() as isize - remove_len as isize;
                    self.cursor_col = (self.cursor_col as isize + chars_diff).max(0) as usize;
                    crate::log!(
                        "YANK-POP: replaced {} bytes with {} chars",
                        remove_len,
                        new_text.len()
                    );
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Word Navigation (Ctrl+Left/Right, Ctrl+Delete)
    // ═══════════════════════════════════════════════════════════════════════

    /// Move cursor to previous word boundary.
    fn word_left(&mut self) {
        use crate::word_segmenter::{byte_to_char_offset, prev_word_boundary};

        let byte_pos = self.cursor_pos();
        if byte_pos == 0 {
            return;
        }

        let new_byte_pos = prev_word_boundary(&self.input, byte_pos);
        self.cursor_col = byte_to_char_offset(&self.input, new_byte_pos);
    }

    /// Move cursor to next word boundary.
    fn word_right(&mut self) {
        use crate::word_segmenter::{byte_to_char_offset, next_word_boundary};

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
        let user_indices: Vec<usize> = messages
            .iter()
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
        let target_idx = user_indices
            .iter()
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
        let user_indices: Vec<usize> = messages
            .iter()
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
        let target_idx = user_indices
            .iter()
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
            self.transcript
                .push(Message::new("system", "No models available"));
            return;
        }

        self.model_sel.select_next();
        self.apply_current_model();
    }

    /// Cycle to previous model and apply immediately.
    fn cycle_model_prev(&mut self) {
        if self.model_sel.model_count() == 0 {
            self.transcript
                .push(Message::new("system", "No models available"));
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
                    crate::log!(
                        "MODEL CYCLE: session={} provider={} id={}",
                        &session_id,
                        &provider,
                        &id
                    );
                    match client.set_model(&session_id, holder, &provider, &id) {
                        Ok(_) => {
                            crate::log!("MODEL CYCLE: success");
                            self.transcript
                                .push(Message::new("system", format!("Model: {}", display_name)));
                        }
                        Err(e) => {
                            crate::log!("MODEL CYCLE: error {:?}", e);
                            self.transcript.push(Message::new(
                                "system",
                                format!("Failed to set model: {:?}", e),
                            ));
                        }
                    }
                } else {
                    // No session yet - just show the selection.
                    self.transcript.push(Message::new(
                        "system",
                        format!("Model: {} (will apply on next message)", display_name),
                    ));
                }
            } else {
                // No lease - just show the selection.
                self.transcript.push(Message::new(
                    "system",
                    format!("Model: {} (will apply on next message)", display_name),
                ));
            }
        } else {
            self.transcript.push(Message::new(
                "system",
                format!("Model: {} (not connected)", display_name),
            ));
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Prompt Stash (/stash and /unstash commands)
    // ═══════════════════════════════════════════════════════════════════════

    /// Stash current prompt state.
    fn stash_prompt(&mut self) {
        if self.input.is_empty() {
            self.transcript
                .push(Message::new("system", "Nothing to stash (input is empty)"));
            return;
        }
        self.prompt_stash
            .stash(&self.input, self.cursor_row, self.cursor_col);
        let len = self.input.len();
        self.input.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.transcript
            .push(Message::new("system", format!("Stashed {} chars", len)));
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
                self.transcript
                    .push(Message::new("system", "Swapped stash with current input"));
            }
            self.input = text;
            self.cursor_row = row;
            self.cursor_col = col.min(self.current_line_len());
        } else {
            self.transcript
                .push(Message::new("system", "Stash is empty"));
        }
    }

    /// Expand or collapse one reasoning card.
    fn toggle_thinking_at(&mut self, msg_idx: usize, card_idx: usize) {
        if let Some(card) = self
            .transcript
            .messages_mut()
            .get_mut(msg_idx)
            .and_then(|m| m.thinking.get_mut(card_idx))
        {
            card.collapsed = !card.collapsed;
        }
    }

    /// Toggle every reasoning card: collapse them if any is open, else expand them all.
    fn toggle_all_thinking(&mut self) {
        let mut count = 0;
        let mut any_expanded = false;
        for msg in self.transcript.messages() {
            for card in &msg.thinking {
                count += 1;
                any_expanded |= !card.collapsed;
            }
        }
        if count == 0 {
            return;
        }

        let collapse = any_expanded;
        for msg in self.transcript.messages_mut() {
            for card in &mut msg.thinking {
                card.collapsed = collapse;
            }
        }
        crate::log!(
            "THINKING: {} {} cards",
            if collapse { "collapsed" } else { "expanded" },
            count
        );
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
                    self.transcript.push(Message::new(
                        "error",
                        format!("Failed to create session: {}", e),
                    ));
                }
            }
            "session_list" => {
                self.overlay = Some(Overlay::SessionSelect {
                    selected: 0,
                    filter: String::new(),
                });
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
                self.toggle_all_thinking();
            }
            "keybindings" => {
                self.which_key_panel = WhichKeyPanel::new();
                self.overlay = Some(Overlay::WhichKey);
            }
            // Tools
            "models" => {
                self.overlay = Some(Overlay::ModelSelect {
                    selected: self.model_sel.selected(),
                    filter: String::new(),
                });
            }
            "compact" => {
                // Fire-and-forget: triggers background compaction, result via SSE.
                let sid = self.session.state.session_id.clone();
                let lease = self.session.state.lease.clone();
                if sid.is_empty() {
                    self.transcript
                        .push(Message::new("system", "No active session to compact."));
                } else if let (Some(client), Some(holder)) = (&self.client, lease) {
                    match client.compact_session(&sid, &holder) {
                        Ok(()) => self
                            .transcript
                            .push(Message::new("system", "Compaction started...")),
                        Err(e) => self
                            .transcript
                            .push(Message::new("system", format!("Compact failed: {}", e))),
                    }
                } else {
                    self.transcript
                        .push(Message::new("system", "No lease — cannot compact."));
                }
            }
            "export" => {
                let sid = self.session.state.session_id.clone();
                if sid.is_empty() {
                    self.transcript
                        .push(Message::new("system", "No active session to export."));
                } else if let Some(client) = &self.client {
                    match client.export_session(&sid) {
                        Ok(val) => {
                            let out_path =
                                format!("/tmp/kn9t-export-{}.json", &sid[..8.min(sid.len())]);
                            let json =
                                serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".into());
                            match std::fs::write(&out_path, json) {
                                Ok(_) => self.transcript.push(Message::new(
                                    "system",
                                    format!("Exported to {}", out_path),
                                )),
                                Err(e) => self.transcript.push(Message::new(
                                    "system",
                                    format!("Export write failed: {}", e),
                                )),
                            }
                        }
                        Err(e) => self
                            .transcript
                            .push(Message::new("system", format!("Export failed: {}", e))),
                    }
                }
            }
            "tools_manager" => {
                // Refresh tools to get current disabled state, then open overlay.
                if let Some(client) = &self.client {
                    let session_id = if self.session.state.session_id.is_empty() {
                        None
                    } else {
                        Some(self.session.state.session_id.as_str())
                    };
                    if let Ok(entries) = client.get_tools(session_id) {
                        self.tools = entries;
                    }
                }
                self.overlay = Some(Overlay::ToolsManager {
                    selected: 0,
                    filter: String::new(),
                });
            }

            // Settings
            "theme_toggle" => {
                self.config.theme.toggle_mode();
                self.theme_mode = self.config.theme.get_mode().to_string();
                self.transcript.push(Message::new(
                    "system",
                    format!("Theme toggled to {}.", self.theme_mode),
                ));
            }
            "quit" => {
                self.quit = true;
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod mouse_routing_tests {
    use super::*;
    use ratatui::layout::Rect;

    fn app() -> App {
        App::new(Config::default(), crate::event::TickControl::dummy())
    }

    #[test]
    fn clicks_outside_transcript_are_ignored() {
        let mut a = app();
        a.transcript_area = Some(Rect::new(0, 0, 80, 10));

        assert!(!a.is_outside_transcript(5), "row inside transcript");
        assert!(a.is_outside_transcript(15), "row below transcript");
    }

    #[test]
    fn unknown_geometry_does_not_block_clicks() {
        // Before the first frame nothing is recorded; clicks must still work.
        let a = app();
        assert!(!a.is_outside_transcript(10));
    }
}
