use kn9t_core::{wildcard_match, value_to_pattern, DefaultPolicy, ToolPolicy};

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
