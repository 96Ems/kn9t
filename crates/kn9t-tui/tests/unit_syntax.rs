//! Unit tests for syntax — extracted from src/syntax.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::syntax::{highlight_code, is_language_supported, supported_languages};
use kn9t_tui::theme::Theme;
use ratatui::style::Style;

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
