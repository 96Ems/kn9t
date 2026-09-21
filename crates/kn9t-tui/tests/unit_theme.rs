//! Unit tests for theme — extracted from src/theme.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::config::ThemeSection;
use kn9t_tui::theme::{color_to_string, parse_color, Theme};
use ratatui::style::Color;
use std::collections::HashMap;

#[test]
fn test_parse_color_hex() {
    assert_eq!(parse_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
    assert_eq!(parse_color("#00ff00"), Some(Color::Rgb(0, 255, 0)));
    assert_eq!(parse_color("#0000ff"), Some(Color::Rgb(0, 0, 255)));
    assert_eq!(parse_color("#ffffff"), Some(Color::Rgb(255, 255, 255)));
    assert_eq!(parse_color("#000000"), Some(Color::Rgb(0, 0, 0)));
}

#[test]
fn test_parse_color_hex_case_insensitive() {
    assert_eq!(parse_color("#FF0000"), Some(Color::Rgb(255, 0, 0)));
    assert_eq!(parse_color("#aAbBcC"), Some(Color::Rgb(170, 187, 204)));
}

#[test]
fn test_parse_color_named() {
    assert_eq!(parse_color("red"), Some(Color::Red));
    assert_eq!(parse_color("RED"), Some(Color::Red));
    assert_eq!(parse_color("green"), Some(Color::Green));
    assert_eq!(parse_color("blue"), Some(Color::Blue));
    assert_eq!(parse_color("cyan"), Some(Color::Cyan));
    assert_eq!(parse_color("white"), Some(Color::White));
    assert_eq!(parse_color("black"), Some(Color::Black));
}

#[test]
fn test_parse_color_gray_variants() {
    assert_eq!(parse_color("gray"), Some(Color::Gray));
    assert_eq!(parse_color("grey"), Some(Color::Gray));
    assert_eq!(parse_color("darkgray"), Some(Color::DarkGray));
    assert_eq!(parse_color("darkgrey"), Some(Color::DarkGray));
}

#[test]
fn test_parse_color_invalid() {
    assert_eq!(parse_color(""), None);
    assert_eq!(parse_color("invalid"), None);
    assert_eq!(parse_color("#fffffff"), None); // wrong length
    assert_eq!(parse_color("#gggggg"), None); // invalid hex
    assert_eq!(parse_color("#gg"), None); // invalid short hex
}

/// The bright ANSI colours must parse: the built-in `50_status.lua` styles its context
/// gauge with `lightgreen`/`lightred`, and while the parser lacked them the
/// built-in palette silently fell back to the theme default.
#[test]
fn parses_bright_colors_under_both_spellings() {
    assert_eq!(parse_color("lightgreen"), Some(Color::LightGreen));
    assert_eq!(parse_color("brightgreen"), Some(Color::LightGreen));
    assert_eq!(parse_color("LightRed"), Some(Color::LightRed));
    assert_eq!(parse_color("lightblack"), Some(Color::DarkGray));
}

/// A ricer's colourscheme is usually distributed as 256-palette indices or
/// short hex, so both are accepted.
#[test]
fn parses_palette_index_and_short_hex() {
    assert_eq!(parse_color("33"), Some(Color::Indexed(33)));
    assert_eq!(parse_color("0"), Some(Color::Indexed(0)));
    assert_eq!(parse_color("255"), Some(Color::Indexed(255)));
    assert_eq!(parse_color("256"), None, "out of palette range");
    // #rgb expands by doubling nibbles, as in CSS.
    assert_eq!(parse_color("#f00"), Some(Color::Rgb(255, 0, 0)));
    assert_eq!(parse_color("#fff"), Some(Color::Rgb(255, 255, 255)));
}

/// `kn9t.theme` is published to Lua as strings and fed back as widget
/// colours, so the round-trip has to be lossless or a ricer's palette
/// silently drifts.
#[test]
fn color_strings_round_trip() {
    for c in [
        Color::Red,
        Color::LightCyan,
        Color::DarkGray,
        Color::Rgb(18, 52, 86),
        Color::Indexed(200),
        Color::Reset,
    ] {
        assert_eq!(parse_color(&color_to_string(c)), Some(c), "{c:?}");
    }
}

/// Every name in `Theme::NAMES` must be a real slot: the list is what Lua
/// reads to discover the palette, so a stale entry would publish a nil.
#[test]
fn every_theme_name_resolves() {
    let mut t = Theme::dark();
    for name in Theme::NAMES {
        assert!(t.get(name).is_some(), "{name} has no slot");
        assert!(t.set(name, Color::Magenta), "{name} not settable");
        assert_eq!(t.get(name), Some(Color::Magenta));
    }
    assert!(!t.set("no_such_slot", Color::Red));
}

#[test]
fn test_parse_color_trimmed() {
    assert_eq!(parse_color("  red  "), Some(Color::Red));
    assert_eq!(parse_color("  #ff0000  "), Some(Color::Rgb(255, 0, 0)));
}

#[test]
fn test_theme_dark() {
    let theme = Theme::dark();
    assert_eq!(theme.mode, "dark");
    // Chrome, accent and alert must stay three distinct things, or the hierarchy the
    // palette exists to create is gone (PLAN §P7 D16).
    assert_ne!(theme.fg, theme.muted);
    assert_ne!(theme.primary, theme.warning);
    assert_ne!(theme.primary, theme.fg);
    // Diff is the only place green and red appear, and they must be tellable apart.
    assert_ne!(theme.diff_add, theme.diff_remove);
}

#[test]
fn test_theme_light() {
    let theme = Theme::light();
    assert_eq!(theme.mode, "light");
    // The two modes must actually differ, or `toggle_mode` is a no-op with a label.
    assert_ne!(theme.fg, Theme::dark().fg);
    assert_ne!(theme.primary, Theme::dark().primary);
}

#[test]
fn test_theme_from_config_dark() {
    let section = ThemeSection {
        mode: Some("dark".into()),
        colors: None,
    };
    assert_eq!(Theme::from_config(section).fg, Theme::dark().fg);
}

#[test]
fn test_theme_from_config_light() {
    let section = ThemeSection {
        mode: Some("light".into()),
        colors: None,
    };
    assert_eq!(Theme::from_config(section).fg, Theme::light().fg);
}

#[test]
fn test_theme_from_config_with_overrides() {
    let mut colors = HashMap::new();
    colors.insert("error".into(), "#ff00ff".into());

    let section = ThemeSection {
        mode: Some("dark".into()),
        colors: Some(colors),
    };
    let theme = Theme::from_config(section);
    assert_eq!(theme.error, Color::Rgb(255, 0, 255));
}

#[test]
fn test_theme_default_is_dark() {
    let theme = Theme::default();
    assert_eq!(theme.mode, "dark");
}
