//! Theming — R-TUI-180.
//!
//! Auto light/dark detection + user color overrides.

use std::collections::HashMap;

use ratatui::style::Color;

use crate::config::ThemeSection;

/// Theme colors.
#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub primary: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool: Color,
    pub diff_add: Color,
    pub diff_remove: Color,
    pub selection: Color,

    // Tool card colors
    pub tab_active_fg: Color,
    pub tab_active_bg: Color,
    pub tab_inactive_fg: Color,
    pub tool_focus_bg: Color,
    pub tool_focus_border: Color,
    pub input_key: Color,
    pub input_value: Color,
    /// Background of a tool card. A theme slot rather than a constant so a
    /// colourscheme can restyle cards without recompiling.
    pub tool_card_bg: Color,
    /// Background for user messages to distinguish them visually.
    pub user_msg_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        // Dark theme defaults.
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: Color::Reset, // Use terminal default
            fg: Color::White,
            muted: Color::DarkGray,
            primary: Color::Cyan,
            error: Color::Red,
            warning: Color::Yellow,
            success: Color::Green,
            user: Color::Cyan,
            assistant: Color::Magenta,
            tool: Color::Yellow,
            diff_add: Color::Green,
            diff_remove: Color::Red,
            selection: Color::Rgb(60, 60, 80), // Subtle blue-gray highlight

            // Tool card colors
            tab_active_fg: Color::Black,
            tab_active_bg: Color::Cyan,
            tab_inactive_fg: Color::DarkGray,
            tool_focus_bg: Color::Rgb(40, 44, 52), // Subtle dark highlight
            tool_focus_border: Color::Cyan,
            input_key: Color::Yellow,
            input_value: Color::White,
            tool_card_bg: Color::Rgb(30, 33, 39),
            user_msg_bg: Color::Rgb(20, 35, 45), // Subtle teal background for user messages
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Black,
            muted: Color::DarkGray,
            primary: Color::Blue,
            error: Color::Red,
            warning: Color::Rgb(180, 100, 0),
            success: Color::Green,
            user: Color::Blue,
            assistant: Color::Magenta,
            tool: Color::Rgb(180, 100, 0),
            diff_add: Color::Green,
            diff_remove: Color::Red,
            selection: Color::Rgb(220, 220, 240), // Subtle blue-gray highlight

            // Tool card colors
            tab_active_fg: Color::White,
            tab_active_bg: Color::Blue,
            tab_inactive_fg: Color::DarkGray,
            tool_focus_bg: Color::Rgb(230, 235, 245), // Subtle light highlight
            tool_focus_border: Color::Blue,
            input_key: Color::Rgb(180, 100, 0),
            input_value: Color::Black,
            user_msg_bg: Color::Rgb(220, 235, 245), // Subtle light blue background for user messages
            tool_card_bg: Color::Rgb(240, 242, 246),
        }
    }

    pub fn from_config(section: ThemeSection) -> Self {
        let base = match section.mode.as_deref() {
            Some("light") => Self::light(),
            Some("dark") => Self::dark(),
            Some("auto") | None => Self::auto_detect(),
            _ => Self::dark(),
        };

        if let Some(colors) = section.colors {
            base.with_overrides(colors)
        } else {
            base
        }
    }

    /// Auto-detect light/dark mode from terminal environment.
    ///
    /// Checks COLORFGBG (set by some terminals like rxvt, xterm) and
    /// falls back to dark theme if detection fails.
    fn auto_detect() -> Self {
        // COLORFGBG format: "fg;bg" where higher bg values suggest light background
        if let Ok(colorfgbg) = std::env::var("COLORFGBG") {
            if let Some(bg_str) = colorfgbg.split(';').last() {
                if let Ok(bg) = bg_str.trim().parse::<u32>() {
                    // ANSI colors: 0-7 are dark, 8-15 are bright
                    // Values > 7 (especially 15 = white) suggest light theme
                    if bg > 7 {
                        return Self::light();
                    }
                }
            }
        }

        // Check for common light terminal indicators
        if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
            let lower = term_program.to_lowercase();
            // Some terminals that commonly default to light mode
            if lower.contains("apple_terminal") {
                // macOS Terminal.app often uses light theme by default
                // but we can't know for sure, so still default to dark
            }
        }

        // Default to dark theme (most common for developer terminals)
        Self::dark()
    }

    fn with_overrides(mut self, colors: HashMap<String, String>) -> Self {
        for (name, value) in colors {
            if let Some(color) = parse_color(&value) {
                self.set(&name, color);
            }
        }
        self
    }

    /// Mutable access to a colour slot by its public name.
    ///
    /// The single name-to-field mapping in the TUI. `[theme.colors]` in
    /// `config.toml`, `kn9t.theme` published to Lua, and `kn9t.set_theme{}` all
    /// route through here, so a slot cannot exist for one and not the others.
    fn slot_mut(&mut self, name: &str) -> Option<&mut Color> {
        Some(match name {
            "bg" | "background" => &mut self.bg,
            "fg" | "foreground" => &mut self.fg,
            "muted" => &mut self.muted,
            "primary" => &mut self.primary,
            "error" => &mut self.error,
            "warning" => &mut self.warning,
            "success" => &mut self.success,
            "user" => &mut self.user,
            "assistant" => &mut self.assistant,
            "tool" => &mut self.tool,
            "diff_add" => &mut self.diff_add,
            "diff_remove" => &mut self.diff_remove,
            "selection" => &mut self.selection,
            "tab_active_fg" => &mut self.tab_active_fg,
            "tab_active_bg" => &mut self.tab_active_bg,
            "tab_inactive_fg" => &mut self.tab_inactive_fg,
            "tool_focus_bg" => &mut self.tool_focus_bg,
            "tool_focus_border" => &mut self.tool_focus_border,
            "input_key" => &mut self.input_key,
            "input_value" => &mut self.input_value,
            "tool_card_bg" => &mut self.tool_card_bg,
            "user_msg_bg" => &mut self.user_msg_bg,
            _ => return None,
        })
    }

    /// Every colour slot name, in a stable order.
    ///
    /// Used to publish `kn9t.theme` wholesale: a ricer discovers the palette by
    /// dumping the table, rather than by reading Rust source.
    pub const NAMES: &'static [&'static str] = &[
        "bg",
        "fg",
        "muted",
        "primary",
        "error",
        "warning",
        "success",
        "user",
        "assistant",
        "tool",
        "diff_add",
        "diff_remove",
        "selection",
        "tab_active_fg",
        "tab_active_bg",
        "tab_inactive_fg",
        "tool_focus_bg",
        "tool_focus_border",
        "input_key",
        "input_value",
        "tool_card_bg",
        "user_msg_bg",
    ];

    /// Read a colour slot by name.
    pub fn get(&self, name: &str) -> Option<Color> {
        // Goes through `slot_mut` on a clone so the mapping is defined once.
        let mut probe = self.clone();
        probe.slot_mut(name).copied()
    }

    /// Write a colour slot by name. Returns false for an unknown slot, so the
    /// caller can surface the typo instead of dropping it silently.
    pub fn set(&mut self, name: &str, color: Color) -> bool {
        match self.slot_mut(name) {
            Some(slot) => {
                *slot = color;
                true
            }
            None => false,
        }
    }
}

/// Render a colour back to a string `parse_color` accepts.
///
/// Needed so `kn9t.theme` is round-trippable: Lua reads `kn9t.theme.user`, uses
/// it as a widget `fg`, and gets the same colour back.
pub fn color_to_string(c: Color) -> String {
    match c {
        Color::Reset => "reset".into(),
        Color::Black => "black".into(),
        Color::Red => "red".into(),
        Color::Green => "green".into(),
        Color::Yellow => "yellow".into(),
        Color::Blue => "blue".into(),
        Color::Magenta => "magenta".into(),
        Color::Cyan => "cyan".into(),
        Color::Gray => "gray".into(),
        Color::DarkGray => "darkgray".into(),
        Color::LightRed => "lightred".into(),
        Color::LightGreen => "lightgreen".into(),
        Color::LightYellow => "lightyellow".into(),
        Color::LightBlue => "lightblue".into(),
        Color::LightMagenta => "lightmagenta".into(),
        Color::LightCyan => "lightcyan".into(),
        Color::White => "white".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => i.to_string(),
    }
}

/// Parse a colour written the way a user would write it in a config or in Lua.
///
/// This is the **single** colour parser in the TUI: `[theme.colors]` in
/// `config.toml`, every `fg=`/`bg=`/`border_fg=` in a Lua widget, and
/// `kn9t.theme` all go through it. A second copy is how `lightgreen` came to
/// work in one place and silently resolve to the default in the other.
///
/// Accepted forms:
/// - `#rrggbb` and the CSS-style short `#rgb`
/// - the 16 ANSI names, with `light*` and `bright*` both accepted as spellings
///   of the same colour, plus `gray`/`grey`
/// - `0`-`255` for the 256-colour palette, which is what most terminal
///   colourschemes are actually distributed as
/// - `reset`/`default`/`none` for "whatever the terminal uses"
///
/// Returns `None` on anything else so the caller can report the typo instead of
/// painting a wrong colour.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();

    if let Some(hex) = s.strip_prefix('#') {
        // #rgb is expanded by doubling each nibble, as in CSS: #f00 == #ff0000.
        let expand = |c: u8| c * 17;
        match hex.len() {
            3 => {
                let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(expand);
                return Some(Color::Rgb(d(0)?, d(1)?, d(2)?));
            }
            6 => {
                let d = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                return Some(Color::Rgb(d(0)?, d(2)?, d(4)?));
            }
            _ => return None,
        }
    }

    // Bare number: 256-colour palette index.
    if let Ok(idx) = s.parse::<u8>() {
        return Some(Color::Indexed(idx));
    }

    let lower = s.to_lowercase();
    // `light` and `bright` are the same 8 colours under two common spellings;
    // normalising here means a colourscheme written for either works as-is.
    let name = lower
        .strip_prefix("bright")
        .map(|rest| format!("light{rest}"))
        .unwrap_or(lower);

    match name.as_str() {
        "reset" | "default" | "none" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" | "lightblack" => Some(Color::DarkGray),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        "lightwhite" => Some(Color::White),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The bright ANSI colours must parse: `default_tui.lua` styles its context
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
        assert_eq!(theme.fg, Color::White);
        assert_eq!(theme.error, Color::Red);
    }

    #[test]
    fn test_theme_light() {
        let theme = Theme::light();
        assert_eq!(theme.fg, Color::Black);
        assert_eq!(theme.error, Color::Red);
    }

    #[test]
    fn test_theme_from_config_dark() {
        let section = ThemeSection {
            mode: Some("dark".into()),
            colors: None,
        };
        let theme = Theme::from_config(section);
        assert_eq!(theme.fg, Color::White);
    }

    #[test]
    fn test_theme_from_config_light() {
        let section = ThemeSection {
            mode: Some("light".into()),
            colors: None,
        };
        let theme = Theme::from_config(section);
        assert_eq!(theme.fg, Color::Black);
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
        assert_eq!(theme.fg, Color::White);
    }
}
