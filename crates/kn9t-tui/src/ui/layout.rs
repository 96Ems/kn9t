//! Layout — R-TUI-030.
//!
//! 2-column layout: [transcript + input + status | right sidebar]
//! No borders. Clean minimal design.
//! Left sidebar removed - sessions accessed via /session command.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Sidebar state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sidebar {
    Hidden,
    Collapsed,
    Expanded,
}

/// Layout configuration.
#[derive(Debug, Clone)]
pub struct LayoutState {
    pub right_enabled: bool,
    pub right: Sidebar,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            right_enabled: true,
            right: Sidebar::Expanded, // Right: always visible
        }
    }
}

impl LayoutState {
    pub fn toggle_right(&mut self) {
        self.right = match self.right {
            Sidebar::Collapsed => Sidebar::Expanded,
            Sidebar::Expanded => Sidebar::Collapsed,
            Sidebar::Hidden => Sidebar::Hidden,
        };
    }

    pub fn expand_right(&mut self) {
        if self.right == Sidebar::Collapsed {
            self.right = Sidebar::Expanded;
        }
    }

    pub fn collapse_right(&mut self) {
        if self.right == Sidebar::Expanded {
            self.right = Sidebar::Collapsed;
        }
    }
}

// Widths.
const RIGHT_EXPANDED: u16 = 24;
const RIGHT_COLLAPSED: u16 = 2;
const MIN_WIDTH: u16 = 60;
const MIN_CENTER: u16 = 40;

/// Computed areas.
#[derive(Debug, Clone)]
pub struct Areas {
    pub center: Rect,
    pub right: Rect,
    pub transcript: Rect,
    pub input: Rect,
    pub status: Rect,
}

/// Calculate required input height based on content and available width.
/// Returns number of lines needed (minimum 1, capped at max_lines).
pub fn calculate_input_height(input: &str, available_width: u16, max_lines: u16) -> u16 {
    if input.is_empty() {
        return 1;
    }

    // Account for prompt "› " (2 chars).
    let content_width = available_width.saturating_sub(2) as usize;
    if content_width == 0 {
        return 1;
    }

    let mut total_lines: u16 = 0;
    for line in input.lines() {
        // Each logical line may wrap into multiple display lines.
        let char_count = line.chars().count();
        let wrapped_lines = if char_count == 0 {
            1
        } else {
            ((char_count + content_width - 1) / content_width) as u16
        };
        total_lines = total_lines.saturating_add(wrapped_lines);
    }

    // Handle trailing newline (adds an empty line).
    if input.ends_with('\n') {
        total_lines = total_lines.saturating_add(1);
    }

    // Ensure at least 1 line, cap at max_lines.
    total_lines.max(1).min(max_lines)
}

/// Compute layout with dynamic input height.
pub fn compute(area: Rect, state: &LayoutState) -> Areas {
    compute_with_input(area, state, "", 1)
}

/// Compute layout with dynamic input height based on content.
pub fn compute_with_input(
    area: Rect,
    state: &LayoutState,
    input: &str,
    max_input_lines: u16,
) -> Areas {
    let right_state = effective_right_state(area.width, state);

    let right_w = width(right_state);
    let center_w = area.width.saturating_sub(right_w);

    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(center_w), Constraint::Length(right_w)])
        .split(area);

    // Calculate dynamic input height based on content.
    let input_height = calculate_input_height(input, center_w, max_input_lines);

    // Vertical split in center.
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),               // transcript
            Constraint::Length(input_height), // input (dynamic)
            Constraint::Length(1),            // status
        ])
        .split(h_chunks[0]);

    Areas {
        center: h_chunks[0],
        right: h_chunks[1],
        transcript: v_chunks[0],
        input: v_chunks[1],
        status: v_chunks[2],
    }
}

fn effective_right_state(w: u16, state: &LayoutState) -> Sidebar {
    if w < MIN_WIDTH {
        return Sidebar::Hidden;
    }

    let right = if state.right_enabled {
        state.right
    } else {
        Sidebar::Hidden
    };
    let right_w = width(right);

    if right_w + MIN_CENTER > w {
        // Collapse if needed.
        if w < RIGHT_COLLAPSED + MIN_CENTER {
            Sidebar::Hidden
        } else if right == Sidebar::Hidden {
            Sidebar::Hidden
        } else {
            Sidebar::Collapsed
        }
    } else {
        right
    }
}

fn width(s: Sidebar) -> u16 {
    match s {
        Sidebar::Hidden => 0,
        Sidebar::Collapsed => RIGHT_COLLAPSED,
        Sidebar::Expanded => RIGHT_EXPANDED,
    }
}
