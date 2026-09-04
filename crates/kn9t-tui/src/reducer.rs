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
use crate::message_handler::{Message, ToolCard};
use crate::model_selector::ModelSelector;
use crate::page_state::{self, PageKey, UiPage};
use crate::session_manager::SessionEntry;
use crate::token_tracker::{TokenCounts, TokenTracker};
use crate::wire::SseFrame;

/// 96E-27 — collapsible subagent entry nested under its spawning tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentEntry {
    pub call_id: String,
    pub plugin: String,
    pub task: String,
    pub visibility: String, // silent|progress|full
    pub collapsed: bool,
    pub session_id: Option<String>,
    pub page_key: Option<PageKey>,
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
    /// 96E-25: plugin-declared pages (plugin, page_id) -> UiPage for rendering.
    pub ui_pages: std::collections::HashMap<PageKey, UiPage>,
    /// 96E-25: selected page key for multi-page tab switcher (None = first page).
    pub ui_page_selected: Option<PageKey>,
    /// 96E-27: collapsible subagent sub-entries nested under spawning tool calls.
    pub subagents: Vec<SubagentEntry>,
    /// 96E-27: attached subagent transcript view (call_id -> transcript preview).
    pub attached_subagent: Option<(String, Vec<crate::wire::TranscriptMessage>)>,
    /// R-PLUG2-110: set by reducer when `PluginDeclared` received; App clears after refresh.
    pub tools_need_refresh: bool,
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
            ui_pages: std::collections::HashMap::new(),
            ui_page_selected: None,
            subagents: Vec::new(),
            attached_subagent: None,
            tools_need_refresh: false,
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
            state.transcript.append_delta(&delta);
        }
        SseFrame::MessageAppended { msg, .. } => {
            if msg.role == "user" || msg.silent {
                return;
            }
            let (text, tool_calls, tool_results, _) = extract_message_content(&msg.content);
            for (call_id, output, is_error) in &tool_results {
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
                text
            };
            let tools: Vec<ToolCard> = tool_calls
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
            for (call_id, name, args) in &tool_calls {
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
                        page_key: None,
                    });
                }
            }
            if !final_content.is_empty() || !tools.is_empty() {
                state
                    .transcript
                    .push(Message::new(&msg.role, final_content).with_tools(tools));
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
        SseFrame::Compacted {
            replaced, summary, ..
        } => {
            let (text, _, _, _) = extract_message_content(&summary.content);
            let summary_text = if text.is_empty() {
                "Conversation compacted.".to_string()
            } else {
                text
            };
            state.transcript.push(Message::new(
                "system",
                format!(
                    "Compacted {}..{}: {}",
                    replaced.start,
                    replaced.end,
                    summary_text.clone()
                ),
            ));
            state
                .transcript
                .push(Message::new(&summary.role, summary_text));
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
            } else if phase == "retrying" && !message.is_empty() {
                if !state
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
                "declare_page" => {
                    let layout = payload.get("layout").unwrap_or(&payload);
                    let _ =
                        page_state::apply_declare(&mut state.ui_pages, &plugin, &target, layout);
                    // Auto-select first page if none selected
                    if state.ui_page_selected.is_none() {
                        state.ui_page_selected = Some((plugin.clone(), target.clone()));
                    }
                    // 96E-27: link page to most recent pending subagent without a page (nested view)
                    if let Some(entry) = state
                        .subagents
                        .iter_mut()
                        .rev()
                        .find(|e| e.page_key.is_none())
                    {
                        entry.page_key = Some((plugin.clone(), target.clone()));
                    }
                }
                "write_placeholder" => {
                    let pid = payload
                        .get("placeholder_id")
                        .or_else(|| payload.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Some(val) = payload.get("value").cloned() {
                        let _ = page_state::apply_write(
                            &mut state.ui_pages,
                            &plugin,
                            &target,
                            pid,
                            val,
                        );
                    }
                }
                "clear_page" => {
                    let _ = page_state::apply_clear(&mut state.ui_pages, &plugin, &target);
                    if state
                        .ui_page_selected
                        .as_ref()
                        .map(|k| k.0 == plugin && k.1 == target)
                        .unwrap_or(false)
                    {
                        state.ui_page_selected = state.ui_pages.keys().next().cloned();
                    }
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
    }
}

fn extract_message_content(
    content: &[crate::wire::WireContent],
) -> (
    String,
    Vec<(String, String, String)>,
    Vec<(String, String, bool)>,
    usize,
) {
    use crate::wire::WireContent;
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut image_count = 0;
    for c in content {
        match c {
            WireContent::Text { text } => text_parts.push(text.as_str()),
            WireContent::Thinking { text } => text_parts.push(text.as_str()),
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
            WireContent::Image { .. } => image_count += 1,
        }
    }
    (text_parts.join("\n"), tool_calls, tool_results, image_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{WireContent, WireMessage, WireModelRef, WireSeqRange, WireTokens};

    fn text_msg(role: &str, text: &str, seq: u64) -> SseFrame {
        SseFrame::MessageAppended {
            seq,
            msg: WireMessage {
                id: format!("m{}", seq),
                role: role.into(),
                content: vec![WireContent::Text { text: text.into() }],
                silent: false,
            },
        }
    }

    fn delta(text: &str) -> SseFrame {
        SseFrame::TextDelta {
            msg_id: "m1".into(),
            idx: 0,
            delta: text.into(),
        }
    }

    fn thinking_delta(text: &str) -> SseFrame {
        SseFrame::ThinkingDelta {
            msg_id: "m1".into(),
            idx: 0,
            delta: text.into(),
        }
    }

    fn tool_call_msg(seq: u64, call_id: &str) -> SseFrame {
        SseFrame::MessageAppended {
            seq,
            msg: WireMessage {
                id: format!("m{}", seq),
                role: "assistant".into(),
                content: vec![WireContent::ToolCall {
                    id: call_id.into(),
                    name: "bash".into(),
                    args_json: "{\"cmd\": \"ls\"}".into(),
                }],
                silent: false,
            },
        }
    }

    fn tool_result_msg(seq: u64, call_id: &str, output: &str) -> SseFrame {
        SseFrame::MessageAppended {
            seq,
            msg: WireMessage {
                id: format!("m{}", seq),
                role: "tool".into(),
                content: vec![WireContent::ToolResult {
                    id: call_id.into(),
                    content: vec![WireContent::Text {
                        text: output.into(),
                    }],
                    is_error: false,
                }],
                silent: false,
            },
        }
    }

    #[test]
    fn turn_sequence() {
        let mut s = State::default();
        s.session_id = "sess1".into();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        assert!(s.streaming);
        reduce(&mut s, delta("hello "));
        reduce(&mut s, delta("world"));
        assert_eq!(s.transcript.live_delta(), "hello world");
        reduce(&mut s, text_msg("assistant", "hello world", 1));
        assert_eq!(s.transcript.messages().len(), 1);
        assert_eq!(s.last_seq, 1);
        reduce(
            &mut s,
            SseFrame::TurnEnded {
                turn: 1,
                stop: "stop".into(),
            },
        );
        assert!(!s.streaming);
    }

    #[test]
    fn thinking_delta_handled() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(&mut s, thinking_delta("thinking..."));
        assert_eq!(s.transcript.live_delta(), "thinking...");
        // thinking delta should not be ignored — previously only seq recorded
    }

    #[test]
    fn model_changed_handled() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::ModelChanged {
                seq: 5,
                model: WireModelRef {
                    provider: "openai".into(),
                    id: "gpt-4".into(),
                },
            },
        );
        assert_eq!(s.last_seq, 5);
        // Should push a system message
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("Model changed")));
    }

    #[test]
    fn tools_toggled_flags_refresh_and_advances_seq() {
        let mut s = State::default();
        assert!(!s.tools_need_refresh);
        reduce(
            &mut s,
            SseFrame::ToolsToggled {
                seq: 7,
                disabled: vec!["bash".into(), "write".into()],
            },
        );
        // Durable event: must advance last_seq or reconnect would replay it forever.
        assert_eq!(s.last_seq, 7);
        // App re-reads GET /tools on this flag; that is what syncs a second client.
        assert!(s.tools_need_refresh);
        // Silent: a toggle is already visible in the tools panel.
        assert!(s.transcript.messages().is_empty());
    }

    #[test]
    fn tools_toggled_is_idempotent_last_wins() {
        // The payload is the full disabled set, not a diff, so replaying an older
        // frame after a newer one must not resurrect stale state. The reducer holds
        // no copy of the set precisely so this cannot go wrong.
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::ToolsToggled {
                seq: 3,
                disabled: vec!["bash".into()],
            },
        );
        reduce(
            &mut s,
            SseFrame::ToolsToggled {
                seq: 4,
                disabled: vec![],
            },
        );
        assert_eq!(s.last_seq, 4);
        assert!(s.tools_need_refresh);
    }

    #[test]
    fn compacted_handled() {
        let mut s = State::default();
        reduce(&mut s, text_msg("assistant", "old 1", 1));
        reduce(&mut s, text_msg("assistant", "old 2", 2));
        let summary = WireMessage {
            id: "sum".into(),
            role: "assistant".into(),
            content: vec![WireContent::Text {
                text: "summary".into(),
            }],
            silent: false,
        };
        reduce(
            &mut s,
            SseFrame::Compacted {
                seq: 3,
                replaced: WireSeqRange { start: 1, end: 2 },
                summary,
            },
        );
        assert_eq!(s.last_seq, 3);
        // Compacted should push two messages (system note + summary)
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("Compacted")));
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content == "summary"));
    }

    #[test]
    fn approval_request_sets_overlay() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::ApprovalRequest {
                id: 42,
                tool: "bash".into(),
                args: serde_json::json!({"cmd":"ls"}),
                cwd: "/tmp".into(),
            },
        );
        assert_eq!(s.active_approval_id, Some(42));
        assert!(matches!(s.overlay, Some(Overlay::Approval { .. })));
    }

    #[test]
    fn title_changed_updates_session() {
        let mut s = State::default();
        s.session_id = "sess1".into();
        s.sessions.push(SessionEntry {
            id: "sess1".into(),
            name: "Old".into(),
            running: false,
            created_at: None,
        });
        reduce(
            &mut s,
            SseFrame::TitleChanged {
                title: "New Title".into(),
            },
        );
        assert_eq!(s.session_title.as_deref(), Some("New Title"));
        assert_eq!(s.sessions[0].name, "New Title");
    }

    #[test]
    fn compacted_seq_recorded() {
        let mut s = State::default();
        let summary = WireMessage {
            id: "sum".into(),
            role: "assistant".into(),
            content: vec![],
            silent: false,
        };
        reduce(
            &mut s,
            SseFrame::Compacted {
                seq: 99,
                replaced: WireSeqRange { start: 10, end: 20 },
                summary,
            },
        );
        assert_eq!(s.last_seq, 99);
    }

    #[test]
    fn sse_reconnect_seq_tracking() {
        // Simulate frames with seq and ensure last_seq tracks durable events
        let mut s = State::default();
        reduce(&mut s, text_msg("assistant", "a", 10));
        assert_eq!(s.last_seq, 10);
        reduce(&mut s, delta("transient")); // no seq, should not change last_seq
        assert_eq!(s.last_seq, 10);
        reduce(
            &mut s,
            SseFrame::UsageRecorded {
                seq: 11,
                provider: "openai".into(),
                model: "gpt-4".into(),
                usage_kind: "main".into(),
                tokens: WireTokens {
                    input: 10,
                    output: 20,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost_usd: 0.001,
                estimated: false,
            },
        );
        assert_eq!(s.last_seq, 11);
    }

    #[test]
    fn retry_attempt_sets_phase_and_transcript() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        assert_eq!(s.turn_phase, "thinking");
        reduce(
            &mut s,
            SseFrame::RetryAttempt {
                attempt: 1,
                max: 3,
                error: "429".into(),
                delay_ms: 500,
                retry_kind: "provider".into(),
            },
        );
        assert_eq!(s.turn_phase, "retrying");
        assert!(s.turn_status_msg.contains("retry 1/3"));
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("retry 1/3")));
        // streaming stays true during retry
        assert!(s.streaming);
    }

    #[test]
    fn turn_status_phases_sync_streaming() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "thinking".into(),
                message: "".into(),
            },
        );
        assert_eq!(s.turn_phase, "thinking");
        assert!(s.streaming);
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "streaming".into(),
                message: "".into(),
            },
        );
        assert_eq!(s.turn_phase, "streaming");
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "tool".into(),
                message: "running 1 tool(s)".into(),
            },
        );
        assert_eq!(s.turn_phase, "tool");
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "retrying".into(),
                message: "retry".into(),
            },
        );
        assert_eq!(s.turn_phase, "retrying");
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "idle".into(),
                message: "".into(),
            },
        );
        assert_eq!(s.turn_phase, "idle");
        assert!(!s.streaming);
    }

    #[test]
    fn turn_status_failed_marks_failed_and_error() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(
            &mut s,
            SseFrame::Error {
                message: "provider failed: 500".into(),
            },
        );
        assert_eq!(s.turn_phase, "failed");
        assert!(s.turn_status_msg.contains("500"));
        assert!(s.transcript.messages().iter().any(|m| m.role == "error"));
        // TurnStatus failed also pushes error if message present
        let mut s2 = State::default();
        reduce(
            &mut s2,
            SseFrame::TurnStatus {
                phase: "failed".into(),
                message: "mid-stream".into(),
            },
        );
        assert_eq!(s2.turn_phase, "failed");
        assert!(!s2.streaming);
    }

    #[test]
    fn abort_keeps_partial_via_turn_ended() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(&mut s, delta("partial content here"));
        // live_delta holds partial
        assert_eq!(s.transcript.live_delta(), "partial content here");
        reduce(
            &mut s,
            SseFrame::TurnEnded {
                turn: 1,
                stop: "aborted".into(),
            },
        );
        assert_eq!(s.turn_phase, "aborted");
        assert!(!s.streaming);
        // partial should be surfaced as system message with preview, and live_delta cleared
        assert!(s.transcript.live_delta().is_empty());
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.role == "system" && m.content.contains("Aborted")));
    }

    #[test]
    fn truncation_retry_via_turn_status() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::RetryAttempt {
                attempt: 1,
                max: 4,
                error: "truncated".into(),
                delay_ms: 0,
                retry_kind: "truncation".into(),
            },
        );
        assert_eq!(s.turn_phase, "retrying");
        reduce(
            &mut s,
            SseFrame::TurnStatus {
                phase: "retrying".into(),
                message: "truncated — retry 1/4 with 150 lines".into(),
            },
        );
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("truncated")));
    }

    /// 96E-18/96E-19 — the full live tool round-trip must leave a visible tool card:
    /// MessageAppended(assistant+tool_call) creates it, ToolStarted/ToolFinished drive
    /// its status, MessageAppended(tool results) fills the output. Regression: with
    /// durable events never reaching the SSE bus, no card was ever created live.
    #[test]
    fn live_tool_call_roundtrip_creates_card() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(&mut s, tool_call_msg(1, "call-1"));
        // Card exists, pending.
        assert_eq!(
            s.transcript.tool_count(),
            1,
            "tool card must be created from MessageAppended"
        );
        assert_eq!(s.transcript.messages()[0].tools[0].status, "pending");
        // ToolStarted → running + expanded.
        reduce(
            &mut s,
            SseFrame::ToolStarted {
                call_id: "call-1".into(),
                name: "bash".into(),
            },
        );
        let t = &s.transcript.messages()[0].tools[0];
        assert_eq!(t.status, "running");
        assert!(t.expanded, "running tools are expanded");
        // ToolFinished → done, collapsed, output filled by the results message.
        reduce(
            &mut s,
            SseFrame::ToolFinished {
                call_id: "call-1".into(),
                is_error: false,
            },
        );
        reduce(&mut s, tool_result_msg(2, "call-1", "file1\nfile2"));
        let t = &s.transcript.messages()[0].tools[0];
        assert_eq!(t.status, "done");
        assert_eq!(t.output.as_deref(), Some("file1\nfile2"));
        // The tool results message must NOT become a transcript message.
        assert_eq!(s.transcript.message_count(), 1);
    }

    // ── 96E-19 extended reducer coverage ────────────────────────────────────

    #[test]
    fn interaction_request_sets_overlay() {
        let mut s = State::default();
        // The payload shape the plugin actually emits: kn9t-ask-user normalizes
        // its legacy `{question, choices}` args into a typed question before the
        // request ever reaches the TUI, so `type` is always present.
        reduce(
            &mut s,
            SseFrame::InteractionRequest {
                id: 99,
                plugin: "kn9t-ask-user".into(),
                payload: serde_json::json!({
                    "type": "choice",
                    "question": "choose?",
                    "options": [{"label": "a"}, {"label": "b"}],
                }),
            },
        );
        assert_eq!(s.active_interaction_id, Some(99));
        match &s.overlay {
            Some(crate::app::Overlay::Interaction { id, plugin, state }) => {
                assert_eq!(*id, 99);
                assert_eq!(plugin, "kn9t-ask-user");
                match state {
                    crate::app::InteractionState::Choice {
                        question, options, ..
                    } => {
                        assert_eq!(question, "choose?");
                        assert_eq!(options.len(), 2);
                        assert_eq!(options[0].label, "a");
                    }
                    other => panic!("expected Choice, got {other:?}"),
                }
            }
            other => panic!("expected Interaction overlay, got {:?}", other),
        }
    }

    #[test]
    fn interaction_request_without_type_falls_back_to_text() {
        // An untyped payload that still carries a question must not silently
        // become Generic; it degrades to a text prompt.
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::InteractionRequest {
                id: 7,
                plugin: "other".into(),
                payload: serde_json::json!({"question": "free form?"}),
            },
        );
        match &s.overlay {
            Some(crate::app::Overlay::Interaction { state, .. }) => {
                assert!(matches!(state, crate::app::InteractionState::Text { .. }));
            }
            other => panic!("expected Interaction overlay, got {:?}", other),
        }
    }

    #[test]
    fn interaction_request_replaces_approval_overlay() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::ApprovalRequest {
                id: 1,
                tool: "bash".into(),
                args: serde_json::json!({}),
                cwd: "/tmp".into(),
            },
        );
        assert!(matches!(
            s.overlay,
            Some(crate::app::Overlay::Approval { .. })
        ));
        reduce(
            &mut s,
            SseFrame::InteractionRequest {
                id: 2,
                plugin: "p".into(),
                payload: serde_json::json!({"q":"?"}),
            },
        );
        assert_eq!(s.active_interaction_id, Some(2));
        assert!(matches!(
            s.overlay,
            Some(crate::app::Overlay::Interaction { .. })
        ));
    }

    #[test]
    fn plugin_notification_pushes_message() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::PluginNotification {
                plugin: "kn9t-tools".into(),
                message: "hello from plugin".into(),
            },
        );
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("hello from plugin")));
    }

    #[test]
    fn hook_failed_is_noop_and_does_not_crash() {
        let mut s = State::default();
        reduce(&mut s, text_msg("assistant", "before", 1));
        let count = s.transcript.message_count();
        reduce(
            &mut s,
            SseFrame::HookFailed {
                plugin: "p".into(),
                hook: "before_tool_call".into(),
                reason: "oops".into(),
            },
        );
        assert_eq!(s.transcript.message_count(), count);
    }

    #[test]
    fn tool_progress_updates_card() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(&mut s, tool_call_msg(1, "c-progress"));
        reduce(
            &mut s,
            SseFrame::ToolProgress {
                call_id: "c-progress".into(),
                note: "step 1".into(),
            },
        );
        let t = &s.transcript.messages()[0].tools[0];
        assert!(t.progress_lines.iter().any(|l| l.contains("step 1")));
        assert!(t.status.contains("step 1"));
        reduce(
            &mut s,
            SseFrame::ToolProgress {
                call_id: "c-progress".into(),
                note: "step 2".into(),
            },
        );
        assert_eq!(s.transcript.messages()[0].tools[0].progress_lines.len(), 2);
    }

    #[test]
    fn tool_args_delta_is_ignored_but_not_crashing() {
        let mut s = State::default();
        reduce(&mut s, tool_call_msg(1, "c1"));
        reduce(
            &mut s,
            SseFrame::ToolArgsDelta {
                msg_id: "m1".into(),
                idx: 0,
                delta: "{\"cmd\"".into(),
            },
        );
        assert_eq!(s.transcript.tool_count(), 1);
    }

    #[test]
    fn usage_recorded_updates_tokens() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::UsageRecorded {
                seq: 10,
                provider: "openai".into(),
                model: "gpt-4".into(),
                usage_kind: "main".into(),
                tokens: WireTokens {
                    input: 5,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost_usd: 0.001,
                estimated: false,
            },
        );
        assert_eq!(s.last_seq, 10);
        assert_eq!(s.tokens.tokens_in(), 5);
        assert_eq!(s.tokens.tokens_out(), 10);
    }

    #[test]
    fn error_frame_sets_failed_and_message() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(
            &mut s,
            SseFrame::Error {
                message: "boom".into(),
            },
        );
        assert_eq!(s.turn_phase, "failed");
        assert!(s.turn_status_msg.contains("boom"));
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.role == "error" && m.content.contains("boom")));
    }

    #[test]
    fn compacted_empty_summary_fallback() {
        let mut s = State::default();
        let empty = WireMessage {
            id: "e".into(),
            role: "assistant".into(),
            content: vec![],
            silent: false,
        };
        reduce(
            &mut s,
            SseFrame::Compacted {
                seq: 5,
                replaced: WireSeqRange { start: 1, end: 2 },
                summary: empty,
            },
        );
        assert!(s
            .transcript
            .messages()
            .iter()
            .any(|m| m.content.contains("Conversation compacted")));
    }

    #[test]
    fn silent_user_message_is_ignored() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::MessageAppended {
                seq: 1,
                msg: WireMessage {
                    id: "m1".into(),
                    role: "user".into(),
                    content: vec![WireContent::Text {
                        text: "hidden".into(),
                    }],
                    silent: true,
                },
            },
        );
        assert_eq!(
            s.transcript.message_count(),
            0,
            "silent user message must not create transcript entry"
        );
    }

    #[test]
    fn tool_finished_error_marks_card_error() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::TurnStarted { turn: 1 });
        reduce(&mut s, tool_call_msg(1, "c-err"));
        reduce(
            &mut s,
            SseFrame::ToolFinished {
                call_id: "c-err".into(),
                is_error: true,
            },
        );
        assert_eq!(s.transcript.messages()[0].tools[0].status, "error");
        assert!(!s.transcript.messages()[0].tools[0].expanded);
    }

    #[test]
    fn ui_directive_is_recorded_and_plugin_notification_unaffected() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p".into(),
                target: "sidebar".into(),
                op: "show".into(),
                payload: serde_json::json!({"panel":"x"}),
            },
        );
        assert_eq!(s.ui_directives.len(), 1);
        assert_eq!(s.ui_directives[0].0, "p");
        assert_eq!(s.ui_directives[0].1, "sidebar");
        // Transcript must NOT have been polluted — PluginNotification is separate
        assert_eq!(s.transcript.message_count(), 0);
        // PluginNotification still pushes text
        reduce(
            &mut s,
            SseFrame::PluginNotification {
                plugin: "p".into(),
                message: "hello".into(),
            },
        );
        assert_eq!(s.transcript.message_count(), 1);
        assert_eq!(
            s.ui_directives.len(),
            1,
            "PluginNotification must not affect ui_directives"
        );
    }

    #[test]
    fn ui_directive_payload_opaque_not_interpreted() {
        let mut s = State::default();
        let complex = serde_json::json!({"fields":[{"name":"age","type":"number"}],"title":"hi","arr":[1,2,3]});
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p".into(),
                target: "t".into(),
                op: "render".into(),
                payload: complex.clone(),
            },
        );
        assert_eq!(s.ui_directives[0].3, complex);
    }

    // ── 96E-25 page lifecycle ─────────────────────────────────────────────

    #[test]
    fn page_declare_write_clear_lifecycle() {
        let mut s = State::default();
        // Declare page via UiDirective (target=page_id, op=declare_page)
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "my-plugin".into(),
                target: "dash".into(),
                op: "declare_page".into(),
                payload: serde_json::json!({"page_id":"dash","layout":[{"placeholder_id":"status","kind":"text","default":"idle"},{"placeholder_id":"prog","kind":"bar","default":0}]}),
            },
        );
        assert_eq!(s.ui_pages.len(), 1);
        let key = ("my-plugin".to_string(), "dash".to_string());
        assert!(s.ui_pages.contains_key(&key));
        assert_eq!(s.ui_page_selected, Some(key.clone()));
        // Cheap write: only one placeholder
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "my-plugin".into(),
                target: "dash".into(),
                op: "write_placeholder".into(),
                payload: serde_json::json!({"page_id":"dash","placeholder_id":"prog","value":55}),
            },
        );
        let page = s.ui_pages.get(&key).unwrap();
        assert_eq!(
            page.placeholders.get("prog").unwrap().value,
            serde_json::json!(55)
        );
        // Status must not have changed
        assert_eq!(
            page.placeholders.get("status").unwrap().value,
            serde_json::json!("idle")
        );
        // Clear
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "my-plugin".into(),
                target: "dash".into(),
                op: "clear_page".into(),
                payload: serde_json::json!({"page_id":"dash"}),
            },
        );
        assert!(s.ui_pages.is_empty());
        assert!(s.ui_page_selected.is_none());
    }

    #[test]
    fn page_multiple_concurrent_dont_collide() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p1".into(),
                target: "a".into(),
                op: "declare_page".into(),
                payload: serde_json::json!({"page_id":"a","layout":[{"placeholder_id":"x","kind":"text"}]}),
            },
        );
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p2".into(),
                target: "b".into(),
                op: "declare_page".into(),
                payload: serde_json::json!({"page_id":"b","layout":[{"placeholder_id":"y","kind":"number"}]}),
            },
        );
        assert_eq!(s.ui_pages.len(), 2);
        // Both keys present, no collision despite same placeholder_id name would be different pages
        assert!(s
            .ui_pages
            .contains_key(&("p1".to_string(), "a".to_string())));
        assert!(s
            .ui_pages
            .contains_key(&("p2".to_string(), "b".to_string())));
        // Write to one does not affect other
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p1".into(),
                target: "a".into(),
                op: "write_placeholder".into(),
                payload: serde_json::json!({"page_id":"a","placeholder_id":"x","value":"hello"}),
            },
        );
        assert_eq!(
            s.ui_pages
                .get(&("p2".to_string(), "b".to_string()))
                .unwrap()
                .placeholders
                .get("y")
                .unwrap()
                .value,
            serde_json::json!(0)
        );
    }

    #[test]
    fn page_write_without_full_rerender_preserves_other_placeholders() {
        let mut s = State::default();
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p".into(),
                target: "pg".into(),
                op: "declare_page".into(),
                payload: serde_json::json!({"page_id":"pg","layout":[{"placeholder_id":"t1","kind":"text","default":"a"},{"placeholder_id":"t2","kind":"text","default":"b"}]}),
            },
        );
        let key = ("p".to_string(), "pg".to_string());
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p".into(),
                target: "pg".into(),
                op: "write_placeholder".into(),
                payload: serde_json::json!({"page_id":"pg","placeholder_id":"t1","value":"new_a"}),
            },
        );
        let page = s.ui_pages.get(&key).unwrap();
        assert_eq!(
            page.placeholders.get("t1").unwrap().value,
            serde_json::json!("new_a")
        );
        assert_eq!(
            page.placeholders.get("t2").unwrap().value,
            serde_json::json!("b"),
            "other placeholder must not be clobbered by cheap write"
        );
    }

    // ── 96E-27 subagent collapsible + attach ──────────────────────────────────

    fn spawn_call_msg(seq: u64, call_id: &str, visibility: &str) -> SseFrame {
        SseFrame::MessageAppended {
            seq,
            msg: WireMessage {
                id: format!("m{}", seq),
                role: "assistant".into(),
                content: vec![WireContent::ToolCall {
                    id: call_id.into(),
                    name: "spawn_subagent".into(),
                    args_json: format!(r#"{{"task":"do X","visibility":"{}"}}"#, visibility),
                }],
                silent: false,
            },
        }
    }

    #[test]
    fn subagent_collapsible_under_spawning_tool_call() {
        let mut s = State::default();
        reduce(&mut s, spawn_call_msg(1, "c1", "progress"));
        assert_eq!(s.subagents.len(), 1);
        let e = &s.subagents[0];
        assert_eq!(e.call_id, "c1");
        assert_eq!(e.visibility, "progress");
        assert!(e.collapsed, "progress should be collapsed by default");
        // Must be rendered as sub-entry, not a separate panel — check subagents vector is the source for transcript rendering
        assert_eq!(
            s.ui_pages.len(),
            0,
            "no separate page panel should be created for subagent's collapsed view"
        );
    }

    #[test]
    fn subagent_visibility_distinct_rendering() {
        let mut s = State::default();
        reduce(&mut s, spawn_call_msg(1, "c-silent", "silent"));
        assert!(s.subagents[0].collapsed, "silent collapsed");
        // silent should not expand to show page even when one is declared — still one-liner
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "p".into(),
                target: "pg".into(),
                op: "declare_page".into(),
                payload: serde_json::json!({"page_id":"pg","layout":[{"placeholder_id":"status","kind":"text"}]}),
            },
        );
        // page is still declared side-panel wise, but subagent's collapsed flag stays true (one-liner)
        assert!(s.subagents[0].collapsed);

        let mut s2 = State::default();
        reduce(&mut s2, spawn_call_msg(1, "c-full", "full"));
        assert!(!s2.subagents[0].collapsed, "full should start expanded");

        let mut s3 = State::default();
        reduce(&mut s3, spawn_call_msg(1, "c-prog", "progress"));
        assert!(s3.subagents[0].collapsed, "progress collapsed by default");
    }

    #[test]
    fn subagent_toggle_collapse_and_attach() {
        let mut s = State::default();
        reduce(&mut s, spawn_call_msg(1, "c1", "progress"));
        assert!(s.subagents[0].collapsed);
        s.toggle_subagent("c1");
        assert!(!s.subagents[0].collapsed, "toggle should expand");
        s.toggle_subagent("c1");
        assert!(s.subagents[0].collapsed, "toggle should collapse again");
        // attach: opens full transcript on demand via session_read, without altering parent's default view
        let transcript = vec![crate::wire::TranscriptMessage {
            role: "assistant".into(),
            content: serde_json::json!("hello"),
            silent: false,
        }];
        s.attach_subagent("c1", transcript.clone());
        assert_eq!(s.attached_subagent.as_ref().unwrap().0, "c1");
        assert_eq!(s.attached_subagent.as_ref().unwrap().1.len(), 1);
        // Parent transcript unchanged
        assert_eq!(
            s.transcript.message_count(),
            1,
            "parent's tool call still one message"
        );
        s.detach_subagent();
        assert!(s.attached_subagent.is_none());
    }

    #[test]
    fn subagent_multiple_concurrent_independent() {
        let mut s = State::default();
        reduce(&mut s, spawn_call_msg(1, "c1", "progress"));
        reduce(&mut s, spawn_call_msg(2, "c2", "progress"));
        reduce(&mut s, spawn_call_msg(3, "c3", "full"));
        assert_eq!(s.subagents.len(), 3);
        // Toggle one does not affect others
        s.toggle_subagent("c1");
        assert!(
            !s.subagents
                .iter()
                .find(|e| e.call_id == "c1")
                .unwrap()
                .collapsed
        );
        assert!(
            s.subagents
                .iter()
                .find(|e| e.call_id == "c2")
                .unwrap()
                .collapsed
        );
        assert!(
            !s.subagents
                .iter()
                .find(|e| e.call_id == "c3")
                .unwrap()
                .collapsed,
            "c3 full stays expanded"
        );
        // Attach one does not affect others
        s.attach_subagent("c2", vec![]);
        assert_eq!(s.attached_subagent.as_ref().unwrap().0, "c2");
    }
}
