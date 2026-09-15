//! Event type name helpers for assertions.

use kn9t_core::{Event, LiveEvent};

/// Returns the variant name of a durable `Event` as a string.
pub fn event_tag(e: &Event) -> String {
    match e {
        Event::SessionForked { .. } => "SessionForked",
        Event::MessageAppended { .. } => "MessageAppended",
        Event::ModelChanged { .. } => "ModelChanged",
        Event::Compacted { .. } => "Compacted",
        Event::Handoff { .. } => "Handoff",
        Event::ToolsToggled { .. } => "ToolsToggled",
        Event::UsageRecorded { .. } => "UsageRecorded",
        Event::TurnStarted { .. } => "TurnStarted",
        Event::TextDelta { .. } => "TextDelta",
        Event::ThinkingDelta { .. } => "ThinkingDelta",
        Event::ToolArgsDelta { .. } => "ToolArgsDelta",
        Event::ToolStarted { .. } => "ToolStarted",
        Event::ToolProgress { .. } => "ToolProgress",
        Event::ToolFinished { .. } => "ToolFinished",
        Event::ApprovalRequest { .. } => "ApprovalRequest",
        Event::TurnEnded { .. } => "TurnEnded",
        Event::HookFailed { .. } => "HookFailed",
        Event::Error { .. } => "Error",
        Event::RetryAttempt { .. } => "RetryAttempt",
        Event::TurnStatus { .. } => "TurnStatus",
        Event::TitleChanged { .. } => "TitleChanged",
        Event::PluginNotification { .. } => "PluginNotification",
        Event::PluginDeclared { .. } => "PluginDeclared",
        Event::PluginState { .. } => "PluginState",
        Event::InteractionRequest { .. } => "InteractionRequest",
        Event::UiDirective { .. } => "UiDirective",
    }
    .to_string()
}

/// Returns the variant name of a transient `LiveEvent` as a string.
pub fn live_event_tag(e: &LiveEvent) -> String {
    match e {
        LiveEvent::TurnStarted { .. } => "TurnStarted",
        LiveEvent::TextDelta { .. } => "TextDelta",
        LiveEvent::ThinkingDelta { .. } => "ThinkingDelta",
        LiveEvent::ToolArgsDelta { .. } => "ToolArgsDelta",
        LiveEvent::ToolStarted { .. } => "ToolStarted",
        LiveEvent::ToolProgress { .. } => "ToolProgress",
        LiveEvent::ToolFinished { .. } => "ToolFinished",
        LiveEvent::ApprovalRequest { .. } => "ApprovalRequest",
        LiveEvent::TurnFinishing { .. } => "TurnFinishing",
        LiveEvent::TurnEnded { .. } => "TurnEnded",
        LiveEvent::HookFailed { .. } => "HookFailed",
        LiveEvent::TitleChanged { .. } => "TitleChanged",
        LiveEvent::InteractionRequest { .. } => "InteractionRequest",
        LiveEvent::UiDirective { .. } => "UiDirective",
        LiveEvent::Error { .. } => "Error",
        LiveEvent::RetryAttempt { .. } => "RetryAttempt",
        LiveEvent::TurnStatus { .. } => "TurnStatus",
        LiveEvent::PluginNotification { .. } => "PluginNotification",
    }
    .to_string()
}
