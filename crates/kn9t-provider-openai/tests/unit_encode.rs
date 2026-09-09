use kn9t_provider_core::{CallId, Content, Message, MsgId, Quirks, Role};
use kn9t_provider_openai::encode::encode_messages;

/// 96E-19 — a genuinely empty tool result must not 400 the gateway:
/// `content` on the wire is non-empty while the TUI still renders no output.
#[test]
fn tool_result_empty_content_is_nonempty_on_wire() {
    let msg = Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("call_1".into()),
            content: vec![Content::Text { text: "   ".into() }], // whitespace-only = empty
            is_error: false,
        }],
        silent: false,
    };
    let quirks = Quirks::default();
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);
    assert_eq!(out.len(), 1, "one wire message per tool result");
    assert_eq!(out[0]["role"], "tool");
    assert_eq!(out[0]["tool_call_id"], "call_1");
    assert_eq!(
        out[0]["content"],
        serde_json::json!("(no output)"),
        "empty tool content must be replaced with a non-empty wire form"
    );
}

/// 96E-19 — non-empty results are untouched.
#[test]
fn tool_result_keeps_real_output() {
    let msg = Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("call_2".into()),
            content: vec![Content::Text {
                text: "file1\nfile2".into(),
            }],
            is_error: false,
        }],
        silent: false,
    };
    let quirks = Quirks::default();
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);
    assert_eq!(out[0]["content"], serde_json::json!("file1\nfile2"));
}
