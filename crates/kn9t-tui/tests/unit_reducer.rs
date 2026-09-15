// Extracted from src/reducer.rs — the #[cfg(test)] mod tests block.
// These are pure unit tests for the SSE reducer: no terminal, no I/O.
// See src/reducer.rs module-level doc for the test strategy (96E-19).

#![allow(clippy::unwrap_used)]

use kn9t_tui::app::{InteractionState, Overlay};
use kn9t_tui::reducer::{PluginLuaOp, PluginPlacement, State, reduce};
use kn9t_tui::wire::{SseFrame, WireContent, WireMessage, WireSeqRange, WireTokens};
use kn9t_tui_test_support::*;

// ── local helper not in test-support ─────────────────────────────────────────

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

// ── tests ─────────────────────────────────────────────────────────────────────

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
            model: kn9t_tui::wire::WireModelRef {
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
    // Compacted should push the summary as an assistant message (no header/prefix)
    let msgs = s.transcript.messages();
    let summary_msg = msgs.iter().find(|m| m.content == "summary");
    assert!(summary_msg.is_some(), "should have the summary message");
    assert_eq!(summary_msg.unwrap().role, "assistant");
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
    s.sessions.push(kn9t_tui::session_manager::SessionEntry {
        id: "sess1".into(),
        name: "Old".into(),
        running: false,
        created_at: None,
        ..Default::default()
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

// ── 96E-19 extended reducer coverage ─────────────────────────────────────────

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
        Some(Overlay::Interaction { id, plugin, state }) => {
            assert_eq!(*id, 99);
            assert_eq!(plugin, "kn9t-ask-user");
            match state {
                InteractionState::Choice {
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
        Some(Overlay::Interaction { state, .. }) => {
            assert!(matches!(state, InteractionState::Text { .. }));
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
    assert!(matches!(s.overlay, Some(Overlay::Approval { .. })));
    reduce(
        &mut s,
        SseFrame::InteractionRequest {
            id: 2,
            plugin: "p".into(),
            payload: serde_json::json!({"q":"?"}),
        },
    );
    assert_eq!(s.active_interaction_id, Some(2));
    assert!(matches!(s.overlay, Some(Overlay::Interaction { .. })));
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
fn compacted_empty_summary_adds_nothing() {
    let mut s = State::default();
    let empty = WireMessage {
        id: "e".into(),
        role: "assistant".into(),
        content: vec![],
        silent: false,
    };
    let before = s.transcript.message_count();
    reduce(
        &mut s,
        SseFrame::Compacted {
            seq: 5,
            replaced: WireSeqRange { start: 1, end: 2 },
            summary: empty,
        },
    );
    // Empty summary adds no message — "Compaction started..." was already shown
    assert_eq!(s.transcript.message_count(), before);
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

// ── plugin Lua UI ops ─────────────────────────────────────────────────────────

/// `register_lua` must be queued verbatim: the reducer is pure over State
/// and cannot load Lua, so the source has to survive to the apply step.
#[test]
fn register_lua_is_queued_with_source() {
    let mut s = State::default();
    let src = r#"function render(st) return {type="text",content="x"} end"#;
    reduce(
        &mut s,
        SseFrame::UiDirective {
            plugin: "demo".into(),
            target: "lua".into(),
            op: "register_lua".into(),
            payload: serde_json::json!({"source": src}),
        },
    );
    assert_eq!(s.plugin_lua_pending.len(), 1);
    match &s.plugin_lua_pending[0] {
        PluginLuaOp::Register {
            plugin,
            source,
            placement,
        } => {
            assert_eq!(plugin, "demo");
            assert_eq!(source, src);
            // No placement fields in the payload: the plugin said nothing,
            // so every hint stays None and the config decides.
            assert_eq!(placement, &PluginPlacement::default());
        }
        other => panic!("expected Register, got {other:?}"),
    }
}

/// State is opaque JSON: the reducer must not reshape or validate it, since
/// only the plugin's own Lua knows what it means.
#[test]
fn set_state_is_queued_verbatim() {
    let mut s = State::default();
    let st = serde_json::json!({"items": [1, 2], "deep": {"ok": true}});
    reduce(
        &mut s,
        SseFrame::UiDirective {
            plugin: "demo".into(),
            target: "lua".into(),
            op: "set_state".into(),
            payload: serde_json::json!({"state": st}),
        },
    );
    match &s.plugin_lua_pending[0] {
        PluginLuaOp::SetState { plugin, state } => {
            assert_eq!(plugin, "demo");
            assert_eq!(state, &st);
        }
        other => panic!("expected SetState, got {other:?}"),
    }
}

#[test]
fn clear_is_queued() {
    let mut s = State::default();
    reduce(
        &mut s,
        SseFrame::UiDirective {
            plugin: "demo".into(),
            target: "lua".into(),
            op: "clear".into(),
            payload: serde_json::json!({}),
        },
    );
    match &s.plugin_lua_pending[0] {
        PluginLuaOp::Clear { plugin } => assert_eq!(plugin, "demo"),
        other => panic!("expected Clear, got {other:?}"),
    }
}

/// Ops from different plugins must stay separate and ordered, so one
/// plugin's update cannot be attributed to another.
#[test]
fn ops_from_multiple_plugins_stay_attributed_and_ordered() {
    let mut s = State::default();
    for name in ["a", "b"] {
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: name.into(),
                target: "lua".into(),
                op: "set_state".into(),
                payload: serde_json::json!({"state": {"who": name}}),
            },
        );
    }
    let names: Vec<&str> = s
        .plugin_lua_pending
        .iter()
        .map(|o| match o {
            PluginLuaOp::SetState { plugin, .. } => plugin.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(names, vec!["a", "b"]);
}

/// A malformed directive must be dropped, not queued half-built.
#[test]
fn malformed_ops_are_ignored() {
    let mut s = State::default();
    // register_lua without source, set_state without state.
    reduce(
        &mut s,
        SseFrame::UiDirective {
            plugin: "demo".into(),
            target: "lua".into(),
            op: "register_lua".into(),
            payload: serde_json::json!({}),
        },
    );
    reduce(
        &mut s,
        SseFrame::UiDirective {
            plugin: "demo".into(),
            target: "lua".into(),
            op: "set_state".into(),
            payload: serde_json::json!({}),
        },
    );
    assert!(s.plugin_lua_pending.is_empty());
}

/// The removed placeholder API must not silently queue anything.
#[test]
fn removed_page_ops_are_ignored() {
    let mut s = State::default();
    for op in ["declare_page", "write_placeholder", "clear_page"] {
        reduce(
            &mut s,
            SseFrame::UiDirective {
                plugin: "demo".into(),
                target: "pg".into(),
                op: op.into(),
                payload: serde_json::json!({"layout": []}),
            },
        );
    }
    assert!(s.plugin_lua_pending.is_empty());
}

// ── subagent tests ────────────────────────────────────────────────────────────

#[test]
fn subagent_collapsible_under_spawning_tool_call() {
    let mut s = State::default();
    reduce(&mut s, spawn_call_msg(1, "c1", "progress"));
    assert_eq!(s.subagents.len(), 1);
    let e = &s.subagents[0];
    assert_eq!(e.call_id, "c1");
    assert_eq!(e.visibility, "progress");
    assert!(e.collapsed, "progress should be collapsed by default");
    // Rendered as a sub-entry of the spawning tool call. Spawning a subagent
    // must not register any plugin UI of its own; a subagent plugin that
    // wants a display ships its own Lua like any other plugin.
    assert!(
        s.plugin_lua_pending.is_empty(),
        "spawning a subagent must not create a plugin UI"
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
    let transcript = vec![kn9t_tui::wire::TranscriptMessage {
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

