//! Unit tests extracted from src/session.rs — private helpers exposed via
//! #[cfg(test)] re-exports in lib.rs.
#![allow(clippy::unwrap_used)]

use kn9t_core::{CallId, Content, Event, Message, ModelRef, MsgId, Role, StopReason};
use kn9t_store::{event_kind_name, now_ts};

#[test]
fn test_now_ts_returns_positive() {
    let ts = now_ts();
    assert!(ts > 0, "timestamp should be positive");
}

#[test]
fn test_now_ts_returns_reasonable_value() {
    let ts = now_ts();
    // Should be after 2020 (1577836800000 ms) and before 2100
    assert!(ts > 1577836800000, "timestamp should be after 2020");
    assert!(ts < 4102444800000, "timestamp should be before 2100");
}

#[test]
fn test_event_kind_name_message_appended() {
    let event = Event::MessageAppended {
        seq: 1,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text {
                text: "hello".into(),
            }],
            silent: false,
        },
    };
    assert_eq!(event_kind_name(&event), "MessageAppended");
}

#[test]
fn test_event_kind_name_model_changed() {
    let event = Event::ModelChanged {
        seq: 1,
        model: ModelRef {
            provider: "openai".into(),
            id: "gpt-4".into(),
        },
    };
    assert_eq!(event_kind_name(&event), "ModelChanged");
}

#[test]
fn test_event_kind_name_turn_started() {
    let event = Event::TurnStarted { turn: 1 };
    assert_eq!(event_kind_name(&event), "TurnStarted");
}

#[test]
fn test_event_kind_name_turn_ended() {
    let event = Event::TurnEnded {
        turn: 1,
        stop: StopReason::Stop,
    };
    assert_eq!(event_kind_name(&event), "TurnEnded");
}

#[test]
fn test_event_kind_name_text_delta() {
    let event = Event::TextDelta {
        msg_id: MsgId::new(),
        idx: 0,
        delta: "hello".into(),
    };
    assert_eq!(event_kind_name(&event), "TextDelta");
}

#[test]
fn test_event_kind_name_tool_started() {
    let event = Event::ToolStarted {
        call_id: CallId("call-1".into()),
        name: "bash".into(),
    };
    assert_eq!(event_kind_name(&event), "ToolStarted");
}

#[test]
fn test_event_kind_name_tool_finished() {
    let event = Event::ToolFinished {
        call_id: CallId("call-1".into()),
        is_error: false,
    };
    assert_eq!(event_kind_name(&event), "ToolFinished");
}

#[test]
fn test_event_kind_name_error() {
    let event = Event::Error {
        message: "something went wrong".into(),
    };
    assert_eq!(event_kind_name(&event), "Error");
}
