//! Thinking block detection and rendering.
//!
//! Detects `<thinking>`, `<antThinking>`, or similar tags in message content
//! and renders them as collapsible blocks with muted styling.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// A parsed segment of message content.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentSegment {
    /// Regular text content.
    Text(String),
    /// Thinking block content with tag name.
    Thinking { tag: String, content: String },
}

/// State for tracking collapsed thinking blocks.
#[derive(Debug, Default)]
pub struct ThinkingState {
    /// Set of collapsed block indices (by index in message).
    collapsed: std::collections::HashSet<usize>,
}

impl ThinkingState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle collapse state for a block.
    pub fn toggle(&mut self, index: usize) {
        if self.collapsed.contains(&index) {
            self.collapsed.remove(&index);
        } else {
            self.collapsed.insert(index);
        }
    }

    /// Check if a block is collapsed.
    pub fn is_collapsed(&self, index: usize) -> bool {
        self.collapsed.contains(&index)
    }

    /// Collapse all blocks.
    pub fn collapse_all(&mut self, count: usize) {
        self.collapsed = (0..count).collect();
    }

    /// Expand all blocks.
    pub fn expand_all(&mut self) {
        self.collapsed.clear();
    }
}

/// Parse message content into segments, extracting thinking blocks.
pub fn parse_content(content: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut remaining = content;

    // Look for common thinking tag patterns
    let tag_patterns = [
        ("<thinking>", "</thinking>"),
        ("<antThinking>", "</antThinking>"),
        ("<reflection>", "</reflection>"),
        ("<reasoning>", "</reasoning>"),
    ];

    while !remaining.is_empty() {
        // Find the earliest opening tag
        let mut earliest: Option<(usize, &str, &str)> = None;

        for (open, close) in &tag_patterns {
            if let Some(pos) = remaining.find(open) {
                if earliest.is_none() || pos < earliest.unwrap().0 {
                    earliest = Some((pos, *open, *close));
                }
            }
        }

        match earliest {
            Some((start, open_tag, close_tag)) => {
                // Find the closing tag first to decide how to handle
                let after_open = &remaining[start + open_tag.len()..];
                if let Some(end) = after_open.find(close_tag) {
                    // Found closing tag - add text before, then thinking block
                    if start > 0 {
                        segments.push(ContentSegment::Text(remaining[..start].to_string()));
                    }

                    let thinking_content = &after_open[..end];
                    let tag_name = open_tag
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_string();

                    segments.push(ContentSegment::Thinking {
                        tag: tag_name,
                        content: thinking_content.to_string(),
                    });

                    remaining = &after_open[end + close_tag.len()..];
                } else {
                    // No closing tag found, treat entire remaining as text
                    segments.push(ContentSegment::Text(remaining.to_string()));
                    break;
                }
            }
            None => {
                // No more tags, add remaining as text
                if !remaining.is_empty() {
                    segments.push(ContentSegment::Text(remaining.to_string()));
                }
                break;
            }
        }
    }

    segments
}

/// Render thinking block header (collapsed state).
pub fn render_collapsed_header(tag: &str, line_count: usize, theme: &Theme) -> Line<'static> {
    let style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);

    Line::from(vec![
        Span::styled("▶ ", style),
        Span::styled(format!("{} ", tag), style),
        Span::styled(
            format!("({} lines)", line_count),
            Style::default().fg(theme.muted),
        ),
    ])
}

/// Render thinking block header (expanded state).
pub fn render_expanded_header(tag: &str, theme: &Theme) -> Line<'static> {
    let style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);

    Line::from(vec![
        Span::styled("▼ ", style),
        Span::styled(tag.to_string(), style),
    ])
}

/// Render thinking block content with muted styling.
///
/// Applies reduced opacity by using the muted color for text.
pub fn render_thinking_content(content: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Use muted color and italic for "reduced opacity" effect
    let text_style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);

    let border_style = Style::default().fg(theme.muted);

    for line in content.lines() {
        // Wrap long lines
        let wrapped = crate::ui::render::wrap_text(line, width.saturating_sub(4));
        for w in wrapped {
            lines.push(Line::from(vec![
                Span::styled("│ ", border_style),
                Span::styled(w, text_style),
            ]));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_no_thinking() {
        let segments = parse_content("Hello world");
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], ContentSegment::Text(t) if t == "Hello world"));
    }

    #[test]
    fn test_parse_simple_thinking() {
        let content = "Before <thinking>I'm thinking</thinking> After";
        let segments = parse_content(content);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], ContentSegment::Text(t) if t == "Before "));
        assert!(
            matches!(&segments[1], ContentSegment::Thinking { tag, content } 
            if tag == "thinking" && content == "I'm thinking")
        );
        assert!(matches!(&segments[2], ContentSegment::Text(t) if t == " After"));
    }

    #[test]
    fn test_parse_ant_thinking() {
        let content = "<antThinking>reasoning here</antThinking>";
        let segments = parse_content(content);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], ContentSegment::Thinking { tag, .. } 
            if tag == "antThinking"));
    }

    #[test]
    fn test_parse_multiple_blocks() {
        let content = "A <thinking>first</thinking> B <reasoning>second</reasoning> C";
        let segments = parse_content(content);
        assert_eq!(segments.len(), 5);
    }

    #[test]
    fn test_parse_unclosed_tag() {
        let content = "Text <thinking>unclosed";
        let segments = parse_content(content);
        // Should treat unclosed tag as text
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn test_thinking_state() {
        let mut state = ThinkingState::new();
        assert!(!state.is_collapsed(0));
        state.toggle(0);
        assert!(state.is_collapsed(0));
        state.toggle(0);
        assert!(!state.is_collapsed(0));
    }

    #[test]
    fn test_collapse_all() {
        let mut state = ThinkingState::new();
        state.collapse_all(3);
        assert!(state.is_collapsed(0));
        assert!(state.is_collapsed(1));
        assert!(state.is_collapsed(2));
        assert!(!state.is_collapsed(3));
    }
}
