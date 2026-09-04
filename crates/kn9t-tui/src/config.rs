//! Configuration — R-TUI-180 (theming), R-TUI-160 (keybinds).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::theme::Theme;

/// TUI configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub token: Option<String>,
    pub right_sidebar: bool,
    pub theme: Theme,
    pub keybinds: HashMap<String, String>,
    pub streaming_phrases: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:7474".into(),
            token: None,
            right_sidebar: true,
            theme: Theme::default(),
            keybinds: default_keybinds(),
            streaming_phrases: default_phrases(),
        }
    }
}

impl Config {
    /// Load config from ~/.kn9t/config.toml and env vars.
    pub fn load() -> Self {
        let mut config = Config::default();

        // Try to load from file.
        if let Some(path) = config_path() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(file) = toml::from_str::<ConfigFile>(&content) {
                    config.apply_file(file);
                }
            }
        }

        // Env overrides.
        if let Ok(url) = env::var("KN9T_URL") {
            config.base_url = url;
        }
        if let Ok(token) = env::var("KN9T_TOKEN") {
            config.token = Some(token);
        }
        if let Ok(model) = env::var("KN9T_MODEL") {
            // Store for later use if needed.
            let _ = model;
        }

        // Read token from ~/.kn9t/token if not set.
        if config.token.is_none() {
            if let Some(mut path) = home_dir() {
                path.push(".kn9t");
                path.push("token");
                if let Ok(t) = fs::read_to_string(&path) {
                    config.token = Some(t.trim().to_string());
                }
            }
        }

        // Read port from ~/.kn9t/port if base_url not overridden.
        if env::var("KN9T_URL").is_err() {
            if let Some(mut path) = home_dir() {
                path.push(".kn9t");
                path.push("port");
                if let Ok(p) = fs::read_to_string(&path) {
                    if let Ok(port) = p.trim().parse::<u16>() {
                        config.base_url = format!("http://127.0.0.1:{}", port);
                    }
                }
            }
        }

        config
    }

    fn apply_file(&mut self, file: ConfigFile) {
        if let Some(tui) = file.tui {
            // left_sidebar removed - sessions accessed via /session command
            if let Some(v) = tui.right_sidebar {
                self.right_sidebar = v;
            }
            if let Some(streaming) = tui.streaming {
                if let Some(phrases) = streaming.phrases {
                    self.streaming_phrases = phrases;
                }
            }
        }
        if let Some(theme) = file.theme {
            self.theme = Theme::from_config(theme);
        }
        if let Some(kb) = file.keybinds {
            for (k, v) in kb {
                self.keybinds.insert(k, v);
            }
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let mut path = home_dir()?;
    path.push(".kn9t");
    path.push("config.toml");
    Some(path)
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

fn default_keybinds() -> HashMap<String, String> {
    // NOTE: All keybinds that use letters MUST use Ctrl modifier (C-x)
    // to avoid blocking user input. Only special keys (PageUp, arrows, etc)
    // can be used without modifiers.
    HashMap::new() // All defaults now set in keybind.rs with proper modifiers
}

fn default_phrases() -> Vec<String> {
    vec![
        "thinking...".into(),
        "pondering...".into(),
        "cooking...".into(),
        "summoning bytes...".into(),
        "consulting the void...".into(),
        "reticulating splines...".into(),
    ]
}

/// Config file structure (TOML).
#[derive(Debug, Deserialize)]
struct ConfigFile {
    tui: Option<TuiSection>,
    theme: Option<ThemeSection>,
    keybinds: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct TuiSection {
    #[allow(dead_code)]
    left_sidebar: Option<bool>, // Deprecated: sessions accessed via /session
    right_sidebar: Option<bool>,
    streaming: Option<StreamingSection>,
}

#[derive(Debug, Deserialize)]
struct StreamingSection {
    phrases: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ThemeSection {
    pub mode: Option<String>,
    pub colors: Option<HashMap<String, String>>,
}
