//! Pure SSE reducer — `(state, frame) -> state`, no `&mut self`, no terminal, no I/O.
//!
//! Phase 4.4a: extract a pure reducer so `handle_sse` (the most important function
//! in the crate, previously 0 tests) is testable by constructing only `State`.
//! The interface is the test surface — a pure reducer would have caught F5 and F7 immediately.
//!
//! Handles the three frames previously ignored: `ThinkingDelta`, `ModelChanged`, `Compacted`
//! (only their `seq` was recorded). `Compacted` especially — the transcript now reflects a compaction.
//!
//! ## Test strategy (96E-19)
//!
//! This module is the **primary unit-test seam** for the TUI (pure logic, no terminal).
//! All state transitions are tested here without a PTY. Terminal rendering is covered
//! separately via golden-snapshot tests in `ui::render` (`ui/render.rs` `golden_*` tests)
//! which render to a `TestBackend` and assert the buffer string. What's intentionally
//! left untested: raw crossterm event loop, `App::run` poll loop, and `Client` HTTP I/O
//! — pure I/O glue with no branching worth unit-testing.

use crate::app::Overlay;
use crate::message_handler::{Message, ThinkingCard, ToolCard};
use crate::model_selector::ModelSelector;
use crate::session_manager::SessionEntry;
use crate::token_tracker::{TokenCounts, TokenTracker};
use crate::wire::SseFrame;

/// Extracted content from a wire message: text, tool calls, tool results.
struct ExtractedContent {
    text: String,
    /// (id, name, args_json)
    tool_calls: Vec<(String, String, String)>,
    /// (id, output, is_error)
    tool_results: Vec<(String, String, bool)>,
    /// Reasoning blocks, kept out of `text` so they render as their own cards.
    thinking: Vec<ThinkingCard>,
}

/// 96E-27 — collapsible subagent entry nested under its spawning tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentEntry {
    pub call_id: String,
    pub plugin: String,
    pub task: String,
    pub visibility: String, // silent|progress|full
    pub collapsed: bool,
    pub session_id: Option<String>,
}

impl SubagentEntry {
    fn collapsed_for(visibility: &str) -> bool {
        match visibility {
            "silent" => true, // one-liner, collapsed
            "full" => false,  // expanded inline
            _ => true,        // progress: collapsed by default
        }
    }
}

/// Minimal TUI state mutated by SSE frames.
/// This is the pure state slice of `App` — no `Client`, no `Terminal`, no `EventLoop`.
#[derive(Debug)]
pub struct State {
    pub streaming: bool,
    pub turn_phase: String,
    pub turn_status_msg: String,
    pub last_seq: u64,
    pub transcript: crate::message_handler::Transcript,
    pub tokens: TokenTracker,
    pub active_approval_id: Option<u64>,
    /// 96E-28: active generic interaction id (opaque payload).
    pub active_interaction_id: Option<u64>,
    pub overlay: Option<Overlay>,
    pub session_id: String,
    pub session_title: Option<String>,
    pub sessions: Vec<SessionEntry>,
    pub model_sel: ModelSelector,
    /// 96E-23: structured UI directives received (plugin, target, op, payload) — transport only.
    pub ui_directives: Vec<(String, String, String, serde_json::Value)>,
    /// 96E-27: collapsible subagent sub-entries nested under spawning tool calls.
    pub subagents: Vec<SubagentEntry>,
    /// 96E-27: attached subagent transcript view (call_id -> transcript preview).
    pub attached_subagent: Option<(String, Vec<crate::wire::TranscriptMessage>)>,
    /// R-PLUG2-110: set by reducer when `PluginDeclared` received; App clears after refresh.
    pub tools_need_refresh: bool,
    /// Plugin Lua UI operations to apply after reduce.
    ///
    /// Queued rather than applied inline because the reducer is pure over
    /// `State` and has no access to the Lua runtime.
    pub plugin_lua_pending: Vec<PluginLuaOp>,
}

/// A plugin's request to change its TUI display.
///
/// The plugin ships Lua (`Register`) and pushes data (`SetState`); the TUI owns
/// the widget vocabulary, so neither the source nor the state is interpreted here.
#[derive(Debug, Clone)]
pub enum PluginLuaOp {
    Register {
        plugin: String,
        source: String,
        /// How the plugin would like to be placed, if it said.
        ///
        /// A hint, not an instruction: a config routes on it (`"sidebar"` /
        /// `"main"` / `"status"`) instead of matching plugin names, and is free
        /// to ignore it. Without this a config could only place a view by
        /// hardcoding the plugin's name.
        placement: PluginPlacement,
    },
    SetState {
        plugin: String,
        state: serde_json::Value,
    },
    Clear {
        plugin: String,
    },
}

/// A plugin's declared placement preferences, as sent with `ui_register_lua`.
///
/// Every field is optional because a plugin that only wants to draw something
/// should not have to answer layout questions. `None`/empty means "the config
/// decides", which is the safe default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginPlacement {
    /// `"sidebar"` | `"main"` | `"status"`. Validated server-side, so an
    /// unknown value never reaches here.
    pub zone: Option<String>,
    /// Human-readable panel title. Falls back to the plugin name, which is an
    /// id and reads poorly as a heading ("kn9t-git-integration" vs "Git").
    pub title: Option<String>,
    /// Preferred height in rows / width in columns. Advisory: a config clamps
    /// them to what the terminal actually has.
    pub rows: Option<u16>,
    pub cols: Option<u16>,
}

impl PluginPlacement {
    /// Read the optional placement fields out of a `register_lua` payload.
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        let str_field = |k: &str| {
            payload
                .get(k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        // `as_u64` then narrow: a negative or absurd value becomes None rather
        // than wrapping into a nonsense size.
        let dim = |k: &str| {
            payload
                .get(k)
                .and_then(|v| v.as_u64())
                .filter(|n| *n > 0 && *n <= u16::MAX as u64)
                .map(|n| n as u16)
        };
        Self {
            zone: str_field("placement"),
            title: str_field("title"),
            rows: dim("rows"),
            cols: dim("cols"),
        }
    }
}

impl State {
    /// Toggle collapse for a subagent sub-entry.
    pub fn toggle_subagent(&mut self, call_id: &str) {
        if let Some(entry) = self.subagents.iter_mut().find(|e| e.call_id == call_id) {
            entry.collapsed = !entry.collapsed;
        }
    }
    /// Attach: open the subagent's full transcript on demand (session_read result).
    pub fn attach_subagent(
        &mut self,
        call_id: &str,
        transcript: Vec<crate::wire::TranscriptMessage>,
    ) {
        if self.subagents.iter().any(|e| e.call_id == call_id) {
            self.attached_subagent = Some((call_id.to_string(), transcript));
        }
    }
    pub fn detach_subagent(&mut self) {
        self.attached_subagent = None;
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            streaming: false,
            turn_phase: "idle".into(),
            turn_status_msg: String::new(),
            last_seq: 0,
            transcript: crate::message_handler::Transcript::new(),
            tokens: TokenTracker::new(),
            active_approval_id: None,
            active_interaction_id: None,
            overlay: None,
            session_id: String::new(),
            session_title: None,
            sessions: Vec::new(),
            model_sel: ModelSelector::new(),
            ui_directives: Vec::new(),
            subagents: Vec::new(),
            attached_subagent: None,
            tools_need_refresh: false,
            plugin_lua_pending: Vec::new(),
        }
    }
}

/// Pure reducer: apply one SSE frame to state.
/// No I/O, no client, no terminal — just state transition.
pub fn reduce(state: &mut State, frame: SseFrame) {
    if let Some(seq) = frame.seq() {
        state.last_seq = seq;
    }
    match frame {
        SseFrame::TurnStarted { .. } => {
            state.streaming = true;
            state.turn_phase = "thinking".into();
            state.turn_status_msg.clear();
            state.transcript.take_delta();
            state.tokens.on_turn_started();
        }
        SseFrame::TurnEnded { stop, .. } => {
            state.streaming = false;
            state.tokens.on_turn_ended();
            if stop.to_ascii_lowercase().contains("abort") {
                state.turn_phase = "aborted".into();
                let partial = state.transcript.take_delta();
                if !partial.is_empty() {
                    let preview: String = partial.chars().take(400).collect();
                    state.transcript.push(Message::new(
                        "system",
                        format!(
                            "Aborted — kept partial ({} chars): {}",
                            partial.len(),
                            preview
                        ),
                    ));
                } else {
                    state.transcript.push(Message::new("system", "Aborted"));
                }
            } else if stop.to_ascii_lowercase().contains("failed") || state.turn_phase == "failed" {
                state.turn_phase = "failed".into();
            } else {
                state.turn_phase = "idle".into();
                state.turn_status_msg.clear();
            }
        }
        SseFrame::TextDelta { delta, .. } => {
            if state.turn_phase == "thinking" {
                state.turn_phase = "streaming".into();
            }
            state.transcript.append_delta(&delta);
        }
        SseFrame::ThinkingDelta { delta, .. } => {
            state.turn_phase = "thinking".into();
            state.transcript.append_thinking_delta(&delta);
        }
        SseFrame::MessageAppended { msg, .. } => {
            if msg.role == "user" || msg.silent {
                return;
            }
            let extracted = extract_message_content(&msg.content);
            for (call_id, output, is_error) in &extracted.tool_results {
                state.transcript.update_tool(call_id, |tool| {
                    tool.output = Some(output.clone());
                    if *is_error {
                        tool.status = "error".into();
                    }
                });
            }
            let final_content = if !state.transcript.live_delta().is_empty() {
                state.transcript.take_delta()
            } else {
                extracted.text
            };
            // Reasoning from this turn. The appended message already carries the persisted
            // block, so the streamed copy is only a fallback for when it does not — using
            // both renders the same reasoning twice.
            let mut thinking = extracted.thinking;
            let live_thinking = state.transcript.take_thinking_delta();
            if thinking.is_empty() && !live_thinking.trim().is_empty() {
                thinking.push(ThinkingCard {
                    text: live_thinking,
                    collapsed: true,
                });
            }
            let tools: Vec<ToolCard> = extracted
                .tool_calls
                .iter()
                .map(|(id, name, args)| ToolCard {
                    call_id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    status: "pending".into(),
                    output: None,
                    progress_lines: Vec::new(),
                    expanded: false,
                    active_tab: crate::message_handler::ToolTab::Input,
                    scroll_offset: 0,
                })
                .collect();
            // 96E-27: detect SubagentSpec spawns (args contains task) and create collapsed sub-entry
            for (call_id, name, args) in &extracted.tool_calls {
                // Heuristic: SubagentSpec tools have task in args; also name often spawn_subagent
                let is_spawn = name.contains("spawn") || args.contains("\"task\"");
                if is_spawn {
                    // Parse args_json for task + visibility; fallback to name if parse fails
                    let parsed: Option<serde_json::Value> = serde_json::from_str(args).ok();
                    let task = parsed
                        .as_ref()
                        .and_then(|v| v.get("task"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(name.as_str())
                        .to_string();
                    let visibility = parsed
                        .as_ref()
                        .and_then(|v| v.get("visibility"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("progress")
                        .to_string();
                    let collapsed = SubagentEntry::collapsed_for(&visibility);
                    // Derive plugin from tool name? For tests, plugin is tool name prefix; default to spawn plugin.
                    let plugin = "unknown".to_string();
                    state.subagents.push(SubagentEntry {
                        call_id: call_id.clone(),
                        plugin,
                        task,
                        visibility: visibility.clone(),
                        collapsed,
                        session_id: None,
                    });
                }
            }
            if !final_content.is_empty() || !tools.is_empty() || !thinking.is_empty() {
                state
                    .transcript
                    .push(Message::new(&msg.role, final_content).with_tools(tools).with_thinking(thinking));
            }
        }
        SseFrame::UsageRecorded {
            tokens,
            cost_usd,
            usage_kind,
            ..
        } => {
            let counts = TokenCounts::new(
                tokens.input as usize,
                tokens.output as usize,
                tokens.cache_read as usize,
                tokens.cache_write as usize,
            );
            state
                .tokens
                .record_usage(counts, cost_usd, usage_kind == "title");
        }
        SseFrame::ToolStarted { call_id, .. } => {
            state.turn_phase = "tool".into();
            state.transcript.update_tool(&call_id, |tool| {
                tool.status = "running".into();
                tool.expanded = true;
                tool.active_tab = crate::message_handler::ToolTab::Progress;
            });
        }
        SseFrame::ToolArgsDelta { .. } => {}
        SseFrame::ToolProgress { call_id, note, .. } => {
            state.transcript.update_tool(&call_id, |tool| {
                tool.progress_lines.push(note.clone());
                tool.status = format!("running: {}", note);
            });
        }
        SseFrame::ToolFinished {
            call_id, is_error, ..
        } => {
            state.transcript.update_tool(&call_id, |tool| {
                tool.status = if is_error {
                    "error".into()
                } else {
                    "done".into()
                };
                tool.active_tab = crate::message_handler::ToolTab::Output;
                tool.expanded = false;
                tool.scroll_offset = 0;
            });
        }
        SseFrame::ApprovalRequest { id, tool, args, .. } => {
            state.active_approval_id = Some(id);
            state.overlay = Some(Overlay::Approval {
                tool,
                args: serde_json::to_string(&args).unwrap_or_default(),
                selected: 0,
            });
        }
        SseFrame::InteractionRequest {
            id,
            plugin,
            payload,
        } => {
            state.active_interaction_id = Some(id);
            // Parse payload into structured interaction state.
            let payload_str =
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| format!("{payload:?}"));
            let state_parsed = crate::app::InteractionState::from_payload(&payload_str);
            state.overlay = Some(Overlay::Interaction {
                id,
                plugin,
                state: state_parsed,
            });
        }
        SseFrame::ModelChanged { model, .. } => {
            let name = format!("{}:{}", model.provider, model.id);
            if let Some(idx) = state
                .model_sel
                .models()
                .iter()
                .position(|m| m.provider == model.provider && m.id == model.id)
            {
                state.model_sel.set_selected(idx);
            }
            state
                .transcript
                .push(Message::new("system", format!("Model changed to {}", name)));
        }
        SseFrame::ToolsToggled { .. } => {
            // The event carries the full disabled set, not a diff, so the last one
            // wins and a replay is idempotent.
            //
            // The payload's `disabled` list is deliberately ignored: `App.tools`
            // already holds `enabled` per tool, and `GET /tools?session=` returns
            // the authoritative `disabled` flag (client.rs), so keeping a copy on
            // `State` would be a second source of truth that can drift. The reducer
            // is pure and does no I/O, so it only flags the refresh and `App`
            // re-reads. That is also what keeps a second attached client in sync --
            // the reason this event is broadcast over SSE at all.
            //
            // No transcript message on purpose: a local toggle is already visible
            // in the tools panel, and announcing every keystroke would be noise.
            state.tools_need_refresh = true;
        }
        SseFrame::Compacted { summary, .. } => {
            // Just show the summary as an assistant message.
            // "Compaction started..." was already shown when the user triggered it.
            let extracted = extract_message_content(&summary.content);
            if !extracted.text.is_empty() {
                state.transcript.push(Message::new(&summary.role, extracted.text));
            }
        }
        SseFrame::Error { message } => {
            state.turn_phase = "failed".into();
            state.turn_status_msg = message.clone();
            state.transcript.push(Message::new("error", message));
        }
        SseFrame::RetryAttempt {
            attempt,
            max,
            error,
            delay_ms,
            retry_kind,
        } => {
            state.turn_phase = "retrying".into();
            state.turn_status_msg = format!(
                "retry {}/{} {} in {}ms: {}",
                attempt, max, retry_kind, delay_ms, error
            );
            state.transcript.push(Message::new(
                "system",
                format!(
                    "↻ retry {}/{} {} in {}ms: {}",
                    attempt, max, retry_kind, delay_ms, error
                ),
            ));
        }
        SseFrame::TurnStatus { phase, message } => {
            state.turn_phase = phase.clone();
            state.turn_status_msg = message.clone();
            if phase == "failed" && !message.is_empty() {
                state.transcript.push(Message::new(
                    "error",
                    format!("turn {}: {}", phase, message),
                ));
            } else if phase == "retrying" && !message.is_empty()
                && !state
                    .transcript
                    .messages()
                    .last()
                    .map(|m| m.content.contains(&message))
                    .unwrap_or(false)
                {
                    state
                        .transcript
                        .push(Message::new("system", message.clone()));
                }
            match phase.as_str() {
                "idle" | "failed" | "aborted" => state.streaming = false,
                "thinking" | "streaming" | "tool" | "retrying" => state.streaming = true,
                _ => {}
            }
        }
        SseFrame::TitleChanged { title } => {
            state.session_title = Some(title.clone());
            if let Some(s) = state.sessions.iter_mut().find(|s| s.id == state.session_id) {
                s.name = title;
            }
        }
        SseFrame::PluginNotification { plugin, message } => {
            state.transcript.push(Message::new(&plugin, message));
        }
        SseFrame::UiDirective {
            plugin,
            target,
            op,
            payload,
        } => {
            // 96E-23 transport — record verbatim.
            state
                .ui_directives
                .push((plugin.clone(), target.clone(), op.clone(), payload.clone()));
            // 96E-25: page ops are tunneled through UiDirective with target=page_id and
            // op declare_page/write_placeholder/clear_page. Update structured map.
            match op.as_str() {
                // Plugin ships Lua defining `render(state)`; the TUI owns the
                // widget vocabulary, so the source is opaque until it is loaded.
                "register_lua" => {
                    if let Some(src) = payload.get("source").and_then(|v| v.as_str()) {
                        state.plugin_lua_pending.push(PluginLuaOp::Register {
                            plugin: plugin.clone(),
                            source: src.to_string(),
                            placement: PluginPlacement::from_payload(&payload),
                        });
                    }
                }
                // Cheap data update: no source re-sent, no re-registration.
                "set_state" => {
                    if let Some(val) = payload.get("state").cloned() {
                        state.plugin_lua_pending.push(PluginLuaOp::SetState {
                            plugin: plugin.clone(),
                            state: val,
                        });
                    }
                }
                "clear" => {
                    state.plugin_lua_pending.push(PluginLuaOp::Clear {
                        plugin: plugin.clone(),
                    });
                }
                _ => {}
            }
        }
        SseFrame::HookFailed { .. } => {}
        SseFrame::PluginDeclared {
            plugin,
            tools_added,
            tools_removed,
        } => {
            // R-PLUG2-110: a plugin hot-declared new tools. Set flag for App to refresh.
            state.tools_need_refresh = true;
            // Optionally push a system message about the change.
            if !tools_added.is_empty() || !tools_removed.is_empty() {
                let msg = format!(
                    "Plugin '{}' updated: +{} tool(s), -{} tool(s)",
                    plugin,
                    tools_added.len(),
                    tools_removed.len()
                );
                state.transcript.push(Message::new("system", msg));
            }
        }
        // 96E-47: a plugin's run state changed. The tool list is unaffected by a stop (the
        // specs stay registered, calls are refused at execution), but the *display* should
        // say so, and a reload/start may have changed the set — hence the refresh flag.
        SseFrame::PluginState {
            plugin,
            state: plugin_state,
            error,
        } => {
            state.tools_need_refresh = true;
            let msg = match plugin_state.as_str() {
                "stopped" => format!("Plugin '{plugin}' stopped — its tools will refuse to run."),
                "started" => format!("Plugin '{plugin}' started."),
                "reloaded" => format!("Plugin '{plugin}' reloaded."),
                // A crash is the case worth naming the cause for: the agent may have just
                // lost tools it was using, and the reason is the only clue why.
                "crashed" if !error.is_empty() => format!("Plugin '{plugin}' crashed: {error}"),
                "crashed" => format!("Plugin '{plugin}' crashed."),
                other => format!("Plugin '{plugin}': {other}"),
            };
            state.transcript.push(Message::new("system", msg));
        }
    }
}

fn extract_message_content(content: &[crate::wire::WireContent]) -> ExtractedContent {
    use crate::wire::WireContent;
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut thinking = Vec::new();
    for c in content {
        match c {
            WireContent::Text { text } => text_parts.push(text.as_str()),
            WireContent::Thinking { text } => thinking.push(ThinkingCard {
                text: text.clone(),
                collapsed: true,
            }),
            WireContent::ToolCall {
                id,
                name,
                args_json,
            } => tool_calls.push((id.clone(), name.clone(), args_json.clone())),
            WireContent::ToolResult {
                id,
                content: rc,
                is_error,
            } => {
                let output: String = rc
                    .iter()
                    .filter_map(|x| match x {
                        WireContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                tool_results.push((id.clone(), output, *is_error));
            }
            WireContent::Image { .. } => {} // images handled separately
        }
    }
    ExtractedContent {
        text: text_parts.join("\n"),
        tool_calls,
        tool_results,
        thinking,
    }
}

