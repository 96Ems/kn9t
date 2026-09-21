//! Message and transcript handling - extracted from app.rs for better separation of concerns.
//!
//! Manages the transcript state, message parsing, and SSE frame processing.

use std::collections::HashMap;

/// Active tab in expanded tool card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
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
    /// Reasoning blocks, held apart from `content` so they render as their own cards.
    /// Blending them into the answer (what the tag parser did) made the model's
    /// scratchpad read as part of its reply.
    pub thinking: Vec<ThinkingCard>,
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
            thinking: Vec::new(),
            image_count: 0,
        }
    }

    /// Create a message with images.
    pub fn with_images(
        role: impl Into<String>,
        content: impl Into<String>,
        image_count: usize,
    ) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tools: Vec::new(),
            thinking: Vec::new(),
            image_count,
        }
    }

    /// Add tools to a message (builder pattern).
    pub fn with_tools(mut self, tools: Vec<ToolCard>) -> Self {
        self.tools = tools;
        self
    }

    /// Add reasoning cards to a message (builder pattern).
    pub fn with_thinking(mut self, thinking: Vec<ThinkingCard>) -> Self {
        self.thinking = thinking;
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
    pub progress_lines: Vec<String>, // Accumulated progress notes (e.g., diff lines)
    pub expanded: bool,
    pub active_tab: ToolTab,
    pub scroll_offset: usize,
}

/// A reasoning block, rendered as its own collapsible card.
#[derive(Debug, Clone)]
pub struct ThinkingCard {
    pub text: String,
    /// Collapsed or expanded. History loads collapsed; the live card stays open while it
    /// streams and is collapsed when the turn commits.
    pub collapsed: bool,
}

/// Manages the message transcript.
#[derive(Debug)]
pub struct Transcript {
    /// All messages in the conversation.
    messages: Vec<Message>,
    /// Live streaming delta (not yet committed to messages).
    live_delta: String,
    /// Reasoning for the in-flight turn. Separate from `live_delta` so the answer and the
    /// scratchpad never render as one stream.
    live_thinking: String,
    /// Scroll position (0 = bottom, higher = scrolled up).
    scroll: usize,
    /// Auto-scroll. While true the view is pinned to the newest line, so streaming output
    /// arrives in view. Scrolling up clears it; returning to the bottom sets it again.
    follow: bool,
    /// Rendered line count from the previous frame. `on_render` uses the delta to keep a
    /// scrolled-up view anchored to its content instead of to the bottom.
    last_total: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            live_delta: String::new(),
            live_thinking: String::new(),
            scroll: 0,
            follow: true,
            last_total: 0,
        }
    }

    /// Clear all messages and state.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.live_delta.clear();
        self.live_thinking.clear();
        self.scroll = 0;
        self.follow = true;
        self.last_total = 0;
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

    /// Reasoning accumulated for the in-flight turn.
    pub fn live_thinking(&self) -> &str {
        &self.live_thinking
    }

    /// Append to the live reasoning buffer.
    pub fn append_thinking_delta(&mut self, delta: &str) {
        self.live_thinking.push_str(delta);
    }

    /// Clear live reasoning and return its contents.
    pub fn take_thinking_delta(&mut self) -> String {
        std::mem::take(&mut self.live_thinking)
    }

    /// Get scroll position.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Set scroll position.
    pub fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
        // Landing at the bottom means the user is following the turn again.
        self.follow = scroll == 0;
    }

    /// Scroll up by amount.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount);
        self.follow = false;
    }

    /// Scroll down by amount.
    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
        if self.scroll == 0 {
            self.follow = true;
        }
    }

    /// Scroll to top.
    pub fn scroll_top(&mut self) {
        self.scroll = usize::MAX;
        self.follow = false;
    }

    /// Scroll to bottom.
    pub fn scroll_bottom(&mut self) {
        self.scroll = 0;
        self.follow = true;
    }

    /// Whether the view is pinned to the newest line.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Reconcile the scroll position after a render. When the user has scrolled up, add the
    /// frame's growth to the from-bottom offset so the same absolute line stays at the top —
    /// otherwise a streaming turn drags the view down and loses their place.
    pub fn on_render(&mut self, total: usize, max_scroll: usize) {
        if self.follow {
            self.scroll = 0;
        } else {
            if total > self.last_total {
                self.scroll = self.scroll.saturating_add(total - self.last_total);
            }
            self.scroll = self.scroll.min(max_scroll);
            // Content that fits on screen has nowhere to scroll up to, so the user is at the
            // bottom by definition and following resumes.
            if self.scroll == 0 {
                self.follow = true;
            }
        }
        self.last_total = total;
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
            thinking: Vec::new(),
            image_count: 0,
        });
    }

    /// Push a system message.
    pub fn push_system(&mut self, content: String) {
        self.messages.push(Message {
            role: "system".into(),
            content,
            tools: Vec::new(),
            thinking: Vec::new(),
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
                thinking: Vec::new(),
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
                expanded: true, // Auto-expand while running
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
    /// Parse a server-JSON transcript: content is a string or a block array, and tool results
    /// arrive in separate messages that must be matched by call_id.
    pub fn parse(transcript: &[serde_json::Value]) -> Vec<Message> {
        let tool_results = Self::collect_tool_results(transcript);

        let mut messages = Vec::new();

        for msg in transcript {
            let Some(role) = msg.get("role").and_then(|r| r.as_str()) else {
                continue;
            };
            let content = msg.get("content");

            let mut text_parts = Vec::new();
            let mut tools = Vec::new();
            let mut thinking = Vec::new();
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
                        Self::parse_block(
                            block,
                            &tool_results,
                            &mut text_parts,
                            &mut tools,
                            &mut thinking,
                        );
                    }
                }
                _ => {}
            }

            let content_text = text_parts.join("\n");

            // Only add message if there's content, tools, or reasoning.
            if !content_text.is_empty()
                || !tools.is_empty()
                || !thinking.is_empty()
                || image_count > 0
            {
                messages.push(Message {
                    role: role.to_string(),
                    content: content_text,
                    tools,
                    thinking,
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
                    let Some(btype) = block.get("type").and_then(|t| t.as_str()) else {
                        continue;
                    };
                    if btype == "tool_result" {
                        // Canonical kn9t format: the result block carries `id`.
                        if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
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
        thinking: &mut Vec<ThinkingCard>,
    ) {
        // The server is authoritative; a block without a `type` is not a content
        // block we recognize — skip it explicitly rather than matching "".
        let Some(block_type) = block.get("type").and_then(|t| t.as_str()) else {
            return;
        };

        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "thinking" => {
                if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                    thinking.push(ThinkingCard {
                        text: text.to_string(),
                        collapsed: true, // History loads collapsed; Ctrl+E expands.
                    });
                }
            }
            "tool_call" => {
                // Canonical field is `args_json` (JSON string); one format only (F6 / §10).
                let Some(call_id) = block.get("id").and_then(|v| v.as_str()) else {
                    return;
                };
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = block
                    .get("args_json")
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
                    .get(call_id)
                    .cloned()
                    .unwrap_or((String::new(), false));

                tools.push(ToolCard {
                    call_id: call_id.to_string(),
                    name,
                    args,
                    status: if is_error {
                        "error".into()
                    } else {
                        "done".into()
                    },
                    output: if output.is_empty() {
                        None
                    } else {
                        Some(output)
                    },
                    progress_lines: Vec::new(), // Not available when loading from DB
                    expanded: true, // Expanded by default so tool output visible (fix: collapsed hid output, user reports no collapse line)
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
