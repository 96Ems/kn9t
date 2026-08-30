//! Syntax highlighting for code blocks using syntect.
//!
//! Provides language-aware syntax highlighting for code in markdown.
//! Uses embedded syntaxes for common languages and a theme-aware color scheme.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::theme::Theme;

/// Global syntax set (loaded once).
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Global theme set (loaded once).
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

/// Get the global syntax set.
fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(|| SyntaxSet::load_defaults_newlines())
}

/// Get the global theme set.
fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Highlight code with the given language.
/// 
/// Returns a vector of styled Lines. If the language is not supported,
/// falls back to plain text rendering with the provided fallback style.
pub fn highlight_code(
    code: &str,
    language: Option<&str>,
    theme: &Theme,
    line_number_style: Style,
) -> Vec<Line<'static>> {
    let ss = syntax_set();
    let ts = theme_set();
    
    // Choose a syntax theme based on the TUI theme
    let syntax_theme_name = if is_dark_theme(theme) {
        "base16-ocean.dark"
    } else {
        "base16-ocean.light"
    };
    
    let syntax_theme = ts.themes.get(syntax_theme_name)
        .unwrap_or_else(|| ts.themes.values().next().unwrap());
    
    // Find syntax for language
    let syntax = language
        .and_then(|lang| ss.find_syntax_by_token(lang))
        .or_else(|| language.and_then(|lang| ss.find_syntax_by_extension(lang)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    
    let mut highlighter = HighlightLines::new(syntax, syntax_theme);
    let mut lines = Vec::new();
    
    for (i, line) in code.lines().enumerate() {
        let line_num = format!("{:3} │ ", i + 1);
        let mut spans = vec![Span::styled(line_num, line_number_style)];
        
        // Highlight this line
        match highlighter.highlight_line(line, ss) {
            Ok(highlighted) => {
                for (style, text) in highlighted {
                    spans.push(Span::styled(
                        text.to_string(),
                        syntect_to_ratatui_style(style),
                    ));
                }
            }
            Err(_) => {
                // Fallback: plain text
                spans.push(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.fg),
                ));
            }
        }
        
        lines.push(Line::from(spans));
    }
    
    lines
}

/// Check if the theme is dark (simple heuristic).
fn is_dark_theme(theme: &Theme) -> bool {
    // If background is darker than midpoint, it's a dark theme
    match theme.bg {
        Color::Rgb(r, g, b) => {
            let luminance = (r as u32 + g as u32 + b as u32) / 3;
            luminance < 128
        }
        Color::Black | Color::DarkGray => true,
        Color::White | Color::Gray | Color::LightRed | Color::LightGreen 
        | Color::LightYellow | Color::LightBlue | Color::LightMagenta 
        | Color::LightCyan => false,
        _ => true, // Default to dark
    }
}

/// Convert syntect Style to ratatui Style.
fn syntect_to_ratatui_style(style: highlighting::Style) -> Style {
    let fg = Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    );
    
    let mut ratatui_style = Style::default().fg(fg);
    
    // Apply font style
    if style.font_style.contains(highlighting::FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(ratatui::style::Modifier::BOLD);
    }
    if style.font_style.contains(highlighting::FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(ratatui::style::Modifier::ITALIC);
    }
    if style.font_style.contains(highlighting::FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(ratatui::style::Modifier::UNDERLINED);
    }
    
    ratatui_style
}

/// Get list of supported language extensions.
pub fn supported_languages() -> Vec<&'static str> {
    let ss = syntax_set();
    ss.syntaxes()
        .iter()
        .flat_map(|s| s.file_extensions.iter())
        .map(|s| s.as_str())
        .collect()
}

/// Check if a language is supported.
pub fn is_language_supported(lang: &str) -> bool {
    let ss = syntax_set();
    ss.find_syntax_by_token(lang).is_some() || ss.find_syntax_by_extension(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_rust() {
        let theme = Theme::default();
        let code = "fn main() {\n    println!(\"Hello\");\n}";
        let lines = highlight_code(code, Some("rust"), &theme, Style::default());
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_highlight_unknown_lang() {
        let theme = Theme::default();
        let code = "some text";
        let lines = highlight_code(code, Some("unknownlang123"), &theme, Style::default());
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_highlight_no_lang() {
        let theme = Theme::default();
        let code = "plain text\nline 2";
        let lines = highlight_code(code, None, &theme, Style::default());
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_supported_languages() {
        let langs = supported_languages();
        assert!(langs.contains(&"rs"));
        assert!(langs.contains(&"py"));
        assert!(langs.contains(&"js"));
    }

    #[test]
    fn test_is_language_supported() {
        assert!(is_language_supported("rust"));
        assert!(is_language_supported("python"));
        assert!(is_language_supported("javascript"));
        assert!(!is_language_supported("notareallanguage123"));
    }
}
