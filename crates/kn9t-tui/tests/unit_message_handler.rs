use kn9t_tui::message_handler::{Message, Transcript, TranscriptParser};

#[test]
fn test_transcript_new() {
    let transcript = Transcript::new();
    assert!(transcript.messages().is_empty());
    assert!(transcript.live_delta().is_empty());
    assert_eq!(transcript.scroll(), 0);
}

#[test]
fn test_transcript_scroll() {
    let mut transcript = Transcript::new();

    transcript.scroll_up(5);
    assert_eq!(transcript.scroll(), 5);

    transcript.scroll_down(3);
    assert_eq!(transcript.scroll(), 2);

    transcript.scroll_down(10); // Should not go negative.
    assert_eq!(transcript.scroll(), 0);

    transcript.scroll_top();
    assert_eq!(transcript.scroll(), usize::MAX);

    transcript.scroll_bottom();
    assert_eq!(transcript.scroll(), 0);
}

#[test]
fn test_scrolling_up_stops_auto_scroll() {
    let mut transcript = Transcript::new();
    assert!(transcript.is_following(), "a fresh transcript follows the tail");

    // Render a transcript that overflows the viewport: 100 lines, 20 visible.
    transcript.on_render(100, 80);
    assert!(transcript.is_following());

    transcript.scroll_up(10);
    assert!(!transcript.is_following(), "scrolling up must stop the follow");
    assert_eq!(transcript.scroll(), 10);

    // Reaching the bottom by scrolling down restores it.
    transcript.scroll_down(10);
    assert!(transcript.is_following());
    assert_eq!(transcript.scroll(), 0);
}

#[test]
fn test_a_scrolled_up_view_does_not_drift_as_content_grows() {
    let mut transcript = Transcript::new();
    // Frame 1: 100 lines, 20 visible → max scroll 80.
    transcript.on_render(100, 80);
    transcript.scroll_up(10);
    transcript.on_render(100, 80);

    // The top line on screen is `max_scroll - scroll` = 70.
    let top_before = 80 - transcript.scroll();

    // Frame 2: a streaming turn adds 30 lines.
    transcript.on_render(130, 110);

    // The offset from the bottom grew by exactly the growth, so the same absolute line is
    // still at the top instead of the view having slid down by 30 lines.
    let top_after = 110 - transcript.scroll();
    assert_eq!(top_after, top_before, "the view must stay anchored to its content");
    assert!(!transcript.is_following(), "growing content must not re-enable follow");
}

#[test]
fn test_following_pins_to_the_bottom_as_content_grows() {
    let mut transcript = Transcript::new();
    transcript.on_render(100, 80);
    assert!(transcript.is_following());

    transcript.on_render(130, 110);
    assert_eq!(transcript.scroll(), 0, "following keeps the newest line in view");
    assert!(transcript.is_following());
}

#[test]
fn test_content_that_fits_the_screen_is_always_following() {
    let mut transcript = Transcript::new();
    // The user tries to scroll up, but there is nowhere to go: max_scroll is 0.
    transcript.scroll_up(10);
    transcript.on_render(10, 0);

    assert_eq!(transcript.scroll(), 0);
    assert!(transcript.is_following(), "no overflow means the view is at the bottom");
}

#[test]
fn test_transcript_push_messages() {
    let mut transcript = Transcript::new();

    transcript.push(Message {
        role: "user".into(),
        content: "Hello".into(),
        tools: Vec::new(),
        thinking: Vec::new(),
        image_count: 0,
    });

    transcript.push_error("Something went wrong".into());
    transcript.push_system("System message".into());

    assert_eq!(transcript.message_count(), 3);
    assert_eq!(transcript.messages()[0].role, "user");
    assert_eq!(transcript.messages()[1].role, "error");
    assert_eq!(transcript.messages()[2].role, "system");
}

#[test]
fn test_transcript_delta() {
    let mut transcript = Transcript::new();

    transcript.append_delta("Hello ");
    transcript.append_delta("world!");

    assert_eq!(transcript.live_delta(), "Hello world!");

    let delta = transcript.take_delta();
    assert_eq!(delta, "Hello world!");
    assert!(transcript.live_delta().is_empty());
}

#[test]
fn test_transcript_ensure_assistant_message() {
    let mut transcript = Transcript::new();

    // Empty - should create.
    transcript.ensure_assistant_message();
    assert_eq!(transcript.message_count(), 1);
    assert_eq!(transcript.messages()[0].role, "assistant");

    // Already assistant - should not create.
    transcript.ensure_assistant_message();
    assert_eq!(transcript.message_count(), 1);

    // Add user message, then ensure.
    transcript.push(Message {
        role: "user".into(),
        content: "Hi".into(),
        tools: Vec::new(),
        thinking: Vec::new(),
        image_count: 0,
    });
    transcript.ensure_assistant_message();
    assert_eq!(transcript.message_count(), 3);
}

#[test]
fn test_transcript_tool_operations() {
    let mut transcript = Transcript::new();

    transcript.start_tool("call-123".into(), "bash".into());

    assert_eq!(transcript.message_count(), 1);
    assert_eq!(transcript.tool_count(), 1);

    let msg = &transcript.messages()[0];
    assert_eq!(msg.tools[0].call_id, "call-123");
    assert_eq!(msg.tools[0].name, "bash");
    assert_eq!(msg.tools[0].status, "running");

    transcript.append_tool_args("ls -la");
    assert_eq!(transcript.messages()[0].tools[0].args, "ls -la");

    transcript.update_tool("call-123", |t| {
        t.status = "done".into();
        t.output = Some("file1.txt\nfile2.txt".into());
    });

    assert_eq!(transcript.messages()[0].tools[0].status, "done");
    assert!(transcript.messages()[0].tools[0].output.is_some());
}

#[test]
fn test_parser_simple_text() {
    let transcript = vec![serde_json::json!({
        "role": "user",
        "content": "Hello world"
    })];

    let messages = TranscriptParser::parse(&transcript);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, "Hello world");
}

#[test]
fn test_parser_content_blocks() {
    let transcript = vec![serde_json::json!({
        "role": "assistant",
        "content": [
            { "type": "text", "text": "Here's the result:" },
            { "type": "tool_call", "id": "call-1", "name": "bash", "args_json": "{\"command\": \"ls\"}" }
        ]
    })];

    let messages = TranscriptParser::parse(&transcript);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Here's the result:");
    assert_eq!(messages[0].tools.len(), 1);
    assert_eq!(messages[0].tools[0].name, "bash");
}

#[test]
fn test_parser_tool_results() {
    let transcript = vec![
        serde_json::json!({
            "role": "assistant",
            "content": [
                { "type": "tool_call", "id": "call-1", "name": "bash", "args_json": "{}" }
            ]
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "id": "call-1",
                    "content": "file1.txt\nfile2.txt",
                    "is_error": false
                }
            ]
        }),
    ];

    let messages = TranscriptParser::parse(&transcript);

    // Tool result message should be skipped (content matched to tool card).
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].tools[0].output,
        Some("file1.txt\nfile2.txt".into())
    );
    assert_eq!(messages[0].tools[0].status, "done");
}
