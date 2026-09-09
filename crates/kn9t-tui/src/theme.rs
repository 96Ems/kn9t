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
    /// Current theme mode: "light" or "dark" (for toggling).
    pub mode: String,
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
            mode: "dark".into(),
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
            mode: "light".into(),
        }
    }

    pub fn from_config(section: ThemeSection) -> Self {
        let mut theme = match section.mode.as_deref() {
            Some("light") => Self::light(),
            Some("dark") => Self::dark(),
            Some("auto") | None => Self::auto_detect(),
            _ => Self::dark(),
        };

        if let Some(colors) = section.colors {
            theme = theme.with_overrides(colors);
        }
        theme
    }

    /// Auto-detect light/dark mode from terminal environment.
    ///
    /// Checks COLORFGBG (set by some terminals like rxvt, xterm) and
    /// falls back to dark theme if detection fails.
    fn auto_detect() -> Self {
        // COLORFGBG format: "fg;bg" where higher bg values suggest light background
        if let Ok(colorfgbg) = std::env::var("COLORFGBG") {
            if let Some(bg_str) = colorfgbg.split(';').next_back() {
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

    /// Get the current theme mode ("light" or "dark").
    pub fn get_mode(&self) -> &str {
        &self.mode
    }

    /// Toggle between light and dark mode.
    pub fn toggle_mode(&mut self) {
        let new_theme = match self.mode.as_str() {
            "dark" => Self::light(),
            _ => Self::dark(),
        };

        // Copy over color overrides from current theme
        // (user's customizations should persist when toggling)
        *self = new_theme;
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

