//! Custom widgets — 96E-25 first real content: placeholder renderers for plugin pages.
//!
//! Each `kind` from 96E-24 (`text|number|bar|list`) gets a minimal line renderer.
//! Used by `ui/render.rs` to draw the toggleable side panel (transcript stays primary).

use ratatui::style::{Style, Modifier};
use ratatui::text::{Line, Span};
use crate::theme::Theme;
use crate::page_state::{PlaceholderKind, Placeholder};

/// Render one placeholder as one or more lines (list may expand).
/// `label` is the placeholder id, `width` is the available content width.
pub fn render_placeholder(label: &str, ph: &Placeholder, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let label_style = Style::default().fg(theme.muted).add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(theme.fg);
    match &ph.kind {
        PlaceholderKind::Text => {
            let val = ph.value.as_str().unwrap_or("");
            // Truncate/wrap
            let text = format!("{}: {}", label, val);
            for chunk in crate::ui::render::wrap_text(&text, width) {
                lines.push(Line::from(vec![Span::styled(chunk, value_style)]));
            }
            if lines.is_empty() {
                lines.push(Line::from(vec![Span::styled(format!("{}:", label), label_style), Span::styled(" ", value_style)]));
            }
        }
        PlaceholderKind::Number => {
            let n = ph.value.as_f64().map(|v| format!("{}", v)).unwrap_or_else(|| ph.value.to_string());
            lines.push(Line::from(vec![
                Span::styled(format!("{}: ", label), label_style),
                Span::styled(n, Style::default().fg(theme.primary).add_modifier(Modifier::BOLD)),
            ]));
        }
        PlaceholderKind::Bar => {
            let pct = ph.value.as_f64().unwrap_or(0.0).clamp(0.0, 100.0);
            let bar_w = width.saturating_sub(label.len() + 4).max(6);
            let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
            let empty = bar_w.saturating_sub(filled);
            let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", label), label_style),
                Span::styled(bar, Style::default().fg(theme.success)),
                Span::styled(format!(" {}%", pct as i64), value_style),
            ]));
        }
        PlaceholderKind::List => {
            lines.push(Line::from(vec![Span::styled(format!("{}:", label), label_style)]));
            if let Some(arr) = ph.value.as_array() {
                if arr.is_empty() {
                    lines.push(Line::from(vec![Span::styled("  (empty)", Style::default().fg(theme.muted).add_modifier(Modifier::ITALIC))]));
                } else {
                    for item in arr {
                        let s = match item {
                            serde_json::Value::String(st) => st.clone(),
                            other => other.to_string(),
                        };
                        for chunk in crate::ui::render::wrap_text(&format!("• {}", s), width.saturating_sub(2)) {
                            lines.push(Line::from(vec![Span::styled(format!("  {}", chunk), value_style)]));
                        }
                    }
                }
            }
        }
    }
    lines
}

/// Tab label for page switcher — "plugin/page_id" truncated.
pub fn page_tab_label(plugin: &str, page_id: &str, width: usize) -> String {
    let raw = format!("{}/{}", plugin, page_id);
    crate::ui::render::truncate(&raw, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use serde_json::json;

    fn theme() -> Theme { Theme::dark() }

    #[test]
    fn render_text_placeholder() {
        let ph = Placeholder { kind: PlaceholderKind::Text, value: json!("hello world") };
        let lines = render_placeholder("status", &ph, 40, &theme());
        assert!(lines.iter().any(|l| format!("{:?}", l).contains("hello")));
    }

    #[test]
    fn render_bar_placeholder() {
        let ph = Placeholder { kind: PlaceholderKind::Bar, value: json!(50) };
        let lines = render_placeholder("prog", &ph, 40, &theme());
        let s = format!("{:?}", lines);
        assert!(s.contains("prog"));
        assert!(s.contains("50%"));
    }

    #[test]
    fn render_list_placeholder() {
        let ph = Placeholder { kind: PlaceholderKind::List, value: json!(["a","b"]) };
        let lines = render_placeholder("items", &ph, 40, &theme());
        let s = format!("{:?}", lines);
        assert!(s.contains("items"));
    }
}
