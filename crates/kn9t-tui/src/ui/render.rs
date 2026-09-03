//! Rendering — draws all UI components.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Overlay, Screen, ToolHitArea};
use crate::which_key;
use crate::message_handler::{ToolCard, ToolTab};
use crate::slash::fuzzy_match;
use crate::theme::Theme;
use crate::thinking::{self, ContentSegment};
use crate::ui::layout::{compute_with_input, Sidebar};
use serde_json;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Main render function.
pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let theme = app.config.theme.clone();

    match app.screen {
        Screen::Welcome => render_welcome(f, app, area, &theme),
        Screen::Chat => render_chat(f, app, area, &theme),
    }
}

/// Maximum number of lines the input area can grow to.
const MAX_INPUT_LINES: u16 = 10;

fn render_chat(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let areas = compute_with_input(area, &app.layout, &app.input, MAX_INPUT_LINES);

    // Right sidebar.
    if areas.right.width > 0 {
        render_right_sidebar(f, app, areas.right, theme);
    }

    // Transcript (also populates tool_hit_areas).
    // When search is active, shrink transcript by 1 row to make room for the bar.
    let transcript_area = if app.search_state.is_some() && areas.transcript.height > 1 {
        Rect::new(
            areas.transcript.x,
            areas.transcript.y,
            areas.transcript.width,
            areas.transcript.height - 1,
        )
    } else {
        areas.transcript
    };
    render_transcript(f, app, transcript_area, theme);

    // Search bar (at the bottom of the transcript area, above input).
    if let Some(ref search) = app.search_state {
        let bar_y = areas.transcript.y + areas.transcript.height - 1;
        let bar_area = Rect::new(areas.transcript.x, bar_y, areas.transcript.width, 1);
        let buf = f.buffer_mut();
        search.render_bar(bar_area, buf);
    }

    // Input.
    render_input(f, app, areas.input, theme);
    
    // Slash command dropdown (above input).
    if app.slash.active {
        render_slash_dropdown(f, app, areas.input, theme);
    }

    // Status bar.
    render_status(f, app, areas.status, theme);

    // Diff viewer (separate from overlay - needs &mut for mouse hit tracking)
    if let Some(ref mut viewer) = app.diff_viewer {
        let buf = f.buffer_mut();
        if viewer.fullscreen {
            viewer.render(area, buf);
        } else {
            let w = area.width.min(100);
            let h = area.height.saturating_sub(4);
            let x = area.x + (area.width.saturating_sub(w)) / 2;
            let y = area.y + 2;
            viewer.render(Rect::new(x, y, w, h), buf);
        }
        return;
    }
    
    // Overlay (approval, help, model select, session select, etc).
    if let Some(ref overlay) = app.overlay {
        match overlay {
            Overlay::ModelSelect { selected, filter } => {
                render_model_select(f, app, *selected, filter, area, theme);
            }
            Overlay::SessionSelect { selected, filter } => {
                render_session_select(f, app, *selected, filter, area, theme);
            }
            Overlay::WhichKey => {
                let groups = which_key::get_keybindings(app.tool_mode);
                let buf = f.buffer_mut();
                app.which_key_panel.render(&groups, area, buf);
            }
            Overlay::CommandPalette => {
                render_command_palette(f, app, area, theme);
            }
            _ => render_overlay(f, overlay, area, theme),
        }
    }
}

fn render_welcome(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    // Calculate positions first.
    let content_width = 60u16.min(area.width.saturating_sub(4));
    let start_x = area.x + (area.width.saturating_sub(content_width)) / 2;
    let center_y = area.y + area.height / 2;
    let input_y = center_y;
    let input_width = content_width.min(50);
    let input_x = start_x + (content_width.saturating_sub(input_width)) / 2;
    
    // Inner content width (excluding borders).
    let inner_width = input_width.saturating_sub(2) as usize;
    
    // Build wrapped lines for input content (image markers are inline in text).
    let (display_lines, cursor_display_row, cursor_display_col) = if app.input.is_empty() {
        (vec!["Type a message or /models to select...".to_string()], 0, 0)
    } else {
        wrap_input_for_welcome(&app.input, inner_width, app.cursor_col)
    };
    
    // Calculate dynamic input box height (top border + content lines + bottom border).
    let content_lines = display_lines.len().min(6) as u16; // Max 6 content lines.
    let input_box_height = content_lines + 2; // +2 for top/bottom borders
    
    let content_start_y = input_y + 1;
    
    // Calculate cursor position before borrowing buffer.
    let cursor_pos = if !app.slash.active && !app.input.is_empty() 
        && input_y > area.y && input_y + input_box_height < area.y + area.height {
        let cursor_y = content_start_y + cursor_display_row as u16;
        let cursor_x = input_x + 1 + cursor_display_col as u16;
        if cursor_x < input_x + input_width - 1 {
            Some((cursor_x, cursor_y))
        } else {
            None
        }
    } else {
        None
    };
    
    {
        let buf = f.buffer_mut();
        
        // Logo / title (centered, above input).
        let title = "kn9t";
        let title_y = center_y.saturating_sub(5);
        let title_x = start_x + (content_width.saturating_sub(title.len() as u16)) / 2;
        for (i, ch) in title.chars().enumerate() {
            if title_x + (i as u16) < area.x + area.width && title_y < area.y + area.height {
                buf[(title_x + i as u16, title_y)].set_char(ch).set_style(
                    Style::default().fg(theme.primary).add_modifier(Modifier::BOLD)
                );
            }
        }

        // Subtitle.
        let subtitle = "minimal coding agent";
        let sub_y = title_y + 1;
        let sub_x = start_x + (content_width.saturating_sub(subtitle.len() as u16)) / 2;
        for (i, ch) in subtitle.chars().enumerate() {
            if sub_x + (i as u16) < area.x + area.width && sub_y < area.y + area.height {
                buf[(sub_x + i as u16, sub_y)].set_char(ch).set_fg(theme.muted);
            }
        }

        // Model display (above input).
        let model_y = center_y.saturating_sub(2);
        let model_text = format!("Model: {}", app.current_model_name());
        let model_x = start_x + (content_width.saturating_sub(model_text.len() as u16)) / 2;
        for (i, ch) in model_text.chars().enumerate() {
            let x = model_x + i as u16;
            if x < area.x + area.width && model_y < area.y + area.height {
                let style = if i < 7 {
                    Style::default().fg(theme.muted)
                } else {
                    Style::default().fg(theme.primary)
                };
                buf[(x, model_y)].set_char(ch).set_style(style);
            }
        }

        // Draw input box with dynamic height.
        if input_y > area.y && input_y + input_box_height < area.y + area.height {
            // Top border.
            buf[(input_x, input_y)].set_char('╭').set_fg(theme.muted);
            for i in 1..input_width.saturating_sub(1) {
                buf[(input_x + i, input_y)].set_char('─').set_fg(theme.muted);
            }
            buf[(input_x + input_width - 1, input_y)].set_char('╮').set_fg(theme.muted);
            
            // Content lines.
            let text_style = if app.input.is_empty() {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.fg)
            };
            
            for (line_idx, line) in display_lines.iter().enumerate().take(content_lines as usize) {
                let y = content_start_y + line_idx as u16;
                
                // Left border.
                buf[(input_x, y)].set_char('│').set_fg(theme.muted);
                
                // Content.
                for (i, ch) in line.chars().enumerate() {
                    let x = input_x + 1 + i as u16;
                    if x < input_x + input_width - 1 {
                        buf[(x, y)].set_char(ch).set_style(text_style);
                    }
                }
                
                // Right border.
                buf[(input_x + input_width - 1, y)].set_char('│').set_fg(theme.muted);
            }
            
            // Bottom border.
            let bottom_y = input_y + input_box_height - 1;
            buf[(input_x, bottom_y)].set_char('╰').set_fg(theme.muted);
            for i in 1..input_width.saturating_sub(1) {
                buf[(input_x + i, bottom_y)].set_char('─').set_fg(theme.muted);
            }
            buf[(input_x + input_width - 1, bottom_y)].set_char('╯').set_fg(theme.muted);
        }

        // Hints below input (adjust for dynamic height).
        let hints_y = input_y + input_box_height + 1;
        let hints = "/session  /models  /new  /help  /quit";
        let hints_x = start_x + (content_width.saturating_sub(hints.len() as u16)) / 2;
        for (i, ch) in hints.chars().enumerate() {
            let x = hints_x + i as u16;
            if x < area.x + area.width && hints_y < area.y + area.height {
                buf[(x, hints_y)].set_char(ch).set_fg(theme.muted);
            }
        }

        // Recent sessions hint (if any).
        if !app.session.sessions.is_empty() {
            let sessions_y = hints_y + 2;
            let sessions_hint = format!("{} recent sessions (/session to browse)", app.session.sessions.len());
            let sessions_x = start_x + (content_width.saturating_sub(sessions_hint.len() as u16)) / 2;
            for (i, ch) in sessions_hint.chars().enumerate() {
                let x = sessions_x + i as u16;
                if x < area.x + area.width && sessions_y < area.y + area.height {
                    buf[(x, sessions_y)].set_char(ch).set_fg(theme.muted);
                }
            }
        }
    } // End of buffer borrow
    
    // Set cursor position (after buffer borrow is released).
    if let Some((x, y)) = cursor_pos {
        f.set_cursor_position((x, y));
    }
    
    // Render slash command dropdown if active.
    if app.slash.active {
        render_slash_dropdown(f, app, Rect::new(input_x, input_y + input_box_height, input_width, 8), theme);
    }
    
    // Overlay (model select, session select, help, etc).
    if let Some(ref overlay) = app.overlay {
        match overlay {
            Overlay::ModelSelect { selected, filter } => {
                render_model_select(f, app, *selected, filter, area, theme);
            }
            Overlay::SessionSelect { selected, filter } => {
                render_session_select(f, app, *selected, filter, area, theme);
            }
            Overlay::Help => {
                render_overlay(f, overlay, area, theme);
            }
            Overlay::WhichKey => {
                let groups = which_key::get_keybindings(app.tool_mode);
                let buf = f.buffer_mut();
                app.which_key_panel.render(&groups, area, buf);
            }
            Overlay::CommandPalette => {
                render_command_palette(f, app, area, theme);
            }
            _ => {} // Other overlays not applicable on welcome
        }
    }
    
    // Diff viewer (separate from overlay - needs &mut for mouse hit tracking)
    if let Some(ref mut viewer) = app.diff_viewer {
        let buf = f.buffer_mut();
        if viewer.fullscreen {
            viewer.render(area, buf);
        } else {
            let w = area.width.min(100);
            let h = area.height.saturating_sub(4);
            let x = area.x + (area.width.saturating_sub(w)) / 2;
            let y = area.y + 2;
            viewer.render(Rect::new(x, y, w, h), buf);
        }
    }
}

/// Wrap input text for welcome screen and compute cursor position.
/// Returns (display_lines, cursor_row, cursor_col).
fn wrap_input_for_welcome(input: &str, width: usize, cursor_char_pos: usize) -> (Vec<String>, usize, usize) {
    if width == 0 {
        return (vec![input.to_string()], 0, cursor_char_pos);
    }
    
    let chars: Vec<char> = input.chars().collect();
    let mut display_lines: Vec<String> = Vec::new();
    let mut cursor_row: usize = 0;
    let mut cursor_col: usize = 0;
    
    if chars.is_empty() {
        return (vec![String::new()], 0, 0);
    }
    
    let mut pos = 0;
    while pos < chars.len() {
        let end = (pos + width).min(chars.len());
        let segment: String = chars[pos..end].iter().collect();
        display_lines.push(segment);
        
        // Map cursor position.
        if cursor_char_pos >= pos && cursor_char_pos < end {
            cursor_row = display_lines.len() - 1;
            cursor_col = cursor_char_pos - pos;
        } else if cursor_char_pos >= end && end == chars.len() {
            // Cursor at end.
            cursor_row = display_lines.len() - 1;
            cursor_col = end - pos;
        }
        
        pos = end;
    }
    
    if display_lines.is_empty() {
        display_lines.push(String::new());
    }
    
    (display_lines, cursor_row, cursor_col)
}

fn render_right_sidebar(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if app.layout.right == Sidebar::Collapsed {
        // Collapsed: full-height heavy border with distinct background.
        let buf = f.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_bg(theme.tool_focus_bg);
            }
        }
        for y in area.y..area.y + area.height {
            buf[(area.x, y)].set_char('┃').set_style(Style::default().fg(theme.tool_focus_border).bg(theme.tool_focus_bg).add_modifier(Modifier::BOLD));
        }
        return;
    }

    let buf = f.buffer_mut();

    // Fill sidebar background to distinguish from transcript.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_bg(theme.tool_focus_bg);
        }
    }

    // Heavy left border in accent color.
    for y in area.y..area.y + area.height {
        buf[(area.x, y)].set_char('┃').set_style(Style::default().fg(theme.tool_focus_border).bg(theme.tool_focus_bg).add_modifier(Modifier::BOLD));
    }

    // Content is inset by one column so text doesn't overwrite the border.
    let content_x = area.x + 1;
    let content_w = (area.width as usize).saturating_sub(1);
    if content_w == 0 {
        return;
    }
    let mut y = area.y;
    let w = content_w;

    // Session info at the top.
    if !app.session.state.session_id.is_empty() {
        // Show short session ID (first 8 chars).
        let sid = &app.session.state.session_id;
        let short_id = if sid.len() > 8 { &sid[..8] } else { sid.as_str() };
        y = render_line_at(buf, content_x, y, w, &format!("#{}", short_id), Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
        // Show title if available.
        if let Some(title) = app.session.session_title() {
            y = render_line_at(buf, content_x, y, w, title, Style::default().bg(theme.tool_focus_bg).fg(theme.primary));
        }
        y += 1;
    }

    // Model section.
    y = render_line_at(buf, content_x, y, w, "MODEL", Style::default().bg(theme.tool_focus_bg).fg(theme.fg).add_modifier(Modifier::BOLD));
    y = render_line_at(buf, content_x, y, w, &app.current_model_name(), Style::default().bg(theme.tool_focus_bg).fg(theme.primary));
    y = render_line_at(buf, content_x, y, w, &format!("${:.4}", app.tokens.cost), Style::default().bg(theme.tool_focus_bg).fg(theme.warning));
    y += 1;

    // Last turn stats - what matters for context window usage.
    // Anthropic: input = non-cached tokens, cache_read = cached tokens
    // Total context = input + cache_read; cache_hit% = cache_read / total
    y = render_line_at(buf, content_x, y, w, "LAST TURN", Style::default().bg(theme.tool_focus_bg).fg(theme.fg).add_modifier(Modifier::BOLD));
    let lt_input = app.tokens.last_turn_input();
    let lt_output = app.tokens.last_turn_output();
    let lt_cache_read = app.tokens.last_turn_cache_read();
    let lt_cache_write = app.tokens.last_turn_cache_write();
    if lt_input > 0 || lt_cache_read > 0 {
        let total_input = lt_input + lt_cache_read;
        if lt_cache_read > 0 && total_input > 0 {
            let hit_pct = (lt_cache_read as f64 / total_input as f64 * 100.0) as u32;
            y = render_line_at(buf, content_x, y, w,
                &format!("in: {} ({}%)", format_tokens(total_input), hit_pct),
                Style::default().bg(theme.tool_focus_bg).fg(theme.success));
        } else {
            y = render_line_at(buf, content_x, y, w,
                &format!("in: {}", format_tokens(lt_input)),
                Style::default().bg(theme.tool_focus_bg).fg(theme.fg));
        }
        y = render_line_at(buf, content_x, y, w,
            &format!("out: {}", format_tokens(lt_output)),
            Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
        if lt_cache_read > 0 || lt_cache_write > 0 {
            y = render_line_at(buf, content_x, y, w,
                &format!("r:{} w:{}", format_tokens(lt_cache_read), format_tokens(lt_cache_write)),
                Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
        }
    } else {
        y = render_line_at(buf, content_x, y, w, "-", Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
    }

    if let Some(tps) = app.tokens.last_toks_per_sec {
        y = render_line_at(buf, content_x, y, w, &format!("{:.0} tok/s", tps), Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
    }
    y += 1;

    // Session totals - cumulative for billing/cost.
    // tokens_in = non-cached tokens, cache_read = cached tokens, total = both
    y = render_line_at(buf, content_x, y, w, "SESSION", Style::default().bg(theme.tool_focus_bg).fg(theme.fg).add_modifier(Modifier::BOLD));
    let s_in = app.tokens.tokens_in();
    let s_out = app.tokens.tokens_out();
    let s_cache_read = app.tokens.cache_read();
    let s_cache_write = app.tokens.cache_write();
    let session_total = s_in + s_cache_read;
    if s_cache_read > 0 && session_total > 0 {
        let session_hit_pct = (s_cache_read as f64 / session_total as f64 * 100.0) as u32;
        y = render_line_at(buf, content_x, y, w,
            &format!("in: {} ({}%)", format_tokens(session_total), session_hit_pct),
            Style::default().bg(theme.tool_focus_bg).fg(theme.success));
    } else {
        y = render_line_at(buf, content_x, y, w,
            &format!("in: {}", format_tokens(s_in)),
            Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
    }
    y = render_line_at(buf, content_x, y, w,
        &format!("out: {}", format_tokens(s_out)),
        Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
    if s_cache_read > 0 || s_cache_write > 0 {
        y = render_line_at(buf, content_x, y, w,
            &format!("r:{} w:{}", format_tokens(s_cache_read), format_tokens(s_cache_write)),
            Style::default().bg(theme.tool_focus_bg).fg(theme.muted));
    }
    y += 1;

    // Tools section.
    if y < area.y + area.height {
        y = render_line_at(buf, content_x, y, w, "TOOLS", Style::default().bg(theme.tool_focus_bg).fg(theme.fg).add_modifier(Modifier::BOLD));
        for tool in &app.tools {
            if y >= area.y + area.height {
                break;
            }
            let check = if tool.enabled { "☑" } else { "☐" };
            let style = if tool.enabled { Style::default().bg(theme.tool_focus_bg).fg(theme.fg) } else { Style::default().bg(theme.tool_focus_bg).fg(theme.muted) };
            y = render_line_at(buf, content_x, y, w, &format!("{} {}", check, tool.name), style);
        }
    }
}

fn render_line_at(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, w: usize, text: &str, style: Style) -> u16 {
    let truncated = truncate(text, w);
    for (i, ch) in truncated.chars().enumerate() {
        buf[(x + i as u16, y)].set_char(ch).set_style(style);
    }
    y + 1
}

fn render_transcript(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let inner_w = area.width as usize;
    
    // Track tool positions for click detection.
    // We'll calculate actual screen Y after scroll adjustment.
    let mut tool_line_info: Vec<(String, usize, usize)> = Vec::new();  // (call_id, header_line_idx, content_end_line_idx)

    // Determine which message contains the current search match.
    let current_match_msg_idx = app.search_state.as_ref()
        .and_then(|s| s.current_match())
        .map(|m| m.msg_idx);

    for (msg_idx, msg) in app.transcript.messages().iter().enumerate() {
        // Is this the message containing the current search match?
        let is_current_match_msg = current_match_msg_idx == Some(msg_idx);
        
        // Role label.
        let (role_style, prefix) = match msg.role.as_str() {
            "user" => (Style::default().fg(theme.user).add_modifier(Modifier::BOLD), "▸ "),
            "assistant" => (Style::default().fg(theme.assistant).add_modifier(Modifier::BOLD), "◂ "),
            "error" => (Style::default().fg(theme.error).add_modifier(Modifier::BOLD), "✗ "),
            "system" => (Style::default().fg(theme.muted).add_modifier(Modifier::ITALIC), "ℹ "),
            _ => (Style::default().fg(theme.muted), "  "),
        };

        // Add role line.
        lines.push(Line::from(Span::styled(format!("{}{}", prefix, msg.role), role_style)));

        // Content lines — use markdown renderer for assistant, plain for user.
        if msg.role == "assistant" && !msg.content.is_empty() {
            // Width for markdown: subtract 2 for indentation.
            let md_width = inner_w.saturating_sub(2);
            
            // Parse content for thinking blocks.
            let segments = thinking::parse_content(&msg.content);
            let mut thinking_idx = 0;
            
            for segment in segments {
                match segment {
                    ContentSegment::Text(text) => {
                        // Render as markdown.
                        let md_lines = crate::markdown::render(&text, theme, md_width);
                        for line in md_lines {
                            // Apply search highlighting if active.
                            let highlighted_line = if let Some(ref search) = app.search_state {
                                search.highlight_line(line, is_current_match_msg)
                            } else {
                                line
                            };
                            let mut indented = vec![Span::raw("  ")];
                            indented.extend(highlighted_line.spans);
                            lines.push(Line::from(indented));
                        }
                    }
                    ContentSegment::Thinking { tag, content } => {
                        let is_collapsed = app.thinking_state.is_collapsed(thinking_idx);
                        let line_count = content.lines().count();
                        
                        if is_collapsed {
                            // Collapsed: show header only.
                            let header = thinking::render_collapsed_header(&tag, line_count, theme);
                            let mut indented = vec![Span::raw("  ")];
                            indented.extend(header.spans);
                            lines.push(Line::from(indented));
                        } else {
                            // Expanded: show header and content.
                            let header = thinking::render_expanded_header(&tag, theme);
                            let mut indented = vec![Span::raw("  ")];
                            indented.extend(header.spans);
                            lines.push(Line::from(indented));
                            
                            // Render thinking content with muted style.
                            let thinking_lines = thinking::render_thinking_content(&content, theme, md_width);
                            for line in thinking_lines {
                                // Apply search highlighting if active.
                                let highlighted_line = if let Some(ref search) = app.search_state {
                                    search.highlight_line(line, is_current_match_msg)
                                } else {
                                    line
                                };
                                let mut indented = vec![Span::raw("  ")];
                                indented.extend(highlighted_line.spans);
                                lines.push(Line::from(indented));
                            }
                        }
                        thinking_idx += 1;
                    }
                }
            }
        } else {
            // Image markers [img1] etc are already inline in the text.
            // Apply search highlighting if search is active.
            let base_style = Style::default().fg(theme.fg);
            for content_line in msg.content.lines() {
                for wrapped in wrap_text(content_line, inner_w.saturating_sub(2)) {
                    let line_spans = if let Some(ref search) = app.search_state {
                        let mut spans = vec![Span::raw("  ")];
                        // Use direct text search (not byte offsets) since text is wrapped
                        spans.extend(search.highlight_in_text(&wrapped, base_style, is_current_match_msg));
                        spans
                    } else {
                        vec![Span::styled(format!("  {}", wrapped), base_style)]
                    };
                    lines.push(Line::from(line_spans));
                }
            }
        }

        // Tool cards.
        for card in &msg.tools {
            let header_line_idx = lines.len();
            render_tool_card(card, app, &mut lines, inner_w, theme);
            let content_end_line_idx = lines.len();
            tool_line_info.push((card.call_id.clone(), header_line_idx, content_end_line_idx));
        }

        lines.push(Line::from("")); // spacing
    }

    // Live delta — render as markdown.
    if !app.transcript.live_delta().is_empty() {
        lines.push(Line::from(Span::styled("◂ assistant", Style::default().fg(theme.assistant).add_modifier(Modifier::BOLD))));
        let md_width = inner_w.saturating_sub(2);
        let md_lines = crate::markdown::render(app.transcript.live_delta(), theme, md_width);
        for line in md_lines {
            let mut indented = vec![Span::raw("  ")];
            indented.extend(line.spans);
            lines.push(Line::from(indented));
        }
    }

    // Phase-synced streaming indicator — server is source of truth.
    if app.streaming || matches!(app.turn_phase.as_str(), "retrying" | "failed" | "tool" | "thinking") {
        let spinner = SPINNER[app.spinner_frame % SPINNER.len()];
        if app.aborting {
            lines.push(Line::from(Span::styled(format!("  {} Aborting...", spinner), Style::default().fg(theme.error))));
        } else if app.turn_phase == "retrying" {
            let detail = if app.turn_status_msg.is_empty() { "retrying...".to_string() } else { app.turn_status_msg.clone() };
            lines.push(Line::from(Span::styled(format!("  {} {}", spinner, detail), Style::default().fg(theme.warning))));
        } else if app.turn_phase == "failed" {
            let detail = if app.turn_status_msg.is_empty() { "failed".to_string() } else { app.turn_status_msg.clone() };
            lines.push(Line::from(Span::styled(format!("  {} failed: {}", spinner, detail), Style::default().fg(theme.error))));
        } else if app.turn_phase == "tool" {
            let detail = if app.turn_status_msg.is_empty() { "running tool...".to_string() } else { app.turn_status_msg.clone() };
            lines.push(Line::from(Span::styled(format!("  {} {}", spinner, detail), Style::default().fg(theme.warning))));
        } else if app.turn_phase == "thinking" {
            lines.push(Line::from(Span::styled(format!("  {} thinking...", spinner), Style::default().fg(theme.muted))));
        } else if !app.transcript.live_delta().is_empty() {
            // streaming with deltas — show spinner without extra phrase (content already visible)
            lines.push(Line::from(Span::styled(format!("  {} streaming...", spinner), Style::default().fg(theme.muted))));
        } else {
            let phrase = &app.config.streaming_phrases[app.phrase_idx % app.config.streaming_phrases.len()];
            lines.push(Line::from(Span::styled(format!("  {} {}", spinner, phrase), Style::default().fg(theme.muted))));
        }
    } else if app.turn_phase == "failed" && !app.turn_status_msg.is_empty() {
        // Brief failed notice even if not streaming (TurnEnded already cleared streaming but phase stays failed until next turn)
        lines.push(Line::from(Span::styled(format!("  ✗ {}", app.turn_status_msg), Style::default().fg(theme.error))));
    }

    // Scroll logic:
    // - scroll=0 means "at bottom" (show latest messages)
    // - scroll>0 means "scrolled up N lines from bottom"
    let total = lines.len();
    let visible = area.height as usize;
    let max_scroll = total.saturating_sub(visible);
    
    // Clamp scroll to valid range
    let effective_scroll = app.transcript.scroll().min(max_scroll);
    
    // scroll_offset is how many lines to skip from top
    // At bottom (scroll=0): skip max_scroll lines (show last 'visible' lines)
    // Scrolled up: skip fewer lines
    let scroll_offset = max_scroll.saturating_sub(effective_scroll);

    // Build tool hit areas for click detection.
    // Convert line indices to screen Y positions.
    app.tool_hit_areas.clear();
    for (call_id, header_line_idx, content_end_line_idx) in tool_line_info {
        // Check if tool is visible on screen
        if header_line_idx >= scroll_offset && header_line_idx < scroll_offset + visible {
            let header_y = area.y + (header_line_idx - scroll_offset) as u16;
            let content_y_start = if header_line_idx + 1 >= scroll_offset {
                area.y + (header_line_idx + 1).saturating_sub(scroll_offset) as u16
            } else {
                area.y
            };
            let content_y_end = if content_end_line_idx > scroll_offset {
                area.y + (content_end_line_idx - scroll_offset).min(visible) as u16
            } else {
                content_y_start
            };
            
            // Tab positions (approximate - tabs start at column 4)
            // Layout: "    " + " Progress " + " " + " Output " + " " + " Input "
            let tab_base_x = area.x + 4;
            app.tool_hit_areas.push(ToolHitArea {
                call_id,
                header_y,
                content_y_start,
                content_y_end,
                progress_tab_x: (tab_base_x, tab_base_x + 10),       // " Progress "
                output_tab_x: (tab_base_x + 11, tab_base_x + 19),   // " Output "
                input_tab_x: (tab_base_x + 20, tab_base_x + 28),    // " Input "
            });
        }
    }

    let para = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0));
    f.render_widget(para, area);

    // Jump to end button (show when not at bottom).
    if effective_scroll > 0 {
        let btn = "↓ Jump to end (Ctrl+End)";
        let x = area.x + area.width.saturating_sub(btn.len() as u16 + 1);
        let y = area.y + area.height.saturating_sub(1);
        let buf = f.buffer_mut();
        for (i, ch) in btn.chars().enumerate() {
            if x + (i as u16) < area.x + area.width {
                buf[(x + i as u16, y)].set_char(ch)
                    .set_fg(theme.bg)
                    .set_bg(theme.primary);
            }
        }
    }
}

fn render_input(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let buf = f.buffer_mut();

    // Prompt indicator.
    let has_lease = app.session.state.lease.is_some();
    let prompt = if has_lease { "› " } else { "○ " };
    let prompt_style = if has_lease { Style::default().fg(theme.primary) } else { Style::default().fg(theme.muted) };

    // Content width after prompt.
    let prefix_width = 2; // "› "
    let content_width = area.width.saturating_sub(prefix_width as u16) as usize;
    if content_width == 0 {
        return;
    }
    
    // Use input directly (image markers are already inline).
    let display_input = &app.input;

    // Build wrapped lines from input.
    // Each logical line may wrap into multiple display lines.
    let mut display_lines: Vec<String> = Vec::new();
    let mut cursor_display_row: usize = 0;
    let mut cursor_display_col: usize = 0;
    
    // Track position for cursor mapping.
    let mut logical_row: usize = 0;
    let input_char_count = app.input.chars().count();
    
    // Use display_input (with image suffix) for rendering.
    for (line_idx, line) in display_input.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            // Empty line.
            display_lines.push(String::new());
            if logical_row == app.cursor_row {
                cursor_display_row = display_lines.len() - 1;
                cursor_display_col = 0;
            }
        } else {
            // Wrap long lines.
            let mut pos = 0;
            while pos < chars.len() {
                let end = (pos + content_width).min(chars.len());
                let segment: String = chars[pos..end].iter().collect();
                display_lines.push(segment);
                
                // Map cursor position (only within original input, not img suffix).
                if logical_row == app.cursor_row && line_idx == 0 {
                    let cursor_in_line = app.cursor_col;
                    if cursor_in_line >= pos && cursor_in_line < end && cursor_in_line <= input_char_count {
                        cursor_display_row = display_lines.len() - 1;
                        cursor_display_col = cursor_in_line - pos;
                    } else if cursor_in_line >= end && cursor_in_line <= input_char_count {
                        // Cursor at end of input (before img suffix).
                        cursor_display_row = display_lines.len() - 1;
                        cursor_display_col = input_char_count.min(end) - pos;
                    }
                }
                
                pos = end;
            }
        }
        logical_row += 1;
    }
    
    // Handle empty input.
    if display_lines.is_empty() {
        display_lines.push(String::new());
        cursor_display_row = 0;
        cursor_display_col = 0;
    }

    // Render display lines.
    for (display_row, line) in display_lines.iter().enumerate() {
        let y = area.y + display_row as u16;
        if y >= area.y + area.height {
            break;
        }
        
        let mut x_offset = area.x;
        
        // First display line gets the prompt.
        if display_row == 0 {
            // Draw prompt.
            for ch in prompt.chars() {
                if x_offset < area.x + area.width {
                    buf[(x_offset, y)].set_char(ch).set_style(prompt_style);
                    x_offset += 1;
                }
            }
        } else {
            // Continuation lines: indent to align with content.
            x_offset += prefix_width as u16;
        }
        
        // Draw content.
        for ch in line.chars() {
            if x_offset >= area.x + area.width {
                break;
            }
            buf[(x_offset, y)].set_char(ch).set_fg(theme.fg);
            x_offset += 1;
        }
    }

    // Set cursor position (account for prefix width).
    let cursor_x = area.x + prefix_width as u16 + cursor_display_col as u16;
    let cursor_y = area.y + cursor_display_row as u16;
    if cursor_y < area.y + area.height && cursor_x < area.x + area.width {
        f.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    // Last turn context and cache hit.
    let lt_input = app.tokens.last_turn_input();
    let lt_cache_read = app.tokens.last_turn_cache_read();
    let turn_ctx = lt_input + lt_cache_read;
    let turn_info = if turn_ctx > 0 {
        if lt_cache_read > 0 {
            let hit_pct = (lt_cache_read as f64 / turn_ctx as f64 * 100.0) as u32;
            format!("ctx:{} ({}% hit)", format_tokens(turn_ctx), hit_pct)
        } else {
            format!("ctx:{}", format_tokens(turn_ctx))
        }
    } else {
        String::new()
    };

    // Build throughput info (tok/s from last turn).
    let tps_info = if let Some(tps) = app.tokens.last_toks_per_sec {
        format!(" {:.0}tok/s", tps)
    } else {
        String::new()
    };

    let phase_disp = match app.turn_phase.as_str() {
        "idle" => "idle",
        "thinking" => "thinking",
        "streaming" => "streaming",
        "tool" => "tool",
        "retrying" => "retrying",
        "failed" => "failed",
        "aborted" => "aborted",
        other => other,
    };
    let status = format!(
        "{} | ${:.4} | {}{} | {} | ^P",
        app.current_model_name(),
        app.tokens.cost,
        turn_info,
        tps_info,
        phase_disp,
    );

    let para = Paragraph::new(Line::from(Span::styled(status, Style::default().fg(theme.muted))));
    f.render_widget(para, area);
}

fn render_overlay(f: &mut Frame, overlay: &Overlay, area: Rect, theme: &Theme) {
    // Clear area with semi-transparent effect (dim background).
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    // Center the overlay.
    let overlay_w = 60.min(area.width.saturating_sub(4));
    let overlay_h = 15.min(area.height.saturating_sub(4));
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;

    // Clear overlay area.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }

    match overlay {
        Overlay::Approval { tool, args, selected } => {
            // Highly visible border — heavy box in warning color with bold.
            let border_fg = theme.warning;
            let border_style = Style::default().fg(border_fg).bg(Color::Black).add_modifier(Modifier::BOLD);
            if overlay_w >= 2 && overlay_h >= 2 {
                buf[(overlay_x, overlay_y)].set_char('┏').set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y)].set_char('┓').set_style(border_style);
                buf[(overlay_x, overlay_y + overlay_h - 1)].set_char('┗').set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)].set_char('┛').set_style(border_style);
                for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
                    buf[(x, overlay_y)].set_char('━').set_style(border_style);
                    buf[(x, overlay_y + overlay_h - 1)].set_char('━').set_style(border_style);
                }
                for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
                    buf[(overlay_x, y)].set_char('┃').set_style(border_style);
                    buf[(overlay_x + overlay_w - 1, y)].set_char('┃').set_style(border_style);
                }
            }
            let mut y = overlay_y + 1;
            
            // Title.
            let title = "APPROVAL REQUIRED";
            let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
            for (i, ch) in title.chars().enumerate() {
                buf[(title_x + i as u16, y)].set_char(ch).set_fg(theme.warning).set_bg(Color::Black);
            }
            y += 2;

            // Nice per-tool display with highlighting and reason.
            let inner_w = (overlay_w.saturating_sub(4)) as usize;
            let inner_x = overlay_x + 2;
            let body_lines = approval_body_lines(tool, args, theme, inner_w.max(1));
            for line in body_lines {
                if y >= overlay_y + overlay_h - 2 { break; }
                let mut x = inner_x;
                for span in line.spans {
                    let style = if span.style.bg.is_none() { span.style.bg(Color::Black) } else { span.style };
                    for ch in span.content.chars() {
                        if x >= overlay_x + overlay_w - 1 { break; }
                        buf[(x, y)].set_char(ch).set_style(style);
                        x += 1;
                    }
                    if x >= overlay_x + overlay_w - 1 { break; }
                }
                y += 1;
            }
            // Ensure at least one blank line before buttons if space
            if y < overlay_y + overlay_h - 2 {
                y += 1;
            }

            // Buttons.
            let buttons = ["Allow", "Always", "Deny"];
            let btn_y = y;
            let mut btn_x = overlay_x + 2;
            for (i, btn) in buttons.iter().enumerate() {
                let is_selected = *selected == i;
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(theme.primary)
                } else {
                    Style::default().fg(theme.fg).bg(Color::DarkGray)
                };

                let label = format!(" {} ", btn);
                for (j, ch) in label.chars().enumerate() {
                    buf[(btn_x + j as u16, btn_y)].set_char(ch).set_style(style);
                }
                btn_x += label.len() as u16 + 2;
            }
        }

        Overlay::Interaction { plugin, payload, input, .. } => {
            let border_fg = theme.primary;
            let border_style = Style::default().fg(border_fg).bg(Color::Black).add_modifier(Modifier::BOLD);
            if overlay_w >= 2 && overlay_h >= 2 {
                buf[(overlay_x, overlay_y)].set_char('┏').set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y)].set_char('┓').set_style(border_style);
                buf[(overlay_x, overlay_y + overlay_h - 1)].set_char('┗').set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)].set_char('┛').set_style(border_style);
                for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
                    buf[(x, overlay_y)].set_char('━').set_style(border_style);
                    buf[(x, overlay_y + overlay_h - 1)].set_char('━').set_style(border_style);
                }
                for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
                    buf[(overlay_x, y)].set_char('┃').set_style(border_style);
                    buf[(overlay_x + overlay_w - 1, y)].set_char('┃').set_style(border_style);
                }
            }
            let mut y = overlay_y + 1;
            let title = format!("{} asks:", plugin);
            let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
            for (i, ch) in title.chars().enumerate() {
                buf[(title_x + i as u16, y)].set_char(ch).set_fg(theme.primary).set_bg(Color::Black);
            }
            y += 2;
            let inner_w = (overlay_w.saturating_sub(4)) as usize;
            let inner_x = overlay_x + 2;
            for line in wrap_text(payload, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 3 { break; }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 { break; }
                    buf[(inner_x + i as u16, y)].set_char(ch).set_fg(theme.fg).set_bg(Color::Black);
                }
                y += 1;
            }
            y += 1;
            let prompt = "› ";
            for (i, ch) in prompt.chars().enumerate() {
                buf[(inner_x + i as u16, y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
            }
            let input_x = inner_x + prompt.len() as u16;
            for (i, ch) in input.chars().enumerate() {
                if input_x + i as u16 >= overlay_x + overlay_w - 2 { break; }
                buf[(input_x + i as u16, y)].set_char(ch).set_fg(theme.fg).set_bg(Color::Black);
            }
            let cx = input_x + input.chars().count() as u16;
            if cx < overlay_x + overlay_w - 1 {
                buf[(cx, y)].set_char('▏').set_fg(theme.primary).set_bg(Color::Black);
            }
            let footer = "Enter send · Esc cancel";
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
            }
        }

        Overlay::Help => {
            let mut y = overlay_y + 1;
            
            let title = "HELP";
            let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
            for (i, ch) in title.chars().enumerate() {
                buf[(title_x + i as u16, y)].set_char(ch).set_fg(theme.primary).set_bg(Color::Black);
            }
            y += 2;

            let help_lines = [
                "─── Navigation ───",
                "Ctrl+↑/↓      scroll transcript",
                "PageUp/Down   scroll transcript",
                "Ctrl+Home     scroll to top",
                "Ctrl+End      scroll to bottom",
                "Ctrl+J/K      prev/next message",
                "",
                "─── Actions ───",
                "Enter         send message",
                "Escape        abort turn",
                "Ctrl+C/Q      quit",
                "Ctrl+P        this help",
                "Ctrl+B        switch session",
                "Ctrl+N        new session",
                "",
                "─── Slash Commands ───",
                "/session      switch session",
                "/models       switch model",
                "",
                "Press Esc to close",
            ];

            for line in help_lines {
                if y >= overlay_y + overlay_h - 1 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if (overlay_x + 2 + i as u16) < overlay_x + overlay_w {
                        buf[(overlay_x + 2 + i as u16, y)].set_char(ch).set_fg(theme.fg).set_bg(Color::Black);
                    }
                }
                y += 1;
            }
        }
        
        Overlay::ModelSelect { .. } | Overlay::SessionSelect { .. } | Overlay::WhichKey | Overlay::CommandPalette => {
            // These overlays are rendered elsewhere with access to app state.
        }
    }
}

fn render_slash_dropdown(f: &mut Frame, app: &App, input_area: Rect, theme: &Theme) {
    use crate::slash::COMMANDS;
    
    let matches = &app.slash.matches;
    if matches.is_empty() {
        return;
    }
    
    let buf = f.buffer_mut();
    
    // Position dropdown above input.
    let dropdown_h = (matches.len() as u16).min(8);
    let dropdown_y = input_area.y.saturating_sub(dropdown_h + 1);
    let dropdown_x = input_area.x + 2; // Align with input text.
    let dropdown_w = 40.min(input_area.width.saturating_sub(4));
    
    // Background.
    for y in dropdown_y..dropdown_y + dropdown_h {
        for x in dropdown_x..dropdown_x + dropdown_w {
            if x < input_area.x + input_area.width && y < input_area.y {
                buf[(x, y)].set_char(' ').set_bg(Color::DarkGray);
            }
        }
    }
    
    // Items.
    for (i, &cmd_idx) in matches.iter().enumerate().take(dropdown_h as usize) {
        let cmd = &COMMANDS[cmd_idx];
        let y = dropdown_y + i as u16;
        let is_selected = i == app.slash.selected;
        
        let (fg, bg) = if is_selected {
            (theme.bg, theme.primary)
        } else {
            (theme.fg, Color::DarkGray)
        };
        
        // Command name.
        let name = format!("/{}", cmd.name);
        for (j, ch) in name.chars().enumerate() {
            let x = dropdown_x + j as u16;
            if x < dropdown_x + dropdown_w {
                buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
            }
        }
        
        // Description.
        let desc_start = dropdown_x + 12;
        for (j, ch) in cmd.description.chars().enumerate() {
            let x = desc_start + j as u16;
            if x < dropdown_x + dropdown_w {
                let desc_fg = if is_selected { theme.bg } else { theme.muted };
                buf[(x, y)].set_char(ch).set_fg(desc_fg).set_bg(bg);
            }
        }
        
        // Fill rest of line with bg.
        for x in (desc_start + cmd.description.len() as u16)..dropdown_x + dropdown_w {
            if x < dropdown_x + dropdown_w {
                buf[(x, y)].set_char(' ').set_bg(bg);
            }
        }
    }
}

fn render_model_select(f: &mut Frame, app: &App, selected: usize, filter: &str, area: Rect, theme: &Theme) {
    let buf = f.buffer_mut();
    
    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }
    
    // Filter models by fuzzy match on display name OR provider.
    let filtered: Vec<(usize, &crate::model_selector::ModelEntry)> = app.model_sel.models().iter()
        .enumerate()
        .filter(|(_, m)| {
            filter.is_empty() 
            || fuzzy_match(&m.display_name(), filter)
            || fuzzy_match(&m.provider, filter)
        })
        .collect();
    
    // Group by provider, maintaining selection index.
    // Build display rows: either a provider header or a model entry.
    #[derive(Clone)]
    enum Row<'a> {
        Header(&'a str),
        Model(usize, &'a crate::model_selector::ModelEntry), // (original_idx, model)
    }
    
    let mut rows: Vec<Row> = Vec::new();
    let mut current_provider: Option<&str> = None;
    let mut selectable_idx = 0usize; // Index for selection (headers don't count)
    let mut selected_row_idx: Option<usize> = None; // Row index of selected model
    
    for (orig_idx, model) in &filtered {
        if current_provider != Some(&model.provider) {
            current_provider = Some(&model.provider);
            rows.push(Row::Header(&model.provider));
        }
        if selectable_idx == selected {
            selected_row_idx = Some(rows.len());
        }
        rows.push(Row::Model(*orig_idx, model));
        selectable_idx += 1;
    }
    
    // Calculate visible window for scrolling.
    let max_visible_rows = (area.height as usize).saturating_sub(8); // title + filter + footer + padding
    let scroll_offset = if let Some(sel_row) = selected_row_idx {
        if sel_row >= max_visible_rows {
            sel_row.saturating_sub(max_visible_rows / 2)
        } else {
            0
        }
    } else {
        0
    };
    
    // Center overlay — wider to fit model names.
    let overlay_w = 50.min(area.width.saturating_sub(4));
    let overlay_h = (rows.len() as u16 + 6).min(area.height.saturating_sub(4)).max(8);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    
    // Background.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }
    
    // Title.
    let title = "SELECT MODEL";
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    let mut y = overlay_y + 1;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, y)].set_char(ch).set_fg(theme.primary).set_bg(Color::Black);
    }
    y += 1;
    
    // Filter input.
    let filter_display = if filter.is_empty() { "Type to filter..." } else { filter };
    let filter_style = if filter.is_empty() { theme.muted } else { theme.fg };
    for (i, ch) in format!("› {}", filter_display).chars().enumerate() {
        let x = overlay_x + 2 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, y)].set_char(ch).set_fg(filter_style).set_bg(Color::Black);
        }
    }
    y += 2;
    
    // Render rows with scrolling.
    let mut model_display_idx = 0usize;
    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx < scroll_offset {
            if matches!(row, Row::Model(..)) {
                model_display_idx += 1;
            }
            continue;
        }
        if y >= overlay_y + overlay_h - 1 {
            break;
        }
        
        match row {
            Row::Header(provider) => {
                // Provider header — dimmed, not selectable.
                let header = format!("─ {} ─", provider);
                for (j, ch) in header.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            Row::Model(_, model) => {
                let is_selected = model_display_idx == selected;
                let (fg, bg) = if is_selected {
                    (theme.bg, theme.primary)
                } else {
                    (theme.fg, Color::Black)
                };
                
                // Indent model names under provider header.
                let line = format!("  {} ", model.display_name());
                let content_w = (overlay_w - 4) as usize;
                let truncated = if line.chars().count() > content_w {
                    line.chars().take(content_w.saturating_sub(1)).collect::<String>() + "…"
                } else {
                    line.clone()
                };
                
                for (j, ch) in truncated.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
                    }
                }
                // Fill rest of line.
                for x in (overlay_x + 2 + truncated.chars().count() as u16)..overlay_x + overlay_w - 2 {
                    buf[(x, y)].set_char(' ').set_bg(bg);
                }
                y += 1;
                model_display_idx += 1;
            }
        }
    }
    
    // Show empty message if no matches.
    if filtered.is_empty() {
        let msg = "No matching models";
        for (i, ch) in msg.chars().enumerate() {
            let x = overlay_x + 2 + i as u16;
            if x < overlay_x + overlay_w - 2 {
                buf[(x, y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
            }
        }
    }
    
    // Footer.
    let footer = "↑/↓ select · Enter confirm · Esc cancel";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        buf[(footer_x + i as u16, footer_y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
    }
}

fn render_session_select(f: &mut Frame, app: &App, selected: usize, filter: &str, area: Rect, theme: &Theme) {
    let buf = f.buffer_mut();
    
    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }
    
    // Filter sessions (names fuzzy, ids substring — see session_matches).
    let filtered: Vec<(usize, &crate::session_manager::SessionEntry)> = app.session.sessions.iter()
        .enumerate()
        .filter(|(_, s)| crate::session_manager::session_matches(s, filter))
        .collect();
    
    // Build rows with date headers.
    #[derive(Clone)]
    enum SessionRow<'a> {
        DateHeader(String),  // "Today", "Yesterday", "Aug 27", etc.
        NewSession,
        Session(usize, &'a crate::session_manager::SessionEntry),
    }
    
    let mut rows: Vec<SessionRow> = Vec::new();
    
    // Add "New session" option at top.
    rows.push(SessionRow::NewSession);
    
    // Group sessions by date.
    let mut current_date: Option<String> = None;
    for (orig_idx, session) in &filtered {
        let date_label = session.created_at.as_ref()
            .and_then(|ts| format_date_header(ts))
            .unwrap_or_else(|| "Unknown".to_string());
        
        if current_date.as_ref() != Some(&date_label) {
            current_date = Some(date_label.clone());
            rows.push(SessionRow::DateHeader(date_label));
        }
        rows.push(SessionRow::Session(*orig_idx, session));
    }
    
    // Center overlay.
    let overlay_w = 55.min(area.width.saturating_sub(4));
    let overlay_h = (rows.len() as u16 + 6).min(area.height.saturating_sub(4)).max(8);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    
    // Background.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }
    
    // Title.
    let title = "SELECT SESSION";
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    let mut y = overlay_y + 1;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, y)].set_char(ch).set_fg(theme.primary).set_bg(Color::Black);
    }
    y += 1;
    
    // Filter input.
    let filter_display = if filter.is_empty() { "Type to filter..." } else { filter };
    let filter_style = if filter.is_empty() { theme.muted } else { theme.fg };
    for (i, ch) in format!("› {}", filter_display).chars().enumerate() {
        let x = overlay_x + 2 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, y)].set_char(ch).set_fg(filter_style).set_bg(Color::Black);
        }
    }
    y += 2;
    
    // Render rows.
    let mut selectable_idx = 0usize;
    for row in &rows {
        if y >= overlay_y + overlay_h - 1 {
            break;
        }
        
        match row {
            SessionRow::DateHeader(label) => {
                // Date header — dimmed, not selectable.
                let header = format!("─ {} ─", label);
                for (j, ch) in header.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            SessionRow::NewSession => {
                let is_selected = selectable_idx == selected;
                let (fg, bg) = if is_selected {
                    (theme.bg, theme.primary)
                } else {
                    (theme.success, Color::Black)
                };
                let line = "  ✚ New session";
                for (j, ch) in line.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
                    }
                }
                for x in (overlay_x + 2 + line.chars().count() as u16)..overlay_x + overlay_w - 2 {
                    buf[(x, y)].set_char(' ').set_bg(bg);
                }
                y += 1;
                selectable_idx += 1;
            }
            SessionRow::Session(_, session) => {
                let is_selected = selectable_idx == selected;
                let is_active = session.id == app.session.state.session_id;
                
                let (fg, bg) = if is_selected {
                    (theme.bg, theme.primary)
                } else {
                    (theme.fg, Color::Black)
                };
                
                // Show indicator for running/active sessions.
                let prefix = if session.running {
                    format!("{} ", SPINNER[app.spinner_frame % SPINNER.len()])
                } else if is_active {
                    "▸ ".to_string()
                } else {
                    "  ".to_string()
                };
                
                let line = format!("{}{}", prefix, truncate(&session.name, (overlay_w - 8) as usize));
                for (j, ch) in line.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
                    }
                }
                // Fill rest of line.
                for x in (overlay_x + 2 + line.chars().count() as u16)..overlay_x + overlay_w - 2 {
                    buf[(x, y)].set_char(' ').set_bg(bg);
                }
                y += 1;
                selectable_idx += 1;
            }
        }
    }
    
    // Show empty message if no matches.
    if filtered.is_empty() && app.session.sessions.is_empty() {
        let msg = "No sessions yet";
        for (i, ch) in msg.chars().enumerate() {
            let x = overlay_x + 2 + i as u16;
            if x < overlay_x + overlay_w - 2 {
                buf[(x, y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
            }
        }
    }
    
    // Footer.
    let footer = "↑/↓ select · Enter open · Del delete · Esc cancel";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        if footer_x + (i as u16) < overlay_x + overlay_w {
            buf[(footer_x + i as u16, footer_y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
        }
    }
}

fn render_command_palette(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    use crate::command_palette::COMMANDS;
    
    let buf = f.buffer_mut();
    let palette = &app.command_palette;
    
    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }
    
    // Center overlay.
    let overlay_w = 60.min(area.width.saturating_sub(4));
    let max_items = 12usize;
    let overlay_h = (palette.matches.len().min(max_items) as u16 + 5).min(area.height.saturating_sub(4)).max(6);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 3; // Upper third
    
    // Draw background.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }
    
    // Draw border.
    // Top.
    buf[(overlay_x, overlay_y)].set_char('╭').set_fg(theme.primary).set_bg(Color::Black);
    buf[(overlay_x + overlay_w - 1, overlay_y)].set_char('╮').set_fg(theme.primary).set_bg(Color::Black);
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, overlay_y)].set_char('─').set_fg(theme.primary).set_bg(Color::Black);
    }
    // Bottom.
    buf[(overlay_x, overlay_y + overlay_h - 1)].set_char('╰').set_fg(theme.primary).set_bg(Color::Black);
    buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)].set_char('╯').set_fg(theme.primary).set_bg(Color::Black);
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, overlay_y + overlay_h - 1)].set_char('─').set_fg(theme.primary).set_bg(Color::Black);
    }
    // Sides.
    for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
        buf[(overlay_x, y)].set_char('│').set_fg(theme.primary).set_bg(Color::Black);
        buf[(overlay_x + overlay_w - 1, y)].set_char('│').set_fg(theme.primary).set_bg(Color::Black);
    }
    
    // Title.
    let title = " Command Palette ";
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, overlay_y)].set_char(ch).set_fg(theme.primary).set_bg(Color::Black);
    }
    
    // Search input.
    let input_y = overlay_y + 1;
    let prompt = "> ";
    for (i, ch) in prompt.chars().enumerate() {
        buf[(overlay_x + 2 + i as u16, input_y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
    }
    let query = &palette.query;
    for (i, ch) in query.chars().enumerate() {
        let x = overlay_x + 4 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, input_y)].set_char(ch).set_fg(theme.fg).set_bg(Color::Black);
        }
    }
    // Cursor.
    let cursor_x = overlay_x + 4 + query.chars().count() as u16;
    if cursor_x < overlay_x + overlay_w - 2 {
        buf[(cursor_x, input_y)].set_char('▏').set_fg(theme.primary).set_bg(Color::Black);
    }
    
    // Separator.
    let sep_y = overlay_y + 2;
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, sep_y)].set_char('─').set_fg(Color::DarkGray).set_bg(Color::Black);
    }
    
    // Command list.
    let mut y = overlay_y + 3;
    for (i, &cmd_idx) in palette.matches.iter().enumerate().take(max_items) {
        if y >= overlay_y + overlay_h - 1 {
            break;
        }
        
        let cmd = &COMMANDS[cmd_idx];
        let is_selected = i == palette.selected;
        let (fg, bg) = if is_selected {
            (theme.bg, theme.primary)
        } else {
            (theme.fg, Color::Black)
        };
        
        // Clear line.
        for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
            buf[(x, y)].set_char(' ').set_bg(bg);
        }
        
        // Command label.
        let label = cmd.label;
        for (j, ch) in label.chars().enumerate() {
            let x = overlay_x + 2 + j as u16;
            if x < overlay_x + overlay_w - 20 {
                buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
            }
        }
        
        // Keybinding (right-aligned).
        if let Some(kb) = cmd.keybinding {
            let kb_x = overlay_x + overlay_w - 2 - kb.len() as u16;
            let kb_fg = if is_selected { theme.bg } else { theme.muted };
            for (j, ch) in kb.chars().enumerate() {
                buf[(kb_x + j as u16, y)].set_char(ch).set_fg(kb_fg).set_bg(bg);
            }
        }
        
        y += 1;
    }
    
    // Show match count if filtered.
    if !palette.query.is_empty() {
        let count = format!("{} matches", palette.matches.len());
        let count_x = overlay_x + overlay_w - 2 - count.len() as u16;
        let count_y = overlay_y + overlay_h - 1;
        for (i, ch) in count.chars().enumerate() {
            buf[(count_x + i as u16, count_y)].set_char(ch).set_fg(theme.muted).set_bg(Color::Black);
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max > 1 {
        chars[..max - 1].iter().collect::<String>() + "…"
    } else {
        "…".to_string()
    }
}

/// Format ISO 8601 timestamp to a human-readable date label.
/// Returns "Today", "Yesterday", or "Mon DD" format.
fn format_date_header(timestamp: &str) -> Option<String> {
    // Parse date part from ISO 8601 (e.g., "2026-08-28T10:30:00").
    let date_part = timestamp.split('T').next()?;
    let parts: Vec<&str> = date_part.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    
    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    
    // Get current date (approximation using system time).
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    
    // Days since epoch for session date.
    // Simplified: assume 365.25 days/year, 30.44 days/month.
    let session_days = (year as i64 - 1970) * 365 + (month as i64 - 1) * 30 + day as i64;
    let today_days = (now / 86400) as i64;
    let diff = today_days - session_days;
    
    let month_names = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", 
                       "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month_name = month_names.get(month.saturating_sub(1) as usize).unwrap_or(&"???");
    
    // Approximate comparison (may be off by a day due to timezone, but good enough).
    if diff <= 0 {
        Some("Today".to_string())
    } else if diff == 1 {
        Some("Yesterday".to_string())
    } else if diff < 7 {
        Some(format!("{} days ago", diff))
    } else {
        Some(format!("{} {}", month_name, day))
    }
}

/// Format token count: 1500 → "1.5k", 150000 → "150k", 1500000 → "1.5M"
fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in s.chars() {
        current.push(ch);
        current_width += 1;
        if current_width >= width {
            result.push(current);
            current = String::new();
            current_width = 0;
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

// ═══════════════════════════════════════════════════════════════════════════
// Tool Card Rendering
// ═══════════════════════════════════════════════════════════════════════════

/// Visible lines in expanded tool output (for virtual scrolling).
const TOOL_OUTPUT_VISIBLE_LINES: usize = 20;

/// Render a single tool card with expand/collapse, tabs, and virtual scroll.
fn render_tool_card(
    card: &ToolCard,
    app: &App,
    lines: &mut Vec<Line>,
    inner_w: usize,
    theme: &Theme,
) {
    let is_focused = app.focused_tool.as_ref() == Some(&card.call_id);
    let _is_running = card.status.starts_with("running");

    // Determine tool name color based on status
    let name_color = match card.status.as_str() {
        "done" => theme.success,
        "error" => theme.error,
        _ => theme.tool,  // Running or unknown
    };

    // Status indicator
    let status_ch = match card.status.as_str() {
        s if s.starts_with("running") => SPINNER[app.spinner_frame % SPINNER.len()],
        "done" => '✓',
        "error" => '✗',
        _ => '?',
    };
    let status_color = match card.status.as_str() {
        s if s.starts_with("running") => theme.primary,
        "done" => theme.success,
        "error" => theme.error,
        _ => theme.muted,
    };

    // Expand/collapse indicator
    let expand_indicator = if card.expanded { "[-]" } else { "[+]" };

    // Key arg preview
    let key_arg = format_tool_key_arg(&card.args, inner_w.saturating_sub(25));

    // Build header line (clone strings to avoid lifetime issues)
    let header_spans = vec![
        Span::styled(format!("  {} ", expand_indicator), Style::default().fg(theme.muted)),
        Span::styled(format!("{} ", status_ch), Style::default().fg(status_color)),
        Span::styled(card.name.clone(), Style::default().fg(name_color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", key_arg), Style::default().fg(theme.muted)),
    ];

    // Apply focus highlight
    if is_focused {
        lines.push(Line::from(header_spans).style(Style::default().bg(theme.tool_focus_bg)));
    } else {
        lines.push(Line::from(header_spans));
    }

    // Expanded content
    if card.expanded {
        // Tab bar: Progress | Output | Input
        let progress_style = if card.active_tab == ToolTab::Progress {
            Style::default().fg(theme.tab_active_fg).bg(theme.tab_active_bg)
        } else {
            Style::default().fg(theme.tab_inactive_fg)
        };
        let output_style = if card.active_tab == ToolTab::Output {
            Style::default().fg(theme.tab_active_fg).bg(theme.tab_active_bg)
        } else {
            Style::default().fg(theme.tab_inactive_fg)
        };
        let input_style = if card.active_tab == ToolTab::Input {
            Style::default().fg(theme.tab_active_fg).bg(theme.tab_active_bg)
        } else {
            Style::default().fg(theme.tab_inactive_fg)
        };

        let tab_line = if is_focused {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(" Progress ", progress_style),
                Span::raw(" "),
                Span::styled(" Output ", output_style),
                Span::raw(" "),
                Span::styled(" Input ", input_style),
            ]).style(Style::default().bg(theme.tool_focus_bg))
        } else {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(" Progress ", progress_style),
                Span::raw(" "),
                Span::styled(" Output ", output_style),
                Span::raw(" "),
                Span::styled(" Input ", input_style),
            ])
        };
        lines.push(tab_line);

        // Tab content
        match card.active_tab {
            ToolTab::Progress => {
                render_tool_progress(card, lines, inner_w, theme, is_focused);
            }
            ToolTab::Output => {
                render_tool_output(card, lines, inner_w, theme, is_focused);
            }
            ToolTab::Input => {
                render_tool_input(card, lines, inner_w, theme, is_focused);
            }
        }
    }
}

/// Render the Progress tab content (streaming chunks).
fn render_tool_progress(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    inner_w: usize,
    theme: &Theme,
    is_focused: bool,
) {
    let bg_style = if is_focused {
        Style::default().bg(theme.tool_focus_bg)
    } else {
        Style::default()
    };

    if card.progress_lines.is_empty() {
        let msg = if card.status.starts_with("running") {
            "(waiting for progress...)"
        } else {
            "(no progress output)"
        };
        lines.push(Line::from(Span::styled(
            format!("    │ {}", msg),
            Style::default().fg(theme.muted),
        )).style(bg_style));
        return;
    }

    let total_lines = card.progress_lines.len();
    let start = card.scroll_offset;
    let end = (start + TOOL_OUTPUT_VISIBLE_LINES).min(total_lines);

    for (i, line) in card.progress_lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        
        // Color diff lines appropriately
        let line_color = if line.starts_with('+') && !line.starts_with("+++") {
            theme.diff_add
        } else if line.starts_with('-') && !line.starts_with("---") {
            theme.diff_remove
        } else if line.starts_with("@@") {
            theme.primary
        } else {
            theme.fg
        };
        
        lines.push(Line::from(Span::styled(
            format!("    │ {:>4}: {}", line_num, truncate(line, inner_w.saturating_sub(14))),
            Style::default().fg(line_color),
        )).style(bg_style));
    }

    // Scroll indicator
    if total_lines > TOOL_OUTPUT_VISIBLE_LINES {
        let pct = if total_lines > 0 { (end * 100) / total_lines } else { 100 };
        lines.push(Line::from(Span::styled(
            format!("    └─ [{}/{}] {}%", end, total_lines, pct),
            Style::default().fg(theme.muted),
        )).style(bg_style));
    }
}

/// Render the Output tab content (what the agent sees in tool_result).
fn render_tool_output(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    inner_w: usize,
    theme: &Theme,
    is_focused: bool,
) {
    let bg_style = if is_focused {
        Style::default().bg(theme.tool_focus_bg)
    } else {
        Style::default()
    };

    // Output only (no progress lines - those are in Progress tab)
    let output = match &card.output {
        Some(o) if !o.is_empty() => o,
        _ => {
            let msg = if card.status.starts_with("running") {
                "(running...)"
            } else {
                "(no output)"
            };
            lines.push(Line::from(Span::styled(
                format!("    │ {}", msg),
                Style::default().fg(theme.muted),
            )).style(bg_style));
            return;
        }
    };

    let output_lines: Vec<&str> = output.lines().collect();
    let total_lines = output_lines.len();
    let start = card.scroll_offset.min(total_lines);
    let end = (start + TOOL_OUTPUT_VISIBLE_LINES).min(total_lines);

    // Error styling
    let base_color = if card.status == "error" {
        theme.error
    } else {
        theme.fg
    };

    for (i, line) in output_lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        lines.push(Line::from(Span::styled(
            format!("    │ {:>4}: {}", line_num, truncate(line, inner_w.saturating_sub(14))),
            Style::default().fg(base_color),
        )).style(bg_style));
    }

    // Scroll indicator
    if total_lines > TOOL_OUTPUT_VISIBLE_LINES {
        let pct = if total_lines > 0 { (end * 100) / total_lines } else { 100 };
        lines.push(Line::from(Span::styled(
            format!("    └─ [{}/{}] {}%", end, total_lines, pct),
            Style::default().fg(theme.muted),
        )).style(bg_style));
    }
}

/// Render the Input tab content with key-value display.
fn render_tool_input(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    inner_w: usize,
    theme: &Theme,
    is_focused: bool,
) {
    let bg_style = if is_focused {
        Style::default().bg(theme.tool_focus_bg)
    } else {
        Style::default()
    };

    // Parse args JSON
    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&card.args) {
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let value_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };

                // Handle multi-line values
                let value_lines: Vec<&str> = value_str.lines().collect();
                if value_lines.len() <= 1 {
                    // Single line value
                    let max_val_len = inner_w.saturating_sub(key.len() + 12);
                    lines.push(Line::from(vec![
                        Span::raw("    │ "),
                        Span::styled(format!("{}: ", key), Style::default().fg(theme.input_key)),
                        Span::styled(
                            truncate(&value_str, max_val_len),
                            Style::default().fg(theme.input_value),
                        ),
                    ]).style(bg_style));
                } else {
                    // Multi-line value - show key then indented lines
                    lines.push(Line::from(vec![
                        Span::raw("    │ "),
                        Span::styled(format!("{}:", key), Style::default().fg(theme.input_key)),
                    ]).style(bg_style));

                    let max_show = 30;  // Limit displayed lines for very long content
                    for vline in value_lines.iter().take(max_show) {
                        lines.push(Line::from(Span::styled(
                            format!("    │   {}", truncate(vline, inner_w.saturating_sub(12))),
                            Style::default().fg(theme.input_value),
                        )).style(bg_style));
                    }
                    if value_lines.len() > max_show {
                        lines.push(Line::from(Span::styled(
                            format!("    │   ... ({} more lines)", value_lines.len() - max_show),
                            Style::default().fg(theme.muted),
                        )).style(bg_style));
                    }
                }
            }
        } else {
            // Not an object, show raw
            lines.push(Line::from(Span::styled(
                format!("    │ {}", truncate(&card.args, inner_w.saturating_sub(8))),
                Style::default().fg(theme.muted),
            )).style(bg_style));
        }
    } else {
        // Parse failed, show raw args
        lines.push(Line::from(Span::styled(
            format!("    │ {}", truncate(&card.args, inner_w.saturating_sub(8))),
            Style::default().fg(theme.muted),
        )).style(bg_style));
    }
}

/// Format the key argument for a tool (path, command, content preview, etc).
fn format_tool_key_arg(args_json: &str, max_len: usize) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args_json) {
        if let serde_json::Value::Object(map) = &v {
            // Priority order for key args.
            let key_priority = ["path", "filePath", "file_path", "command", "content", "query", "url"];
            
            for key in key_priority {
                if let Some(val) = map.get(key) {
                    let val_str = match val {
                        serde_json::Value::String(s) => s.clone(),
                        _ => val.to_string(),
                    };
                    return format!("{}={}", key, truncate(&val_str, max_len));
                }
            }
            
            // Fallback: show first key.
            if let Some((k, v)) = map.iter().next() {
                let val_str = match v {
                    serde_json::Value::String(s) => s.clone(),
                    _ => v.to_string(),
                };
                return format!("{}={}", k, truncate(&val_str, max_len));
            }
        }
    }
    truncate(args_json, max_len)
}



// ── Approval nice display + dangerous highlighting ────────────
fn approval_reason(tool: &str, args: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    match tool {
        "bash" => {
            let cmd = val.get("cmd").and_then(|v| v.as_str()).unwrap_or(args);
            let lower = cmd.to_lowercase();
            let always_ask = ["rm","mv","cp","chmod","chown","kill","dd","curl","wget","ssh","scp","sh","bash","zsh","python","python3","node","perl","ruby","eval","pwsh","powershell","iex","sudo","mkfs","fdisk","reboot","shutdown"];
            for w in always_ask {
                if lower.split_whitespace().any(|tok| tok.eq_ignore_ascii_case(w)) {
                    return Some(format!("Uses `{}` (always requires approval)", w));
                }
            }
            if cmd.contains('>') || cmd.contains('<') {
                return Some("Contains redirection (`>`/`<`)".into());
            }
            if cmd.contains("$(") || cmd.contains('`') {
                return Some("Contains command substitution".into());
            }
            if lower.contains("sudo") {
                return Some("Uses `sudo` (never permitted without explicit approval)".into());
            }
            return Some("Potentially destructive shell command".into());
        },
        "write" | "edit" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() {
                return Some(format!("Writes to `{}`", path));
            }
            return Some("Writes to file".into());
        },
        _ => None,
    }
}
fn bash_highlight_spans(cmd: &str, theme: &Theme) -> Vec<Span<'static>> {
    let always_ask = ["rm","mv","cp","chmod","chown","kill","dd","curl","wget","ssh","scp","sh","bash","zsh","python","python3","node","perl","ruby","eval","pwsh","powershell","iex","sudo","mkfs","fdisk","reboot","shutdown"];
    let mut spans = Vec::new();
    let mut current = String::new();
    for ch in cmd.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                let lower = current.to_lowercase();
                let is_dangerous = always_ask.iter().any(|w| lower == *w);
                let style = if is_dangerous { Style::default().fg(theme.error).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.fg) };
                spans.push(Span::styled(current.clone(), style));
                current.clear();
            }
            spans.push(Span::raw(ch.to_string()));
        } else if ch == '>' || ch == '<' || ch == '|' || ch == ';' || ch == '&' || ch == '$' || ch == '`' {
            if !current.is_empty() {
                let lower = current.to_lowercase();
                let is_dangerous = always_ask.iter().any(|w| lower == *w);
                let style = if is_dangerous { Style::default().fg(theme.error).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.fg) };
                spans.push(Span::styled(current.clone(), style));
                current.clear();
            }
            spans.push(Span::styled(ch.to_string(), Style::default().fg(theme.warning).add_modifier(Modifier::BOLD)));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        let lower = current.to_lowercase();
        let is_dangerous = always_ask.iter().any(|w| lower == *w);
        let style = if is_dangerous { Style::default().fg(theme.error).add_modifier(Modifier::BOLD) } else { Style::default().fg(theme.fg) };
        spans.push(Span::styled(current, style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(cmd.to_string(), Style::default().fg(theme.fg)));
    }
    spans
}
fn approval_body_lines(tool: &str, args: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let val: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.to_string()));
    match tool {
        "bash" => {
            let cmd = val.get("cmd").and_then(|v| v.as_str()).unwrap_or(args);
            if let Some(reason) = approval_reason(tool, args) {
                for w in wrap_text(&format!("Reason: {}", reason), width) {
                    lines.push(Line::from(vec![Span::styled(w, Style::default().fg(theme.warning).add_modifier(Modifier::ITALIC))]));
                }
            }
            lines.push(Line::from(vec![Span::styled("Command:", Style::default().fg(theme.muted).add_modifier(Modifier::BOLD))]));
            if cmd.len() <= width {
                lines.push(Line::from(bash_highlight_spans(cmd, theme)));
            } else {
                for chunk in wrap_text(cmd, width) {
                    lines.push(Line::from(bash_highlight_spans(&chunk, theme)));
                }
            }
            if let Some(t) = val.get("timeout_secs").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![Span::styled(format!("Timeout: {}s", t), Style::default().fg(theme.muted))]));
            }
        },
        "read" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or(args);
            lines.push(Line::from(vec![Span::styled("Path: ", Style::default().fg(theme.muted).add_modifier(Modifier::BOLD)), Span::styled(path.to_string(), Style::default().fg(theme.primary).add_modifier(Modifier::BOLD))]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(reason, Style::default().fg(theme.warning).add_modifier(Modifier::ITALIC))]));
            }
            if let Some(off) = val.get("offset").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![Span::styled(format!("Offset: {}", off), Style::default().fg(theme.muted))]));
            }
        },
        "write" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(Line::from(vec![Span::styled("Path: ", Style::default().fg(theme.muted).add_modifier(Modifier::BOLD)), Span::styled(path.to_string(), Style::default().fg(theme.primary).add_modifier(Modifier::BOLD))]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(reason, Style::default().fg(theme.warning).add_modifier(Modifier::ITALIC))]));
            }
            let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let preview: String = content.lines().take(3).collect::<Vec<_>>().join(" ⏎ ");
            let preview = if preview.len() > width*2 { format!("{}…", &preview[..width*2-1]) } else { preview };
            lines.push(Line::from(vec![Span::styled("Content: ", Style::default().fg(theme.muted).add_modifier(Modifier::BOLD)), Span::styled(truncate(&preview, width.saturating_sub(9)), Style::default().fg(theme.fg))]));
        },
        "edit" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(Line::from(vec![Span::styled("Path: ", Style::default().fg(theme.muted).add_modifier(Modifier::BOLD)), Span::styled(path.to_string(), Style::default().fg(theme.primary).add_modifier(Modifier::BOLD))]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(reason, Style::default().fg(theme.warning).add_modifier(Modifier::ITALIC))]));
            }
            let old = val.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
            let new = val.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            let old_preview = old.lines().next().unwrap_or("").trim();
            let new_preview = new.lines().next().unwrap_or("").trim();
            lines.push(Line::from(vec![Span::styled("Old: ", Style::default().fg(theme.error).add_modifier(Modifier::BOLD)), Span::styled(truncate(old_preview, width.saturating_sub(5)), Style::default().fg(theme.error))]));
            lines.push(Line::from(vec![Span::styled("New: ", Style::default().fg(theme.success).add_modifier(Modifier::BOLD)), Span::styled(truncate(new_preview, width.saturating_sub(5)), Style::default().fg(theme.success))]));
        },
        _ => {
            let pretty = match &val {
                serde_json::Value::Object(map) => {
                    let mut parts = Vec::new();
                    for (k,v) in map {
                        let v_str = match v {
                            serde_json::Value::String(s) => format!("\"{}\"", truncate(s, 40)),
                            _ => truncate(&v.to_string(), 40),
                        };
                        parts.push(format!("{}: {}", k, v_str));
                    }
                    parts.join(", ")
                },
                _ => truncate(&val.to_string(), width*2),
            };
            for w in wrap_text(&pretty, width) {
                lines.push(Line::from(Span::styled(w, Style::default().fg(theme.muted))));
            }
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(reason, Style::default().fg(theme.warning).add_modifier(Modifier::ITALIC))]));
            }
        }
    }
    lines
}

#[cfg(test)]
mod golden {
    use super::*;
    use crate::app::Overlay;
    use crate::theme::Theme;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> Theme { Theme::dark() }

    fn render_overlay_to_string(overlay: &Overlay, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let th = theme();
        terminal.draw(|f| {
            let area = f.area();
            super::render_overlay(f, overlay, area, &th);
        }).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x, y)].symbol());
            }
            if y + 1 < height { out.push('\n'); }
        }
        out
    }

    #[test]
    fn golden_approval_overlay_contains_tool_and_actions() {
        let overlay = Overlay::Approval { tool: "bash".into(), args: r#"{"cmd":"rm -rf /"}"#.into(), selected: 0 };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        // Buffer must contain the command snippet and the action hints; tool name is implicit via Command line
        assert!(snap.contains("rm -rf"), "approval overlay must show args preview, got:\n{snap}");
        assert!(snap.contains("APPROVAL") || snap.contains("Allow"), "must show approval header or actions, got:\n{snap}");
        assert!(snap.contains("Allow") && snap.contains("Deny"), "must show Allow/Deny actions, got:\n{snap}");
    }

    #[test]
    fn golden_interaction_overlay_contains_plugin_and_payload() {
        let overlay = Overlay::Interaction { id: 42, plugin: "kn9t-ask-user".into(), payload: "{\n  \"question\": \"choose?\"\n}".into(), input: "my ans".into() };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        assert!(snap.contains("kn9t-ask-user"), "must show plugin name, got:\n{snap}");
        assert!(snap.contains("choose?"), "must show payload question, got:\n{snap}");
        assert!(snap.contains("my ans"), "must show current input, got:\n{snap}");
        assert!(snap.contains("Enter") && snap.contains("Esc"), "must show footer hints, got:\n{snap}");
    }

    #[test]
    fn golden_interaction_overlay_esc_hint_present_even_empty_input() {
        let overlay = Overlay::Interaction { id: 1, plugin: "p".into(), payload: "hello".into(), input: String::new() };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        // Must be renderable without panic and contain the payload
        assert!(snap.contains("hello"), "payload must be visible, got:\n{snap}");
        assert!(snap.contains("Esc"), "cancel hint must be present, got:\n{snap}");
    }

    #[test]
    fn golden_help_overlay_renders() {
        let overlay = Overlay::Help;
        let snap = render_overlay_to_string(&overlay, 60, 15);
        assert!(snap.contains("HELP") || snap.contains("Navigation") || snap.contains("Actions"), "help overlay must contain headings, got:\n{snap}");
    }
}
