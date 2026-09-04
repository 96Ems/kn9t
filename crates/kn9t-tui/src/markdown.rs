//! Markdown renderer for TUI.
//!
//! Converts markdown text to styled ratatui Lines.
//! Supports: headers, bold, italic, code, code blocks, lists, tables, links.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::syntax;
use crate::theme::Theme;

/// Render markdown text to styled Lines with word-wrapping.
///
/// `width` is the available width for text. If 0, no wrapping is applied.
pub fn render(text: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    // Enable tables, strikethrough, and other extensions.
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(text, options);
    let mut renderer = MarkdownRenderer::new(theme, width);

    for event in parser {
        renderer.process(event);
    }

    renderer.finish()
}

struct MarkdownRenderer<'t> {
    theme: &'t Theme,
    width: usize,
    lines: Vec<Line<'static>>,
    current_line: Vec<Span<'static>>,
    current_line_len: usize, // Track visual length for wrapping.

    // Style stack.
    bold: bool,
    italic: bool,
    code: bool,
    strikethrough: bool,

    // Block state.
    in_code_block: bool,
    code_block_lang: Option<String>,
    code_block_content: String,

    in_heading: Option<HeadingLevel>,
    in_list: bool,
    list_depth: usize,
    in_blockquote: bool,

    // Table state.
    in_table: bool,
    table_row: Vec<String>,
    table_rows: Vec<Vec<String>>,
    table_alignments: Vec<pulldown_cmark::Alignment>,
    in_table_head: bool,

    // Link state.
    in_link: bool,
    link_url: String,
}

impl<'t> MarkdownRenderer<'t> {
    fn new(theme: &'t Theme, width: usize) -> Self {
        Self {
            theme,
            width,
            lines: Vec::new(),
            current_line: Vec::new(),
            current_line_len: 0,
            bold: false,
            italic: false,
            code: false,
            strikethrough: false,
            in_code_block: false,
            code_block_lang: None,
            code_block_content: String::new(),
            in_heading: None,
            in_list: false,
            list_depth: 0,
            in_blockquote: false,
            in_table: false,
            table_row: Vec::new(),
            table_rows: Vec::new(),
            table_alignments: Vec::new(),
            in_table_head: false,
            in_link: false,
            link_url: String::new(),
        }
    }

    fn process(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline_code(&code),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.horizontal_rule(),
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.in_heading = Some(level);
            }
            Tag::BlockQuote => {
                self.in_blockquote = true;
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.to_string();
                        if l.is_empty() {
                            None
                        } else {
                            Some(l)
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code_block_content.clear();
            }
            Tag::List(_) => {
                self.in_list = true;
                self.list_depth += 1;
            }
            Tag::Item => {
                // Add list bullet.
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                let bullet = if self.list_depth % 2 == 1 {
                    "•"
                } else {
                    "◦"
                };
                self.current_line.push(Span::styled(
                    format!("{}{} ", indent, bullet),
                    Style::default().fg(self.theme.muted),
                ));
            }
            Tag::Emphasis => {
                self.italic = true;
            }
            Tag::Strong => {
                self.bold = true;
            }
            Tag::Strikethrough => {
                self.strikethrough = true;
            }
            Tag::Link { dest_url, .. } => {
                self.in_link = true;
                self.link_url = dest_url.to_string();
            }
            Tag::Table(alignments) => {
                self.in_table = true;
                self.table_alignments = alignments;
                self.table_rows.clear();
            }
            Tag::TableHead => {
                self.in_table_head = true;
                self.table_row.clear();
            }
            Tag::TableRow => {
                self.table_row.clear();
            }
            Tag::TableCell => {}
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
                self.lines.push(Line::default()); // Empty line after paragraph.
            }
            TagEnd::Heading(_) => {
                self.flush_line();
                self.in_heading = None;
            }
            TagEnd::BlockQuote => {
                self.in_blockquote = false;
            }
            TagEnd::CodeBlock => {
                self.render_code_block();
                self.in_code_block = false;
                self.code_block_lang = None;
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                if self.list_depth == 0 {
                    self.in_list = false;
                }
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::Emphasis => {
                self.italic = false;
            }
            TagEnd::Strong => {
                self.bold = false;
            }
            TagEnd::Strikethrough => {
                self.strikethrough = false;
            }
            TagEnd::Link => {
                // Add link indicator.
                if !self.link_url.is_empty() {
                    self.current_line.push(Span::styled(
                        format!(" [{}]", truncate_url(&self.link_url, 30)),
                        Style::default().fg(self.theme.muted),
                    ));
                }
                self.in_link = false;
                self.link_url.clear();
            }
            TagEnd::Table => {
                self.render_table();
                self.in_table = false;
            }
            TagEnd::TableHead => {
                self.table_rows.push(self.table_row.clone());
                self.in_table_head = false;
            }
            TagEnd::TableRow => {
                if !self.in_table_head {
                    self.table_rows.push(self.table_row.clone());
                }
            }
            TagEnd::TableCell => {
                // Collect current line content as cell.
                let cell_text: String = self
                    .current_line
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                self.table_row.push(cell_text);
                self.current_line.clear();
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_content.push_str(text);
            return;
        }

        let style = self.current_style();

        // Handle blockquote prefix.
        if self.in_blockquote && self.current_line.is_empty() {
            self.current_line.push(Span::styled(
                "│ ".to_string(),
                Style::default().fg(self.theme.muted),
            ));
            self.current_line_len += 2;
        }

        // Word-wrap text if width is set.
        if self.width > 0 {
            self.wrap_and_push(text, style);
        } else {
            self.current_line
                .push(Span::styled(text.to_string(), style));
        }
    }

    /// Word-wrap text and push spans, breaking at word boundaries when possible.
    fn wrap_and_push(&mut self, text: &str, style: Style) {
        for word in text.split_inclusive(char::is_whitespace) {
            let word_len = word.chars().count();

            // If adding this word would exceed width, flush current line first.
            if self.current_line_len + word_len > self.width && self.current_line_len > 0 {
                self.flush_line();
                // Re-add any prefix for continued lines (blockquote, list).
                self.add_continuation_prefix();
            }

            // Handle words longer than width - split them.
            if word_len > self.width {
                let mut remaining = word;
                while !remaining.is_empty() {
                    let available = self.width.saturating_sub(self.current_line_len);
                    if available == 0 {
                        self.flush_line();
                        self.add_continuation_prefix();
                        continue;
                    }
                    let take: String = remaining.chars().take(available).collect();
                    let take_len = take.chars().count();
                    self.current_line.push(Span::styled(take, style));
                    self.current_line_len += take_len;
                    remaining = &remaining[remaining
                        .char_indices()
                        .nth(take_len)
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len())..];
                }
            } else {
                self.current_line
                    .push(Span::styled(word.to_string(), style));
                self.current_line_len += word_len;
            }
        }
    }

    /// Add prefix for continuation lines (blockquote, list indent).
    fn add_continuation_prefix(&mut self) {
        if self.in_blockquote {
            self.current_line.push(Span::styled(
                "│ ".to_string(),
                Style::default().fg(self.theme.muted),
            ));
            self.current_line_len += 2;
        }
        if self.in_list {
            let indent = "  ".repeat(self.list_depth);
            self.current_line.push(Span::raw(indent.clone()));
            self.current_line_len += indent.len();
        }
    }

    fn inline_code(&mut self, code: &str) {
        self.current_line.push(Span::styled(
            format!("`{}`", code),
            Style::default()
                .fg(self.theme.warning)
                .add_modifier(Modifier::DIM),
        ));
    }

    fn soft_break(&mut self) {
        self.current_line.push(Span::raw(" "));
    }

    fn hard_break(&mut self) {
        self.flush_line();
    }

    fn horizontal_rule(&mut self) {
        self.flush_line();
        self.lines.push(Line::styled(
            "─".repeat(40),
            Style::default().fg(self.theme.muted),
        ));
    }

    fn current_style(&self) -> Style {
        let mut style = Style::default();

        // Heading styles.
        if let Some(level) = self.in_heading {
            style = style.fg(self.theme.primary).add_modifier(Modifier::BOLD);
            match level {
                HeadingLevel::H1 => {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                HeadingLevel::H2 => {}
                _ => {
                    style = style.fg(self.theme.fg);
                }
            }
            return style;
        }

        // Link style.
        if self.in_link {
            style = style
                .fg(self.theme.primary)
                .add_modifier(Modifier::UNDERLINED);
        }

        // Inline styles.
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.code {
            style = style.fg(self.theme.warning);
        }

        style
    }

    fn flush_line(&mut self) {
        if !self.current_line.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current_line)));
        }
        self.current_line_len = 0;
    }

    fn render_code_block(&mut self) {
        // Language header.
        let border_style = Style::default().fg(self.theme.muted);
        if let Some(ref lang) = self.code_block_lang {
            self.lines
                .push(Line::styled(format!("┌─ {} ", lang), border_style));
        } else {
            self.lines
                .push(Line::styled("┌─".to_string(), border_style));
        }

        // Code lines with syntax highlighting.
        let line_num_style = Style::default().fg(self.theme.muted);
        let highlighted_lines = syntax::highlight_code(
            &self.code_block_content,
            self.code_block_lang.as_deref(),
            self.theme,
            line_num_style,
        );

        for line in highlighted_lines {
            self.lines.push(line);
        }

        // Footer.
        self.lines
            .push(Line::styled("└─".to_string(), border_style));
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        let num_cols = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return;
        }

        // Calculate natural column widths based on content.
        let mut widths: Vec<usize> = vec![0; num_cols];
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(cell.chars().count());
                }
            }
        }

        // Ensure minimum column width of 3 (for "..." truncation).
        for w in &mut widths {
            *w = (*w).max(3);
        }

        // Calculate total table width: "│ " + (col + " │ ") * num_cols
        // Each column takes: width + 3 (for " │ ")
        // Plus leading "│ " = 2
        let total_width: usize = 2 + widths.iter().map(|w| w + 3).sum::<usize>();

        // If table exceeds available width, shrink columns proportionally.
        if self.width > 0 && total_width > self.width {
            let overhead = 2 + num_cols * 3; // "│ " + " │ " per column
            let available_for_content = self.width.saturating_sub(overhead);
            let total_content: usize = widths.iter().sum();

            if total_content > 0 && available_for_content > 0 {
                // Distribute available space proportionally, with minimum of 3.
                let mut new_widths: Vec<usize> = widths
                    .iter()
                    .map(|&w| {
                        let ratio = w as f64 / total_content as f64;
                        (ratio * available_for_content as f64).floor() as usize
                    })
                    .collect();

                // Ensure minimum width and adjust rounding errors.
                for w in &mut new_widths {
                    *w = (*w).max(3);
                }

                widths = new_widths;
            }
        }

        // Render header.
        if let Some(header) = self.table_rows.first() {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(self.theme.muted))];
            for (i, cell) in header.iter().enumerate() {
                let width = widths.get(i).copied().unwrap_or(3);
                let truncated = truncate_cell(cell, width);
                spans.push(Span::styled(
                    format!("{:width$}", truncated, width = width),
                    Style::default()
                        .fg(self.theme.primary)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(" │ ", Style::default().fg(self.theme.muted)));
            }
            self.lines.push(Line::from(spans));

            // Separator.
            let sep: String = widths
                .iter()
                .map(|w| "─".repeat(*w + 2))
                .collect::<Vec<_>>()
                .join("┼");
            self.lines.push(Line::styled(
                format!("├{}┤", sep),
                Style::default().fg(self.theme.muted),
            ));
        }

        // Render data rows.
        for row in self.table_rows.iter().skip(1) {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(self.theme.muted))];
            for (i, cell) in row.iter().enumerate() {
                let width = widths.get(i).copied().unwrap_or(3);
                let truncated = truncate_cell(cell, width);
                spans.push(Span::styled(
                    format!("{:width$}", truncated, width = width),
                    Style::default().fg(self.theme.fg),
                ));
                spans.push(Span::styled(" │ ", Style::default().fg(self.theme.muted)));
            }
            self.lines.push(Line::from(spans));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        self.lines
    }
}

fn truncate_url(url: &str, max_len: usize) -> String {
    if url.len() <= max_len {
        url.to_string()
    } else {
        format!("{}...", &url[..max_len.saturating_sub(3)])
    }
}

/// Truncate a table cell to fit within the given width.
/// Uses "…" (single char) for truncation to save space.
fn truncate_cell(cell: &str, max_width: usize) -> String {
    let char_count = cell.chars().count();
    if char_count <= max_width {
        cell.to_string()
    } else if max_width <= 1 {
        "…".to_string()
    } else {
        let take = max_width - 1; // Leave room for "…"
        let truncated: String = cell.chars().take(take).collect();
        format!("{}…", truncated)
    }
}
