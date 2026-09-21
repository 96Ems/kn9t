use kn9t_provider_core::{CallId, Content, Message, MsgId, Quirks, Role};
use kn9t_provider_openai::encode::encode_messages;

/// A real, decodable 2x2 PNG as a data URI.
///
/// R-OAI-IMG-GUARD validates image bytes before they go on the wire, so wire
/// tests must carry an actually-decodable image - a placeholder like `"AAAA"`
/// is (correctly) treated as corrupt and degraded to text.
fn valid_png_data_uri() -> String {
    use base64::Engine;
    let img = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    )
}

/// a genuinely empty tool result must not 400 the gateway:
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

/// non-empty results are untouched.
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

/// R-OAI-IMG - OpenAI's wire format rejects `image_url` parts inside a
/// `role: "tool"` message (only `user` messages may carry images). A tool
/// result containing `Content::Image` must therefore split into a text-only
/// `tool` message plus a synthetic `user` message carrying the image(s).
#[test]
fn tool_result_with_image_splits_into_user_message() {
    let uri = valid_png_data_uri();
    let msg = Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("call_3".into()),
            content: vec![
                Content::Image {
                    sha256: uri.clone(),
                    mime: "image/png".into(),
                },
                Content::Text {
                    text: "[photo.png - 3 KB]".into(),
                },
            ],
            is_error: false,
        }],
        silent: false,
    };
    let quirks = Quirks::default();
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);

    assert_eq!(
        out.len(),
        2,
        "tool message + synthetic user message with image"
    );

    assert_eq!(out[0]["role"], "tool");
    assert_eq!(out[0]["tool_call_id"], "call_3");
    assert_eq!(out[0]["content"], serde_json::json!("[photo.png - 3 KB]"));

    assert_eq!(out[1]["role"], "user");
    let parts = out[1]["content"].as_array().expect("array content");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["type"], "image_url");
    assert_eq!(parts[0]["image_url"]["url"], serde_json::json!(uri));
}

/// R-OAI-IMG-GUARD - a corrupt image must never reach the wire as an image.
///
/// Left alone it would fail this turn *and* every following turn, since it stays
/// in the transcript: the session becomes unusable. Degrading it to text keeps
/// the turn alive and tells the model what happened.
#[test]
fn corrupt_tool_result_image_degrades_to_text_instead_of_poisoning() {
    let msg = Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("call_5".into()),
            content: vec![
                Content::Image {
                    sha256: "data:image/png;base64,AAAA".into(), // not a decodable PNG
                    mime: "image/png".into(),
                },
                Content::Text {
                    text: "[broken.png - 1 KB]".into(),
                },
            ],
            is_error: false,
        }],
        silent: false,
    };
    let quirks = Quirks::default();
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);

    let parts = out[1]["content"].as_array().expect("array content");
    assert_eq!(parts[0]["type"], "text", "must not go out as image_url");
    let note = parts[0]["text"].as_str().unwrap_or_default();
    assert!(
        note.contains("corrupt"),
        "note should explain the failure, got: {note}"
    );
}

/// A tool result with no images must not gain a trailing user message.
#[test]
fn tool_result_without_image_has_no_extra_message() {
    let msg = Message {
        id: MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id: CallId("call_4".into()),
            content: vec![Content::Text {
                text: "plain text".into(),
            }],
            is_error: false,
        }],
        silent: false,
    };
    let quirks = Quirks::default();
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);
    assert_eq!(out.len(), 1, "no image => no synthetic user message");
}

// ── R-OAI-010 / DESIGN §4.2 — thinking_replay on the chat wire ───────────────

fn thinking_msg() -> Message {
    Message {
        id: MsgId::new(),
        role: Role::Assistant,
        content: vec![
            Content::Thinking {
                text: "let me think".into(),
                signature: None,
            },
            Content::Text {
                text: "the answer".into(),
            },
        ],
        silent: false,
    }
}

/// `strip` must remove the block. Chat has no reasoning content part, so leaving it in
/// 400s DeepSeek (`unknown variant 'thinking'`) — and since the block is persisted, it
/// repeats on every later turn.
#[test]
fn thinking_replay_strip_drops_persisted_thinking() {
    let quirks = Quirks {
        thinking_replay: "strip".into(),
        ..Quirks::default()
    };
    let mut out = Vec::new();
    encode_messages(&thinking_msg(), &quirks, false, &mut out);

    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["content"], serde_json::json!("the answer"));
    assert!(out[0].get("reasoning_content").is_none());
}

/// A reasoning-only turn has nothing left after `strip`; `content: []` 400s strict gateways.
#[test]
fn thinking_replay_strip_drops_reasoning_only_turn() {
    let msg = Message {
        id: MsgId::new(),
        role: Role::Assistant,
        content: vec![Content::Thinking {
            text: "hmm".into(),
            signature: None,
        }],
        silent: false,
    };
    let quirks = Quirks {
        thinking_replay: "strip".into(),
        ..Quirks::default()
    };
    let mut out = Vec::new();
    encode_messages(&msg, &quirks, false, &mut out);

    assert!(out.is_empty(), "no empty assistant message may go out");
}

/// `verbatim` keeps the block for a gateway that requires it (LiteLLM fronting Anthropic).
#[test]
fn thinking_replay_verbatim_keeps_the_block() {
    let quirks = Quirks {
        thinking_replay: "verbatim".into(),
        ..Quirks::default()
    };
    let mut out = Vec::new();
    encode_messages(&thinking_msg(), &quirks, false, &mut out);

    let parts = out[0]["content"].as_array().expect("array content");
    assert!(
        parts.iter().any(|p| p["type"] == "thinking"),
        "verbatim must keep the block, got {parts:?}"
    );
}
