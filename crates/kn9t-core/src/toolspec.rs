//! Tool specifications: schema, effects, and approval policy for execution control.

use serde::{Deserialize, Serialize};

/// Effect kind: Shell, FsRead, FsWrite, or Network operation.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Shell,
    FsRead,
    FsWrite,
    Network,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Effect {
    /// Field in args containing the effect: bare name or JSON pointer path.
    pub field: String,
    pub kind: EffectKind,
}

/// Default approval policy: Allow (safe), Ask (default), or Deny (blocked).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    Allow,
    #[default]
    Ask,
    Deny,
}

/// Tool policy: pattern field, default approval, and allow/deny pattern lists.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct ToolPolicy {
    /// Field in args for pattern matching (e.g., "cmd", "path", "url").
    #[serde(default)]
    pub pattern_field: Option<String>,

    /// Default policy when no user config exists.
    #[serde(default)]
    pub default_policy: DefaultPolicy,

    /// Built-in allow patterns: tool author's safe defaults (user can override).
    #[serde(default)]
    pub builtin_allow: Vec<String>,

    /// Built-in deny patterns: hard deny (never shown in approval prompt).
    #[serde(default)]
    pub builtin_deny: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON schema for tool arguments (must be ordered).
    pub schema: serde_json::Value,
    /// If true, tool is registered but hidden from initial system prompt (lazy discovery).
    #[serde(default)]
    pub hidden: bool,
    /// Effects declared by plugin: side effects for approval policy.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Approval policy: controls ask/allow/deny behavior.
    #[serde(default)]
    pub policy: ToolPolicy,
}

impl ToolSpec {
    /// Check if a value matches any of the given patterns.
    pub fn matches_patterns(value: &str, patterns: &[String]) -> bool {
        for pattern in patterns {
            if wildcard_match(pattern, value) {
                return true;
            }
        }
        false
    }
}

/// Simple wildcard matching: `*` matches any sequence of characters.
/// Patterns like `"git *"` match `"git status"`, `"git diff --cached"`, etc.
/// Patterns like `"*.rs"` match `"foo.rs"`, `"src/bar.rs"`.
pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if i == 0 {
            // First part must match at start
            if !value.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last part must match at end
            if !value[pos..].ends_with(part) {
                return false;
            }
        } else {
            // Middle parts must appear somewhere
            if let Some(idx) = value[pos..].find(part) {
                pos += idx + part.len();
            } else {
                return false;
            }
        }
    }

    true
}

/// Generate a wildcard pattern from a value for "always" scope.
/// `"git status --short"` → `"git *"` (command prefix)
/// `"src/foo.rs"` → `"*.rs"` (file extension)
pub fn value_to_pattern(value: &str, is_path: bool) -> String {
    if is_path {
        // Path: use extension
        if let Some(ext) = std::path::Path::new(value).extension() {
            return format!("*.{}", ext.to_string_lossy());
        }
        // Fallback: directory pattern
        if let Some(parent) = std::path::Path::new(value).parent() {
            if !parent.as_os_str().is_empty() {
                return format!("{}/*", parent.display());
            }
        }
    } else {
        // Command: use first word
        let first_word = value.split_whitespace().next().unwrap_or("");
        if !first_word.is_empty() {
            return format!("{} *", first_word);
        }
    }
    "*".to_string()
}
