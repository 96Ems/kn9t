//! Viewport search — find text in transcript with highlighting.
//!
//! Ctrl+F opens the search bar at the bottom of the transcript area.
//! Supports plain-text (case-insensitive by default) and regex modes.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::message_handler::Message;

/// A single match location: (message index, byte offset in message content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchLocation {
    pub msg_idx: usize,
    pub byte_offset: usize,
    pub byte_len: usize,
}

/// State for the in-transcript search bar.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// Current search query string.
    pub query: String,
    /// Cursor position within the query (char index).
    pub cursor_pos: usize,
    /// All match locations found in the transcript.
    pub matches: Vec<MatchLocation>,
    /// Index of the currently highlighted match (0-based).
    pub current_match_idx: usize,
    /// Whether regex mode is active.
    pub regex_mode: bool,
    /// Whether search is case-sensitive.
    pub case_sensitive: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor_pos: 0,
            matches: Vec::new(),
            current_match_idx: 0,
            regex_mode: false,
            case_sensitive: false,
        }
    }

    /// Run search over all messages and populate `self.matches`.
    pub fn search(&mut self, transcript: &[Message]) {
        self.matches.clear();
        self.current_match_idx = 0;

        if self.query.is_empty() {
            return;
        }

        if self.regex_mode {
            self.search_regex(transcript);
        } else {
            self.search_plain(transcript);
        }
    }

    fn search_plain(&mut self, transcript: &[Message]) {
        let needle = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };

        for (msg_idx, msg) in transcript.iter().enumerate() {
            let haystack = if self.case_sensitive {
                msg.content.clone()
            } else {
                msg.content.to_lowercase()
            };

            let mut search_from = 0usize;
            while search_from <= haystack.len() {
                match haystack[search_from..].find(&needle) {
                    None => break,
                    Some(rel_offset) => {
                        let byte_offset = search_from + rel_offset;
                        self.matches.push(MatchLocation {
                            msg_idx,
                            byte_offset,
                            byte_len: needle.len(),
                        });
                        // Advance past this match (at least 1 byte to avoid infinite loop).
                        search_from = byte_offset + needle.len().max(1);
                    }
                }
            }
        }
    }

    fn search_regex(&mut self, transcript: &[Message]) {
        let pattern = if self.case_sensitive {
            self.query.clone()
        } else {
            format!("(?i){}", self.query)
        };

        let re = match regex::Regex::new(&pattern) {
            Ok(r) => r,
            Err(_) => return, // Invalid regex — no matches.
        };

        for (msg_idx, msg) in transcript.iter().enumerate() {
            for m in re.find_iter(&msg.content) {
                self.matches.push(MatchLocation {
                    msg_idx,
                    byte_offset: m.start(),
                    byte_len: m.end() - m.start(),
                });
            }
        }
    }

    /// Advance to the next match (wraps around).
    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current_match_idx = (self.current_match_idx + 1) % self.matches.len();
    }

    /// Go to the previous match (wraps around).
    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        if self.current_match_idx == 0 {
            self.current_match_idx = self.matches.len() - 1;
        } else {
            self.current_match_idx -= 1;
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        let byte_pos = self.char_to_byte(self.cursor_pos);
        self.query.insert(byte_pos, c);
        self.cursor_pos += 1;
    }

    /// Delete the character before the cursor (backspace).
    pub fn delete_before_cursor(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let byte_pos = self.char_to_byte(self.cursor_pos);
        let prev_char_start = self.query[..byte_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.query.remove(prev_char_start);
        self.cursor_pos -= 1;
    }

    /// Convert a char index to a byte index in `self.query`.
    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len())
    }

    /// Match count display string, e.g. "3/42" or "0/0".
    pub fn match_count_display(&self) -> String {
        if self.matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", self.current_match_idx + 1, self.matches.len())
        }
    }

    /// Return the current match location (if any).
    pub fn current_match(&self) -> Option<&MatchLocation> {
        self.matches.get(self.current_match_idx)
    }

    /// Get matches within a specific message by index.
    pub fn matches_for_message(&self, msg_idx: usize) -> Vec<&MatchLocation> {
        self.matches.iter()
            .filter(|m| m.msg_idx == msg_idx)
            .collect()
    }

    /// Check if a match at given index is the current highlighted match.
    pub fn is_current_match(&self, match_idx: usize) -> bool {
        match_idx == self.current_match_idx
    }
    
    /// Create spans from text with search matches highlighted.
    /// Returns Vec of Span with matches highlighted in yellow (current match in inverse).
    /// All spans contain owned Strings to avoid lifetime issues.
    /// 
    /// This version uses byte offsets from pre-computed matches (for user messages).
    pub fn highlight_text(
        &self,
        text: &str,
        msg_idx: usize,
        base_style: Style,
    ) -> Vec<Span<'static>> {
        let matches: Vec<_> = self.matches.iter()
            .enumerate()
            .filter(|(_, m)| m.msg_idx == msg_idx)
            .collect();
        
        if matches.is_empty() || self.query.is_empty() {
            return vec![Span::styled(text.to_string(), base_style)];
        }
        
        let highlight_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow);
        let current_highlight_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        
        let mut spans = Vec::new();
        let mut last_end = 0usize;
        
        for (global_match_idx, loc) in &matches {
            // Add text before this match.
            if loc.byte_offset > last_end && last_end < text.len() {
                let end = loc.byte_offset.min(text.len());
                if let Some(before) = text.get(last_end..end) {
                    spans.push(Span::styled(before.to_string(), base_style));
                }
            }
            
            // Add the highlighted match.
            let match_end = (loc.byte_offset + loc.byte_len).min(text.len());
            if loc.byte_offset < text.len() {
                if let Some(matched) = text.get(loc.byte_offset..match_end) {
                    let style = if *global_match_idx == self.current_match_idx {
                        current_highlight_style
                    } else {
                        highlight_style
                    };
                    spans.push(Span::styled(matched.to_string(), style));
                }
            }
            
            last_end = match_end;
        }
        
        // Add remaining text after last match.
        if last_end < text.len() {
            if let Some(after) = text.get(last_end..) {
                spans.push(Span::styled(after.to_string(), base_style));
            }
        }
        
        if spans.is_empty() {
            vec![Span::styled(text.to_string(), base_style)]
        } else {
            spans
        }
    }
    
    /// Highlight search query occurrences in a Line (post-process markdown output).
    /// Processes each span in the line and highlights query matches.
    /// `is_current_msg` indicates if this line is in the message containing the current match.
    pub fn highlight_line(&self, line: Line<'static>, is_current_msg: bool) -> Line<'static> {
        if self.query.is_empty() {
            return line;
        }
        
        let mut new_spans = Vec::new();
        for span in line.spans {
            let text = span.content.to_string();
            let highlighted = self.highlight_in_text(&text, span.style, is_current_msg);
            new_spans.extend(highlighted);
        }
        
        Line::from(new_spans)
    }
    
    /// Highlight search query occurrences directly in text (for markdown-rendered content).
    /// This searches the query string in the text rather than using pre-computed byte offsets.
    /// Returns spans with all occurrences highlighted.
    /// `is_current_msg` - if true, uses cyan for matches (current match is in this message).
    pub fn highlight_in_text(
        &self,
        text: &str,
        base_style: Style,
        is_current_msg: bool,
    ) -> Vec<Span<'static>> {
        if self.query.is_empty() {
            return vec![Span::styled(text.to_string(), base_style)];
        }
        
        // Yellow for normal matches, cyan for matches in the current message
        let highlight_style = if is_current_msg {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
        };
        
        // For case-insensitive search, we need to find matches in a case-insensitive way
        // but preserve original casing in the output.
        let search_text = if self.case_sensitive {
            text.to_string()
        } else {
            text.to_lowercase()
        };
        let search_query = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };
        
        // Find all match positions.
        let mut match_positions: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        while let Some(pos) = search_text[start..].find(&search_query) {
            let abs_pos = start + pos;
            match_positions.push((abs_pos, abs_pos + search_query.len()));
            start = abs_pos + 1; // Move past this match to find overlapping matches
        }
        
        if match_positions.is_empty() {
            return vec![Span::styled(text.to_string(), base_style)];
        }
        
        let mut spans = Vec::new();
        let mut last_end = 0;
        
        for (match_start, match_end) in match_positions {
            // Add text before this match.
            if match_start > last_end {
                if let Some(before) = text.get(last_end..match_start) {
                    spans.push(Span::styled(before.to_string(), base_style));
                }
            }
            
            // Add the highlighted match (use original text casing).
            if let Some(matched) = text.get(match_start..match_end) {
                spans.push(Span::styled(matched.to_string(), highlight_style));
            }
            
            last_end = match_end;
        }
        
        // Add remaining text after last match.
        if last_end < text.len() {
            if let Some(after) = text.get(last_end..) {
                spans.push(Span::styled(after.to_string(), base_style));
            }
        }
        
        if spans.is_empty() {
            vec![Span::styled(text.to_string(), base_style)]
        } else {
            spans
        }
    }

    /// Render the search bar into `buf` at `area`.
    ///
    /// Layout: ┌─ Search: [query______] ─ 3/42 ─ [.*] [Aa] ─┐
    pub fn render_bar(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width < 10 {
            return;
        }

        let y = area.y;
        let x = area.x;
        let w = area.width as usize;

        // Colours.
        let border_style = Style::default().fg(Color::Cyan);
        let label_style = Style::default().fg(Color::Cyan);
        let query_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
        let count_style = Style::default().fg(Color::Yellow);
        let toggle_on = Style::default().fg(Color::Black).bg(Color::Cyan);
        let toggle_off = Style::default().fg(Color::DarkGray);
        let no_match_style = Style::default().fg(Color::Red);

        // Build the bar text in sections.
        // "Search: " prefix
        let prefix = "Search: ";
        // Toggles suffix: " [.*] [Aa]"
        let regex_label = if self.regex_mode { "[.*]" } else { "[.*]" };
        let case_label = if self.case_sensitive { "[Aa]" } else { "[Aa]" };
        let count_str = self.match_count_display();
        // Suffix: " ─ 3/42 ─ [.*] [Aa] "
        let suffix_len = 3 + count_str.len() + 3 + regex_label.len() + 1 + case_label.len() + 1;
        let prefix_len = prefix.len();
        // Query field width (at least 8 chars).
        let query_field_w = w.saturating_sub(prefix_len + suffix_len).max(8);

        // Render character by character.
        let mut col = x;

        // Helper closure to write a char with style.
        let write_char = |buf: &mut Buffer, col: u16, y: u16, ch: char, style: Style| {
            if col < x + area.width {
                buf[(col, y)].set_char(ch).set_style(style);
            }
        };

        // Prefix "Search: "
        for ch in prefix.chars() {
            write_char(buf, col, y, ch, label_style);
            col += 1;
        }

        // Query field — show query text, padded with spaces.
        let query_chars: Vec<char> = self.query.chars().collect();
        let query_display_start = if query_chars.len() > query_field_w {
            query_chars.len() - query_field_w
        } else {
            0
        };
        let q_style = if !self.query.is_empty() && self.matches.is_empty() {
            no_match_style
        } else {
            query_style
        };
        let mut query_col_count = 0usize;
        for ch in query_chars.iter().skip(query_display_start).copied() {
            if query_col_count >= query_field_w {
                break;
            }
            write_char(buf, col, y, ch, q_style);
            col += 1;
            query_col_count += 1;
        }
        // Pad remaining query field with spaces.
        while query_col_count < query_field_w {
            write_char(buf, col, y, ' ', q_style);
            col += 1;
            query_col_count += 1;
        }

        // " ─ "
        for ch in " ─ ".chars() {
            write_char(buf, col, y, ch, border_style);
            col += 1;
        }

        // Count "3/42"
        for ch in count_str.chars() {
            write_char(buf, col, y, ch, count_style);
            col += 1;
        }

        // " ─ "
        for ch in " ─ ".chars() {
            write_char(buf, col, y, ch, border_style);
            col += 1;
        }

        // Regex toggle "[.*]"
        let re_style = if self.regex_mode { toggle_on } else { toggle_off };
        for ch in regex_label.chars() {
            write_char(buf, col, y, ch, re_style);
            col += 1;
        }

        // " "
        write_char(buf, col, y, ' ', border_style);
        col += 1;

        // Case toggle "[Aa]"
        let cs_style = if self.case_sensitive { toggle_on } else { toggle_off };
        for ch in case_label.chars() {
            write_char(buf, col, y, ch, cs_style);
            col += 1;
        }

        // Fill remainder with spaces (clear any stale chars).
        while col < x + area.width {
            write_char(buf, col, y, ' ', Style::default());
            col += 1;
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_handler::Message;

    fn make_messages(contents: &[&str]) -> Vec<Message> {
        contents
            .iter()
            .map(|c| Message::new("assistant", *c))
            .collect()
    }

    #[test]
    fn test_search_basic() {
        let mut state = SearchState::new();
        state.query = "hello".to_string();
        state.cursor_pos = 5;
        let msgs = make_messages(&["say hello world", "goodbye", "hello again"]);
        state.search(&msgs);
        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.matches[0].msg_idx, 0);
        assert_eq!(state.matches[1].msg_idx, 2);
    }

    #[test]
    fn test_search_case_insensitive() {
        let mut state = SearchState::new();
        // Default: case_sensitive = false
        assert!(!state.case_sensitive);
        state.query = "HELLO".to_string();
        let msgs = make_messages(&["say hello world", "HELLO there", "no match"]);
        state.search(&msgs);
        // Should find both "hello" and "HELLO"
        assert_eq!(state.matches.len(), 2);
    }

    #[test]
    fn test_search_regex() {
        let mut state = SearchState::new();
        state.regex_mode = true;
        state.query = r"\d+".to_string();
        let msgs = make_messages(&["abc 123 def", "no digits here", "456 and 789"]);
        state.search(&msgs);
        // "123" in msg 0, "456" and "789" in msg 2
        assert_eq!(state.matches.len(), 3);
        assert_eq!(state.matches[0].msg_idx, 0);
        assert_eq!(state.matches[1].msg_idx, 2);
        assert_eq!(state.matches[2].msg_idx, 2);
    }

    #[test]
    fn test_search_navigation() {
        let mut state = SearchState::new();
        state.query = "x".to_string();
        let msgs = make_messages(&["x one", "x two", "x three"]);
        state.search(&msgs);
        assert_eq!(state.matches.len(), 3);
        assert_eq!(state.current_match_idx, 0);

        state.next_match();
        assert_eq!(state.current_match_idx, 1);

        state.next_match();
        assert_eq!(state.current_match_idx, 2);

        // Wraps around forward.
        state.next_match();
        assert_eq!(state.current_match_idx, 0);

        // Wraps around backward.
        state.prev_match();
        assert_eq!(state.current_match_idx, 2);

        state.prev_match();
        assert_eq!(state.current_match_idx, 1);
    }
}
