//! Diff viewer — side-by-side or unified diff display.
//!
//! Supports parsing unified diff format (standard git diff output)
//! and rendering in unified or split (side-by-side) mode.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::syntax::highlight_code_inline;
use crate::theme::Theme;

// ── Data model ────────────────────────────────────────────────────────────────

/// A single line in a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// A diff hunk with header info and lines.
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

/// A single file's diff (path + hunks).
#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
}

// ── Viewer state ──────────────────────────────────────────────────────────────

/// A comment on a specific line.
#[derive(Debug, Clone)]
pub struct LineComment {
    pub file: String,
    pub line: u32,
    pub comment: String,
}

/// Hit area for a diff line (for mouse clicks).
#[derive(Debug, Clone)]
pub struct DiffLineHit {
    pub y: u16,
    pub line_idx: usize,
    pub file_idx: usize,
}

/// File status in the diff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
}

impl FileStatus {
    pub fn indicator(&self) -> &'static str {
        match self {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
        }
    }

    pub fn color(&self) -> Color {
        match self {
            FileStatus::Modified => Color::Yellow,
            FileStatus::Added => Color::Green,
            FileStatus::Deleted => Color::Red,
        }
    }
}

/// Diff viewer widget state.
#[derive(Debug, Clone)]
pub struct DiffViewer {
    pub files: Vec<DiffFile>,
    pub current_file: usize,
    pub current_hunk: usize,
    pub scroll: usize,
    pub split_mode: bool,
    /// Full screen mode (no centering, use all terminal space).
    pub fullscreen: bool,
    /// Comments added by user (file:line -> comment).
    pub comments: Vec<LineComment>,
    /// Currently selected line for commenting (file_idx, hunk_idx, line_idx, line_number).
    pub selected_line: Option<(usize, usize, usize, u32)>,
    /// If true, show comment input overlay.
    pub commenting: bool,
    /// Current comment being typed.
    pub comment_input: String,
    /// Cursor position in lines within current hunk (for line selection).
    pub cursor_line: usize,
    /// Hit areas for rendered lines (updated on each render).
    pub line_hits: Vec<DiffLineHit>,
    /// Last render area (for mouse coordinate translation).
    pub last_area: Option<Rect>,
    /// Show file tree sidebar.
    pub show_file_tree: bool,
    /// Selected file in file tree (for navigation).
    pub file_tree_selected: usize,
    /// File tree area (for mouse click detection).
    pub file_tree_area: Option<Rect>,
}

impl DiffViewer {
    /// Create a new viewer from parsed files.
    pub fn new(files: Vec<DiffFile>) -> Self {
        Self {
            files,
            current_file: 0,
            current_hunk: 0,
            scroll: 0,
            split_mode: true, // Default to side-by-side
            fullscreen: true, // Default to fullscreen
            comments: Vec::new(),
            selected_line: None,
            commenting: false,
            comment_input: String::new(),
            cursor_line: 0,
            line_hits: Vec::new(),
            last_area: None,
            show_file_tree: true, // Show by default
            file_tree_selected: 0,
            file_tree_area: None,
        }
    }

    /// Toggle file tree sidebar.
    pub fn toggle_file_tree(&mut self) {
        self.show_file_tree = !self.show_file_tree;
    }

    /// Get file status based on hunks.
    pub fn file_status(&self, file_idx: usize) -> FileStatus {
        if file_idx >= self.files.len() {
            return FileStatus::Modified;
        }
        let file = &self.files[file_idx];
        // Check if all lines are additions (new file) or deletions (deleted file)
        let mut has_add = false;
        let mut has_remove = false;
        for hunk in &file.hunks {
            for line in &hunk.lines {
                match line {
                    DiffLine::Added(_) => has_add = true,
                    DiffLine::Removed(_) => has_remove = true,
                    DiffLine::Context(_) => {}
                }
            }
        }
        if has_add && !has_remove {
            FileStatus::Added
        } else if has_remove && !has_add {
            FileStatus::Deleted
        } else {
            FileStatus::Modified
        }
    }

    /// Get diff stats for a file (+N -M).
    pub fn file_stats(&self, file_idx: usize) -> (usize, usize) {
        if file_idx >= self.files.len() {
            return (0, 0);
        }
        let file = &self.files[file_idx];
        let mut additions = 0;
        let mut deletions = 0;
        for hunk in &file.hunks {
            for line in &hunk.lines {
                match line {
                    DiffLine::Added(_) => additions += 1,
                    DiffLine::Removed(_) => deletions += 1,
                    DiffLine::Context(_) => {}
                }
            }
        }
        (additions, deletions)
    }

    /// Navigate to next file.
    pub fn next_file(&mut self) {
        if self.current_file + 1 < self.files.len() {
            self.current_file += 1;
            self.current_hunk = 0;
            self.cursor_line = 0;
            self.scroll = 0;
            self.file_tree_selected = self.current_file;
        }
    }

    /// Navigate to previous file.
    pub fn prev_file(&mut self) {
        if self.current_file > 0 {
            self.current_file -= 1;
            self.current_hunk = 0;
            self.cursor_line = 0;
            self.scroll = 0;
            self.file_tree_selected = self.current_file;
        }
    }

    /// Select file by index.
    pub fn select_file(&mut self, idx: usize) {
        if idx < self.files.len() {
            self.current_file = idx;
            self.current_hunk = 0;
            self.cursor_line = 0;
            self.scroll = 0;
            self.file_tree_selected = idx;
        }
    }

    /// Handle mouse wheel scroll.
    pub fn handle_scroll(&mut self, delta: i16) {
        if delta > 0 {
            self.scroll_down(delta as usize);
        } else {
            self.scroll_up((-delta) as usize);
        }
    }

    /// Handle mouse click at terminal coordinates.
    /// Returns true if click was handled.
    pub fn handle_click(&mut self, _x: u16, y: u16) -> bool {
        // Find which line was clicked
        for hit in &self.line_hits {
            if y == hit.y {
                self.cursor_line = hit.line_idx;
                // Double-click or single click opens comment
                self.start_comment();
                return true;
            }
        }
        false
    }

    /// Handle mouse click at position. Returns true if click was handled.
    /// Checks file tree first, then diff lines.
    pub fn handle_click_at(&mut self, x: u16, y: u16) -> bool {
        // Check file tree click first
        if let Some(tree_area) = self.file_tree_area {
            if x >= tree_area.x
                && x < tree_area.x + tree_area.width
                && y >= tree_area.y
                && y < tree_area.y + tree_area.height
            {
                // Click is in file tree - calculate which file
                // First row (tree_area.y) is header "Files"
                if y > tree_area.y {
                    let file_idx = (y - tree_area.y - 1) as usize;
                    if file_idx < self.files.len() {
                        self.select_file(file_idx);
                        return true;
                    }
                }
                return false;
            }
        }

        // Check diff line click
        for hit in &self.line_hits {
            if y == hit.y {
                if self.cursor_line == hit.line_idx {
                    // Re-click on same line - open comment
                    self.start_comment();
                } else {
                    // Click on different line - just select
                    self.cursor_line = hit.line_idx;
                }
                return true;
            }
        }
        false
    }

    /// Select line by click (legacy, use handle_click_at instead).
    pub fn select_by_click(&mut self, y: u16) -> bool {
        for hit in &self.line_hits {
            if y == hit.y {
                if self.cursor_line == hit.line_idx {
                    // Re-click on same line - open comment
                    self.start_comment();
                } else {
                    // Click on different line - just select
                    self.cursor_line = hit.line_idx;
                }
                return true;
            }
        }
        false
    }

    /// Toggle fullscreen mode.
    pub fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
    }

    /// Move cursor up in the current hunk.
    pub fn cursor_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            // Auto-scroll to keep cursor visible
            if self.cursor_line < self.scroll {
                self.scroll = self.cursor_line;
            }
        }
    }

    /// Move cursor down in the current hunk.
    pub fn cursor_down(&mut self) {
        if let Some(hunk) = self.current_hunk_data() {
            if self.cursor_line + 1 < hunk.lines.len() {
                self.cursor_line += 1;
                // Auto-scroll to keep cursor visible
                if let Some(area) = self.last_area {
                    // Content height is area height minus header (2) and footer (2)
                    let visible_lines = area.height.saturating_sub(4) as usize;
                    // If cursor is beyond visible area, scroll down
                    if self.cursor_line >= self.scroll + visible_lines {
                        self.scroll = self.cursor_line.saturating_sub(visible_lines) + 1;
                    }
                }
            }
        }
    }

    /// Navigate to the next hunk header in the virtual list.
    pub fn next_hunk(&mut self) {
        if self.files.is_empty() {
            return;
        }
        // Find next hunk header after current cursor position
        let file = match self.files.get(self.current_file) {
            Some(f) => f,
            None => return,
        };
        let virtual_lines = build_unified_virtual_lines(file, None, &Theme::default());
        for (idx, vline) in virtual_lines.iter().enumerate().skip(self.cursor_line + 1) {
            if matches!(vline, VirtualLine::HunkHeader { .. }) {
                self.cursor_line = idx;
                // Adjust scroll to show cursor
                if self.cursor_line < self.scroll {
                    self.scroll = self.cursor_line;
                }
                return;
            }
        }
        // No more hunks in this file, try next file
        if self.current_file + 1 < self.files.len() {
            self.current_file += 1;
            self.cursor_line = 0;
            self.scroll = 0;
        }
    }

    /// Navigate to the previous hunk header in the virtual list.
    pub fn prev_hunk(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let file = match self.files.get(self.current_file) {
            Some(f) => f,
            None => return,
        };
        let virtual_lines = build_unified_virtual_lines(file, None, &Theme::default());
        // Find previous hunk header before current cursor position
        for idx in (0..self.cursor_line).rev() {
            if matches!(virtual_lines.get(idx), Some(VirtualLine::HunkHeader { .. })) {
                self.cursor_line = idx;
                if self.cursor_line < self.scroll {
                    self.scroll = self.cursor_line;
                }
                return;
            }
        }
        // No more hunks before, try previous file
        if self.current_file > 0 {
            self.current_file -= 1;
            let prev_file = &self.files[self.current_file];
            let prev_virtual = build_unified_virtual_lines(prev_file, None, &Theme::default());
            // Go to last hunk header
            for idx in (0..prev_virtual.len()).rev() {
                if matches!(prev_virtual.get(idx), Some(VirtualLine::HunkHeader { .. })) {
                    self.cursor_line = idx;
                    self.scroll = self.cursor_line.saturating_sub(5);
                    return;
                }
            }
            self.cursor_line = 0;
            self.scroll = 0;
        }
    }

    /// Toggle between unified and split view.
    pub fn toggle_split_mode(&mut self) {
        self.split_mode = !self.split_mode;
        self.scroll = 0;
    }

    /// Scroll down by `delta` lines.
    pub fn scroll_down(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_add(delta);
    }

    /// Scroll up by `delta` lines.
    pub fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }

    /// Select current cursor line for commenting.
    pub fn start_comment(&mut self) {
        let file_idx = self.current_file;
        let hunk_idx = self.current_hunk;
        let line_idx = self.cursor_line;

        if file_idx >= self.files.len() {
            return;
        }
        let file = &self.files[file_idx];
        if hunk_idx >= file.hunks.len() {
            return;
        }
        let hunk = &file.hunks[hunk_idx];
        if line_idx >= hunk.lines.len() {
            return;
        }

        // Calculate actual line number in the new file
        let line_num = hunk.new_start + line_idx as u32;
        self.selected_line = Some((file_idx, hunk_idx, line_idx, line_num));
        self.commenting = true;
        self.comment_input.clear();
    }

    /// Check if a line has a comment.
    pub fn has_comment_at(&self, file_idx: usize, line_num: u32) -> bool {
        if file_idx >= self.files.len() {
            return false;
        }
        let path = &self.files[file_idx].path;
        self.comments
            .iter()
            .any(|c| c.file == *path && c.line == line_num)
    }

    /// Get comment for a line if exists.
    pub fn get_comment_at(&self, file_idx: usize, line_num: u32) -> Option<&str> {
        if file_idx >= self.files.len() {
            return None;
        }
        let path = &self.files[file_idx].path;
        self.comments
            .iter()
            .find(|c| c.file == *path && c.line == line_num)
            .map(|c| c.comment.as_str())
    }

    /// Add a comment on the currently selected line.
    pub fn add_comment(&mut self) {
        if let Some((file_idx, _, _, line_num)) = self.selected_line {
            if !self.comment_input.trim().is_empty() {
                let file_path = self.files[file_idx].path.clone();
                self.comments.push(LineComment {
                    file: file_path,
                    line: line_num,
                    comment: self.comment_input.trim().to_string(),
                });
            }
        }
        self.commenting = false;
        self.selected_line = None;
        self.comment_input.clear();
    }

    /// Cancel commenting.
    pub fn cancel_comment(&mut self) {
        self.commenting = false;
        self.selected_line = None;
        self.comment_input.clear();
    }

    /// Get all comments formatted for input box.
    /// Format: `[file:line] comment`
    pub fn format_comments(&self) -> String {
        self.comments
            .iter()
            .map(|c| format!("[{}:{}] {}", c.file, c.line, c.comment))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Check if there are any comments.
    pub fn has_comments(&self) -> bool {
        !self.comments.is_empty()
    }

    /// Get the current file, if any.
    pub fn current_file(&self) -> Option<&DiffFile> {
        self.files.get(self.current_file)
    }

    /// Get the current hunk, if any.
    pub fn current_hunk_data(&self) -> Option<&DiffHunk> {
        self.files
            .get(self.current_file)
            .and_then(|f| f.hunks.get(self.current_hunk))
    }

    /// Render the diff viewer into the given buffer area.
    /// Note: This takes &mut self to update line_hits for mouse support.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        self.last_area = Some(area);
        self.line_hits.clear();

        // Reserve space for footer
        let footer_height = 2;

        // File tree width
        let tree_width = if self.show_file_tree {
            30u16.min(area.width / 4)
        } else {
            0
        };

        // File tree area (left)
        if self.show_file_tree && tree_width > 0 {
            let tree_area = Rect::new(
                area.x,
                area.y,
                tree_width,
                area.height.saturating_sub(footer_height),
            );
            self.file_tree_area = Some(tree_area);
            self.render_file_tree(tree_area, buf);
        } else {
            self.file_tree_area = None;
        }

        // Diff content area (right of file tree)
        let diff_area = Rect::new(
            area.x + tree_width,
            area.y,
            area.width.saturating_sub(tree_width),
            area.height.saturating_sub(footer_height),
        );

        if self.split_mode {
            self.render_split_mut(diff_area, buf, theme);
        } else {
            self.render_unified_mut(diff_area, buf, theme);
        }

        // Render footer (full width)
        self.render_footer(area, buf);

        // Render comment overlay on top if commenting
        if self.commenting {
            self.render_comment_overlay(area, buf);
        }
    }

    fn render_file_tree(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 5 || area.height < 3 {
            return;
        }

        // Background
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_char(' ').set_bg(Color::Rgb(25, 25, 35));
            }
        }

        // Title
        let title = " Files ";
        buf[(area.x, area.y)]
            .set_char('┌')
            .set_fg(Color::DarkGray)
            .set_bg(Color::Rgb(25, 25, 35));
        for (i, ch) in title.chars().enumerate() {
            let x = area.x + 1 + i as u16;
            if x < area.x + area.width - 1 {
                buf[(x, area.y)]
                    .set_char(ch)
                    .set_fg(Color::Cyan)
                    .set_bg(Color::Rgb(25, 25, 35));
            }
        }
        for x in (area.x + 1 + title.len() as u16)..(area.x + area.width - 1) {
            buf[(x, area.y)]
                .set_char('─')
                .set_fg(Color::DarkGray)
                .set_bg(Color::Rgb(25, 25, 35));
        }
        buf[(area.x + area.width - 1, area.y)]
            .set_char('┬')
            .set_fg(Color::DarkGray)
            .set_bg(Color::Rgb(25, 25, 35));

        // Right border
        for y in (area.y + 1)..area.y + area.height {
            buf[(area.x + area.width - 1, y)]
                .set_char('│')
                .set_fg(Color::DarkGray)
                .set_bg(Color::Rgb(25, 25, 35));
        }

        // File list
        let mut y = area.y + 1;
        for (idx, file) in self.files.iter().enumerate() {
            if y >= area.y + area.height {
                break;
            }

            let is_selected = idx == self.current_file;
            let status = self.file_status(idx);
            let (adds, dels) = self.file_stats(idx);

            // Selection highlight
            let bg = if is_selected {
                Color::Rgb(50, 50, 80)
            } else {
                Color::Rgb(25, 25, 35)
            };

            for x in area.x..(area.x + area.width - 1) {
                buf[(x, y)].set_bg(bg);
            }

            // Status indicator
            let x = area.x + 1;
            buf[(x, y)]
                .set_char(status.indicator().chars().next().unwrap_or(' '))
                .set_fg(status.color())
                .set_bg(bg);

            // File name (truncated)
            let name = file.path.rsplit('/').next().unwrap_or(&file.path);
            let max_name_len = (area.width as usize).saturating_sub(12);
            let display_name: String = if name.len() > max_name_len {
                format!("{}…", &name[..max_name_len.saturating_sub(1)])
            } else {
                name.to_string()
            };

            for (i, ch) in display_name.chars().enumerate() {
                let x = area.x + 3 + i as u16;
                if x < area.x + area.width - 8 {
                    let fg = if is_selected {
                        Color::White
                    } else {
                        Color::Gray
                    };
                    buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
                }
            }

            // Stats (+N -M)
            let stats = format!("+{} -{}", adds, dels);
            let stats_x = area.x + area.width - 2 - stats.len() as u16;
            for (i, ch) in stats.chars().enumerate() {
                let x = stats_x + i as u16;
                if x < area.x + area.width - 1 {
                    let fg = if ch == '+' || (i > 0 && stats.chars().nth(i - 1) == Some('+')) {
                        Color::Green
                    } else if ch == '-' {
                        Color::Red
                    } else {
                        Color::DarkGray
                    };
                    buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
                }
            }

            y += 1;
        }
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let footer_y = if self.commenting {
            area.y + area.height.saturating_sub(3)
        } else {
            area.y + area.height.saturating_sub(2)
        };

        // Comment input line (if commenting)
        if self.commenting {
            let input_y = footer_y;
            let file_info = self
                .selected_line
                .map(|(f, _, _, line)| {
                    let path = &self.files[f].path;
                    format!("[{}:{}] ", path, line)
                })
                .unwrap_or_default();

            // Input background
            for x in area.x..area.x + area.width {
                buf[(x, input_y)].set_char(' ').set_bg(Color::DarkGray);
            }

            let prompt = format!("Comment {}", file_info);
            for (i, ch) in prompt.chars().enumerate() {
                let x = area.x + 1 + i as u16;
                if x < area.x + area.width - 1 {
                    buf[(x, input_y)]
                        .set_char(ch)
                        .set_fg(Color::Yellow)
                        .set_bg(Color::DarkGray);
                }
            }

            // Input text
            let input_start = area.x + 1 + prompt.len() as u16;
            for (i, ch) in self.comment_input.chars().enumerate() {
                let x = input_start + i as u16;
                if x < area.x + area.width - 1 {
                    buf[(x, input_y)]
                        .set_char(ch)
                        .set_fg(Color::White)
                        .set_bg(Color::DarkGray);
                }
            }

            // Cursor
            let cursor_x = input_start + self.comment_input.len() as u16;
            if cursor_x < area.x + area.width - 1 {
                buf[(cursor_x, input_y)]
                    .set_char('▏')
                    .set_fg(Color::Cyan)
                    .set_bg(Color::DarkGray);
            }
        }

        // Comments count (if any)
        let comments_y = if self.commenting {
            footer_y + 1
        } else {
            footer_y
        };
        if !self.comments.is_empty() {
            let count_text = format!(
                " {} comment{} ",
                self.comments.len(),
                if self.comments.len() == 1 { "" } else { "s" }
            );
            for (i, ch) in count_text.chars().enumerate() {
                let x = area.x + 1 + i as u16;
                if x < area.x + area.width {
                    buf[(x, comments_y)].set_char(ch).set_fg(Color::Green);
                }
            }
        }

        // Help hints
        let hints_y = area.y + area.height.saturating_sub(1);
        let hints = if self.commenting {
            "Enter: save · Esc: cancel"
        } else {
            "j/k: line · n/p: file · ]/[: hunk · c: comment · b: tree · u: split · Esc: close"
        };
        for (i, ch) in hints.chars().enumerate() {
            let x = area.x + 1 + i as u16;
            if x < area.x + area.width {
                buf[(x, hints_y)].set_char(ch).set_fg(Color::DarkGray);
            }
        }
    }

    /// Render the comment input overlay (centered box).
    pub fn render_comment_overlay(&self, area: Rect, buf: &mut Buffer) {
        if !self.commenting {
            return;
        }

        // Box width and content width
        let box_w = 70.min(area.width.saturating_sub(4));
        let content_w = (box_w - 4) as usize; // 2 for border, 2 for padding

        // Wrap input text into lines
        let input_chars: Vec<char> = self.comment_input.chars().collect();
        let mut wrapped_lines: Vec<String> = Vec::new();
        let mut pos = 0;
        while pos < input_chars.len() {
            let end = (pos + content_w).min(input_chars.len());
            wrapped_lines.push(input_chars[pos..end].iter().collect());
            pos = end;
        }
        if wrapped_lines.is_empty() {
            wrapped_lines.push(String::new());
        }

        // Calculate box height: title + file info + wrapped lines + hint + borders
        let input_lines = wrapped_lines.len().min(8) as u16; // Max 8 lines of input
        let box_h = 4 + input_lines; // title(1) + file(1) + input(N) + hint(1) + border(1)
        let box_x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let box_y = area.y + (area.height.saturating_sub(box_h)) / 2;

        // Draw box background
        for y in box_y..box_y + box_h {
            for x in box_x..box_x + box_w {
                if x < area.x + area.width && y < area.y + area.height {
                    buf[(x, y)].set_char(' ').set_bg(Color::Black);
                }
            }
        }

        // Draw border
        buf[(box_x, box_y)].set_char('╭').set_fg(Color::Yellow);
        buf[(box_x + box_w - 1, box_y)]
            .set_char('╮')
            .set_fg(Color::Yellow);
        buf[(box_x, box_y + box_h - 1)]
            .set_char('╰')
            .set_fg(Color::Yellow);
        buf[(box_x + box_w - 1, box_y + box_h - 1)]
            .set_char('╯')
            .set_fg(Color::Yellow);
        for x in (box_x + 1)..(box_x + box_w - 1) {
            buf[(x, box_y)].set_char('─').set_fg(Color::Yellow);
            buf[(x, box_y + box_h - 1)]
                .set_char('─')
                .set_fg(Color::Yellow);
        }
        for y in (box_y + 1)..(box_y + box_h - 1) {
            buf[(box_x, y)].set_char('│').set_fg(Color::Yellow);
            buf[(box_x + box_w - 1, y)]
                .set_char('│')
                .set_fg(Color::Yellow);
        }

        // Title
        let title = " Add Comment ";
        let title_x = box_x + (box_w.saturating_sub(title.len() as u16)) / 2;
        for (i, ch) in title.chars().enumerate() {
            buf[(title_x + i as u16, box_y)]
                .set_char(ch)
                .set_fg(Color::Yellow)
                .set_bg(Color::Black);
        }

        // File:line info
        if let Some((file_idx, _, _, line_num)) = self.selected_line {
            if file_idx < self.files.len() {
                let path = &self.files[file_idx].path;
                let info = format!("{}:{}", path, line_num);
                let info_x = box_x + 2;
                for (i, ch) in info.chars().take(content_w).enumerate() {
                    buf[(info_x + i as u16, box_y + 1)]
                        .set_char(ch)
                        .set_fg(Color::Cyan)
                        .set_bg(Color::Black);
                }
            }
        }

        // Input lines (wrapped)
        for (line_idx, line) in wrapped_lines.iter().enumerate() {
            let input_y = box_y + 2 + line_idx as u16;
            if input_y >= box_y + box_h - 1 {
                break;
            }

            // Prompt on first line only
            if line_idx == 0 {
                buf[(box_x + 2, input_y)]
                    .set_char('>')
                    .set_fg(Color::DarkGray)
                    .set_bg(Color::Black);
            }

            let text_x = box_x + 4;
            for (i, ch) in line.chars().enumerate() {
                if text_x + (i as u16) < box_x + box_w - 1 {
                    buf[(text_x + i as u16, input_y)]
                        .set_char(ch)
                        .set_fg(Color::White)
                        .set_bg(Color::Black);
                }
            }
        }

        // Cursor position
        let cursor_line =
            (input_chars.len() / content_w).min(wrapped_lines.len().saturating_sub(1));
        let cursor_col = input_chars.len() % content_w;
        let cursor_y = box_y + 2 + cursor_line as u16;
        let cursor_x = box_x + 4 + cursor_col as u16;
        if cursor_y < box_y + box_h - 1 && cursor_x < box_x + box_w - 1 {
            buf[(cursor_x, cursor_y)]
                .set_char('▏')
                .set_fg(Color::Cyan)
                .set_bg(Color::Black);
        }

        // Hint
        let hint = "Enter: save · Esc: cancel";
        let hint_x = box_x + (box_w.saturating_sub(hint.len() as u16)) / 2;
        for (i, ch) in hint.chars().enumerate() {
            buf[(hint_x + i as u16, box_y + 3)]
                .set_char(ch)
                .set_fg(Color::DarkGray)
                .set_bg(Color::Black);
        }
    }

    // ── Private rendering ─────────────────────────────────────────────────────

    fn render_unified_mut(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let file = match self.files.get(self.current_file) {
            Some(f) => f,
            None => return,
        };

        // Clear the area first to avoid artifacts
        clear_area(buf, area);

        // Extract language from file extension
        let lang = file.path.rsplit('.').next();

        // Title bar
        let title = format!("─ Diff: {} ", file.path);
        let title_line = Line::from(vec![
            Span::styled("┌", Style::default().fg(Color::DarkGray)),
            Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "─".repeat(area.width.saturating_sub(3 + file.path.len() as u16 + 9) as usize),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("┐", Style::default().fg(Color::DarkGray)),
        ]);
        render_line(buf, area.x, area.y, area.width, &title_line);

        // Build all virtual lines for all hunks in this file
        let virtual_lines = build_unified_virtual_lines(file, lang, theme);
        let content_height = (area.height.saturating_sub(2)) as usize; // title + bottom border
        let skip = self.scroll.min(virtual_lines.len());
        let mut row = area.y + 1;

        for (vline_idx, vline) in virtual_lines.iter().enumerate().skip(skip) {
            if row >= area.y + area.height.saturating_sub(1) {
                break;
            }

            let is_cursor_line = vline_idx == self.cursor_line;

            match vline {
                VirtualLine::HunkHeader { text, .. } => {
                    let header_line = Line::from(Span::styled(
                        text.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    render_line(buf, area.x, row, area.width, &header_line);
                }
                VirtualLine::HunkSeparator { lines_between } => {
                    let sep_text = format!(" ··· {} unchanged lines ···", lines_between);
                    let sep_line = Line::from(Span::styled(
                        sep_text,
                        Style::default().fg(Color::DarkGray),
                    ));
                    render_line(buf, area.x, row, area.width, &sep_line);
                }
                VirtualLine::Diff { line, old_line_no, new_line_no } => {
                    let mut old_no = *old_line_no;
                    let mut new_no = *new_line_no;
                    let rendered = render_unified_line(line, &mut old_no, &mut new_no, lang, theme);
                    render_line(buf, area.x, row, area.width, &rendered);

                    // Show comment indicator
                    if self.has_comment_at(self.current_file, *new_line_no) {
                        let indicator_x = area.x + area.width - 3;
                        if indicator_x > area.x {
                            buf[(indicator_x, row)].set_char('💬').set_fg(Color::Yellow);
                        }
                    }
                }
            }

            // Highlight cursor line
            if is_cursor_line {
                buf[(area.x, row)]
                    .set_char('▶')
                    .set_fg(Color::Yellow)
                    .set_bg(Color::Rgb(50, 50, 80));
                for x in (area.x + 1)..area.x + area.width {
                    let cell = &mut buf[(x, row)];
                    cell.set_bg(Color::Rgb(50, 50, 80));
                }
            }

            // Record line hit for mouse support
            self.line_hits.push(DiffLineHit {
                y: row,
                line_idx: vline_idx,
                file_idx: self.current_file,
            });

            row += 1;
        }

        // Bottom border
        let bottom_y = area.y + area.height.saturating_sub(1);
        let bottom = Line::from(vec![
            Span::styled("└", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "─".repeat(area.width.saturating_sub(2) as usize),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("┘", Style::default().fg(Color::DarkGray)),
        ]);
        render_line(buf, area.x, bottom_y, area.width, &bottom);
        let _ = content_height;
    }

    fn render_split_mut(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let file = match self.files.get(self.current_file) {
            Some(f) => f,
            None => return,
        };

        // Clear the area first to avoid artifacts
        clear_area(buf, area);

        // Extract language from file extension
        let lang = file.path.rsplit('.').next();

        let half = area.width / 2;

        // Title bar
        let left_title = "─ Old ";
        let right_title = "─ New ";
        let left_fill = "─".repeat(half.saturating_sub(2 + left_title.len() as u16) as usize);
        let right_fill = "─".repeat(
            area.width
                .saturating_sub(half + 2 + right_title.len() as u16) as usize,
        );

        let title_line = Line::from(vec![
            Span::styled("┌", Style::default().fg(Color::DarkGray)),
            Span::styled(
                left_title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(left_fill, Style::default().fg(Color::DarkGray)),
            Span::styled("┬", Style::default().fg(Color::DarkGray)),
            Span::styled(
                right_title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(right_fill, Style::default().fg(Color::DarkGray)),
            Span::styled("┐", Style::default().fg(Color::DarkGray)),
        ]);
        render_line(buf, area.x, area.y, area.width, &title_line);

        // Build all virtual split rows for all hunks
        let split_rows = build_split_virtual_rows(file, lang, theme);
        let skip = self.scroll.min(split_rows.len());
        let mut row = area.y + 1;

        for (line_idx, vrow) in split_rows.iter().enumerate().skip(skip) {
            if row >= area.y + area.height.saturating_sub(1) {
                break;
            }

            let is_cursor_line = line_idx == self.cursor_line;

            match vrow {
                SplitVirtualRow::HunkHeader { text } => {
                    let header_line = Line::from(Span::styled(
                        text.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    render_line(buf, area.x, row, area.width, &header_line);
                }
                SplitVirtualRow::Separator { lines_between } => {
                    let sep_text = format!(" ··· {} unchanged lines ···", lines_between);
                    let sep_line = Line::from(Span::styled(
                        sep_text,
                        Style::default().fg(Color::DarkGray),
                    ));
                    render_line(buf, area.x, row, area.width, &sep_line);
                }
                SplitVirtualRow::Cells { left, right } => {
                    let left_line = render_split_cell(left, half.saturating_sub(1));
                    let right_line = render_split_cell(right, area.width.saturating_sub(half + 1));

                    render_line(buf, area.x, row, half.saturating_sub(1), &left_line);
                    if let Some(cell) = buf.cell_mut((area.x + half.saturating_sub(1), row)) {
                        cell.set_char('│');
                        cell.set_style(Style::default().fg(Color::DarkGray));
                    }
                    render_line(
                        buf,
                        area.x + half,
                        row,
                        area.width.saturating_sub(half),
                        &right_line,
                    );
                }
            }

            // Highlight cursor line
            if is_cursor_line {
                buf[(area.x, row)]
                    .set_char('▶')
                    .set_fg(Color::Black)
                    .set_bg(Color::Yellow);
                for x in (area.x + 1)..area.x + area.width {
                    let cell = &mut buf[(x, row)];
                    cell.set_bg(Color::Rgb(60, 60, 100));
                }
            }

            // Record line hit for mouse support
            self.line_hits.push(DiffLineHit {
                y: row,
                line_idx,
                file_idx: self.current_file,
            });

            row += 1;
        }

        // Bottom border
        let bottom_y = area.y + area.height.saturating_sub(1);
        let bottom = Line::from(vec![
            Span::styled("└", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "─".repeat(half.saturating_sub(2) as usize),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("┴", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "─".repeat(area.width.saturating_sub(half + 1) as usize),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("┘", Style::default().fg(Color::DarkGray)),
        ]);
        render_line(buf, area.x, bottom_y, area.width, &bottom);
    }
}

// ── Free helper functions ─────────────────────────────────────────────────────

// Colors for diff display
const DIFF_ADD_FG: Color = Color::Rgb(0, 200, 80);
const DIFF_ADD_BG: Color = Color::Rgb(0, 35, 0);
const DIFF_DEL_FG: Color = Color::Rgb(255, 100, 100);
const DIFF_DEL_BG: Color = Color::Rgb(50, 0, 0);
const DIFF_LINE_NUM_FG: Color = Color::Rgb(100, 100, 100);

/// Clear an area by filling with spaces.
fn clear_area(buf: &mut Buffer, area: Rect) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }
    }
}

/// Virtual line type for unified view (all hunks flattened).
enum VirtualLine {
    HunkHeader { text: String },
    HunkSeparator { lines_between: u32 },
    Diff { line: DiffLine, old_line_no: u32, new_line_no: u32 },
}

/// Virtual row type for split view (all hunks flattened).
enum SplitVirtualRow {
    HunkHeader { text: String },
    Separator { lines_between: u32 },
    Cells { left: SplitCell, right: SplitCell },
}

/// Build all virtual lines for a file (all hunks with separators).
fn build_unified_virtual_lines(file: &DiffFile, _lang: Option<&str>, _theme: &Theme) -> Vec<VirtualLine> {
    let mut lines = Vec::new();
    let mut prev_hunk_end: Option<u32> = None;

    for hunk in &file.hunks {
        // Add separator between hunks showing lines skipped
        if let Some(prev_end) = prev_hunk_end {
            if hunk.new_start > prev_end {
                let gap = hunk.new_start - prev_end;
                if gap > 0 {
                    lines.push(VirtualLine::HunkSeparator { lines_between: gap });
                }
            }
        }

        // Hunk header
        let header = format!(
            " @@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        );
        lines.push(VirtualLine::HunkHeader { text: header });

        // Diff lines with line numbers
        let mut old_no = hunk.old_start;
        let mut new_no = hunk.new_start;
        for diff_line in &hunk.lines {
            let (old_line_no, new_line_no) = match diff_line {
                DiffLine::Context(_) => {
                    let nums = (old_no, new_no);
                    old_no += 1;
                    new_no += 1;
                    nums
                }
                DiffLine::Removed(_) => {
                    let nums = (old_no, new_no);
                    old_no += 1;
                    nums
                }
                DiffLine::Added(_) => {
                    let nums = (old_no, new_no);
                    new_no += 1;
                    nums
                }
            };
            lines.push(VirtualLine::Diff {
                line: diff_line.clone(),
                old_line_no,
                new_line_no,
            });
        }

        prev_hunk_end = Some(hunk.new_start + hunk.new_count);
    }

    lines
}

/// Build all virtual split rows for a file (all hunks with separators).
fn build_split_virtual_rows(file: &DiffFile, lang: Option<&str>, theme: &Theme) -> Vec<SplitVirtualRow> {
    let mut rows = Vec::new();
    let mut prev_hunk_end: Option<u32> = None;

    for hunk in &file.hunks {
        // Add separator between hunks
        if let Some(prev_end) = prev_hunk_end {
            if hunk.new_start > prev_end {
                let gap = hunk.new_start - prev_end;
                if gap > 0 {
                    rows.push(SplitVirtualRow::Separator { lines_between: gap });
                }
            }
        }

        // Hunk header
        let header = format!(
            " @@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        );
        rows.push(SplitVirtualRow::HunkHeader { text: header });

        // Build split cells for this hunk
        let split_cells = build_split_rows(&hunk.lines, lang, theme);
        for (left, right) in split_cells {
            rows.push(SplitVirtualRow::Cells { left, right });
        }

        prev_hunk_end = Some(hunk.new_start + hunk.new_count);
    }

    rows
}

/// Render a `Line` into the buffer at (x, y), clipped to `width`.
fn render_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &Line) {
    let mut col = x;
    for span in &line.spans {
        for ch in span.content.chars() {
            if col >= x + width {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, y)) {
                cell.set_char(ch);
                cell.set_style(span.style);
            }
            col += 1;
        }
    }
    // Pad remainder with spaces.
    while col < x + width {
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_char(' ');
            cell.set_style(Style::default());
        }
        col += 1;
    }
}

/// Build a single unified-mode line with syntax-highlighted content.
fn render_unified_line(
    diff_line: &DiffLine,
    old_no: &mut u32,
    new_no: &mut u32,
    lang: Option<&str>,
    theme: &Theme,
) -> Line<'static> {
    match diff_line {
        DiffLine::Context(text) => {
            let line_num = format!("{:>4}", new_no);
            *old_no += 1;
            *new_no += 1;

            let mut spans = vec![
                Span::styled(line_num, Style::default().fg(DIFF_LINE_NUM_FG)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            ];
            spans.extend(highlight_code_inline(text, lang, theme));
            Line::from(spans)
        }
        DiffLine::Removed(text) => {
            let line_num = format!("{:>4}", old_no);
            *old_no += 1;

            let mut spans = vec![
                Span::styled(
                    line_num,
                    Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG),
                ),
                Span::styled(
                    " ┃─",
                    Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG).add_modifier(Modifier::BOLD),
                ),
            ];
            // Highlight then apply diff background
            for span in highlight_code_inline(text, lang, theme) {
                spans.push(Span::styled(
                    span.content.to_string(),
                    span.style.bg(DIFF_DEL_BG),
                ));
            }
            Line::from(spans)
        }
        DiffLine::Added(text) => {
            let line_num = format!("{:>4}", new_no);
            *new_no += 1;

            let mut spans = vec![
                Span::styled(
                    line_num,
                    Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG),
                ),
                Span::styled(
                    " ┃+",
                    Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG).add_modifier(Modifier::BOLD),
                ),
            ];
            // Highlight then apply diff background
            for span in highlight_code_inline(text, lang, theme) {
                spans.push(Span::styled(
                    span.content.to_string(),
                    span.style.bg(DIFF_ADD_BG),
                ));
            }
            Line::from(spans)
        }
    }
}

/// A cell in split view: optional line number + highlighted content spans.
#[derive(Clone)]
struct SplitCell {
    line_no: Option<u32>,
    spans: Vec<Span<'static>>,
    is_added: bool,
    is_removed: bool,
}

/// Build paired (left, right) rows for split view with syntax highlighting.
fn build_split_rows(lines: &[DiffLine], lang: Option<&str>, theme: &Theme) -> Vec<(SplitCell, SplitCell)> {
    let mut rows: Vec<(SplitCell, SplitCell)> = Vec::new();
    let mut i = 0;
    let mut old_no: u32 = 1;
    let mut new_no: u32 = 1;

    while i < lines.len() {
        match &lines[i] {
            DiffLine::Context(text) => {
                let highlighted = highlight_code_inline(text, lang, theme);
                rows.push((
                    SplitCell {
                        line_no: Some(old_no),
                        spans: highlighted.clone(),
                        is_added: false,
                        is_removed: false,
                    },
                    SplitCell {
                        line_no: Some(new_no),
                        spans: highlighted,
                        is_added: false,
                        is_removed: false,
                    },
                ));
                old_no += 1;
                new_no += 1;
                i += 1;
            }
            DiffLine::Removed(text) => {
                // Highlight and apply diff background
                let highlighted: Vec<Span<'static>> = highlight_code_inline(text, lang, theme)
                    .into_iter()
                    .map(|s| Span::styled(s.content.to_string(), s.style.bg(DIFF_DEL_BG)))
                    .collect();

                // Peek ahead for a matching Added.
                let right = if i + 1 < lines.len() {
                    if let DiffLine::Added(added) = &lines[i + 1] {
                        let added_highlighted: Vec<Span<'static>> = highlight_code_inline(added, lang, theme)
                            .into_iter()
                            .map(|s| Span::styled(s.content.to_string(), s.style.bg(DIFF_ADD_BG)))
                            .collect();
                        let cell = SplitCell {
                            line_no: Some(new_no),
                            spans: added_highlighted,
                            is_added: true,
                            is_removed: false,
                        };
                        new_no += 1;
                        i += 2;
                        cell
                    } else {
                        i += 1;
                        SplitCell {
                            line_no: None,
                            spans: vec![],
                            is_added: false,
                            is_removed: false,
                        }
                    }
                } else {
                    i += 1;
                    SplitCell {
                        line_no: None,
                        spans: vec![],
                        is_added: false,
                        is_removed: false,
                    }
                };
                rows.push((
                    SplitCell {
                        line_no: Some(old_no),
                        spans: highlighted,
                        is_added: false,
                        is_removed: true,
                    },
                    right,
                ));
                old_no += 1;
            }
            DiffLine::Added(text) => {
                let highlighted: Vec<Span<'static>> = highlight_code_inline(text, lang, theme)
                    .into_iter()
                    .map(|s| Span::styled(s.content.to_string(), s.style.bg(DIFF_ADD_BG)))
                    .collect();
                rows.push((
                    SplitCell {
                        line_no: None,
                        spans: vec![],
                        is_added: false,
                        is_removed: false,
                    },
                    SplitCell {
                        line_no: Some(new_no),
                        spans: highlighted,
                        is_added: true,
                        is_removed: false,
                    },
                ));
                new_no += 1;
                i += 1;
            }
        }
    }
    rows
}

/// Render a split cell into a `Line`.
fn render_split_cell(cell: &SplitCell, width: u16) -> Line<'static> {
    let line_num_style = if cell.is_added {
        Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG)
    } else if cell.is_removed {
        Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG)
    } else if cell.line_no.is_some() {
        Style::default().fg(DIFF_LINE_NUM_FG)
    } else {
        Style::default().fg(Color::DarkGray).bg(Color::Rgb(30, 30, 30))
    };

    let prefix = match cell.line_no {
        Some(n) => format!("{:>4}", n),
        None => "    ".to_string(),
    };

    let indicator = if cell.is_added {
        "┃+"
    } else if cell.is_removed {
        "┃─"
    } else if cell.line_no.is_some() {
        " │"
    } else {
        " ░"
    };

    let indicator_style = if cell.is_added {
        Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG).add_modifier(Modifier::BOLD)
    } else if cell.is_removed {
        Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let content_width = (width as usize).saturating_sub(7);
    
    // Build output spans with prefix and indicator
    let mut out = vec![
        Span::styled(prefix, line_num_style),
        Span::styled(indicator.to_string(), indicator_style),
    ];
    
    // Add highlighted content spans, truncating to fit
    let mut chars_used = 0;
    for span in &cell.spans {
        if chars_used >= content_width {
            break;
        }
        let remaining = content_width - chars_used;
        let content: String = span.content.chars().take(remaining).collect();
        chars_used += content.chars().count();
        out.push(Span::styled(content, span.style));
    }

    Line::from(out)
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a unified diff (git diff output) into a list of `DiffFile`s.
///
/// Handles:
/// - `diff --git a/... b/...` file headers
/// - `--- a/...` / `+++ b/...` path lines
/// - `@@ -old_start,old_count +new_start,new_count @@` hunk headers
/// - ` ` context lines, `+` added lines, `-` removed lines
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<DiffHunk> = None;

    for line in input.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous hunk and file.
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current_file {
                    file.hunks.push(hunk);
                }
            }
            if let Some(file) = current_file.take() {
                files.push(file);
            }
            // Extract path from "diff --git a/path b/path" — use b/ side.
            let path = line
                .split_whitespace()
                .last()
                .and_then(|p| p.strip_prefix("b/"))
                .unwrap_or("")
                .to_string();
            current_file = Some(DiffFile {
                path,
                hunks: Vec::new(),
            });
        } else if line.starts_with("+++ ") {
            // Override path from +++ line (more reliable than diff --git).
            let raw = &line[4..];
            let path = raw.strip_prefix("b/").unwrap_or(raw).to_string();
            if let Some(ref mut file) = current_file {
                if !path.is_empty() && path != "/dev/null" {
                    file.path = path;
                }
            } else {
                // No diff --git header seen yet — create file entry.
                current_file = Some(DiffFile {
                    path,
                    hunks: Vec::new(),
                });
            }
        } else if line.starts_with("--- ") {
            // Ignore --- lines (we use +++ for the canonical path).
        } else if line.starts_with("@@ ") {
            // Flush previous hunk.
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current_file {
                    file.hunks.push(hunk);
                }
            }
            current_hunk = Some(parse_hunk_header(line));
        } else if let Some(ref mut hunk) = current_hunk {
            if line.starts_with('+') {
                hunk.lines.push(DiffLine::Added(line[1..].to_string()));
            } else if line.starts_with('-') {
                hunk.lines.push(DiffLine::Removed(line[1..].to_string()));
            } else if line.starts_with(' ') {
                hunk.lines.push(DiffLine::Context(line[1..].to_string()));
            }
            // Lines that don't match (e.g. "\ No newline at end of file") are ignored.
        }
    }

    // Flush final hunk and file.
    if let Some(hunk) = current_hunk.take() {
        if let Some(ref mut file) = current_file {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current_file.take() {
        files.push(file);
    }

    files
}

/// Parse `@@ -old_start[,old_count] +new_start[,new_count] @@` into a `DiffHunk`.
fn parse_hunk_header(line: &str) -> DiffHunk {
    // Format: @@ -A[,B] +C[,D] @@ [optional context]
    let mut old_start = 0u32;
    let mut old_count = 0u32;
    let mut new_start = 0u32;
    let mut new_count = 0u32;

    // Find the part between the first and second "@@".
    let inner = line.trim_start_matches('@').trim_start();
    let parts: Vec<&str> = inner.split_whitespace().collect();

    for part in &parts {
        if let Some(old) = part.strip_prefix('-') {
            let (s, c) = parse_range(old);
            old_start = s;
            old_count = c;
        } else if let Some(new) = part.strip_prefix('+') {
            let (s, c) = parse_range(new);
            new_start = s;
            new_count = c;
        } else if *part == "@@" {
            break;
        }
    }

    DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    }
}

/// Parse "start[,count]" into (start, count). Count defaults to 1 if absent.
fn parse_range(s: &str) -> (u32, u32) {
    if let Some(comma) = s.find(',') {
        let start = s[..comma].parse().unwrap_or(0);
        let count = s[comma + 1..].parse().unwrap_or(1);
        (start, count)
    } else {
        let start = s.parse().unwrap_or(0);
        (start, 1)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_DIFF: &str = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -10,5 +10,7 @@
 context line
-old line
+new line
+another new
 context
";

    const MULTI_FILE_DIFF: &str = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,3 +1,3 @@
 context
-removed
+added
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5,2 +5,2 @@
 other context
-old lib line
+new lib line
";

    #[test]
    fn test_parse_simple_diff() {
        let files = parse_unified_diff(SIMPLE_DIFF);
        assert_eq!(files.len(), 1, "expected 1 file");
        let file = &files[0];
        assert_eq!(file.path, "src/app.rs");
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.old_start, 10);
        assert_eq!(hunk.old_count, 5);
        assert_eq!(hunk.new_start, 10);
        assert_eq!(hunk.new_count, 7);
        assert_eq!(hunk.lines.len(), 5);
        assert_eq!(hunk.lines[0], DiffLine::Context("context line".into()));
        assert_eq!(hunk.lines[1], DiffLine::Removed("old line".into()));
        assert_eq!(hunk.lines[2], DiffLine::Added("new line".into()));
        assert_eq!(hunk.lines[3], DiffLine::Added("another new".into()));
        assert_eq!(hunk.lines[4], DiffLine::Context("context".into()));
    }

    #[test]
    fn test_parse_multi_file_diff() {
        let files = parse_unified_diff(MULTI_FILE_DIFF);
        assert_eq!(files.len(), 2, "expected 2 files");
        assert_eq!(files[0].path, "src/app.rs");
        assert_eq!(files[1].path, "src/lib.rs");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[1].hunks.len(), 1);
        // Check second file hunk.
        let hunk = &files[1].hunks[0];
        assert_eq!(hunk.old_start, 5);
        assert_eq!(hunk.lines[1], DiffLine::Removed("old lib line".into()));
        assert_eq!(hunk.lines[2], DiffLine::Added("new lib line".into()));
    }

    #[test]
    fn test_hunk_navigation() {
        let diff = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,2 +1,2 @@
 ctx
-old1
+new1
@@ -10,2 +10,2 @@
 ctx2
-old2
+new2
";
        let files = parse_unified_diff(diff);
        let mut viewer = DiffViewer::new(files);
        // In virtual list: line 0 = hunk1 header, lines 1-3 = hunk1 content,
        // line 4 = separator, line 5 = hunk2 header, lines 6-8 = hunk2 content
        assert_eq!(viewer.cursor_line, 0); // starts at first line

        viewer.next_hunk();
        // Should jump to second hunk header
        assert!(viewer.cursor_line > 0, "cursor should move forward to next hunk");

        viewer.prev_hunk();
        // Should go back to first hunk header (line 0)
        assert_eq!(viewer.cursor_line, 0);
    }

    #[test]
    fn test_split_mode_toggle() {
        let files = parse_unified_diff(SIMPLE_DIFF);
        let mut viewer = DiffViewer::new(files);
        assert!(viewer.split_mode, "starts in split mode (side-by-side)");

        viewer.toggle_split_mode();
        assert!(!viewer.split_mode, "should be in unified mode after toggle");

        viewer.toggle_split_mode();
        assert!(viewer.split_mode, "should be back in split mode");
    }

    #[test]
    fn test_empty_diff() {
        let files = parse_unified_diff("");
        assert!(files.is_empty(), "empty input should produce no files");

        let mut viewer = DiffViewer::new(files);
        // Navigation on empty viewer must not panic.
        viewer.next_hunk();
        viewer.prev_hunk();
        assert_eq!(viewer.current_file, 0);
        assert_eq!(viewer.current_hunk, 0);
        assert!(viewer.current_file().is_none());
        assert!(viewer.current_hunk_data().is_none());
    }
}
