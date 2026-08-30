//! Which-key panel — Vim-like keybinding help popup.
//!
//! Triggered by `Action::Help` (Ctrl+P). Shows grouped keybindings,
//! navigable with arrow keys / j/k. Context-aware: tool mode shows
//! different bindings.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

// ─── Data model ─────────────────────────────────────────────────────────────

/// A single keybinding entry: (key description, action description).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEntry {
    pub key: String,
    pub description: String,
}

impl KeyEntry {
    pub fn new(key: &str, description: &str) -> Self {
        Self {
            key: key.to_string(),
            description: description.to_string(),
        }
    }
}

/// A named group of keybindings.
#[derive(Debug, Clone)]
pub struct KeyGroup {
    pub name: String,
    pub entries: Vec<KeyEntry>,
}

impl KeyGroup {
    pub fn new(name: &str, entries: Vec<KeyEntry>) -> Self {
        Self {
            name: name.to_string(),
            entries,
        }
    }
}

// ─── Panel state ─────────────────────────────────────────────────────────────

/// Which-key panel state.
#[derive(Debug, Clone)]
pub struct WhichKeyPanel {
    /// Currently highlighted group index (0-based).
    pub selected_idx: usize,
    /// Scroll offset (lines scrolled from top).
    pub scroll_offset: usize,
}

impl WhichKeyPanel {
    pub fn new() -> Self {
        Self {
            selected_idx: 0,
            scroll_offset: 0,
        }
    }

    /// Move selection up one group.
    pub fn select_prev(&mut self, groups: &[KeyGroup]) {
        if self.selected_idx > 0 {
            self.selected_idx -= 1;
        } else if !groups.is_empty() {
            self.selected_idx = groups.len() - 1;
        }
        self.adjust_scroll(groups);
    }

    /// Move selection down one group.
    pub fn select_next(&mut self, groups: &[KeyGroup]) {
        if !groups.is_empty() {
            self.selected_idx = (self.selected_idx + 1) % groups.len();
        }
        self.adjust_scroll(groups);
    }

    /// Adjust scroll so the selected group is visible.
    fn adjust_scroll(&mut self, groups: &[KeyGroup]) {
        // Compute the line offset of the selected group header.
        let mut line = 0usize;
        for (i, g) in groups.iter().enumerate() {
            if i == self.selected_idx {
                // Scroll up if header is above viewport.
                if line < self.scroll_offset {
                    self.scroll_offset = line;
                }
                break;
            }
            line += 1 + g.entries.len(); // header + entries
        }
    }

    /// Render the panel centered in `area`, writing directly into `buf`.
    pub fn render(&self, groups: &[KeyGroup], area: Rect, buf: &mut Buffer) {
        // Popup dimensions.
        let popup_w = 50u16.min(area.width.saturating_sub(4));
        let popup_h = 20u16.min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;

        // Dim background.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if x < area.x + area.width && y < area.y + area.height {
                    let cell = &mut buf[(x, y)];
                    cell.set_fg(Color::DarkGray);
                }
            }
        }

        // Clear popup area with black background.
        for y in popup_y..popup_y + popup_h {
            for x in popup_x..popup_x + popup_w {
                buf[(x, y)].set_char(' ').set_bg(Color::Black);
            }
        }

        // Border.
        self.draw_border(buf, popup_x, popup_y, popup_w, popup_h);

        // Title.
        let title = " Keybindings ";
        let title_x = popup_x + (popup_w.saturating_sub(title.len() as u16)) / 2;
        let title_style = Style::default()
            .fg(Color::Cyan)
            .bg(Color::Black)
            .add_modifier(Modifier::BOLD);
        for (i, ch) in title.chars().enumerate() {
            if title_x + (i as u16) < popup_x + popup_w - 1 {
                buf[(title_x + i as u16, popup_y)]
                    .set_char(ch)
                    .set_style(title_style);
            }
        }

        // Content area (inside border).
        let content_x = popup_x + 1;
        let content_y = popup_y + 1;
        let content_w = popup_w.saturating_sub(2) as usize;
        let content_h = popup_h.saturating_sub(2) as usize;

        // Build flat list of renderable lines.
        let lines = self.build_lines(groups);

        // Apply scroll.
        let visible_lines = content_h.saturating_sub(1); // reserve 1 for footer
        let max_scroll = lines.len().saturating_sub(visible_lines);
        let scroll = self.scroll_offset.min(max_scroll);

        for (row, line) in lines.iter().skip(scroll).take(visible_lines).enumerate() {
            let y = content_y + row as u16;
            if y >= popup_y + popup_h - 1 {
                break;
            }
            line.render_at(buf, content_x, y, content_w);
        }

        // Footer.
        let footer = "↑/↓ j/k navigate · Esc close";
        let footer_y = popup_y + popup_h - 1;
        let footer_x = popup_x + (popup_w.saturating_sub(footer.len() as u16)) / 2;
        for (i, ch) in footer.chars().enumerate() {
            let x = footer_x + i as u16;
            if x < popup_x + popup_w - 1 {
                buf[(x, footer_y)]
                    .set_char(ch)
                    .set_fg(Color::DarkGray)
                    .set_bg(Color::Black);
            }
        }
    }

    /// Build flat list of render lines from groups.
    fn build_lines<'a>(&self, groups: &'a [KeyGroup]) -> Vec<RenderLine<'a>> {
        let mut lines = Vec::new();
        for (group_idx, group) in groups.iter().enumerate() {
            let is_selected = group_idx == self.selected_idx;
            lines.push(RenderLine::GroupHeader {
                name: &group.name,
                selected: is_selected,
            });
            for entry in &group.entries {
                lines.push(RenderLine::Entry {
                    key: &entry.key,
                    desc: &entry.description,
                    group_selected: is_selected,
                });
            }
        }
        lines
    }

    fn draw_border(&self, buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16) {
        // Top.
        buf[(x, y)].set_char('┌').set_fg(Color::DarkGray).set_bg(Color::Black);
        for i in 1..w.saturating_sub(1) {
            buf[(x + i, y)].set_char('─').set_fg(Color::DarkGray).set_bg(Color::Black);
        }
        buf[(x + w - 1, y)].set_char('┐').set_fg(Color::DarkGray).set_bg(Color::Black);

        // Sides.
        for row in 1..h.saturating_sub(1) {
            buf[(x, y + row)].set_char('│').set_fg(Color::DarkGray).set_bg(Color::Black);
            buf[(x + w - 1, y + row)].set_char('│').set_fg(Color::DarkGray).set_bg(Color::Black);
        }

        // Bottom.
        buf[(x, y + h - 1)].set_char('└').set_fg(Color::DarkGray).set_bg(Color::Black);
        for i in 1..w.saturating_sub(1) {
            buf[(x + i, y + h - 1)].set_char('─').set_fg(Color::DarkGray).set_bg(Color::Black);
        }
        buf[(x + w - 1, y + h - 1)].set_char('┘').set_fg(Color::DarkGray).set_bg(Color::Black);
    }
}

impl Default for WhichKeyPanel {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Internal render line ────────────────────────────────────────────────────

enum RenderLine<'a> {
    GroupHeader { name: &'a str, selected: bool },
    Entry { key: &'a str, desc: &'a str, group_selected: bool },
}

impl<'a> RenderLine<'a> {
    fn render_at(&self, buf: &mut Buffer, x: u16, y: u16, width: usize) {
        match self {
            RenderLine::GroupHeader { name, selected } => {
                let style = if *selected {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                };
                let prefix = if *selected { "▸ " } else { "  " };
                let text = format!("{}{}", prefix, name);
                for (i, ch) in text.chars().take(width).enumerate() {
                    buf[(x + i as u16, y)]
                        .set_char(ch)
                        .set_style(style)
                        .set_bg(Color::Black);
                }
            }
            RenderLine::Entry { key, desc, group_selected } => {
                let key_style = if *group_selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let desc_style = Style::default().fg(Color::Gray);

                // Key column: 12 chars wide, left-padded with 4 spaces.
                let key_col = format!("    {:<12}", key);
                let key_chars: Vec<char> = key_col.chars().collect();
                let key_len = key_chars.len().min(16);
                for (i, &ch) in key_chars.iter().take(key_len).enumerate() {
                    buf[(x + i as u16, y)]
                        .set_char(ch)
                        .set_style(key_style)
                        .set_bg(Color::Black);
                }

                // Description column: rest of width.
                let desc_start = key_len;
                for (i, ch) in desc.chars().take(width.saturating_sub(desc_start)).enumerate() {
                    buf[(x + desc_start as u16 + i as u16, y)]
                        .set_char(ch)
                        .set_style(desc_style)
                        .set_bg(Color::Black);
                }
            }
        }
    }
}

// ─── Keybinding data ─────────────────────────────────────────────────────────

/// Return grouped keybindings. When `tool_mode` is true, tool-mode bindings
/// replace the standard navigation group.
pub fn get_keybindings(tool_mode: bool) -> Vec<KeyGroup> {
    if tool_mode {
        vec![
            KeyGroup::new("Tool Mode", vec![
                KeyEntry::new("Esc",       "Exit tool mode"),
                KeyEntry::new("Up/Down",   "Navigate tools"),
                KeyEntry::new("Enter/Spc", "Expand/collapse tool"),
                KeyEntry::new("Left/Right","Cycle tabs"),
                KeyEntry::new("PgUp/PgDn", "Scroll tool output"),
                KeyEntry::new("Ctrl+T",    "Toggle tool mode"),
            ]),
            KeyGroup::new("Core", vec![
                KeyEntry::new("Enter",     "Send message"),
                KeyEntry::new("Ctrl+C/Q",  "Quit"),
                KeyEntry::new("Ctrl+P",    "This help"),
            ]),
            KeyGroup::new("Scrolling", vec![
                KeyEntry::new("Ctrl+↑/↓",  "Scroll transcript"),
                KeyEntry::new("PgUp/PgDn", "Scroll transcript"),
                KeyEntry::new("Ctrl+Home", "Scroll to top"),
                KeyEntry::new("Ctrl+End",  "Scroll to bottom"),
            ]),
        ]
    } else {
        vec![
            KeyGroup::new("Session", vec![
                KeyEntry::new("Ctrl+B",    "Switch session"),
                KeyEntry::new("Ctrl+N",    "New session"),
            ]),
            KeyGroup::new("Model", vec![
                KeyEntry::new("F2",        "Next model"),
                KeyEntry::new("Shift+F2",  "Previous model"),
                KeyEntry::new("/models",   "Model picker"),
            ]),
            KeyGroup::new("Navigation", vec![
                KeyEntry::new("Ctrl+↑",    "Jump to prev user msg"),
                KeyEntry::new("Ctrl+↓",    "Jump to next user msg"),
                KeyEntry::new("Alt+K",     "Previous message"),
                KeyEntry::new("Alt+J",     "Next message"),
                KeyEntry::new("PgUp/PgDn", "Scroll transcript"),
                KeyEntry::new("Ctrl+Home", "Scroll to top"),
                KeyEntry::new("Ctrl+End",  "Scroll to bottom"),
            ]),
            KeyGroup::new("Input", vec![
                KeyEntry::new("Enter",     "Send message"),
                KeyEntry::new("Shift+Enter","New line"),
                KeyEntry::new("Ctrl+←/→",  "Word left/right"),
                KeyEntry::new("Ctrl+Bksp", "Delete word left"),
                KeyEntry::new("Ctrl+Del",  "Delete word right"),
                KeyEntry::new("Ctrl+Z",    "Undo"),
                KeyEntry::new("Ctrl+Shift+Z","Redo"),
                KeyEntry::new("Ctrl+K",    "Kill to end of line"),
                KeyEntry::new("Ctrl+U",    "Kill to start of line"),
                KeyEntry::new("Ctrl+W",    "Kill word backward"),
                KeyEntry::new("Ctrl+Y",    "Yank (paste kill ring)"),
                KeyEntry::new("Alt+Y",     "Yank pop (cycle ring)"),
                KeyEntry::new("Ctrl+V/F5", "Paste image"),
            ]),
            KeyGroup::new("Actions", vec![
                KeyEntry::new("Esc",       "Abort turn"),
                KeyEntry::new("Ctrl+C/Q",  "Quit"),
                KeyEntry::new("Ctrl+P",    "Command palette"),
                KeyEntry::new("Ctrl+T",    "Enter tool mode"),
                KeyEntry::new("Ctrl+E",    "Toggle thinking blocks"),
                KeyEntry::new("Ctrl+F",    "Search transcript"),
            ]),
            KeyGroup::new("Slash Commands", vec![
                KeyEntry::new("/session",  "Session picker"),
                KeyEntry::new("/new",      "New session"),
                KeyEntry::new("/stash",    "Stash prompt"),
                KeyEntry::new("/pop",      "Pop stashed prompt"),
                KeyEntry::new("/help",     "Show help"),
            ]),
        ]
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keybinding_groups() {
        let groups = get_keybindings(false);
        // Must have at least 3 groups.
        assert!(groups.len() >= 3, "expected at least 3 groups, got {}", groups.len());

        // Every group must have a non-empty name.
        for g in &groups {
            assert!(!g.name.is_empty(), "group name must not be empty");
            assert!(!g.entries.is_empty(), "group '{}' must have entries", g.name);
        }

        // Verify specific expected groups exist.
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert!(names.contains(&"Session"), "expected 'Session' group");
        assert!(names.contains(&"Navigation"), "expected 'Navigation' group");
        assert!(names.contains(&"Input"), "expected 'Input' group");

        // Verify specific entries exist in Session group.
        let session = groups.iter().find(|g| g.name == "Session").unwrap();
        let session_keys: Vec<&str> = session.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(session_keys.contains(&"Ctrl+N"), "Session group must contain Ctrl+N");
        assert!(session_keys.contains(&"Ctrl+B"), "Session group must contain Ctrl+B");
    }

    #[test]
    fn test_navigation() {
        let groups = get_keybindings(false);
        let mut panel = WhichKeyPanel::new();

        // Initial state.
        assert_eq!(panel.selected_idx, 0);

        // Move down.
        panel.select_next(&groups);
        assert_eq!(panel.selected_idx, 1);

        panel.select_next(&groups);
        assert_eq!(panel.selected_idx, 2);

        // Move up.
        panel.select_prev(&groups);
        assert_eq!(panel.selected_idx, 1);

        panel.select_prev(&groups);
        assert_eq!(panel.selected_idx, 0);

        // Wrap around: prev from 0 goes to last.
        panel.select_prev(&groups);
        assert_eq!(panel.selected_idx, groups.len() - 1);

        // Wrap around: next from last goes to 0.
        panel.select_next(&groups);
        assert_eq!(panel.selected_idx, 0);
    }

    #[test]
    fn test_context_aware() {
        let normal_groups = get_keybindings(false);
        let tool_groups = get_keybindings(true);

        // Tool mode must have different groups.
        let normal_names: Vec<&str> = normal_groups.iter().map(|g| g.name.as_str()).collect();
        let tool_names: Vec<&str> = tool_groups.iter().map(|g| g.name.as_str()).collect();

        // Tool mode must include "Tool Mode" group.
        assert!(
            tool_names.contains(&"Tool Mode"),
            "tool mode must have 'Tool Mode' group, got: {:?}", tool_names
        );

        // Normal mode must NOT have "Tool Mode" group.
        assert!(
            !normal_names.contains(&"Tool Mode"),
            "normal mode must not have 'Tool Mode' group"
        );

        // Tool mode must have tool-specific entries.
        let tool_mode_group = tool_groups.iter().find(|g| g.name == "Tool Mode").unwrap();
        let tool_keys: Vec<&str> = tool_mode_group.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(tool_keys.contains(&"Esc"), "tool mode must have Esc binding");
        assert!(tool_keys.contains(&"Up/Down"), "tool mode must have Up/Down binding");

        // Normal mode must have Session group (not in tool mode).
        assert!(
            normal_names.contains(&"Session"),
            "normal mode must have 'Session' group"
        );
    }
}
