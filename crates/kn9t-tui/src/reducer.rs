//! Pure SSE reducer — `(state, frame) -> state`, no `&mut self`, no terminal, no I/O.
//!
//! Phase 4.4a: extract a pure reducer so `handle_sse` (the most important function
//! in the crate, previously 0 tests) is testable by constructing only `State`.
//! The interface is the test surface — a pure reducer would have caught F5 and F7 immediately.
//!
//! Handles the three frames previously ignored: `ThinkingDelta`, `ModelChanged`, `Compacted`
//! (only their `seq` was recorded). `Compacted` especially — the transcript now reflects a compaction.

use crate::message_handler::{Message, ToolCard};
use crate::model_selector::ModelSelector;
use crate::session_manager::SessionEntry;
use crate::token_tracker::{TokenCounts, TokenTracker};
use crate::wire::SseFrame;
use crate::app::Overlay;

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
    pub overlay: Option<Overlay>,
    pub session_id: String,
    pub session_title: Option<String>,
    pub sessions: Vec<SessionEntry>,
    pub model_sel: ModelSelector,
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
            overlay: None,
            session_id: String::new(),
            session_title: None,
            sessions: Vec::new(),
            model_sel: ModelSelector::new(),
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
                    state.transcript.push(Message::new("system", format!("Aborted — kept partial ({} chars): {}", partial.len(), preview)));
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
            if state.turn_phase == "thinking" { state.turn_phase = "streaming".into(); }
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
            let tools: Vec<ToolCard> = tool_calls.iter().map(|(id, name, args)| ToolCard {
                call_id: id.clone(),
                name: name.clone(),
                args: args.clone(),
                status: "pending".into(),
                output: None,
                progress_lines: Vec::new(),
                expanded: false,
                active_tab: crate::message_handler::ToolTab::Input,
                scroll_offset: 0,
            }).collect();
            if !final_content.is_empty() || !tools.is_empty() {
                state.transcript.push(Message::new(&msg.role, final_content).with_tools(tools));
            }
        }
        SseFrame::UsageRecorded { tokens, cost_usd, usage_kind, .. } => {
            let counts = TokenCounts::new(tokens.input as usize, tokens.output as usize, tokens.cache_read as usize, tokens.cache_write as usize);
            state.tokens.record_usage(counts, cost_usd, usage_kind == "title");
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
        SseFrame::ToolFinished { call_id, is_error, .. } => {
            state.transcript.update_tool(&call_id, |tool| {
                tool.status = if is_error { "error".into() } else { "done".into() };
                tool.active_tab = crate::message_handler::ToolTab::Output;
                tool.expanded = false;
                tool.scroll_offset = 0;
            });
        }
        SseFrame::ApprovalRequest { id, tool, args, .. } => {
            state.active_approval_id = Some(id);
            state.overlay = Some(Overlay::Approval { tool, args: serde_json::to_string(&args).unwrap_or_default(), selected: 0 });
        }
        SseFrame::ModelChanged { model, .. } => {
            let name = format!("{}:{}", model.provider, model.id);
            if let Some(idx) = state.model_sel.models().iter().position(|m| m.provider == model.provider && m.id == model.id) {
                state.model_sel.set_selected(idx);
            }
            state.transcript.push(Message::new("system", format!("Model changed to {}", name)));
        }
        SseFrame::Compacted { replaced, summary, .. } => {
            let (text, _, _, _) = extract_message_content(&summary.content);
            let summary_text = if text.is_empty() { "Conversation compacted.".to_string() } else { text };
            state.transcript.push(Message::new("system", format!("Compacted {}..{}: {}", replaced.start, replaced.end, summary_text.clone())));
            state.transcript.push(Message::new(&summary.role, summary_text));
        }
        SseFrame::Error { message } => {
            state.turn_phase = "failed".into();
            state.turn_status_msg = message.clone();
            state.transcript.push(Message::new("error", message));
        }
        SseFrame::RetryAttempt { attempt, max, error, delay_ms, retry_kind } => {
            state.turn_phase = "retrying".into();
            state.turn_status_msg = format!("retry {}/{} {} in {}ms: {}", attempt, max, retry_kind, delay_ms, error);
            state.transcript.push(Message::new("system", format!("↻ retry {}/{} {} in {}ms: {}", attempt, max, retry_kind, delay_ms, error)));
        }
        SseFrame::TurnStatus { phase, message } => {
            state.turn_phase = phase.clone();
            state.turn_status_msg = message.clone();
            if phase == "failed" && !message.is_empty() {
                state.transcript.push(Message::new("error", format!("turn {}: {}", phase, message)));
            } else if phase == "retrying" && !message.is_empty() {
                if !state.transcript.messages().last().map(|m| m.content.contains(&message)).unwrap_or(false) {
                    state.transcript.push(Message::new("system", message.clone()));
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
        SseFrame::HookFailed { .. } => {}
    }
}

fn extract_message_content(content: &[crate::wire::WireContent]) -> (String, Vec<(String, String, String)>, Vec<(String, String, bool)>, usize) {
    use crate::wire::WireContent;
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();
    let mut image_count = 0;
    for c in content {
        match c {
            WireContent::Text { text } => text_parts.push(text.as_str()),
            WireContent::Thinking { text } => text_parts.push(text.as_str()),
            WireContent::ToolCall { id, name, args_json } => tool_calls.push((id.clone(), name.clone(), args_json.clone())),
            WireContent::ToolResult { id, content: rc, is_error } => {
                let output: String = rc.iter().filter_map(|x| match x { WireContent::Text { text } => Some(text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
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
    use crate::wire::{WireContent, WireMessage, WireTokens, WireSeqRange, WireModelRef};

    fn text_msg(role: &str, text: &str, seq: u64) -> SseFrame {
        SseFrame::MessageAppended { seq, msg: WireMessage { id: format!("m{}", seq), role: role.into(), content: vec![WireContent::Text { text: text.into() }], silent: false } }
    }

    fn delta(text: &str) -> SseFrame {
        SseFrame::TextDelta { msg_id: "m1".into(), idx: 0, delta: text.into() }
    }

    fn thinking_delta(text: &str) -> SseFrame {
        SseFrame::ThinkingDelta { msg_id: "m1".into(), idx: 0, delta: text.into() }
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
        reduce(&mut s, SseFrame::TurnEnded { turn: 1, stop: "stop".into() });
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
        reduce(&mut s, SseFrame::ModelChanged { seq: 5, model: WireModelRef { provider: "openai".into(), id: "gpt-4".into() } });
        assert_eq!(s.last_seq, 5);
        // Should push a system message
        assert!(s.transcript.messages().iter().any(|m| m.content.contains("Model changed")));
    }

    #[test]
    fn compacted_handled() {
        let mut s = State::default();
        reduce(&mut s, text_msg("assistant", "old 1", 1));
        reduce(&mut s, text_msg("assistant", "old 2", 2));
        let summary = WireMessage { id: "sum".into(), role: "assistant".into(), content: vec![WireContent::Text { text: "summary".into() }], silent: false };
        reduce(&mut s, SseFrame::Compacted { seq: 3, replaced: WireSeqRange { start: 1, end: 2 }, summary });
        assert_eq!(s.last_seq, 3);
        // Compacted should push two messages (system note + summary)
        assert!(s.transcript.messages().iter().any(|m| m.content.contains("Compacted")));
        assert!(s.transcript.messages().iter().any(|m| m.content == "summary"));
    }

    #[test]
    fn approval_request_sets_overlay() {
        let mut s = State::default();
        reduce(&mut s, SseFrame::ApprovalRequest { id: 42, tool: "bash".into(), args: serde_json::json!({"cmd":"ls"}), cwd: "/tmp".into() });
        assert_eq!(s.active_approval_id, Some(42));
        assert!(matches!(s.overlay, Some(Overlay::Approval { .. })));
    }

    #[test]
    fn title_changed_updates_session() {
        let mut s = State::default();
        s.session_id = "sess1".into();
        s.sessions.push(SessionEntry { id: "sess1".into(), name: "Old".into(), running: false, created_at: None });
        reduce(&mut s, SseFrame::TitleChanged { title: "New Title".into() });
        assert_eq!(s.session_title.as_deref(), Some("New Title"));
        assert_eq!(s.sessions[0].name, "New Title");
    }

    #[test]
    fn compacted_seq_recorded() {
        let mut s = State::default();
        let summary = WireMessage { id: "sum".into(), role: "assistant".into(), content: vec![], silent: false };
        reduce(&mut s, SseFrame::Compacted { seq: 99, replaced: WireSeqRange { start: 10, end: 20 }, summary });
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
        reduce(&mut s, SseFrame::UsageRecorded { seq: 11, provider: "openai".into(), model: "gpt-4".into(), usage_kind: "main".into(), tokens: WireTokens { input: 10, output: 20, cache_read: 0, cache_write: 0, reasoning: 0 }, cost_usd: 0.001, estimated: false });
        assert_eq!(s.last_seq, 11);
    }
}
