//! R-CORE-120 — tool specification.
//!
//! Tools declare their own policy via `ToolPolicy`. This replaces the hardcoded
//! bash classifier with a generic, plugin-extensible system.

use serde::{Deserialize, Serialize};

/// R-CORE-120 — the `schema` value MUST NOT be produced from a `HashMap`; object
/// key order is stable across processes (GI-3). `serde_json::Value::Object` is
/// `BTreeMap`-backed by default, which satisfies this as long as `preserve_order`
/// is off.
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
    /// JSON field name in `args` (e.g. `"cmd"` or `"path"`), or JSON pointer
    /// with leading `/` (e.g. `"/command"`). Bare string is treated as top-level key.
    pub field: String,
    pub kind: EffectKind,
}

/// Default policy when no user config exists for this tool.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPolicy {
    /// Safe tool, auto-allow without prompting (e.g. "read", "glob", "grep").
    Allow,
    /// Needs user approval by default (most tools).
    #[default]
    Ask,
    /// Blocked unless explicitly allowed in user config.
    Deny,
}

/// Policy declaration for a tool. Plugins declare this to control approval behavior
/// without hardcoding tool names in the server.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct ToolPolicy {
    /// Field to extract from args for pattern matching.
    /// e.g. "cmd" for bash, "path" for read/write, "url" for web_fetch.
    /// If None, the tool doesn't support pattern matching.
    #[serde(default)]
    pub pattern_field: Option<String>,

    /// Default policy when no user config exists.
    #[serde(default)]
    pub default_policy: DefaultPolicy,

    /// Built-in allow patterns declared by the tool author.
    /// These are checked AFTER user deny patterns but BEFORE user allow patterns,
    /// so users can override them. Example: `["git log *", "git status *"]` for a git tool.
    #[serde(default)]
    pub builtin_allow: Vec<String>,

    /// Built-in deny patterns (always deny, even if user allows).
    /// Example: `["rm -rf /", "sudo *"]` for bash.
    /// These are "hard deny" — no approval prompt, just rejected.
    #[serde(default)]
    pub builtin_deny: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// Hand-written `json!({...})`, ordered.
    pub schema: serde_json::Value,
    /// If true, tool is registered but not shown in the initial system prompt.
    /// Used for lazy tool discovery: hidden tools can still be executed once
    /// the agent discovers them via a meta-tool like `mcp_search_tools`.
    #[serde(default)]
    pub hidden: bool,
    /// Effects declared by the plugin (ADR-0002). Empty = strictest default
    /// (ask_on_mutation → Ask, deny_all → HardDeny).
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Policy declaration — controls approval behavior.
    /// If absent, uses `DefaultPolicy::Ask` with no pattern matching.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("git *", "git status"));
        assert!(wildcard_match("git *", "git diff --cached"));
        assert!(!wildcard_match("git *", "cargo build"));

        assert!(wildcard_match("*.rs", "foo.rs"));
        assert!(wildcard_match("*.rs", "src/bar.rs"));
        assert!(!wildcard_match("*.rs", "foo.ts"));

        assert!(wildcard_match("src/*", "src/foo.rs"));
        assert!(wildcard_match("src/*.rs", "src/foo.rs"));
        assert!(!wildcard_match("src/*.rs", "src/foo.ts"));

        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "not_exact"));
    }

    #[test]
    fn test_value_to_pattern() {
        assert_eq!(value_to_pattern("git status --short", false), "git *");
        assert_eq!(value_to_pattern("cargo build", false), "cargo *");
        assert_eq!(value_to_pattern("src/foo.rs", true), "*.rs");
        assert_eq!(value_to_pattern("Cargo.toml", true), "*.toml");
    }

    #[test]
    fn test_default_policy() {
        let policy = ToolPolicy::default();
        assert_eq!(policy.default_policy, DefaultPolicy::Ask);
        assert!(policy.pattern_field.is_none());
        assert!(policy.builtin_allow.is_empty());
        assert!(policy.builtin_deny.is_empty());
    }
}
