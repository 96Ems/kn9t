//! Message and transcript handling - extracted from app.rs for better separation of concerns.
//!
//! Manages the transcript state, message parsing, and SSE frame processing.

use std::collections::HashMap;

/// Active tab in expanded tool card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolTab {
    /// Progress chunks (streaming, shown during execution).
    Progress,
    /// Final output (what the agent sees in tool_result).
    #[default]
    Output,
    /// Input args JSON.
    Input,
}

/// Message in transcript.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub tools: Vec<ToolCard>,
    /// Number of images attached to this message (for display).
    pub image_count: usize,
}

impl Message {
    /// Create a simple message without images.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tools: Vec::new(),
            image_count: 0,
        }
    }
    
    /// Create a message with images.
    pub fn with_images(role: impl Into<String>, content: impl Into<String>, image_count: usize) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tools: Vec::new(),
            image_count,
        }
    }
    
    /// Add tools to a message (builder pattern).
    pub fn with_tools(mut self, tools: Vec<ToolCard>) -> Self {
        self.tools = tools;
        self
    }
}

/// Tool call card for display.
#[derive(Debug, Clone)]
pub struct ToolCard {
    pub call_id: String,
    pub name: String,
    pub args: String,
    pub status: String, // "pending", "running", "done", "error"
    pub output: Option<String>,
    pub progress_lines: Vec<String>,  // Accumulated progress notes (e.g., diff lines)
    pub expanded: bool,
    pub active_tab: ToolTab,
    pub scroll_offset: usize,
}

/// Manages the message transcript.
#[derive(Debug)]
pub struct Transcript {
    /// All messages in the conversation.
    messages: Vec<Message>,
    /// Live streaming delta (not yet committed to messages).
    live_delta: String,
    /// Scroll position (0 = bottom, higher = scrolled up).
    scroll: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            live_delta: String::new(),
            scroll: 0,
        }
    }

    /// Clear all messages and state.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.live_delta.clear();
        self.scroll = 0;
    }

    /// Get all messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get mutable reference to messages.
    pub fn messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    /// Get live delta text.
    pub fn live_delta(&self) -> &str {
        &self.live_delta
    }

    /// Append to live delta.
    pub fn append_delta(&mut self, delta: &str) {
        self.live_delta.push_str(delta);
    }

    /// Clear live delta and return its contents.
    pub fn take_delta(&mut self) -> String {
        std::mem::take(&mut self.live_delta)
    }

    /// Get scroll position.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Set scroll position.
    pub fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
    }

    /// Scroll up by amount.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    /// Scroll down by amount.
    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    /// Scroll to top.
    pub fn scroll_top(&mut self) {
        self.scroll = usize::MAX;
    }

    /// Scroll to bottom.
    pub fn scroll_bottom(&mut self) {
        self.scroll = 0;
    }

    /// Push a message.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Push an error message.
    pub fn push_error(&mut self, content: String) {
        self.messages.push(Message {
            role: "error".into(),
            content,
            tools: Vec::new(),
            image_count: 0,
        });
    }

    /// Push a system message.
    pub fn push_system(&mut self, content: String) {
        self.messages.push(Message {
            role: "system".into(),
            content,
            tools: Vec::new(),
            image_count: 0,
        });
    }

    /// Get the last message if it's from the assistant.
    pub fn last_assistant_message_mut(&mut self) -> Option<&mut Message> {
        self.messages.last_mut().filter(|m| m.role == "assistant")
    }

    /// Ensure there's an assistant message at the end, creating one if needed.
    pub fn ensure_assistant_message(&mut self) {
        if self.messages.is_empty()
            || self
                .messages
                .last()
                .map(|m| m.role != "assistant")
                .unwrap_or(true)
        {
            self.messages.push(Message {
                role: "assistant".into(),
                content: String::new(),
                tools: Vec::new(),
                image_count: 0,
            });
        }
    }

    /// Find a tool card by call_id and update it.
    pub fn update_tool<F>(&mut self, call_id: &str, f: F)
    where
        F: FnOnce(&mut ToolCard),
    {
        for msg in self.messages.iter_mut().rev() {
            if let Some(tool) = msg.tools.iter_mut().find(|t| t.call_id == call_id) {
                f(tool);
                return;
            }
        }
    }

    /// Start a tool (add to last assistant message).
    pub fn start_tool(&mut self, call_id: String, name: String) {
        self.ensure_assistant_message();
        if let Some(msg) = self.messages.last_mut() {
            msg.tools.push(ToolCard {
                call_id,
                name,
                args: String::new(),
                status: "running".into(),
                output: None,
                progress_lines: Vec::new(),
                expanded: true,  // Auto-expand while running
                active_tab: ToolTab::Output,
                scroll_offset: 0,
            });
        }
    }

    /// Append args to the last tool.
    pub fn append_tool_args(&mut self, delta: &str) {
        if let Some(msg) = self.messages.last_mut() {
            if let Some(tool) = msg.tools.last_mut() {
                tool.args.push_str(delta);
            }
        }
    }

    /// Count total tools across all messages.
    pub fn tool_count(&self) -> usize {
        self.messages.iter().map(|m| m.tools.len()).sum()
    }

    /// Message count.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// Parser for loading transcript from server JSON.
pub struct TranscriptParser;

impl TranscriptParser {
    /// Parse a transcript from server JSON format.
    ///
    /// The server returns messages with content as either a string or array of blocks.
    /// Tool results are in separate messages and need to be matched by call_id.
    pub fn parse(transcript: &[serde_json::Value]) -> Vec<Message> {
        // First pass: collect all tool results by call_id.
        let tool_results = Self::collect_tool_results(transcript);

        // Second pass: build messages with tools.
        let mut messages = Vec::new();

        for msg in transcript {
            let role = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let content = msg.get("content");

            let mut text_parts = Vec::new();
            let mut tools = Vec::new();
            let mut image_count = 0;

            match content {
                Some(serde_json::Value::String(s)) => {
                    text_parts.push(s.clone());
                }
                Some(serde_json::Value::Array(arr)) => {
                    for block in arr {
                        // Count images
                        if let Some(t) = block.get("type").and_then(|t| t.as_str()) {
                            if t == "image" {
                                image_count += 1;
                            }
                        }
                        Self::parse_block(block, &tool_results, &mut text_parts, &mut tools);
                    }
                }
                _ => {}
            }

            let content_text = text_parts.join("\n");

            // Only add message if there's content or tools.
            if !content_text.is_empty() || !tools.is_empty() || image_count > 0 {
                messages.push(Message {
                    role,
                    content: content_text,
                    tools,
                    image_count,
                });
            }
        }

        messages
    }

    /// Collect tool results from transcript.
    fn collect_tool_results(transcript: &[serde_json::Value]) -> HashMap<String, (String, bool)> {
        let mut results = HashMap::new();

        for msg in transcript {
            if let Some(serde_json::Value::Array(arr)) = msg.get("content") {
                for block in arr {
                    let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if btype == "tool_result" {
                        // Try both "id" (kn9t-core format) and "tool_use_id" (Anthropic API format)
                        if let Some(id) = block.get("id").and_then(|v| v.as_str())
                            .or_else(|| block.get("tool_use_id").and_then(|v| v.as_str())) {
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let output = Self::extract_tool_result_content(block);
                            results.insert(id.to_string(), (output, is_error));
                        }
                    }
                }
            }
        }

        results
    }

    /// Extract content from a tool_result block.
    fn extract_tool_result_content(block: &serde_json::Value) -> String {
        if let Some(content) = block.get("content") {
            match content {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            }
        } else {
            String::new()
        }
    }

    /// Parse a single content block.
    fn parse_block(
        block: &serde_json::Value,
        tool_results: &HashMap<String, (String, bool)>,
        text_parts: &mut Vec<String>,
        tools: &mut Vec<ToolCard>,
    ) {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "thinking" => {
                if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "tool_call" | "tool_use" => {
                let call_id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Args can be in "args_json" (kn9t-core), "args", or "input" (Anthropic) depending on format
                let args = block
                    .get("args_json")
                    .or_else(|| block.get("args"))
                    .or_else(|| block.get("input"))
                    .map(|v| {
                        if v.is_string() {
                            v.as_str().unwrap_or("").to_string()
                        } else {
                            serde_json::to_string(v).unwrap_or_default()
                        }
                    })
                    .unwrap_or_default();

                // Look up the result.
                let (output, is_error) = tool_results
                    .get(&call_id)
                    .cloned()
                    .unwrap_or((String::new(), false));

                tools.push(ToolCard {
                    call_id,
                    name,
                    args,
                    status: if is_error {
                        "error".into()
                    } else {
                        "done".into()
                    },
                    output: if output.is_empty() { None } else { Some(output) },
                    progress_lines: Vec::new(),  // Not available when loading from DB
                    expanded: false,  // Collapsed by default when done
                    active_tab: ToolTab::Output,
                    scroll_offset: 0,
                });
            }
            "tool_result" => {
                // Already handled in first pass.
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_transcript_push_messages() {
        let mut transcript = Transcript::new();

        transcript.push(Message {
            role: "user".into(),
            content: "Hello".into(),
            tools: Vec::new(),
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
                { "type": "tool_call", "id": "call-1", "name": "bash", "args": {"command": "ls"} }
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
                    { "type": "tool_call", "id": "call-1", "name": "bash", "args": {} }
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call-1",
                        "content": "file1.txt\nfile2.txt",
                        "is_error": false
                    }
                ]
            }),
        ];

        let messages = TranscriptParser::parse(&transcript);

        // Tool result message should be skipped (content matched to tool card).
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tools[0].output, Some("file1.txt\nfile2.txt".into()));
        assert_eq!(messages[0].tools[0].status, "done");
    }
}
