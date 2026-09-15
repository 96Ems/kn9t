//! Rendering — draws all UI components.
//!
//! Render functions take many parameters (buffer, theme, coords, state...) by design.
//! This is unavoidable in immediate-mode rendering code.
#![allow(clippy::too_many_arguments)]

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::app::{App, InteractionState, Overlay, Screen, ToolHitArea};
use crate::message_handler::ToolCard;
use crate::slash::fuzzy_match;
use crate::syntax;
use crate::theme::Theme;
use crate::thinking::{self, ContentSegment};
use crate::which_key;
use serde_json;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Right margin for tool cards (characters from right edge).
const TOOL_CARD_RIGHT_MARGIN: usize = 20;

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
    // Reset hit-test geometry: whatever is actually drawn this frame re-records
    // it. Without this, a region Lua stopped drawing would keep taking clicks.
    app.transcript_area = None;
    app.lua_click_areas.clear();
    app.plugin_click_areas.clear();
    app.plugin_view_areas.clear();

    // Publish live state before any Lua runs this frame, so render_ui() and
    // render_status() observe the same snapshot.
    if let Some(ref runtime) = app.lua_runtime {
        // Measured from the previous frame, so the number Lua receives matches
        // the box Lua actually drew. Falls back to the full width on frame one.
        let input_width = app.input_width.unwrap_or(area.width);
        let input_height =
            crate::ui::layout::input_height_for(input_width, &app.input, MAX_INPUT_LINES);
        let stats = crate::lua::context::collect_stats(
            app.transcript.messages(),
            &app.tokens,
            &app.current_model_name(),
            &app.turn_phase,
            app.session.session_title().unwrap_or(""),
            input_height,
            app.streaming,
            app.model_sel.current_model(),
            app.plugins_ready,
            app.steering.len(),
            app.queue.len(),
        );
        runtime.update_context(&stats);

        // Cheap bounded snapshot; message bodies stay behind lazy accessors so
        // per-frame cost does not scale with transcript length.
        let snap = crate::lua::state::StateSnapshot::collect(app);
        runtime.update_state(&snap);

        // Refresh the backing store for `kn9t.get_messages`/`get_tools` only
        // when the transcript actually changed — the version check is the whole
        // reason this can be done from the render path at all.
        let version = crate::lua::state::LazyData::version(app);
        runtime.refresh_lazy(version, || crate::lua::state::LazyData::collect(app));
    }

    // The built-in Lua UI always loads, so it normally owns the whole frame.
    let outcome = app
        .lua_runtime
        .as_ref()
        .map(|rt| rt.build_ui_outcome(area.width, area.height))
        .unwrap_or(crate::lua::widgets::UiOutcome::NotDefined);

    match outcome {
        crate::lua::widgets::UiOutcome::Ok(root) => {
            // Lua-declared widgets first, then native views at Lua's rects.
            crate::lua::widgets::render_widget(f, &root, area, theme, &app.lua_input_states);

            let mut natives: Vec<(String, Rect)> = Vec::new();
            crate::lua::widgets::collect_natives(&root, area, &mut natives);
            for (view, rect) in natives {
                render_native_view(f, app, &view, rect, theme);
            }

            // Record `id="..."` rects for click dispatch, following the exact
            // geometry just painted — same reasoning as `transcript_area`.
            app.lua_click_areas.clear();
            crate::lua::widgets::collect_clickable_areas(&root, area, &mut app.lua_click_areas);

            render_plugin_views(f, app, &root, area, theme);

            render_lua_panels(f, app, area, theme);
            render_chat_overlays(f, app, area, theme);
        }
        crate::lua::widgets::UiOutcome::Failed(err) => {
            // Broken config: show a usable minimum plus the error, never the
            // full Rust chrome, which would look like nothing is wrong.
            render_lua_error_shell(f, app, area, theme, &err);
            render_chat_overlays(f, app, area, theme);
        }
        crate::lua::widgets::UiOutcome::NotDefined => {
            // Only reachable if the embedded built-in itself failed to load,
            // which is a build-time bug (covered by default_config tests).
            // There is deliberately no second Rust layout to fall back to.
            render_lua_error_shell(
                f,
                app,
                area,
                theme,
                "built-in UI failed to load (no render_ui defined)",
            );
            render_chat_overlays(f, app, area, theme);
        }
    }
}

/// Overlays (approval, help, model select, ...).
///
/// Shared by the Lua layout and the error shell, so overlays behave identically
/// no matter which path drew the frame.
fn render_chat_overlays(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
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
            Overlay::ToolsManager { selected, filter } => {
                render_tools_manager(f, app, *selected, filter, area, theme);
            }
            Overlay::SessionTree { selected } => {
                render_session_tree(f, app, *selected, area, theme);
            }
            _ => render_overlay(f, overlay, area, theme),
        }
    }
}

/// Minimal shell shown when the user's Lua UI is broken.
///
/// Deliberately *not* the Rust chrome: a red banner naming the error, plus the
/// transcript and input so the session stays usable while the config is fixed.
fn render_lua_error_shell(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme, err: &str) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Paragraph, Wrap};

    let input_h = crate::ui::layout::input_height_for(
        app.input_width.unwrap_or(area.width),
        &app.input,
        MAX_INPUT_LINES,
    );
    // Cap the banner so a long error cannot squeeze out the transcript.
    let banner_h = 3.min(area.height.saturating_sub(input_h + 2)).max(1);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_h),
            Constraint::Min(1),
            Constraint::Length(input_h),
        ])
        .split(area);

    let banner = Paragraph::new(format!("tui.lua error: {err}"))
        .style(
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(banner, rows[0]);

    render_transcript(f, app, rows[1], theme);
    render_input(f, app, rows[2], theme);
}

/// Render Lua-registered floating panels on top of the layout.
fn render_lua_panels(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    // Reset unconditionally, mirroring `lua_click_areas`: a panel that just
    // hid itself must stop taking clicks, not keep its last rect forever.
    app.lua_panel_click_areas.clear();

    let Some(runtime) = app.lua_runtime.clone() else {
        return;
    };
    if app.lua_panels.is_empty() {
        return;
    }
    // Resolve geometry first: build_panel_widget borrows the runtime, and the
    // input states are borrowed again while rendering.
    let jobs: Vec<(crate::lua::widgets::Widget, Rect)> = app
        .lua_panels
        .visible()
        .filter_map(|panel| {
            let widget = runtime.build_panel_widget(panel)?;
            let rect = compute_panel_area(panel, area);
            (rect.width > 0 && rect.height > 0).then_some((widget, rect))
        })
        .collect();

    for (widget, rect) in &jobs {
        crate::lua::widgets::collect_clickable_areas(widget, *rect, &mut app.lua_panel_click_areas);
    }
    for (widget, rect) in jobs {
        crate::lua::widgets::render_widget(f, &widget, rect, theme, &app.lua_input_states);
    }
}

/// Resolve a panel's declared position into a concrete rect.
///
/// `position` is a free-form string from Lua, so unknown values fall back to
/// centred-floating rather than erroring: a typo should misplace a panel, not
/// take the UI down.
fn compute_panel_area(panel: &crate::lua::panels::Panel, area: Rect) -> Rect {
    let w = panel.width.unwrap_or(area.width / 3).min(area.width);
    let h = panel.height.unwrap_or(area.height / 3).min(area.height);

    match panel.position.as_str() {
        "left" => Rect::new(area.x, area.y, w, area.height),
        "right" => Rect::new(
            area.x + area.width.saturating_sub(w),
            area.y,
            w,
            area.height,
        ),
        "top" => Rect::new(area.x, area.y, area.width, h),
        "bottom" => Rect::new(
            area.x,
            area.y + area.height.saturating_sub(h),
            area.width,
            h,
        ),
        _ => {
            // Explicit x/y when given, otherwise centre it.
            let x = panel
                .x
                .map(|v| area.x + v.min(area.width.saturating_sub(w)))
                .unwrap_or(area.x + (area.width.saturating_sub(w)) / 2);
            let y = panel
                .y
                .map(|v| area.y + v.min(area.height.saturating_sub(h)))
                .unwrap_or(area.y + (area.height.saturating_sub(h)) / 2);
            Rect::new(x, y, w, h)
        }
    }
}

/// Render a Rust-owned view at a rect chosen by Lua.
///
/// This is the mechanism/policy boundary: Lua decides *where*, Rust decides
/// *how* (markdown, syntax highlighting, scroll math, render cache).
fn render_native_view(f: &mut Frame, app: &mut App, view: &str, area: Rect, theme: &Theme) {
    match view {
        "transcript" => {
            // Record what was actually drawn, so hit-testing follows the Lua
            // layout instead of assuming a fixed region.
            app.transcript_area = Some(area);
            let transcript_area = if app.search_state.is_some() && area.height > 1 {
                Rect::new(area.x, area.y, area.width, area.height - 1)
            } else {
                area
            };
            render_transcript(f, app, transcript_area, theme);
            if let Some(ref search) = app.search_state {
                let bar_y = area.y + area.height - 1;
                let bar_area = Rect::new(area.x, bar_y, area.width, 1);
                let buf = f.buffer_mut();
                search.render_bar(bar_area, buf);
            }
        }
        "input" => {
            // Record the width Lua gave us; `input_height_for` reads it next
            // frame so the published row count cannot drift from the layout.
            app.input_width = Some(area.width);
            render_input(f, app, area, theme);
            if app.slash.active {
                render_slash_dropdown(f, app, area, theme);
            }
        }
        "status" => render_status(f, app, area, theme),
        "welcome" => render_welcome(f, app, area, theme),
        other => {
            crate::log!("Lua native view: unknown '{}'", other);
        }
    }
}

/// Draw every `{type="plugin", plugin="name"}` slot the layout declared.
///
/// The plugin's registered Lua produces the subtree; this only supplies the
/// rect, so a plugin cannot choose its own placement or size.
///
/// A plugin that is broken or absent gets an error message drawn in its slot
/// rather than empty space — a silent blank would look like a layout bug and
/// hide the actual cause.
fn render_plugin_views(
    f: &mut Frame,
    app: &mut App,
    root: &crate::lua::widgets::Widget,
    area: Rect,
    theme: &Theme,
) {
    let Some(runtime) = app.lua_runtime.clone() else {
        return;
    };

    let mut slots: Vec<(String, Rect)> = Vec::new();
    crate::lua::widgets::collect_plugin_slots(root, area, &mut slots);

    for (plugin, rect) in slots {
        match runtime.build_plugin_view(&plugin) {
            Ok(widget) => {
                crate::lua::widgets::render_widget(f, &widget, rect, theme, &app.lua_input_states);

                // Record hit geometry for the plugin's own `id=` widgets, using
                // the tree that was just painted — without this a plugin view
                // could declare `id=` and bind `on_click` and never be hit.
                app.plugin_view_areas.push((plugin.clone(), rect));
                let mut ids: Vec<(String, Rect)> = Vec::new();
                crate::lua::widgets::collect_clickable_areas(&widget, rect, &mut ids);
                for (id, id_rect) in ids {
                    app.plugin_click_areas.push((plugin.clone(), id, id_rect));
                }
            }
            Err(err) => {
                let msg = format!("[{plugin}] {err}");
                let para = Paragraph::new(msg)
                    .style(Style::default().fg(theme.error))
                    .wrap(Wrap { trim: true });
                f.render_widget(para, rect);
            }
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
        (
            vec!["Type a message or /models to select...".to_string()],
            0,
            0,
        )
    } else {
        wrap_input_for_welcome(&app.input, inner_width, app.cursor_col)
    };

    // Calculate dynamic input box height (top border + content lines + bottom border).
    let content_lines = display_lines.len().min(6) as u16; // Max 6 content lines.
    let input_box_height = content_lines + 2; // +2 for top/bottom borders

    let content_start_y = input_y + 1;

    // Calculate cursor position before borrowing buffer.
    let cursor_pos = if !app.slash.active
        && !app.input.is_empty()
        && input_y > area.y
        && input_y + input_box_height < area.y + area.height
    {
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
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }

        // Subtitle.
        let subtitle = "minimal coding agent";
        let sub_y = title_y + 1;
        let sub_x = start_x + (content_width.saturating_sub(subtitle.len() as u16)) / 2;
        for (i, ch) in subtitle.chars().enumerate() {
            if sub_x + (i as u16) < area.x + area.width && sub_y < area.y + area.height {
                buf[(sub_x + i as u16, sub_y)]
                    .set_char(ch)
                    .set_fg(theme.muted);
            }
        }

        // 96E-45: Plugins loading indicator.
        if !app.plugins_ready {
            let spinner = SPINNER[app.spinner_frame % SPINNER.len()];
            let loading_text = format!("{} plugins loading...", spinner);
            let loading_y = sub_y + 1;
            let loading_x = start_x + (content_width.saturating_sub(loading_text.len() as u16)) / 2;
            for (i, ch) in loading_text.chars().enumerate() {
                let x = loading_x + i as u16;
                if x < area.x + area.width && loading_y < area.y + area.height {
                    buf[(x, loading_y)]
                        .set_char(ch)
                        .set_fg(theme.warning);
                }
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
                buf[(input_x + i, input_y)]
                    .set_char('─')
                    .set_fg(theme.muted);
            }
            buf[(input_x + input_width - 1, input_y)]
                .set_char('╮')
                .set_fg(theme.muted);

            // Content lines.
            let text_style = if app.input.is_empty() {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.fg)
            };

            for (line_idx, line) in display_lines
                .iter()
                .enumerate()
                .take(content_lines as usize)
            {
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
                buf[(input_x + input_width - 1, y)]
                    .set_char('│')
                    .set_fg(theme.muted);
            }

            // Bottom border.
            let bottom_y = input_y + input_box_height - 1;
            buf[(input_x, bottom_y)].set_char('╰').set_fg(theme.muted);
            for i in 1..input_width.saturating_sub(1) {
                buf[(input_x + i, bottom_y)]
                    .set_char('─')
                    .set_fg(theme.muted);
            }
            buf[(input_x + input_width - 1, bottom_y)]
                .set_char('╯')
                .set_fg(theme.muted);
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
            let sessions_hint = format!(
                "{} recent sessions (/session to browse)",
                app.session.sessions.len()
            );
            let sessions_x =
                start_x + (content_width.saturating_sub(sessions_hint.len() as u16)) / 2;
            for (i, ch) in sessions_hint.chars().enumerate() {
                let x = sessions_x + i as u16;
                if x < area.x + area.width && sessions_y < area.y + area.height {
                    buf[(x, sessions_y)].set_char(ch).set_fg(theme.muted);
                }
            }
        }

        // 96E-45: Show steering/queue sections if any pending messages.
        let pending_start_y = hints_y + 4;
        let mut pending_y = pending_start_y;

        if !app.steering.is_empty() {
            let header = "── Steering ──";
            let header_x = start_x + (content_width.saturating_sub(header.len() as u16)) / 2;
            for (i, ch) in header.chars().enumerate() {
                let x = header_x + i as u16;
                if x < area.x + area.width && pending_y < area.y + area.height {
                    buf[(x, pending_y)].set_char(ch).set_fg(theme.success);
                }
            }
            pending_y += 1;

            for (i, prompt) in app.steering.iter().enumerate() {
                if pending_y >= area.y + area.height {
                    break;
                }
                let preview: String = if prompt.text.len() > 40 {
                    format!("{}. {}...", i + 1, &prompt.text[..37])
                } else {
                    format!("{}. {}", i + 1, &prompt.text)
                };
                let preview_x = start_x + (content_width.saturating_sub(preview.len() as u16)) / 2;
                for (j, ch) in preview.chars().enumerate() {
                    let x = preview_x + j as u16;
                    if x < area.x + area.width {
                        buf[(x, pending_y)].set_char(ch).set_fg(theme.muted);
                    }
                }
                pending_y += 1;
            }
            pending_y += 1; // spacing
        }

        if !app.queue.is_empty() {
            let header = "── Queue ──";
            let header_x = start_x + (content_width.saturating_sub(header.len() as u16)) / 2;
            for (i, ch) in header.chars().enumerate() {
                let x = header_x + i as u16;
                if x < area.x + area.width && pending_y < area.y + area.height {
                    buf[(x, pending_y)].set_char(ch).set_fg(theme.muted);
                }
            }
            pending_y += 1;

            for (i, prompt) in app.queue.iter().enumerate() {
                if pending_y >= area.y + area.height {
                    break;
                }
                let preview: String = if prompt.text.len() > 40 {
                    format!("{}. {}...", i + 1, &prompt.text[..37])
                } else {
                    format!("{}. {}", i + 1, &prompt.text)
                };
                let preview_x = start_x + (content_width.saturating_sub(preview.len() as u16)) / 2;
                for (j, ch) in preview.chars().enumerate() {
                    let x = preview_x + j as u16;
                    if x < area.x + area.width {
                        buf[(x, pending_y)].set_char(ch).set_fg(theme.muted);
                    }
                }
                pending_y += 1;
            }
        }
    } // End of buffer borrow

    // Set cursor position (after buffer borrow is released).
    if let Some((x, y)) = cursor_pos {
        f.set_cursor_position((x, y));
    }

    // Render slash command dropdown if active.
    if app.slash.active {
        render_slash_dropdown(
            f,
            app,
            Rect::new(input_x, input_y + input_box_height, input_width, 8),
            theme,
        );
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
            Overlay::ToolsManager { selected, filter } => {
                render_tools_manager(f, app, *selected, filter, area, theme);
            }
            _ => {} // Other overlays not applicable on welcome
        }
    }
}

/// Wrap input text for welcome screen and compute cursor position.
/// Returns (display_lines, cursor_row, cursor_col).
fn wrap_input_for_welcome(
    input: &str,
    width: usize,
    cursor_char_pos: usize,
) -> (Vec<String>, usize, usize) {
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

fn render_transcript(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let inner_w = area.width as usize;

    // Track tool positions for click detection.
    // We'll calculate actual screen Y after scroll adjustment.
    let mut tool_line_info: Vec<(String, usize, usize)> = Vec::new(); // (call_id, header_line_idx, content_end_line_idx)

    // Determine which message contains the current search match.
    let current_match_msg_idx = app
        .search_state
        .as_ref()
        .and_then(|s| s.current_match())
        .map(|m| m.msg_idx);

    // Check if search is active (affects caching - search highlighting invalidates cache).
    let search_active = app.search_state.is_some();

    // Count messages to determine if last message is "in progress".
    let msg_count = app.transcript.messages().len();
    let is_streaming = app.streaming;

    for (msg_idx, msg) in app.transcript.messages().iter().enumerate() {
        // Can we use cached rendering for this message?
        // Cache is valid when:
        // - Not searching (search highlighting requires per-frame update)
        // - Not the last message while streaming (may be incomplete)
        // - Width hasn't changed (tracked in update_state)
        let is_last_msg = msg_idx == msg_count.saturating_sub(1);
        let can_use_cache = !(search_active || (is_last_msg && is_streaming));

        // Compute tool info hash - changes when tool state changes (expanded, scroll, status, etc.)
        let tool_info_hash = crate::render_cache::compute_tool_info_hash(&msg.tools);

        // Try cache first
        if can_use_cache {
            if let Some((cached_lines, cached_tools)) =
                app.render_cache
                    .get_message(msg_idx, &msg.content, tool_info_hash)
            {
                let base_line_idx = lines.len();
                lines.extend(cached_lines.iter().cloned());

                // Restore tool positions with correct base offset
                for tool_info in cached_tools {
                    tool_line_info.push((
                        tool_info.call_id.clone(),
                        base_line_idx + tool_info.header_line_offset,
                        base_line_idx + tool_info.content_end_offset,
                    ));
                }
                continue;
            }
        }

        // Remember line count before rendering this message (for caching)
        let lines_before = lines.len();

        // Track tool positions relative to message start (for caching)
        let mut msg_tool_infos: Vec<crate::render_cache::CachedToolInfo> = Vec::new();

        // Is this the message containing the current search match?
        let is_current_match_msg = current_match_msg_idx == Some(msg_idx);

        // Role label.
        let (role_style, prefix) = match msg.role.as_str() {
            "user" => (
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
                "▸ ",
            ),
            "assistant" => (
                Style::default()
                    .fg(theme.assistant)
                    .add_modifier(Modifier::BOLD),
                "◂ ",
            ),
            "error" => (
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
                "✗ ",
            ),
            "system" => (
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
                "ℹ ",
            ),
            _ => (Style::default().fg(theme.muted), "  "),
        };

        // Add role line.
        let role_display = match msg.role.as_str() {
            "assistant" => "kn9t".to_string(),
            _ => msg.role.clone(),
        };
        let role_line_style = if msg.role == "user" {
            role_style.bg(theme.user_msg_bg)
        } else {
            role_style
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, role_display),
            role_line_style,
        )));

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
                            let thinking_lines =
                                thinking::render_thinking_content(&content, theme, md_width);
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
            let base_style = if msg.role == "user" {
                Style::default().fg(theme.fg).bg(theme.user_msg_bg)
            } else {
                Style::default().fg(theme.fg)
            };
            for content_line in msg.content.lines() {
                for wrapped in wrap_text(content_line, inner_w.saturating_sub(2)) {
                    let line_spans = if let Some(ref search) = app.search_state {
                        let mut spans = vec![Span::raw("  ")];
                        // Use direct text search (not byte offsets) since text is wrapped
                        spans.extend(search.highlight_in_text(
                            &wrapped,
                            base_style,
                            is_current_match_msg,
                        ));
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
            // Track header position relative to message start (for cache)
            let header_line_offset = lines.len() - lines_before;
            let header_line_idx = lines.len();

            render_tool_card(card, app, &mut lines, inner_w, theme, area.height as usize);

            let content_end_line_idx = lines.len();
            let content_end_offset = lines.len() - lines_before;

            // Add to global tool_line_info for click detection
            tool_line_info.push((card.call_id.clone(), header_line_idx, content_end_line_idx));

            // Add to message-local tool infos for caching
            msg_tool_infos.push(crate::render_cache::CachedToolInfo {
                call_id: card.call_id.clone(),
                header_line_offset,
                content_end_offset,
            });

            // 96E-27: collapsible subagent sub-entry nested under its spawning tool call
            if let Some(sub) = app.subagents.iter().find(|s| s.call_id == card.call_id) {
                let collapsed = sub.collapsed;
                let vis = sub.visibility.as_str();
                let indicator = if collapsed { "[+]" } else { "[-]" };
                let vis_style = match vis {
                    "silent" => Style::default().fg(theme.muted),
                    "full" => Style::default().fg(theme.primary),
                    _ => Style::default().fg(theme.success),
                };
                let header = Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        format!("{} subagent ", indicator),
                        Style::default().fg(theme.muted),
                    ),
                    Span::styled(
                        truncate(&sub.task, inner_w.saturating_sub(20)),
                        Style::default().fg(theme.fg).add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(format!(" [{}]", vis), vis_style),
                    Span::styled(
                        "  (a:attach ".to_string()
                            + if collapsed { "expand" } else { "collapse" }
                            + ")",
                        Style::default().fg(theme.muted),
                    ),
                ]);
                lines.push(header);
            }
        }

        lines.push(Line::from("")); // spacing

        // Cache the rendered lines for this message if cacheable
        if can_use_cache {
            let msg_lines: Vec<Line<'static>> = lines[lines_before..].to_vec();
            app.render_cache.set_message(
                msg_idx,
                &msg.content,
                tool_info_hash,
                msg_lines,
                msg_tool_infos,
            );
        }
    }

    // Update cache state
    app.render_cache.update_state(
        app.transcript.messages().len(),
        app.transcript.live_delta().len(),
        inner_w,
    );

    // 96E-27: attached subagent full transcript on demand (session_read result)
    if let Some((ref call_id, ref transcript)) = app.attached_subagent {
        lines.push(Line::from(vec![
            Span::styled("── ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("Attached subagent {} ", &call_id[..8.min(call_id.len())]),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({} msgs)", transcript.len()),
                Style::default().fg(theme.muted),
            ),
            Span::styled(
                " ── (Esc to close)",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        for msg in transcript {
            let content_val = &msg.content;
            // Try to extract text from content JSON
            let text = if let Some(s) = content_val.as_str() {
                s.to_string()
            } else {
                content_val.to_string()
            };
            let role_style = if msg.role == "assistant" {
                Style::default().fg(theme.assistant)
            } else {
                Style::default().fg(theme.fg)
            };
            for wrapped in wrap_text(&text, inner_w.saturating_sub(4)) {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(wrapped, role_style),
                ]));
            }
        }
        lines.push(Line::from(""));
    }

    // Live delta — render as markdown.
    if !app.transcript.live_delta().is_empty() {
        lines.push(Line::from(Span::styled(
            "◂ kn9t",
            Style::default()
                .fg(theme.assistant)
                .add_modifier(Modifier::BOLD),
        )));
        let md_width = inner_w.saturating_sub(2);
        let md_lines = crate::markdown::render(app.transcript.live_delta(), theme, md_width);
        for line in md_lines {
            let mut indented = vec![Span::raw("  ")];
            indented.extend(line.spans);
            lines.push(Line::from(indented));
        }
    }

    // Phase-synced streaming indicator — server is source of truth.
    if app.streaming
        || matches!(
            app.turn_phase.as_str(),
            "retrying" | "failed" | "tool" | "thinking"
        )
    {
        let spinner = SPINNER[app.spinner_frame % SPINNER.len()];
        if app.aborting {
            lines.push(Line::from(Span::styled(
                format!("  {} Aborting...", spinner),
                Style::default().fg(theme.error),
            )));
        } else if app.turn_phase == "retrying" {
            let detail = if app.turn_status_msg.is_empty() {
                "retrying...".to_string()
            } else {
                app.turn_status_msg.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {} {}", spinner, detail),
                Style::default().fg(theme.warning),
            )));
        } else if app.turn_phase == "failed" {
            let detail = if app.turn_status_msg.is_empty() {
                "failed".to_string()
            } else {
                app.turn_status_msg.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {} failed: {}", spinner, detail),
                Style::default().fg(theme.error),
            )));
        } else if app.turn_phase == "tool" {
            let detail = if app.turn_status_msg.is_empty() {
                "running tool...".to_string()
            } else {
                app.turn_status_msg.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {} {}", spinner, detail),
                Style::default().fg(theme.warning),
            )));
        } else if app.turn_phase == "thinking" {
            lines.push(Line::from(Span::styled(
                format!("  {} thinking...", spinner),
                Style::default().fg(theme.muted),
            )));
        } else if !app.transcript.live_delta().is_empty() {
            // streaming with deltas — show spinner without extra phrase (content already visible)
            lines.push(Line::from(Span::styled(
                format!("  {} streaming...", spinner),
                Style::default().fg(theme.muted),
            )));
        } else {
            let phrase =
                &app.config.streaming_phrases[app.phrase_idx % app.config.streaming_phrases.len()];
            lines.push(Line::from(Span::styled(
                format!("  {} {}", spinner, phrase),
                Style::default().fg(theme.muted),
            )));
        }
    } else if app.turn_phase == "failed" && !app.turn_status_msg.is_empty() {
        // Brief failed notice even if not streaming (TurnEnded already cleared streaming but phase stays failed until next turn)
        lines.push(Line::from(Span::styled(
            format!("  ✗ {}", app.turn_status_msg),
            Style::default().fg(theme.error),
        )));
    }

    // 96E-45: Render Steering section (muted, at bottom before queue).
    if !app.steering.is_empty() {
        lines.push(Line::from(Span::styled(
            "── Steering ──",
            Style::default().fg(theme.muted),
        )));
        for (i, prompt) in app.steering.iter().enumerate() {
            let preview = if prompt.text.len() > 60 {
                format!("{}...", &prompt.text[..57])
            } else {
                prompt.text.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}. {}", i + 1, preview),
                Style::default().fg(theme.muted),
            )));
        }
    }

    // 96E-45: Render Queue section (muted, at bottom).
    if !app.queue.is_empty() {
        lines.push(Line::from(Span::styled(
            "── Queue ──",
            Style::default().fg(theme.muted),
        )));
        for (i, prompt) in app.queue.iter().enumerate() {
            let preview = if prompt.text.len() > 60 {
                format!("{}...", &prompt.text[..57])
            } else {
                prompt.text.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}. {}", i + 1, preview),
                Style::default().fg(theme.muted),
            )));
        }
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

            // Card width calculation (must match render_tool_card)
            let card_w = inner_w.saturating_sub(TOOL_CARD_RIGHT_MARGIN).max(50);

            app.tool_hit_areas.push(ToolHitArea {
                call_id,
                header_y,
                content_y_start,
                content_y_end,
                x_start: area.x,
                x_end: area.x + card_w as u16,
                progress_tab_x: (tab_base_x, tab_base_x + 10), // " Progress "
                output_tab_x: (tab_base_x + 11, tab_base_x + 19), // " Output "
                input_tab_x: (tab_base_x + 20, tab_base_x + 28), // " Input "
            });
        }
    }

    let para = Paragraph::new(lines).scroll((scroll_offset as u16, 0));
    f.render_widget(para, area);

    // ─────────────────────────────────────────────────────────────────
    // Scrollbar on the right edge
    // ─────────────────────────────────────────────────────────────────
    if total > visible {
        let buf = f.buffer_mut();
        let scrollbar_x = area.x + area.width.saturating_sub(1);
        let scrollbar_height = area.height as usize;

        // Store scrollbar area for mouse interaction
        app.scrollbar_area = Some((scrollbar_x, area.y, area.y + area.height, total, visible));

        // Calculate thumb position and size
        let thumb_size = (visible * scrollbar_height / total)
            .max(1)
            .min(scrollbar_height);
        let thumb_pos = if max_scroll > 0 {
            (max_scroll - effective_scroll) * (scrollbar_height - thumb_size) / max_scroll
        } else {
            scrollbar_height - thumb_size
        };

        // Draw scrollbar track and thumb
        for i in 0..scrollbar_height {
            let y = area.y + i as u16;
            let in_thumb = i >= thumb_pos && i < thumb_pos + thumb_size;

            if in_thumb {
                // Thumb - solid block
                buf[(scrollbar_x, y)].set_char('┃').set_fg(theme.primary);
            } else {
                // Track - light line
                buf[(scrollbar_x, y)].set_char('│').set_fg(theme.muted);
            }
        }
    } else {
        app.scrollbar_area = None;
    }

    // Jump to end button (show when not at bottom).
    if effective_scroll > 0 {
        let btn = "↓ End";
        let x = area.x + area.width.saturating_sub(btn.len() as u16 + 2);
        let y = area.y + area.height.saturating_sub(1);
        let buf = f.buffer_mut();
        for (i, ch) in btn.chars().enumerate() {
            if x + (i as u16) < area.x + area.width - 1 {
                buf[(x + i as u16, y)]
                    .set_char(ch)
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
    let prompt_style = if has_lease {
        Style::default().fg(theme.primary)
    } else {
        Style::default().fg(theme.muted)
    };

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
    // logical_row tracks newlines in the original input, not display lines after wrapping.
    let mut logical_row: usize = 0;
    let input_char_count = app.input.chars().count();

    // Use display_input (with image suffix) for rendering.
    #[allow(clippy::explicit_counter_loop)]
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
                    if cursor_in_line >= pos
                        && cursor_in_line < end
                        && cursor_in_line <= input_char_count
                    {
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
    // Lua owns the status line when it defines `render_status()`. This is the
    // whole point of the hook: without it the built-in Lua status bar was built
    // every frame and thrown away, and no config could change this line.
    if let Some(spans) = app
        .lua_runtime
        .as_ref()
        .and_then(|rt| rt.call_status_spans())
    {
        let line = Line::from(
            spans
                .iter()
                .map(|s| Span::styled(s.text.clone(), s.style.to_ratatui_style(theme)))
                .collect::<Vec<_>>(),
        );
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    // Fallback only for a config with no `render_status`.
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

    let phase_disp = app.turn_phase.as_str();
    let status = format!(
        "{} | ${:.4} | {}{} | {} | ^P",
        app.current_model_name(),
        app.tokens.cost,
        turn_info,
        tps_info,
        phase_disp,
    );

    let para = Paragraph::new(Line::from(Span::styled(
        status,
        Style::default().fg(theme.muted),
    )));
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
    let base_overlay_h = 15u16;

    // Calculate dynamic height for Interaction overlays
    let overlay_h = if let Overlay::Interaction { state, .. } = overlay {
        let inner_w = (overlay_w.saturating_sub(4)) as usize;
        compute_interaction_height(state, inner_w, area.height.saturating_sub(4))
    } else {
        base_overlay_h.min(area.height.saturating_sub(4))
    };

    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;

    // Clear overlay area.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }

    match overlay {
        Overlay::Approval {
            tool,
            args,
            selected,
        } => {
            // Highly visible border — heavy box in warning color with bold.
            let border_fg = theme.warning;
            let border_style = Style::default()
                .fg(border_fg)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD);
            if overlay_w >= 2 && overlay_h >= 2 {
                buf[(overlay_x, overlay_y)]
                    .set_char('┏')
                    .set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y)]
                    .set_char('┓')
                    .set_style(border_style);
                buf[(overlay_x, overlay_y + overlay_h - 1)]
                    .set_char('┗')
                    .set_style(border_style);
                buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)]
                    .set_char('┛')
                    .set_style(border_style);
                for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
                    buf[(x, overlay_y)].set_char('━').set_style(border_style);
                    buf[(x, overlay_y + overlay_h - 1)]
                        .set_char('━')
                        .set_style(border_style);
                }
                for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
                    buf[(overlay_x, y)].set_char('┃').set_style(border_style);
                    buf[(overlay_x + overlay_w - 1, y)]
                        .set_char('┃')
                        .set_style(border_style);
                }
            }
            let mut y = overlay_y + 1;

            // Title.
            let title = "APPROVAL REQUIRED";
            let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
            for (i, ch) in title.chars().enumerate() {
                buf[(title_x + i as u16, y)]
                    .set_char(ch)
                    .set_fg(theme.warning)
                    .set_bg(Color::Black);
            }
            y += 2;

            // Nice per-tool display with highlighting and reason.
            let inner_w = (overlay_w.saturating_sub(4)) as usize;
            let inner_x = overlay_x + 2;
            let body_lines = approval_body_lines(tool, args, theme, inner_w.max(1));
            for line in body_lines {
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                let mut x = inner_x;
                for span in line.spans {
                    let style = if span.style.bg.is_none() {
                        span.style.bg(Color::Black)
                    } else {
                        span.style
                    };
                    for ch in span.content.chars() {
                        if x >= overlay_x + overlay_w - 1 {
                            break;
                        }
                        buf[(x, y)].set_char(ch).set_style(style);
                        x += 1;
                    }
                    if x >= overlay_x + overlay_w - 1 {
                        break;
                    }
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

        Overlay::Interaction { plugin, state, .. } => {
            render_interaction_overlay(
                buf, theme, overlay_x, overlay_y, overlay_w, overlay_h, plugin, state,
            );
        }

        Overlay::Help => {
            let mut y = overlay_y + 1;

            let title = "HELP";
            let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
            for (i, ch) in title.chars().enumerate() {
                buf[(title_x + i as u16, y)]
                    .set_char(ch)
                    .set_fg(theme.primary)
                    .set_bg(Color::Black);
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
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if (overlay_x + 2 + i as u16) < overlay_x + overlay_w {
                        buf[(overlay_x + 2 + i as u16, y)]
                            .set_char(ch)
                            .set_fg(theme.fg)
                            .set_bg(Color::Black);
                    }
                }
                y += 1;
            }
        }

        Overlay::ModelSelect { .. }
        | Overlay::SessionSelect { .. }
        | Overlay::WhichKey
        | Overlay::CommandPalette
        | Overlay::ToolsManager { .. }
        | Overlay::SessionTree { .. } => {
            // These overlays are rendered elsewhere with access to app state.
        }
    }
}

fn render_slash_dropdown(f: &mut Frame, app: &App, input_area: Rect, theme: &Theme) {
    let matches = &app.slash.matches;
    if matches.is_empty() {
        return;
    }
    let entries = app.slash.entries();

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
        let Some(cmd) = entries.get(cmd_idx) else {
            continue;
        };
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

fn render_model_select(
    f: &mut Frame,
    app: &App,
    selected: usize,
    filter: &str,
    area: Rect,
    theme: &Theme,
) {
    let buf = f.buffer_mut();

    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    // Filter models by fuzzy match on display name OR provider.
    let filtered: Vec<(usize, &crate::model_selector::ModelEntry)> = app
        .model_sel
        .models()
        .iter()
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
        Model(&'a crate::model_selector::ModelEntry),
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut current_provider: Option<&str> = None;
    // selectable_idx counts models only (headers don't count), so it differs from enumerate().
    let mut selectable_idx = 0usize;
    let mut selected_row_idx: Option<usize> = None; // Row index of selected model

    #[allow(clippy::explicit_counter_loop)]
    for (_orig_idx, model) in &filtered {
        if current_provider != Some(&model.provider) {
            current_provider = Some(&model.provider);
            rows.push(Row::Header(&model.provider));
        }
        if selectable_idx == selected {
            selected_row_idx = Some(rows.len());
        }
        rows.push(Row::Model(model));
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
    let overlay_h = (rows.len() as u16 + 6)
        .min(area.height.saturating_sub(4))
        .max(8);
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
        buf[(title_x + i as u16, y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    y += 1;

    // Filter input.
    let filter_display = if filter.is_empty() {
        "Type to filter..."
    } else {
        filter
    };
    let filter_style = if filter.is_empty() {
        theme.muted
    } else {
        theme.fg
    };
    for (i, ch) in format!("› {}", filter_display).chars().enumerate() {
        let x = overlay_x + 2 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, y)]
                .set_char(ch)
                .set_fg(filter_style)
                .set_bg(Color::Black);
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
                        buf[(x, y)]
                            .set_char(ch)
                            .set_fg(theme.muted)
                            .set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            Row::Model(model) => {
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
                    line.chars()
                        .take(content_w.saturating_sub(1))
                        .collect::<String>()
                        + "…"
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
                for x in
                    (overlay_x + 2 + truncated.chars().count() as u16)..overlay_x + overlay_w - 2
                {
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
                buf[(x, y)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }
    }

    // Footer.
    let footer = "↑/↓ select · Enter confirm · Esc cancel";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        buf[(footer_x + i as u16, footer_y)]
            .set_char(ch)
            .set_fg(theme.muted)
            .set_bg(Color::Black);
    }
}

fn render_session_select(
    f: &mut Frame,
    app: &App,
    selected: usize,
    filter: &str,
    area: Rect,
    theme: &Theme,
) {
    let buf = f.buffer_mut();

    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    // 96E-53: rows in tree order (a branch directly under its parent), from the single
    // shared `picker_order` the key handler also calls — they must not diverge (96E-19).
    let ordered = crate::session_tree::picker_order(&app.session.sessions, filter);
    let filtered: Vec<(usize, &crate::session_manager::SessionEntry)> = ordered
        .iter()
        .filter_map(|&(idx, _)| app.session.sessions.get(idx).map(|s| (idx, s)))
        .collect();
    let depth_of: std::collections::HashMap<usize, usize> = ordered.iter().copied().collect();

    // Build rows with date headers.
    #[derive(Clone)]
    enum SessionRow<'a> {
        DateHeader(String), // "Today", "Yesterday", "Aug 27", etc.
        NewSession,
        Session(&'a crate::session_manager::SessionEntry, usize),
    }

    let mut rows: Vec<SessionRow> = Vec::new();

    // Add "New session" option at top.
    rows.push(SessionRow::NewSession);

    // Group sessions by date. Only roots get a header: a branch belongs under the
    // conversation it came from, not under the day it happened to be created.
    let mut current_date: Option<String> = None;
    for (orig_idx, session) in &filtered {
        let depth = depth_of.get(orig_idx).copied().unwrap_or(0);
        if depth == 0 {
            let date_label = session
                .created_at
                .as_ref()
                .and_then(|ts| format_date_header(ts))
                .unwrap_or_else(|| "Unknown".to_string());

            if current_date.as_ref() != Some(&date_label) {
                current_date = Some(date_label.clone());
                rows.push(SessionRow::DateHeader(date_label));
            }
        }
        rows.push(SessionRow::Session(session, depth));
    }

    // Center overlay.
    let overlay_w = 55.min(area.width.saturating_sub(4));
    let overlay_h = (rows.len() as u16 + 6)
        .min(area.height.saturating_sub(4))
        .max(8);
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
        buf[(title_x + i as u16, y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    y += 1;

    // Filter input.
    let filter_display = if filter.is_empty() {
        "Type to filter..."
    } else {
        filter
    };
    let filter_style = if filter.is_empty() {
        theme.muted
    } else {
        theme.fg
    };
    for (i, ch) in format!("› {}", filter_display).chars().enumerate() {
        let x = overlay_x + 2 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, y)]
                .set_char(ch)
                .set_fg(filter_style)
                .set_bg(Color::Black);
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
                        buf[(x, y)]
                            .set_char(ch)
                            .set_fg(theme.muted)
                            .set_bg(Color::Black);
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
            SessionRow::Session(session, depth) => {
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

                // 96E-53: indent by depth and badge the reason, so a rewind reads as a
                // step back from its parent rather than as an unrelated session.
                let indent = "  ".repeat(*depth);
                let badge = crate::session_tree::reason_badge(session.fork_reason.as_deref());
                let badge = if badge.is_empty() {
                    String::new()
                } else {
                    format!("{badge} ")
                };
                let room = (overlay_w as usize)
                    .saturating_sub(8 + indent.len() + badge.chars().count())
                    .max(4);
                let line = format!(
                    "{}{}{}{}",
                    prefix,
                    indent,
                    badge,
                    truncate(&session.name, room)
                );
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
                buf[(x, y)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }
    }

    // Footer.
    let footer = "↑/↓ select · Enter open · Del delete · Esc cancel";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        if footer_x + (i as u16) < overlay_x + overlay_w {
            buf[(footer_x + i as u16, footer_y)]
                .set_char(ch)
                .set_fg(theme.muted)
                .set_bg(Color::Black);
        }
    }
}

/// 96E-54 — the session tree as a git-style graph.
///
/// Nodes are sessions, edges are `origin_session` links, and the label on an edge is the
/// fork reason. The shape comes from `session_tree::build_forest`, the same function the
/// sidebar uses: one tree implementation, two renderings.
fn render_session_tree(f: &mut Frame, app: &App, selected: usize, area: Rect, theme: &Theme) {
    use crate::session_tree::{build_forest, reason_badge};

    let buf = f.buffer_mut();

    // Dim background, as the other full-screen overlays do.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    let entries = &app.session.sessions;
    let nodes = build_forest(entries).flatten();

    let overlay_w = 72.min(area.width.saturating_sub(4));
    let overlay_h = (nodes.len() as u16 + 5)
        .min(area.height.saturating_sub(4))
        .max(8);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;

    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }

    let title = "SESSION TREE";
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    let mut y = overlay_y + 1;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    y += 2;

    // A single session and no forks is not an error state — just a tree of one. Say so
    // rather than drawing an empty box the user has to interpret.
    if nodes.len() <= 1 {
        let msg = if nodes.is_empty() {
            "No sessions."
        } else {
            "One session, no branches yet. /fork or /undo creates one."
        };
        for (i, ch) in msg.chars().enumerate() {
            let x = overlay_x + 2 + i as u16;
            if x < overlay_x + overlay_w - 2 {
                buf[(x, y)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }
    }

    // Rows, depth-first so a child always follows its parent.
    let first_visible = {
        // Keep the cursor on screen for a forest taller than the box.
        let rows = overlay_h.saturating_sub(5) as usize;
        if rows > 0 && selected >= rows {
            selected + 1 - rows
        } else {
            0
        }
    };
    for (i, node) in nodes.iter().enumerate().skip(first_visible) {
        if y >= overlay_y + overlay_h - 1 {
            break;
        }
        let Some(entry) = entries.get(node.idx) else {
            continue;
        };
        let is_selected = i == selected;
        let is_current = entry.id == app.session.state.session_id;
        let (fg, bg) = if is_selected {
            (theme.bg, theme.primary)
        } else if is_current {
            (theme.success, Color::Black)
        } else {
            (theme.fg, Color::Black)
        };

        // Indent by depth; the glyph marks the current session, the badge the reason this
        // branch exists. Roots carry no badge — they were not forked from anything.
        let indent = "  ".repeat(node.depth);
        let marker = if is_current { "●" } else { "○" };
        let badge = reason_badge(entry.fork_reason.as_deref());
        let at = match entry.origin_seq {
            Some(seq) if node.depth > 0 => format!(" @{seq}"),
            _ => String::new(),
        };
        let name_room = overlay_w as usize - (indent.len() + 12).min(overlay_w as usize - 1);
        let line = format!(
            "{}{} {} {}{}",
            indent,
            marker,
            badge,
            truncate(&entry.name, name_room),
            at
        );
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
    }

    let footer = "↑/↓ select · Enter switch · Esc close";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        if footer_x + (i as u16) < overlay_x + overlay_w {
            buf[(footer_x + i as u16, footer_y)]
                .set_char(ch)
                .set_fg(theme.muted)
                .set_bg(Color::Black);
        }
    }
}

fn render_tools_manager(
    f: &mut Frame,
    app: &App,
    selected: usize,
    filter: &str,
    area: Rect,
    theme: &Theme,
) {
    let buf = f.buffer_mut();

    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    // Filter tools by name or plugin.
    let filtered: Vec<(usize, &crate::app::ToolEntry)> = app
        .tools
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            filter.is_empty()
                || fuzzy_match(&t.name, filter)
                || t.plugin
                    .as_ref()
                    .map(|p| fuzzy_match(p, filter))
                    .unwrap_or(false)
        })
        .collect();

    // Build rows grouped by plugin.
    #[derive(Clone)]
    enum ToolRow<'a> {
        PluginHeader(&'a str),
        Tool(&'a crate::app::ToolEntry),
    }

    let mut rows: Vec<ToolRow> = Vec::new();
    let mut current_plugin: Option<&str> = None;
    // selectable_idx counts tools only (headers don't count), so it differs from enumerate().
    let mut selectable_idx = 0usize;
    let mut selected_row_idx: Option<usize> = None;

    #[allow(clippy::explicit_counter_loop)]
    for (_orig_idx, tool) in &filtered {
        let plugin = tool.plugin.as_deref().unwrap_or("builtin");
        if current_plugin != Some(plugin) {
            current_plugin = Some(plugin);
            rows.push(ToolRow::PluginHeader(plugin));
        }
        if selectable_idx == selected {
            selected_row_idx = Some(rows.len());
        }
        rows.push(ToolRow::Tool(tool));
        selectable_idx += 1;
    }

    // Overlay dimensions (need these before scroll calculation).
    let overlay_w = 60.min(area.width.saturating_sub(4));
    let overlay_h = (rows.len() as u16 + 6)
        .min(area.height.saturating_sub(4))
        .max(10);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;

    // Calculate visible window for scrolling.
    // Content area: title (1) + blank (1) + filter (1) + blank (1) + rows + footer (1) = 5 lines overhead
    let content_start_y = overlay_y + 4; // after title + filter + spacing
    let content_end_y = overlay_y + overlay_h - 2; // before footer
    let max_visible_rows = (content_end_y.saturating_sub(content_start_y)) as usize;

    // Scroll so selected item is visible, preferring to show it in the middle.
    let scroll_offset = if let Some(sel_row) = selected_row_idx {
        if sel_row >= max_visible_rows {
            // Keep selected item roughly centered, but don't scroll past the end
            let ideal = sel_row.saturating_sub(max_visible_rows / 2);
            let max_scroll = rows.len().saturating_sub(max_visible_rows);
            ideal.min(max_scroll)
        } else {
            0
        }
    } else {
        0
    };

    // Background.
    for y in overlay_y..overlay_y + overlay_h {
        for x in overlay_x..overlay_x + overlay_w {
            buf[(x, y)].set_char(' ').set_bg(Color::Black);
        }
    }

    // Title.
    let enabled_count = app.tools.iter().filter(|t| t.enabled).count();
    let total_count = app.tools.len();
    let title = format!("TOOLS ({}/{})", enabled_count, total_count);
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    let mut y = overlay_y + 1;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    y += 1;

    // Filter input.
    let filter_display = if filter.is_empty() {
        "Type to filter..."
    } else {
        filter
    };
    let filter_style = if filter.is_empty() {
        theme.muted
    } else {
        theme.fg
    };
    for (i, ch) in format!("› {}", filter_display).chars().enumerate() {
        let x = overlay_x + 2 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, y)]
                .set_char(ch)
                .set_fg(filter_style)
                .set_bg(Color::Black);
        }
    }
    y += 2;

    // Render rows with scrolling.
    let mut tool_display_idx = 0usize;
    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx < scroll_offset {
            if matches!(row, ToolRow::Tool(..)) {
                tool_display_idx += 1;
            }
            continue;
        }
        if y >= overlay_y + overlay_h - 2 {
            break;
        }
        match row {
            ToolRow::PluginHeader(name) => {
                let header = format!("─ {} ─", name);
                for (j, ch) in header.chars().enumerate() {
                    let x = overlay_x + 2 + j as u16;
                    if x < overlay_x + overlay_w - 2 {
                        buf[(x, y)]
                            .set_char(ch)
                            .set_fg(theme.muted)
                            .set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            ToolRow::Tool(tool) => {
                let is_selected = tool_display_idx == selected;
                let (fg, bg) = if is_selected {
                    (theme.bg, theme.primary)
                } else {
                    (theme.fg, Color::Black)
                };

                let check = if tool.enabled { "☑" } else { "☐" };
                let line = format!(
                    " {} {}",
                    check,
                    truncate(&tool.name, (overlay_w - 8) as usize)
                );
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
                tool_display_idx += 1;
            }
        }
    }

    // Empty message.
    if filtered.is_empty() {
        let msg = "No matching tools";
        for (i, ch) in msg.chars().enumerate() {
            let x = overlay_x + 2 + i as u16;
            if x < overlay_x + overlay_w - 2 {
                buf[(x, y)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }
    }

    // Footer.
    let footer = "↑/↓ select · Space toggle · Esc close";
    let footer_x = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
    let footer_y = overlay_y + overlay_h - 1;
    for (i, ch) in footer.chars().enumerate() {
        if footer_x + (i as u16) < overlay_x + overlay_w {
            buf[(footer_x + i as u16, footer_y)]
                .set_char(ch)
                .set_fg(theme.muted)
                .set_bg(Color::Black);
        }
    }
}

fn render_command_palette(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let buf = f.buffer_mut();
    let palette = &app.command_palette;
    let entries = palette.entries();

    // Dim background.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_fg(Color::DarkGray);
        }
    }

    // Center overlay.
    let overlay_w = 60.min(area.width.saturating_sub(4));
    let max_items = 12usize;
    let overlay_h = (palette.matches.len().min(max_items) as u16 + 5)
        .min(area.height.saturating_sub(4))
        .max(6);
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
    buf[(overlay_x, overlay_y)]
        .set_char('╭')
        .set_fg(theme.primary)
        .set_bg(Color::Black);
    buf[(overlay_x + overlay_w - 1, overlay_y)]
        .set_char('╮')
        .set_fg(theme.primary)
        .set_bg(Color::Black);
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, overlay_y)]
            .set_char('─')
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    // Bottom.
    buf[(overlay_x, overlay_y + overlay_h - 1)]
        .set_char('╰')
        .set_fg(theme.primary)
        .set_bg(Color::Black);
    buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)]
        .set_char('╯')
        .set_fg(theme.primary)
        .set_bg(Color::Black);
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, overlay_y + overlay_h - 1)]
            .set_char('─')
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    // Sides.
    for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
        buf[(overlay_x, y)]
            .set_char('│')
            .set_fg(theme.primary)
            .set_bg(Color::Black);
        buf[(overlay_x + overlay_w - 1, y)]
            .set_char('│')
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }

    // Title.
    let title = " Command Palette ";
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, overlay_y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }

    // Search input.
    let input_y = overlay_y + 1;
    let prompt = "> ";
    for (i, ch) in prompt.chars().enumerate() {
        buf[(overlay_x + 2 + i as u16, input_y)]
            .set_char(ch)
            .set_fg(theme.muted)
            .set_bg(Color::Black);
    }
    let query = &palette.query;
    for (i, ch) in query.chars().enumerate() {
        let x = overlay_x + 4 + i as u16;
        if x < overlay_x + overlay_w - 2 {
            buf[(x, input_y)]
                .set_char(ch)
                .set_fg(theme.fg)
                .set_bg(Color::Black);
        }
    }
    // Cursor.
    let cursor_x = overlay_x + 4 + query.chars().count() as u16;
    if cursor_x < overlay_x + overlay_w - 2 {
        buf[(cursor_x, input_y)]
            .set_char('▏')
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }

    // Separator.
    let sep_y = overlay_y + 2;
    for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
        buf[(x, sep_y)]
            .set_char('─')
            .set_fg(Color::DarkGray)
            .set_bg(Color::Black);
    }

    // Command list.
    let mut y = overlay_y + 3;
    for (i, &cmd_idx) in palette.matches.iter().enumerate().take(max_items) {
        if y >= overlay_y + overlay_h - 1 {
            break;
        }

        let Some(cmd) = entries.get(cmd_idx) else {
            continue;
        };
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
        let label = &cmd.label;
        for (j, ch) in label.chars().enumerate() {
            let x = overlay_x + 2 + j as u16;
            if x < overlay_x + overlay_w - 20 {
                buf[(x, y)].set_char(ch).set_fg(fg).set_bg(bg);
            }
        }

        // Keybinding (right-aligned).
        if let Some(kb) = cmd.keybinding.as_deref() {
            let kb_x = overlay_x + overlay_w - 2 - kb.len() as u16;
            let kb_fg = if is_selected { theme.bg } else { theme.muted };
            for (j, ch) in kb.chars().enumerate() {
                buf[(kb_x + j as u16, y)]
                    .set_char(ch)
                    .set_fg(kb_fg)
                    .set_bg(bg);
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
            buf[(count_x + i as u16, count_y)]
                .set_char(ch)
                .set_fg(theme.muted)
                .set_bg(Color::Black);
        }
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
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
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();

    // Days since epoch for session date.
    // Simplified: assume 365.25 days/year, 30.44 days/month.
    let session_days = (year as i64 - 1970) * 365 + (month as i64 - 1) * 30 + day as i64;
    let today_days = (now / 86400) as i64;
    let diff = today_days - session_days;

    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_name = month_names
        .get(month.saturating_sub(1) as usize)
        .unwrap_or(&"???");

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
// Tool Card Rendering — Cards with background, margin, and syntax highlighting
// ═══════════════════════════════════════════════════════════════════════════

/// Default visible lines in expanded tool output (fallback when screen size unknown).
const TOOL_OUTPUT_DEFAULT_VISIBLE_LINES: usize = 10;

/// Minimum visible lines in expanded tool output.
const TOOL_OUTPUT_MIN_VISIBLE_LINES: usize = 5;

/// Determine what content to show for a tool (smart defaults based on tool type).
#[derive(Clone, Copy, PartialEq)]
enum ToolDisplayMode {
    /// Show diff/progress lines (edit, write)
    Diff,
    /// Show command output (bash, MCP tools)
    Output,
    /// Show just a summary in header (read - content is too long)
    Summary,
    /// Show streaming progress while running, output when done
    Streaming,
}

impl ToolDisplayMode {
    /// Parse a mode named by Lua, or `None` for an unknown name.
    fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "diff" => Self::Diff,
            "output" => Self::Output,
            "summary" => Self::Summary,
            "streaming" => Self::Streaming,
            _ => return None,
        })
    }
}

/// How a tool's card should be displayed.
///
/// Lua decides via `tool_mode(name)`; the built-in mapping is the fallback so a
/// config that defines nothing keeps the current behaviour. Previously this was
/// a hardcoded `match` on tool name, which meant a plugin's tool could never
/// choose how it rendered.
fn get_tool_display_mode(app: &App, name: &str) -> ToolDisplayMode {
    if let Some(mode) = app
        .lua_runtime
        .as_ref()
        .and_then(|rt| rt.call_tool_mode(name))
        .and_then(|s| ToolDisplayMode::from_name(&s))
    {
        return mode;
    }

    match name {
        "edit" | "write" => ToolDisplayMode::Diff,
        "read" => ToolDisplayMode::Summary,
        "bash" => ToolDisplayMode::Streaming,
        _ => ToolDisplayMode::Output, // MCP tools, others
    }
}

/// Render a single tool card with smart content display (no tabs).
/// Cards show the most relevant content based on tool type.
/// `available_height` is used to show more lines when terminal is tall.
fn render_tool_card(
    card: &ToolCard,
    app: &App,
    lines: &mut Vec<Line>,
    inner_w: usize,
    theme: &Theme,
    available_height: usize,
) {
    let is_focused = app.focused_tool.as_ref() == Some(&card.call_id);
    let is_running = card.status.starts_with("running");

    // Card width: use available width minus right margin (min 50 chars)
    let card_w = inner_w.saturating_sub(TOOL_CARD_RIGHT_MARGIN).max(50);

    // Status/accent color for left border
    let accent_color = match card.status.as_str() {
        "done" => theme.success,
        "error" => theme.error,
        s if s.starts_with("running") => theme.primary,
        _ => theme.muted,
    };

    // Status icon
    let status_icon = match card.status.as_str() {
        s if s.starts_with("running") => SPINNER[app.spinner_frame % SPINNER.len()],
        "done" => '✓',
        "error" => '✗',
        _ => '○',
    };

    // Card background
    let card_bg = if is_focused {
        theme.tool_focus_bg
    } else {
        theme.tool_card_bg
    };

    let display_mode = get_tool_display_mode(app, &card.name);
    let expand_icon = if card.expanded { '▾' } else { '▸' };

    // Build header with smart summary
    let header_extra = build_tool_header_extra(
        card,
        display_mode,
        card_w.saturating_sub(card.name.len() + 12),
    );

    let header_text_len = 6 + card.name.len() + header_extra.chars().count();
    let header_padding = card_w.saturating_sub(header_text_len + 1);

    // ─────────────────────────────────────────────────────────────────
    // Header: ┃ ▸ ✓ edit  src/app.rs  (or bash  cargo test)
    // ─────────────────────────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::styled(
            "┃",
            Style::default()
                .fg(accent_color)
                .bg(card_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", expand_icon),
            Style::default().fg(theme.muted).bg(card_bg),
        ),
        Span::styled(
            format!("{} ", status_icon),
            Style::default()
                .fg(accent_color)
                .bg(card_bg)
                .add_modifier(if is_running {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            card.name.clone(),
            Style::default()
                .fg(theme.fg)
                .bg(card_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", header_extra),
            Style::default().fg(theme.muted).bg(card_bg),
        ),
        Span::styled(" ".repeat(header_padding), Style::default().bg(card_bg)),
    ]));

    // Expanded content (smart, no tabs)
    // Calculate visible lines based on available terminal height
    // Use most of the screen when expanded (leave room for header, footer, scrollbar hint)
    let visible_lines = if available_height > 10 {
        // Use ~60% of available height for expanded tool, capped reasonably
        (available_height * 60 / 100).clamp(TOOL_OUTPUT_MIN_VISIBLE_LINES, 50)
    } else {
        TOOL_OUTPUT_DEFAULT_VISIBLE_LINES
    };

    if card.expanded {
        render_tool_smart_content(
            card,
            lines,
            card_w,
            theme,
            accent_color,
            card_bg,
            display_mode,
            is_running,
            visible_lines,
        );

        // Bottom accent bar with full background
        lines.push(Line::from(vec![
            Span::styled("┗", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(
                "━".repeat(card_w.saturating_sub(1)),
                Style::default()
                    .fg(accent_color)
                    .bg(card_bg)
                    .add_modifier(Modifier::DIM),
            ),
        ]));
    }
}

/// Build the extra info shown in the header (path, command, summary).
fn build_tool_header_extra(card: &ToolCard, mode: ToolDisplayMode, max_len: usize) -> String {
    if let Ok(args) = serde_json::from_str::<serde_json::Value>(&card.args) {
        match mode {
            ToolDisplayMode::Diff | ToolDisplayMode::Summary => {
                // Show path
                if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                    let display = truncate(path, max_len.saturating_sub(10));
                    if mode == ToolDisplayMode::Summary {
                        // For read, add line count from output
                        let line_count =
                            card.output.as_ref().map(|o| o.lines().count()).unwrap_or(0);
                        if line_count > 0 {
                            return format!("{}  ({} lines)", display, line_count);
                        }
                    }
                    return display;
                }
            }
            ToolDisplayMode::Streaming | ToolDisplayMode::Output => {
                // Show command for bash, or key arg for others
                if let Some(cmd) = args
                    .get("cmd")
                    .or_else(|| args.get("command"))
                    .and_then(|v| v.as_str())
                {
                    return truncate(cmd, max_len);
                }
                // Fall back to first string arg
                for (_k, v) in args.as_object().into_iter().flatten() {
                    if let Some(s) = v.as_str() {
                        return truncate(s, max_len);
                    }
                }
            }
        }
    }
    String::new()
}

/// Render tool content based on display mode (no tabs).
fn render_tool_smart_content(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    card_w: usize,
    theme: &Theme,
    accent_color: Color,
    card_bg: Color,
    mode: ToolDisplayMode,
    is_running: bool,
    visible_lines: usize,
) {
    match mode {
        ToolDisplayMode::Diff => {
            // Show diff/progress lines with syntax highlighting
            render_tool_diff_content(
                card,
                lines,
                card_w,
                theme,
                accent_color,
                card_bg,
                visible_lines,
            );
        }
        ToolDisplayMode::Summary => {
            // Read tool: just show a brief summary, content visible in header
            render_tool_summary_content(
                card,
                lines,
                card_w,
                theme,
                accent_color,
                card_bg,
                visible_lines,
            );
        }
        ToolDisplayMode::Streaming => {
            // Bash: show progress while running, output when done
            if is_running && !card.progress_lines.is_empty() {
                render_tool_streaming_content(
                    card,
                    lines,
                    card_w,
                    theme,
                    accent_color,
                    card_bg,
                    visible_lines,
                );
            } else {
                render_tool_output_content(
                    card,
                    lines,
                    card_w,
                    theme,
                    accent_color,
                    card_bg,
                    visible_lines,
                );
            }
        }
        ToolDisplayMode::Output => {
            // MCP tools and others: show output
            render_tool_output_content(
                card,
                lines,
                card_w,
                theme,
                accent_color,
                card_bg,
                visible_lines,
            );
        }
    }
}

/// Render diff content (for edit/write tools).
/// If progress_lines is empty, reconstruct diff from args (old_string/new_string).
fn render_tool_diff_content(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    card_w: usize,
    theme: &Theme,
    accent_color: Color,
    card_bg: Color,
    visible_lines: usize,
) {
    let content_w = card_w.saturating_sub(8);
    let lang = get_lang_from_args(&card.args);
    let lang_ref = lang.as_deref();

    // Get diff lines: from progress_lines if available, otherwise reconstruct from args
    let diff_lines: Vec<String> = if !card.progress_lines.is_empty() {
        card.progress_lines.clone()
    } else {
        // Reconstruct diff from edit args (old_string, new_string)
        reconstruct_diff_from_args(&card.args)
    };

    if diff_lines.is_empty() {
        // Show output as fallback if available
        if let Some(output) = &card.output {
            if !output.is_empty() {
                let display = truncate(output.lines().next().unwrap_or(""), content_w);
                let padding = card_w.saturating_sub(display.chars().count() + 3);
                lines.push(Line::from(vec![
                    Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
                    Span::styled(" ", Style::default().bg(card_bg)),
                    Span::styled(display, Style::default().fg(theme.success).bg(card_bg)),
                    Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
                ]));
                return;
            }
        }
        render_empty_line(lines, card_w, accent_color, card_bg, theme, "no changes");
        return;
    }

    let total = diff_lines.len();
    let start = card.scroll_offset;
    let end = (start + visible_lines).min(total);

    for (i, line) in diff_lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let display = truncate(line, content_w);
        let (highlighted_spans, line_bg) = highlight_diff_line(&display, lang_ref, theme, card_bg);

        let mut spans = vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(
                format!("{:>3} ", line_num),
                Style::default().fg(theme.muted).bg(card_bg),
            ),
        ];

        // Calculate content length from highlighted spans
        let mut content_len = 0;
        for span in &highlighted_spans {
            content_len += span.content.chars().count();
        }
        spans.extend(highlighted_spans);

        // Padding: card_w - (border "┃" = 1) - (line num "NNN " = 4) - content
        let used = 1 + 4 + content_len;
        let padding = card_w.saturating_sub(used);
        spans.push(Span::styled(
            " ".repeat(padding),
            Style::default().bg(line_bg),
        ));

        lines.push(Line::from(spans));
    }

    render_scroll_hint(
        lines,
        card_w,
        accent_color,
        card_bg,
        theme,
        start,
        end,
        total,
        visible_lines,
    );
}

/// Reconstruct a diff from edit/write tool args.
/// For edit: uses old_string/new_string
/// For write: uses content (all lines shown as added)
fn reconstruct_diff_from_args(args: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return Vec::new();
    };

    let old_string = v.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
    let new_string = v.get("new_string").and_then(|v| v.as_str()).unwrap_or("");

    // Handle edit tool (old_string -> new_string)
    if !old_string.is_empty() || !new_string.is_empty() {
        let mut diff_lines = Vec::new();

        // Add removed lines (old_string)
        for line in old_string.lines() {
            diff_lines.push(format!("-{}", line));
        }

        // Add added lines (new_string)
        for line in new_string.lines() {
            diff_lines.push(format!("+{}", line));
        }

        return diff_lines;
    }

    // Handle write tool (content = new file)
    if let Some(content) = v.get("content").and_then(|v| v.as_str()) {
        if !content.is_empty() {
            return content.lines().map(|l| format!("+{}", l)).collect();
        }
    }

    Vec::new()
}

/// Render streaming content (bash while running).
fn render_tool_streaming_content(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    card_w: usize,
    theme: &Theme,
    accent_color: Color,
    card_bg: Color,
    visible_lines: usize,
) {
    let content_w = card_w.saturating_sub(6);
    let total = card.progress_lines.len();
    let start = card.scroll_offset;
    let end = (start + visible_lines).min(total);

    for line in card.progress_lines[start..end].iter() {
        let display = truncate(line, content_w);
        let padding = card_w.saturating_sub(display.chars().count() + 3);

        lines.push(Line::from(vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(" ", Style::default().bg(card_bg)),
            Span::styled(display, Style::default().fg(theme.fg).bg(card_bg)),
            Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
        ]));
    }

    render_scroll_hint(
        lines,
        card_w,
        accent_color,
        card_bg,
        theme,
        start,
        end,
        total,
        visible_lines,
    );
}

/// Render output content (bash result, MCP tools).
fn render_tool_output_content(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    card_w: usize,
    theme: &Theme,
    accent_color: Color,
    card_bg: Color,
    visible_lines: usize,
) {
    let content_w = card_w.saturating_sub(6);

    let output = match &card.output {
        Some(o) if !o.is_empty() => o,
        _ => {
            let msg = if card.status.starts_with("running") {
                "running..."
            } else {
                "no output"
            };
            render_empty_line(lines, card_w, accent_color, card_bg, theme, msg);
            return;
        }
    };

    let output_lines: Vec<&str> = output.lines().collect();
    let total = output_lines.len();
    let start = card.scroll_offset.min(total);
    let end = (start + visible_lines).min(total);

    let is_error = card.status == "error";
    let base_color = if is_error { theme.error } else { theme.fg };

    for line in output_lines[start..end].iter() {
        let display = truncate(line, content_w);
        let padding = card_w.saturating_sub(display.chars().count() + 3);

        lines.push(Line::from(vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(" ", Style::default().bg(card_bg)),
            Span::styled(display, Style::default().fg(base_color).bg(card_bg)),
            Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
        ]));
    }

    render_scroll_hint(
        lines,
        card_w,
        accent_color,
        card_bg,
        theme,
        start,
        end,
        total,
        visible_lines,
    );
}

/// Render summary content (for read tool - minimal since content shown in header).
fn render_tool_summary_content(
    card: &ToolCard,
    lines: &mut Vec<Line>,
    card_w: usize,
    theme: &Theme,
    accent_color: Color,
    card_bg: Color,
    visible_lines: usize,
) {
    // For read, show lines based on available space
    let content_w = card_w.saturating_sub(6);

    let output = match &card.output {
        Some(o) if !o.is_empty() => o,
        _ => {
            render_empty_line(lines, card_w, accent_color, card_bg, theme, "no content");
            return;
        }
    };

    // Get language for syntax highlighting
    let lang = get_lang_from_args(&card.args);
    let lang_ref = lang.as_deref();

    let output_lines: Vec<&str> = output.lines().collect();
    let total = output_lines.len();
    let start = card.scroll_offset.min(total);
    let end = (start + visible_lines).min(total);

    for (i, line) in output_lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let display = truncate(line, content_w.saturating_sub(4));

        // Syntax highlight the content
        let highlighted = syntax::highlight_code_inline(&display, lang_ref, theme);

        let mut spans = vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(
                format!("{:>3} ", line_num),
                Style::default().fg(theme.muted).bg(card_bg),
            ),
        ];

        let mut content_len = 0;
        for span in highlighted {
            content_len += span.content.chars().count();
            spans.push(Span::styled(
                span.content.into_owned(),
                span.style.bg(card_bg),
            ));
        }

        // Padding: card_w - (border "┃" = 1) - (line num "NNN " = 4) - content
        let used = 1 + 4 + content_len;
        let padding = card_w.saturating_sub(used);
        spans.push(Span::styled(
            " ".repeat(padding),
            Style::default().bg(card_bg),
        ));

        lines.push(Line::from(spans));
    }

    if total > visible_lines {
        let remaining = total.saturating_sub(end);
        let hint = format!("... {} more lines (scroll to see)", remaining);
        let padding = card_w.saturating_sub(hint.len() + 3);
        lines.push(Line::from(vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(" ", Style::default().bg(card_bg)),
            Span::styled(
                hint,
                Style::default()
                    .fg(theme.muted)
                    .bg(card_bg)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
        ]));
    }
}

/// Render an empty/placeholder line.
fn render_empty_line(
    lines: &mut Vec<Line>,
    card_w: usize,
    accent_color: Color,
    card_bg: Color,
    theme: &Theme,
    msg: &str,
) {
    let padding = card_w.saturating_sub(msg.len() + 3);
    lines.push(Line::from(vec![
        Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
        Span::styled(" ", Style::default().bg(card_bg)),
        Span::styled(
            msg.to_string(),
            Style::default()
                .fg(theme.muted)
                .bg(card_bg)
                .add_modifier(Modifier::ITALIC),
        ),
        Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
    ]));
}

/// Render scroll hint if content is scrollable.
fn render_scroll_hint(
    lines: &mut Vec<Line>,
    card_w: usize,
    accent_color: Color,
    card_bg: Color,
    theme: &Theme,
    _start: usize,
    end: usize,
    total: usize,
    visible_lines: usize,
) {
    if total > visible_lines {
        let hint = format!("↕ {}/{}", end, total);
        let padding = card_w.saturating_sub(hint.len() + 3);
        lines.push(Line::from(vec![
            Span::styled("┃", Style::default().fg(accent_color).bg(card_bg)),
            Span::styled(" ", Style::default().bg(card_bg)),
            Span::styled(hint, Style::default().fg(theme.muted).bg(card_bg)),
            Span::styled(" ".repeat(padding), Style::default().bg(card_bg)),
        ]));
    }
}

/// Get the file extension from tool args (for syntax highlighting).
fn get_lang_from_args(args: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
            return std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Highlight a diff line with syntax highlighting.
/// Returns spans with proper colors for +/- lines and syntax-highlighted content.
fn highlight_diff_line(
    line: &str,
    lang: Option<&str>,
    theme: &Theme,
    card_bg: Color,
) -> (Vec<Span<'static>>, Color) {
    // Determine line type and colors
    let (prefix, code_content, fg_color, bg_color) =
        if line.starts_with('+') && !line.starts_with("+++") {
            // Added line
            let content = line.strip_prefix('+').unwrap_or(line);
            ("+", content, Color::Rgb(80, 250, 123), Color::Rgb(0, 45, 0))
        } else if line.starts_with('-') && !line.starts_with("---") {
            // Removed line
            let content = line.strip_prefix('-').unwrap_or(line);
            ("-", content, Color::Rgb(255, 85, 85), Color::Rgb(55, 0, 0))
        } else if line.starts_with("@@") {
            // Hunk header - no highlighting
            return (
                vec![Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.primary).bg(card_bg),
                )],
                card_bg,
            );
        } else if line.starts_with("+++") || line.starts_with("---") {
            // File header - no highlighting
            return (
                vec![Span::styled(
                    line.to_string(),
                    Style::default()
                        .fg(theme.primary)
                        .bg(card_bg)
                        .add_modifier(Modifier::BOLD),
                )],
                card_bg,
            );
        } else {
            // Context line
            (
                " ",
                line.strip_prefix(' ').unwrap_or(line),
                theme.fg,
                card_bg,
            )
        };

    // Try to syntax highlight the code content
    let highlighted = syntax::highlight_code_inline(code_content, lang, theme);

    let mut spans = Vec::with_capacity(highlighted.len() + 1);

    // Add prefix with diff color
    spans.push(Span::styled(
        prefix.to_string(),
        Style::default().fg(fg_color).bg(bg_color),
    ));

    // Add highlighted content, preserving syntax colors but with diff background
    for span in highlighted {
        let style = span.style.bg(bg_color);
        spans.push(Span::styled(span.content.into_owned(), style));
    }

    (spans, bg_color)
}

// ── Approval nice display ────────────
// NOTE: The TUI does NOT decide what needs approval — that's the policy plugin's job.
// This function only generates a human-readable description for the overlay.
fn approval_reason(tool: &str, args: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    match tool {
        "bash" => {
            let cmd = val.get("cmd").and_then(|v| v.as_str()).unwrap_or(args);
            // Just show the command, don't judge it
            Some(format!("Command: `{}`", truncate_cmd(cmd, 60)))
        }
        "write" | "edit" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() {
                Some(format!("Writes to `{}`", path))
            } else {
                Some("Writes to file".into())
            }
        }
        _ => None,
    }
}

fn truncate_cmd(cmd: &str, max: usize) -> String {
    if cmd.len() <= max {
        cmd.to_string()
    } else {
        format!("{}...", &cmd[..max])
    }
}
fn bash_highlight_spans(cmd: &str, theme: &Theme) -> Vec<Span<'static>> {
    let always_ask = [
        "rm",
        "mv",
        "cp",
        "chmod",
        "chown",
        "kill",
        "dd",
        "curl",
        "wget",
        "ssh",
        "scp",
        "sh",
        "bash",
        "zsh",
        "python",
        "python3",
        "node",
        "perl",
        "ruby",
        "eval",
        "pwsh",
        "powershell",
        "iex",
        "sudo",
        "mkfs",
        "fdisk",
        "reboot",
        "shutdown",
    ];
    let mut spans = Vec::new();
    let mut current = String::new();
    for ch in cmd.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                let lower = current.to_lowercase();
                let is_dangerous = always_ask.iter().any(|w| lower == *w);
                let style = if is_dangerous {
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                };
                spans.push(Span::styled(current.clone(), style));
                current.clear();
            }
            spans.push(Span::raw(ch.to_string()));
        } else if ch == '>'
            || ch == '<'
            || ch == '|'
            || ch == ';'
            || ch == '&'
            || ch == '$'
            || ch == '`'
        {
            if !current.is_empty() {
                let lower = current.to_lowercase();
                let is_dangerous = always_ask.iter().any(|w| lower == *w);
                let style = if is_dangerous {
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                };
                spans.push(Span::styled(current.clone(), style));
                current.clear();
            }
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        let lower = current.to_lowercase();
        let is_dangerous = always_ask.iter().any(|w| lower == *w);
        let style = if is_dangerous {
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        spans.push(Span::styled(current, style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(cmd.to_string(), Style::default().fg(theme.fg)));
    }
    spans
}
fn approval_body_lines(tool: &str, args: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let val: serde_json::Value =
        serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.to_string()));
    match tool {
        "bash" => {
            let cmd = val.get("cmd").and_then(|v| v.as_str()).unwrap_or(args);
            if let Some(reason) = approval_reason(tool, args) {
                for w in wrap_text(&format!("Reason: {}", reason), width) {
                    lines.push(Line::from(vec![Span::styled(
                        w,
                        Style::default()
                            .fg(theme.warning)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
            lines.push(Line::from(vec![Span::styled(
                "Command:",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )]));
            if cmd.len() <= width {
                lines.push(Line::from(bash_highlight_spans(cmd, theme)));
            } else {
                for chunk in wrap_text(cmd, width) {
                    lines.push(Line::from(bash_highlight_spans(&chunk, theme)));
                }
            }
            if let Some(t) = val.get("timeout_secs").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![Span::styled(
                    format!("Timeout: {}s", t),
                    Style::default().fg(theme.muted),
                )]));
            }
        }
        "read" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or(args);
            lines.push(Line::from(vec![
                Span::styled(
                    "Path: ",
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    path.to_string(),
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(
                    reason,
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::ITALIC),
                )]));
            }
            if let Some(off) = val.get("offset").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![Span::styled(
                    format!("Offset: {}", off),
                    Style::default().fg(theme.muted),
                )]));
            }
        }
        "write" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(
                    "Path: ",
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    path.to_string(),
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(
                    reason,
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::ITALIC),
                )]));
            }
            let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let preview: String = content.lines().take(3).collect::<Vec<_>>().join(" ⏎ ");
            let preview = if preview.len() > width * 2 {
                format!("{}…", &preview[..width * 2 - 1])
            } else {
                preview
            };
            lines.push(Line::from(vec![
                Span::styled(
                    "Content: ",
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(&preview, width.saturating_sub(9)),
                    Style::default().fg(theme.fg),
                ),
            ]));
        }
        "edit" => {
            let path = val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            lines.push(Line::from(vec![
                Span::styled(
                    "Path: ",
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    path.to_string(),
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(
                    reason,
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::ITALIC),
                )]));
            }
            let old = val.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
            let new = val.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            let old_preview = old.lines().next().unwrap_or("").trim();
            let new_preview = new.lines().next().unwrap_or("").trim();
            lines.push(Line::from(vec![
                Span::styled(
                    "Old: ",
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(old_preview, width.saturating_sub(5)),
                    Style::default().fg(theme.error),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    "New: ",
                    Style::default()
                        .fg(theme.success)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(new_preview, width.saturating_sub(5)),
                    Style::default().fg(theme.success),
                ),
            ]));
        }
        _ => {
            let pretty = match &val {
                serde_json::Value::Object(map) => {
                    let mut parts = Vec::new();
                    for (k, v) in map {
                        let v_str = match v {
                            serde_json::Value::String(s) => format!("\"{}\"", truncate(s, 40)),
                            _ => truncate(&v.to_string(), 40),
                        };
                        parts.push(format!("{}: {}", k, v_str));
                    }
                    parts.join(", ")
                }
                _ => truncate(&val.to_string(), width * 2),
            };
            for w in wrap_text(&pretty, width) {
                lines.push(Line::from(Span::styled(
                    w,
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(reason) = approval_reason(tool, args) {
                lines.push(Line::from(vec![Span::styled(
                    reason,
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::ITALIC),
                )]));
            }
        }
    }
    lines
}

/// Compute dynamic height for interaction overlay based on content.
fn compute_interaction_height(state: &InteractionState, inner_w: usize, max_h: u16) -> u16 {
    let prompt_len = 2; // "› "
    let input_w = inner_w.saturating_sub(prompt_len);

    let content_lines: usize = match state {
        InteractionState::Text {
            question,
            header,
            input,
            ..
        } => {
            let header_lines = if header.is_some() { 1 } else { 0 };
            let question_lines = wrap_text(question, inner_w.max(1)).len();
            let input_lines = if input_w > 0 {
                input.chars().count().max(1).div_ceil(input_w).max(1)
            } else {
                1
            };
            // title + header + question + gap + input lines + footer
            1 + header_lines + question_lines + 1 + input_lines + 2
        }
        InteractionState::Choice {
            question,
            header,
            options,
            allow_custom,
            in_custom_mode,
            ..
        } => {
            let header_lines = if header.is_some() { 1 } else { 0 };
            let question_lines = wrap_text(question, inner_w.max(1)).len();
            let mut opt_lines = 0;
            for opt in options {
                opt_lines += 1; // label
                if opt.description.is_some() {
                    opt_lines += 1;
                } // desc
            }
            if *allow_custom {
                opt_lines += 1;
            } // "Other..."
            if *in_custom_mode {
                opt_lines += 1;
            } // custom input line
            1 + header_lines + question_lines + 1 + opt_lines + 2
        }
        InteractionState::Multi {
            question,
            header,
            options,
            ..
        } => {
            let header_lines = if header.is_some() { 1 } else { 0 };
            let question_lines = wrap_text(question, inner_w.max(1)).len();
            let mut opt_lines = 0;
            for opt in options {
                opt_lines += 1;
                if opt.description.is_some() {
                    opt_lines += 1;
                }
            }
            1 + header_lines + question_lines + 1 + opt_lines + 2
        }
        InteractionState::Confirm {
            question, header, ..
        } => {
            let header_lines = if header.is_some() { 1 } else { 0 };
            let question_lines = wrap_text(question, inner_w.max(1)).len();
            // title + header + question + gap + buttons + footer
            1 + header_lines + question_lines + 3 + 2
        }
        InteractionState::Generic { payload, input, .. } => {
            let payload_lines = wrap_text(payload, inner_w.max(1)).len();
            let input_lines = if input_w > 0 {
                input.chars().count().max(1).div_ceil(input_w).max(1)
            } else {
                1
            };
            1 + payload_lines + 1 + input_lines + 2
        }
    };

    // Add border (2 lines) + some padding
    let total = (content_lines + 4) as u16;
    total.min(max_h).max(8)
}

/// Render an interaction overlay based on its state type.
fn render_interaction_overlay(
    buf: &mut ratatui::buffer::Buffer,
    theme: &Theme,
    overlay_x: u16,
    overlay_y: u16,
    overlay_w: u16,
    overlay_h: u16,
    plugin: &str,
    state: &InteractionState,
) {
    let border_fg = theme.primary;
    let border_style = Style::default()
        .fg(border_fg)
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD);

    // Draw border
    if overlay_w >= 2 && overlay_h >= 2 {
        buf[(overlay_x, overlay_y)]
            .set_char('┏')
            .set_style(border_style);
        buf[(overlay_x + overlay_w - 1, overlay_y)]
            .set_char('┓')
            .set_style(border_style);
        buf[(overlay_x, overlay_y + overlay_h - 1)]
            .set_char('┗')
            .set_style(border_style);
        buf[(overlay_x + overlay_w - 1, overlay_y + overlay_h - 1)]
            .set_char('┛')
            .set_style(border_style);
        for x in (overlay_x + 1)..(overlay_x + overlay_w - 1) {
            buf[(x, overlay_y)].set_char('━').set_style(border_style);
            buf[(x, overlay_y + overlay_h - 1)]
                .set_char('━')
                .set_style(border_style);
        }
        for y in (overlay_y + 1)..(overlay_y + overlay_h - 1) {
            buf[(overlay_x, y)].set_char('┃').set_style(border_style);
            buf[(overlay_x + overlay_w - 1, y)]
                .set_char('┃')
                .set_style(border_style);
        }
    }

    let inner_w = (overlay_w.saturating_sub(4)) as usize;
    let inner_x = overlay_x + 2;
    let mut y = overlay_y + 1;

    // Title
    let title = format!("{} asks:", plugin);
    let title_x = overlay_x + (overlay_w.saturating_sub(title.len() as u16)) / 2;
    for (i, ch) in title.chars().enumerate() {
        buf[(title_x + i as u16, y)]
            .set_char(ch)
            .set_fg(theme.primary)
            .set_bg(Color::Black);
    }
    y += 2;

    match state {
        InteractionState::Text {
            question,
            header,
            placeholder,
            input,
        } => {
            // Optional header
            if let Some(h) = header {
                for (i, ch) in h.chars().take(inner_w).enumerate() {
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            // Question
            for line in wrap_text(question, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 3 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 {
                        break;
                    }
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            y += 1;
            // Input lines (wrapped)
            let prompt = "› ";
            let input_w = inner_w.saturating_sub(prompt.len());
            let display = if input.is_empty() {
                placeholder.as_deref().unwrap_or("")
            } else {
                input
            };
            let input_fg = if input.is_empty() {
                theme.muted
            } else {
                theme.fg
            };
            let input_lines = wrap_text(display, input_w.max(1));
            let cursor_pos = input.chars().count();
            let cursor_line = cursor_pos / input_w.max(1);
            let cursor_col = cursor_pos % input_w.max(1);

            for (line_idx, line) in input_lines.iter().enumerate() {
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                // Draw prompt on first line only
                if line_idx == 0 {
                    for (i, ch) in prompt.chars().enumerate() {
                        buf[(inner_x + i as u16, y)]
                            .set_char(ch)
                            .set_fg(theme.muted)
                            .set_bg(Color::Black);
                    }
                } else {
                    // Indent continuation lines
                    for i in 0..prompt.len() {
                        buf[(inner_x + i as u16, y)]
                            .set_char(' ')
                            .set_bg(Color::Black);
                    }
                }
                let input_x = inner_x + prompt.len() as u16;
                for (i, ch) in line.chars().enumerate() {
                    if input_x + i as u16 >= overlay_x + overlay_w - 2 {
                        break;
                    }
                    buf[(input_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(input_fg)
                        .set_bg(Color::Black);
                }
                // Draw cursor on the right line
                if line_idx == cursor_line && !input.is_empty() {
                    let cx = input_x + cursor_col as u16;
                    if cx < overlay_x + overlay_w - 1 {
                        buf[(cx, y)]
                            .set_char('▏')
                            .set_fg(theme.primary)
                            .set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            // Cursor on empty input
            if input.is_empty() {
                let cx = inner_x + prompt.len() as u16;
                if cx < overlay_x + overlay_w - 1 && y > overlay_y + 1 {
                    buf[(cx, y - 1)]
                        .set_char('▏')
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
            }
            // Footer
            let footer = "Enter send · Esc cancel";
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }

        InteractionState::Choice {
            question,
            header,
            options,
            selected,
            allow_custom,
            custom_input,
            in_custom_mode,
        } => {
            if let Some(h) = header {
                for (i, ch) in h.chars().take(inner_w).enumerate() {
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            for line in wrap_text(question, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 4 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 {
                        break;
                    }
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            y += 1;
            // Options list
            for (i, opt) in options.iter().enumerate() {
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                let is_sel = i == *selected && !*in_custom_mode;
                let marker = if is_sel { "● " } else { "○ " };
                let style = if is_sel {
                    Style::default().fg(Color::Black).bg(theme.primary)
                } else {
                    Style::default().fg(theme.fg).bg(Color::Black)
                };
                let line_text = format!("{}{}", marker, opt.label);
                for (j, ch) in line_text.chars().take(inner_w).enumerate() {
                    buf[(inner_x + j as u16, y)].set_char(ch).set_style(style);
                }
                y += 1;
                // Description on next line, indented (single line, truncated if needed)
                if let Some(desc) = &opt.description {
                    if y < overlay_y + overlay_h - 2 {
                        let indent = "    ";
                        let desc_w = inner_w.saturating_sub(indent.len());
                        for (j, ch) in indent.chars().enumerate() {
                            buf[(inner_x + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.muted)
                                .set_bg(Color::Black);
                        }
                        for (j, ch) in desc.chars().take(desc_w).enumerate() {
                            buf[(inner_x + indent.len() as u16 + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.muted)
                                .set_bg(Color::Black);
                        }
                        y += 1;
                    }
                }
            }
            // "Other..." option if allow_custom
            if *allow_custom
                && y < overlay_y + overlay_h - 2 {
                    let is_sel = *selected == options.len() || *in_custom_mode;
                    let marker = if is_sel && !*in_custom_mode {
                        "● "
                    } else {
                        "○ "
                    };
                    let style = if is_sel && !*in_custom_mode {
                        Style::default().fg(Color::Black).bg(theme.primary)
                    } else {
                        Style::default().fg(theme.fg).bg(Color::Black)
                    };
                    let line_text = format!("{}Other...", marker);
                    for (j, ch) in line_text.chars().take(inner_w).enumerate() {
                        buf[(inner_x + j as u16, y)].set_char(ch).set_style(style);
                    }
                    y += 1;
                    // Custom input if in custom mode
                    if *in_custom_mode && y < overlay_y + overlay_h - 2 {
                        let prompt = "  › ";
                        for (j, ch) in prompt.chars().enumerate() {
                            buf[(inner_x + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.muted)
                                .set_bg(Color::Black);
                        }
                        let input_x = inner_x + prompt.len() as u16;
                        for (j, ch) in custom_input.chars().enumerate() {
                            if input_x + j as u16 >= overlay_x + overlay_w - 2 {
                                break;
                            }
                            buf[(input_x + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.fg)
                                .set_bg(Color::Black);
                        }
                        let cx = input_x + custom_input.chars().count() as u16;
                        if cx < overlay_x + overlay_w - 1 {
                            buf[(cx, y)]
                                .set_char('▏')
                                .set_fg(theme.primary)
                                .set_bg(Color::Black);
                        }
                    }
                }
            // Footer
            let footer = if *in_custom_mode {
                "Enter send · Esc back"
            } else {
                "↑↓ select · Enter confirm · Esc cancel"
            };
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }

        InteractionState::Multi {
            question,
            header,
            options,
            cursor,
            selected,
        } => {
            if let Some(h) = header {
                for (i, ch) in h.chars().take(inner_w).enumerate() {
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            for line in wrap_text(question, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 4 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 {
                        break;
                    }
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            y += 1;
            // Options with checkboxes
            for (i, opt) in options.iter().enumerate() {
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                let is_cursor = i == *cursor;
                let is_checked = selected.get(i).copied().unwrap_or(false);
                let checkbox = if is_checked { "☑ " } else { "☐ " };
                let style = if is_cursor {
                    Style::default().fg(Color::Black).bg(theme.primary)
                } else {
                    Style::default().fg(theme.fg).bg(Color::Black)
                };
                let line_text = format!("{}{}", checkbox, opt.label);
                for (j, ch) in line_text.chars().take(inner_w).enumerate() {
                    buf[(inner_x + j as u16, y)].set_char(ch).set_style(style);
                }
                y += 1;
                // Description on next line, indented (single line, truncated if needed)
                if let Some(desc) = &opt.description {
                    if y < overlay_y + overlay_h - 2 {
                        let indent = "    ";
                        let desc_w = inner_w.saturating_sub(indent.len());
                        for (j, ch) in indent.chars().enumerate() {
                            buf[(inner_x + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.muted)
                                .set_bg(Color::Black);
                        }
                        for (j, ch) in desc.chars().take(desc_w).enumerate() {
                            buf[(inner_x + indent.len() as u16 + j as u16, y)]
                                .set_char(ch)
                                .set_fg(theme.muted)
                                .set_bg(Color::Black);
                        }
                        y += 1;
                    }
                }
            }
            let footer = "↑↓ move · Space toggle · Enter confirm · Esc cancel";
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }

        InteractionState::Confirm {
            question,
            header,
            selected,
        } => {
            if let Some(h) = header {
                for (i, ch) in h.chars().take(inner_w).enumerate() {
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            for line in wrap_text(question, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 4 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 {
                        break;
                    }
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            y += 2;
            // Yes / No buttons
            let btn_y = y;
            let yes_style = if *selected {
                Style::default().fg(Color::Black).bg(theme.success)
            } else {
                Style::default().fg(theme.fg).bg(Color::DarkGray)
            };
            let no_style = if !*selected {
                Style::default().fg(Color::Black).bg(theme.error)
            } else {
                Style::default().fg(theme.fg).bg(Color::DarkGray)
            };
            let yes_label = "  Yes  ";
            let no_label = "  No  ";
            let total_w = yes_label.len() + 4 + no_label.len();
            let btn_x = inner_x + ((inner_w.saturating_sub(total_w)) / 2) as u16;
            for (j, ch) in yes_label.chars().enumerate() {
                buf[(btn_x + j as u16, btn_y)]
                    .set_char(ch)
                    .set_style(yes_style);
            }
            let no_x = btn_x + yes_label.len() as u16 + 4;
            for (j, ch) in no_label.chars().enumerate() {
                buf[(no_x + j as u16, btn_y)]
                    .set_char(ch)
                    .set_style(no_style);
            }
            let footer = "←→ select · Enter confirm · Esc cancel";
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }

        InteractionState::Generic { payload, input } => {
            // Fallback: show raw payload and text input
            for line in wrap_text(payload, inner_w.max(1)) {
                if y >= overlay_y + overlay_h - 3 {
                    break;
                }
                for (i, ch) in line.chars().enumerate() {
                    if inner_x + i as u16 >= overlay_x + overlay_w - 1 {
                        break;
                    }
                    buf[(inner_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                y += 1;
            }
            y += 1;
            // Input lines (wrapped)
            let prompt = "› ";
            let input_w = inner_w.saturating_sub(prompt.len());
            let input_lines = wrap_text(input, input_w.max(1));
            let cursor_pos = input.chars().count();
            let cursor_line = cursor_pos / input_w.max(1);
            let cursor_col = cursor_pos % input_w.max(1);

            for (line_idx, line) in input_lines.iter().enumerate() {
                if y >= overlay_y + overlay_h - 2 {
                    break;
                }
                if line_idx == 0 {
                    for (i, ch) in prompt.chars().enumerate() {
                        buf[(inner_x + i as u16, y)]
                            .set_char(ch)
                            .set_fg(theme.muted)
                            .set_bg(Color::Black);
                    }
                } else {
                    for i in 0..prompt.len() {
                        buf[(inner_x + i as u16, y)]
                            .set_char(' ')
                            .set_bg(Color::Black);
                    }
                }
                let input_x = inner_x + prompt.len() as u16;
                for (i, ch) in line.chars().enumerate() {
                    if input_x + i as u16 >= overlay_x + overlay_w - 2 {
                        break;
                    }
                    buf[(input_x + i as u16, y)]
                        .set_char(ch)
                        .set_fg(theme.fg)
                        .set_bg(Color::Black);
                }
                if line_idx == cursor_line && !input.is_empty() {
                    let cx = input_x + cursor_col as u16;
                    if cx < overlay_x + overlay_w - 1 {
                        buf[(cx, y)]
                            .set_char('▏')
                            .set_fg(theme.primary)
                            .set_bg(Color::Black);
                    }
                }
                y += 1;
            }
            if input.is_empty() {
                let cx = inner_x + prompt.len() as u16;
                if cx < overlay_x + overlay_w - 1 && y > overlay_y + 1 {
                    buf[(cx, y - 1)]
                        .set_char('▏')
                        .set_fg(theme.primary)
                        .set_bg(Color::Black);
                }
            }
            let footer = "Enter send · Esc cancel";
            let fx = overlay_x + (overlay_w.saturating_sub(footer.len() as u16)) / 2;
            let fy = overlay_y + overlay_h - 1;
            for (i, ch) in footer.chars().enumerate() {
                buf[(fx + i as u16, fy)]
                    .set_char(ch)
                    .set_fg(theme.muted)
                    .set_bg(Color::Black);
            }
        }
    }
}

#[cfg(test)]
mod golden {
    use crate::app::Overlay;
    use crate::theme::Theme;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> Theme {
        Theme::dark()
    }

    fn render_overlay_to_string(overlay: &Overlay, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let th = theme();
        terminal
            .draw(|f| {
                let area = f.area();
                super::render_overlay(f, overlay, area, &th);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x, y)].symbol());
            }
            if y + 1 < height {
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn golden_approval_overlay_contains_tool_and_actions() {
        let overlay = Overlay::Approval {
            tool: "bash".into(),
            args: r#"{"cmd":"rm -rf /"}"#.into(),
            selected: 0,
        };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        // Buffer must contain the command snippet and the action hints; tool name is implicit via Command line
        assert!(
            snap.contains("rm -rf"),
            "approval overlay must show args preview, got:\n{snap}"
        );
        assert!(
            snap.contains("APPROVAL") || snap.contains("Allow"),
            "must show approval header or actions, got:\n{snap}"
        );
        assert!(
            snap.contains("Allow") && snap.contains("Deny"),
            "must show Allow/Deny actions, got:\n{snap}"
        );
    }

    #[test]
    fn golden_interaction_overlay_contains_plugin_and_payload() {
        use crate::app::InteractionState;
        let state = InteractionState::Choice {
            question: "choose?".into(),
            header: None,
            options: vec![
                crate::app::QuestionOption {
                    label: "Option A".into(),
                    value: "a".into(),
                    description: None,
                },
                crate::app::QuestionOption {
                    label: "Option B".into(),
                    value: "b".into(),
                    description: None,
                },
            ],
            selected: 0,
            allow_custom: false,
            custom_input: String::new(),
            in_custom_mode: false,
        };
        let overlay = Overlay::Interaction {
            id: 42,
            plugin: "kn9t-ask-user".into(),
            state,
        };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        assert!(
            snap.contains("kn9t-ask-user"),
            "must show plugin name, got:\n{snap}"
        );
        assert!(
            snap.contains("choose?"),
            "must show payload question, got:\n{snap}"
        );
        assert!(
            snap.contains("Option A"),
            "must show first option, got:\n{snap}"
        );
        assert!(
            snap.contains("Enter") && snap.contains("Esc"),
            "must show footer hints, got:\n{snap}"
        );
    }

    #[test]
    fn golden_interaction_overlay_esc_hint_present_even_empty_input() {
        use crate::app::InteractionState;
        let state = InteractionState::Text {
            question: "hello".into(),
            header: None,
            placeholder: None,
            input: String::new(),
        };
        let overlay = Overlay::Interaction {
            id: 1,
            plugin: "p".into(),
            state,
        };
        let snap = render_overlay_to_string(&overlay, 60, 15);
        // Must be renderable without panic and contain the payload
        assert!(
            snap.contains("hello"),
            "payload must be visible, got:\n{snap}"
        );
        assert!(
            snap.contains("Esc"),
            "cancel hint must be present, got:\n{snap}"
        );
    }

    #[test]
    fn golden_help_overlay_renders() {
        let overlay = Overlay::Help;
        let snap = render_overlay_to_string(&overlay, 60, 15);
        assert!(
            snap.contains("HELP") || snap.contains("Navigation") || snap.contains("Actions"),
            "help overlay must contain headings, got:\n{snap}"
        );
    }
}
