//! SSE frame fixtures for testing the reducer and other TUI components.

use kn9t_tui::wire::{SseFrame, WireContent, WireMessage, WireModelRef, WireSeqRange, WireTokens};

/// Create a MessageAppended frame with text content.
pub fn text_msg(role: &str, text: &str, seq: u64) -> SseFrame {
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

/// Create a TextDelta frame.
pub fn delta(text: &str) -> SseFrame {
    SseFrame::TextDelta {
        msg_id: "m1".into(),
        idx: 0,
        delta: text.into(),
    }
}

/// Create a TextDelta frame with custom message ID and index.
pub fn delta_at(msg_id: &str, idx: u32, text: &str) -> SseFrame {
    SseFrame::TextDelta {
        msg_id: msg_id.into(),
        idx,
        delta: text.into(),
    }
}

/// Create a ThinkingDelta frame.
pub fn thinking_delta(text: &str) -> SseFrame {
    SseFrame::ThinkingDelta {
        msg_id: "m1".into(),
        idx: 0,
        delta: text.into(),
    }
}

/// Create a MessageAppended frame with a tool call.
pub fn tool_call_msg(seq: u64, call_id: &str) -> SseFrame {
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

/// Create a MessageAppended frame with a tool call (custom name and args).
pub fn tool_call_msg_full(seq: u64, call_id: &str, name: &str, args_json: &str) -> SseFrame {
    SseFrame::MessageAppended {
        seq,
        msg: WireMessage {
            id: format!("m{}", seq),
            role: "assistant".into(),
            content: vec![WireContent::ToolCall {
                id: call_id.into(),
                name: name.into(),
                args_json: args_json.into(),
            }],
            silent: false,
        },
    }
}

/// Create a MessageAppended frame with a tool result.
pub fn tool_result_msg(seq: u64, call_id: &str, output: &str) -> SseFrame {
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

/// Create a MessageAppended frame with an error tool result.
pub fn tool_error_msg(seq: u64, call_id: &str, error: &str) -> SseFrame {
    SseFrame::MessageAppended {
        seq,
        msg: WireMessage {
            id: format!("m{}", seq),
            role: "tool".into(),
            content: vec![WireContent::ToolResult {
                id: call_id.into(),
                content: vec![WireContent::Text { text: error.into() }],
                is_error: true,
            }],
            silent: false,
        },
    }
}

/// Create a TurnStarted frame.
pub fn turn_started(turn: u32) -> SseFrame {
    SseFrame::TurnStarted { turn }
}

/// Create a TurnEnded frame.
pub fn turn_ended(turn: u32, stop: &str) -> SseFrame {
    SseFrame::TurnEnded {
        turn,
        stop: stop.into(),
    }
}

/// Create a ModelChanged frame.
pub fn model_changed(provider: &str, id: &str, seq: u64) -> SseFrame {
    SseFrame::ModelChanged {
        seq,
        model: WireModelRef {
            provider: provider.into(),
            id: id.into(),
        },
    }
}

/// Create a UsageRecorded frame with default tokens.
pub fn usage_recorded(seq: u64, input: u64, output: u64) -> SseFrame {
    SseFrame::UsageRecorded {
        seq,
        cost_usd: 0.0,
        estimated: false,
        model: "test-model".into(),
        provider: "test".into(),
        usage_kind: "message".into(),
        tokens: WireTokens {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
    }
}

/// Create a Compacted frame.
pub fn compacted(seq: u64, start: u64, end: u64, summary_text: &str) -> SseFrame {
    SseFrame::Compacted {
        seq,
        replaced: WireSeqRange { start, end },
        summary: WireMessage {
            id: format!("summary{}", seq),
            role: "assistant".into(),
            content: vec![WireContent::Text {
                text: summary_text.into(),
            }],
            silent: false,
        },
    }
}

/// Create an ApprovalRequest frame.
pub fn approval_request(id: u64, tool: &str) -> SseFrame {
    SseFrame::ApprovalRequest {
        id,
        tool: tool.into(),
        args: serde_json::json!({}),
        cwd: "/tmp".into(),
    }
}

/// Create a TitleChanged frame.
pub fn title_changed(title: &str) -> SseFrame {
    SseFrame::TitleChanged {
        title: title.into(),
    }
}

/// Create a RetryAttempt frame.
pub fn retry_attempt(attempt: u32, max: u32, error: &str) -> SseFrame {
    SseFrame::RetryAttempt {
        attempt,
        max,
        error: error.into(),
        delay_ms: 1000,
        retry_kind: "truncated".into(),
    }
}

/// Create a TurnStatus frame.
pub fn turn_status(phase: &str, message: &str) -> SseFrame {
    SseFrame::TurnStatus {
        phase: phase.into(),
        message: message.into(),
    }
}

/// Create an Error frame.
pub fn error_frame(message: &str) -> SseFrame {
    SseFrame::Error {
        message: message.into(),
    }
}

/// Create a ToolStarted frame.
pub fn tool_started(call_id: &str, name: &str) -> SseFrame {
    SseFrame::ToolStarted {
        call_id: call_id.into(),
        name: name.into(),
    }
}

/// Create a ToolProgress frame.
pub fn tool_progress(call_id: &str, note: &str) -> SseFrame {
    SseFrame::ToolProgress {
        call_id: call_id.into(),
        note: note.into(),
    }
}

/// Create a ToolFinished frame (success).
pub fn tool_finished(call_id: &str) -> SseFrame {
    SseFrame::ToolFinished {
        call_id: call_id.into(),
        is_error: false,
    }
}

/// Create a ToolFinished frame (error).
pub fn tool_finished_error(call_id: &str) -> SseFrame {
    SseFrame::ToolFinished {
        call_id: call_id.into(),
        is_error: true,
    }
}

/// Create a ToolsToggled frame.
pub fn tools_toggled(seq: u64, disabled: Vec<&str>) -> SseFrame {
    SseFrame::ToolsToggled {
        seq,
        disabled: disabled.into_iter().map(String::from).collect(),
    }
}

/// Create a PluginNotification frame.
pub fn plugin_notification(plugin: &str, message: &str) -> SseFrame {
    SseFrame::PluginNotification {
        plugin: plugin.into(),
        message: message.into(),
    }
}

/// Create a HookFailed frame.
pub fn hook_failed(hook: &str, plugin: &str, reason: &str) -> SseFrame {
    SseFrame::HookFailed {
        hook: hook.into(),
        plugin: plugin.into(),
        reason: reason.into(),
    }
}

/// Create a ToolArgsDelta frame.
pub fn tool_args_delta(msg_id: &str, idx: u32, delta: &str) -> SseFrame {
    SseFrame::ToolArgsDelta {
        msg_id: msg_id.into(),
        idx,
        delta: delta.into(),
    }
}

/// Create a UiDirective frame.
pub fn ui_directive(plugin: &str, target: &str, op: &str, payload: serde_json::Value) -> SseFrame {
    SseFrame::UiDirective {
        plugin: plugin.into(),
        target: target.into(),
        op: op.into(),
        payload,
    }
}

/// Create an InteractionRequest frame.
pub fn interaction_request(id: u64, plugin: &str, payload: serde_json::Value) -> SseFrame {
    SseFrame::InteractionRequest {
        id,
        plugin: plugin.into(),
        payload,
    }
}

/// Create a PluginDeclared frame.
pub fn plugin_declared(plugin: &str, tools_added: Vec<&str>) -> SseFrame {
    SseFrame::PluginDeclared {
        plugin: plugin.into(),
        tools_added: tools_added.into_iter().map(String::from).collect(),
        tools_removed: vec![],
    }
}
