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
            bg: Color::Reset,  // Use terminal default
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
            tool_focus_bg: Color::Rgb(40, 44, 52),      // Subtle dark highlight
            tool_focus_border: Color::Cyan,
            input_key: Color::Yellow,
            input_value: Color::White,
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
            tool_focus_bg: Color::Rgb(230, 235, 245),   // Subtle light highlight
            tool_focus_border: Color::Blue,
            input_key: Color::Rgb(180, 100, 0),
            input_value: Color::Black,
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
                match name.as_str() {
                    "background" | "bg" => self.bg = color,
                    "foreground" | "fg" => self.fg = color,
                    "muted" => self.muted = color,
                    "primary" => self.primary = color,
                    "error" => self.error = color,
                    "warning" => self.warning = color,
                    "success" => self.success = color,
                    "user" => self.user = color,
                    "assistant" => self.assistant = color,
                    "tool" => self.tool = color,
                    "diff_add" => self.diff_add = color,
                    "diff_remove" => self.diff_remove = color,
                    "tab_active_fg" => self.tab_active_fg = color,
                    "tab_active_bg" => self.tab_active_bg = color,
                    "tab_inactive_fg" => self.tab_inactive_fg = color,
                    "tool_focus_bg" => self.tool_focus_bg = color,
                    "tool_focus_border" => self.tool_focus_border = color,
                    "input_key" => self.input_key = color,
                    "input_value" => self.input_value = color,
                    _ => {}
                }
            }
        }
        self
    }
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    
    // Hex color: #RRGGBB
    if s.starts_with('#') && s.len() == 7 {
        let r = u8::from_str_radix(&s[1..3], 16).ok()?;
        let g = u8::from_str_radix(&s[3..5], 16).ok()?;
        let b = u8::from_str_radix(&s[5..7], 16).ok()?;
        return Some(Color::Rgb(r, g, b));
    }

    // Named colors.
    match s.to_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
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
        assert_eq!(parse_color("#fff"), None); // too short
        assert_eq!(parse_color("#fffffff"), None); // too long
        assert_eq!(parse_color("#gggggg"), None); // invalid hex
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
