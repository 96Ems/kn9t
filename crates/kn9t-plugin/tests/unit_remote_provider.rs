//! Unit tests for remote_provider helpers — extracted from src/remote_provider.rs.
//! Tests decode_stop, decode_chunk_body, and decode_tokens without spawning a subprocess.

use kn9t_core::StopReason;
use kn9t_plugin::remote_provider::{decode_chunk_body, decode_stop, decode_tokens};
use serde_json::json;

/// Test that decode_stop correctly handles ABORTED from custom provider cancel fix.
#[test]
fn decode_stop_handles_aborted() {
    // The custom provider fix returns stop="ABORTED" when cancelled mid-stream
    let body = json!({"stop": "ABORTED"});
    assert!(matches!(decode_stop(&body), StopReason::Aborted));

    // Also test lowercase
    let body = json!({"stop": "aborted"});
    assert!(matches!(decode_stop(&body), StopReason::Aborted));

    // And mixed case
    let body = json!({"stop": "Aborted"});
    assert!(matches!(decode_stop(&body), StopReason::Aborted));

    // And with prefix/suffix
    let body = json!({"stop": "user_aborted"});
    assert!(matches!(decode_stop(&body), StopReason::Aborted));
}

#[test]
fn decode_stop_handles_tool_call() {
    let body = json!({"stop": "TOOL_CALL"});
    assert!(matches!(decode_stop(&body), StopReason::ToolUse));

    let body = json!({"stop": "tool_use"});
    assert!(matches!(decode_stop(&body), StopReason::ToolUse));
}

#[test]
fn decode_stop_handles_length() {
    let body = json!({"stop": "LENGTH"});
    assert!(matches!(decode_stop(&body), StopReason::Length));

    let body = json!({"stop": "max_tokens"});
    assert!(matches!(decode_stop(&body), StopReason::Length));
}

#[test]
fn decode_stop_defaults_to_stop() {
    let body = json!({"stop": "STOP"});
    assert!(matches!(decode_stop(&body), StopReason::Stop));

    let body = json!({"stop": "end_turn"});
    assert!(matches!(decode_stop(&body), StopReason::Stop));

    // Missing stop field
    let body = json!({});
    assert!(matches!(decode_stop(&body), StopReason::Stop));
}

/// Ensure ABORTED takes priority (checked first).
#[test]
fn decode_stop_aborted_priority() {
    // Edge case: what if someone sends "aborted_tool"?
    // ABORTED should match first since it's checked first
    let body = json!({"stop": "aborted_tool"});
    assert!(
        matches!(decode_stop(&body), StopReason::Aborted),
        "ABORTED should be checked before TOOL"
    );
}

#[test]
fn decode_chunk_body_text_delta() {
    use kn9t_core::Chunk;
    let body = json!({"kind": "text_delta", "idx": 0, "text": "hello"});
    let chunk = decode_chunk_body(&body).unwrap().unwrap();
    assert!(matches!(chunk, Chunk::Text { delta, .. } if delta == "hello"));
}

#[test]
fn decode_chunk_body_missing_kind_is_error() {
    use kn9t_core::ProvErr;
    let body = json!({"idx": 0});
    let result = decode_chunk_body(&body);
    assert!(matches!(result, Err(ProvErr::Decode(_))));
}

#[test]
fn decode_chunk_body_unknown_kind_is_none() {
    let body = json!({"kind": "future_kind", "idx": 0});
    let result = decode_chunk_body(&body).unwrap();
    assert!(result.is_none());
}

#[test]
fn decode_tokens_reads_all_aliases() {
    // Standard OpenAI field names
    let u = json!({"prompt_tokens": 10, "completion_tokens": 20});
    let t = decode_tokens(&u);
    assert_eq!(t.input, 10);
    assert_eq!(t.output, 20);

    // Anthropic field names
    let u = json!({"input_tokens": 5, "output_tokens": 15, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 2});
    let t = decode_tokens(&u);
    assert_eq!(t.input, 5);
    assert_eq!(t.output, 15);
    assert_eq!(t.cache_read, 3);
    assert_eq!(t.cache_write, 2);
}

#[test]
fn decode_tokens_missing_fields_default_zero() {
    let u = json!({});
    let t = decode_tokens(&u);
    assert_eq!(t.input, 0);
    assert_eq!(t.output, 0);
    assert_eq!(t.cache_read, 0);
    assert_eq!(t.cache_write, 0);
    assert_eq!(t.reasoning, 0);
}
