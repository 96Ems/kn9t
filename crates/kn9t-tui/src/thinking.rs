//! Reasoning card rendering.
//!
//! A reasoning block is its own card, not a slice of the answer. This replaced a parser
//! that looked for `<thinking>` tags in the message content — no producer ever wrote
//! those tags, so the machinery was inert and the reasoning rendered as ordinary prose
//! inline with the reply.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Header for a reasoning card.
///
/// A live card shows a spinner instead of a disclosure arrow and states no line count:
/// it is still growing, and the answer has not started, so it is the only thing to watch.
pub fn render_header(collapsed: bool, line_count: usize, live: bool, theme: &Theme) -> Line<'static> {
    let style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);

    let (marker, tail) = if live {
        ("✻", " …".to_string())
    } else if collapsed {
        ("▶", format!(" ({line_count} lines)"))
    } else {
        ("▼", String::new())
    };

    Line::from(vec![
        Span::styled(format!("{marker} "), style),
        Span::styled("thinking", style),
        Span::styled(tail, Style::default().fg(theme.muted)),
    ])
}

/// Body of a reasoning card: indented, muted, italic.
pub fn render_content(content: &str, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let text_style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::ITALIC);
    let border_style = Style::default().fg(theme.muted);

    let mut lines = Vec::new();
    for line in content.lines() {
        for wrapped in crate::ui::render::wrap_text(line, width.saturating_sub(4)) {
            lines.push(Line::from(vec![
                Span::styled("│ ", border_style),
                Span::styled(wrapped, text_style),
            ]));
        }
    }
    lines
}
