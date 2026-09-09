//! Unit tests extracted from src/config.rs
//!
//! These tests were originally inline in the source file and have been
//! extracted to keep production code free of test code.

#![allow(clippy::unwrap_used)]

use kn9t_server::config::{
    build_http_quirks, merge_quirks, resolve, PolicyMode, RawConfig,
};

fn parse_raw(toml: &str) -> RawConfig {
    toml::from_str(toml).expect("parse RawConfig")
}

/// DESIGN §8.3: `[model.quirks]` used to be parsed, merged, then dropped
/// (`let _ = merged_quirks`), so the documented override was a silent no-op.
/// Pin the full path: TOML -> merge over provider quirks -> provider config.
#[test]
fn per_model_quirks_reach_the_provider() {
    let raw = parse_raw(
        r#"
[provider.gw]
kind = "openai"
base_url = "http://localhost:9/v1"
api_key = "x"

[provider.gw.quirks]
reasoning = "none"
max_tokens_field = "max_tokens"

[[model]]
provider = "gw"
id = "plain"
ctx = 1000
max_out = 100
price_in = 0.0
price_out = 0.0
price_cache_read = 0.0
price_cache_write = 0.0
cache = "none"
cache_breakpoints = 0
cache_min_tokens = 0

[[model]]
provider = "gw"
id = "special"
ctx = 1000
max_out = 100
price_in = 0.0
price_out = 0.0
price_cache_read = 0.0
price_cache_write = 0.0
cache = "none"
cache_breakpoints = 0
cache_min_tokens = 0

[model.quirks]
reasoning = "adaptive"
"#,
    );
    // Only the second model declares an override.
    assert!(!raw.models[0].quirks.is_set());
    assert!(raw.models[1].quirks.is_set());

    let merged = merge_quirks(
        build_http_quirks(&raw.provider["gw"].quirks),
        &raw.models[1].quirks,
    );
    // Overridden field takes the model value...
    assert_eq!(merged.reasoning, "adaptive");
    // ...while unspecified fields still inherit from the provider.
    assert_eq!(merged.max_tokens_field, "max_tokens");

    // And resolve() must not fail on this shape.
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.models.len(), 2);
}

#[test]
fn policy_absent_is_default() {
    let raw = parse_raw(r#""#);
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.policy_mode, PolicyMode::AskOnMutation);
}

#[test]
fn policy_mode_variants() {
    for (s, expected) in [
        ("ask_on_mutation", PolicyMode::AskOnMutation),
        ("allow_all", PolicyMode::AllowAll),
        ("deny_all", PolicyMode::DenyAll),
        ("readonly", PolicyMode::ReadOnly),
    ] {
        let raw = parse_raw(&format!(
            r#"[policy]
mode = "{s}"
"#
        ));
        let resolved = resolve(raw).unwrap();
        assert_eq!(resolved.policy_mode, expected);
    }
}

#[test]
fn policy_mode_unknown_is_error() {
    let raw = parse_raw(
        r#"[policy]
mode = "bogus"
"#,
    );
    assert!(resolve(raw).is_err());
}

#[test]
fn plugin_pinned_parses() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "my-tools"
        cmd = ["/abs/path/to/my-tools", "--flag"]
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert_eq!(resolved.plugins[0].name, "my-tools");
    assert_eq!(
        resolved.plugins[0].cmd.as_ref().unwrap(),
        &vec!["/abs/path/to/my-tools".to_string(), "--flag".to_string()]
    );
    assert!(!resolved.plugins[0].disabled);
}

#[test]
fn plugin_disabled_via_enabled_false() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "my-tools"
        enabled = false
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert_eq!(resolved.plugins[0].name, "my-tools");
    assert!(resolved.plugins[0].disabled);
    assert!(resolved.plugins[0].cmd.is_none());
}

#[test]
fn plugin_disabled_via_disabled_true() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "my-tools"
        disabled = true
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert!(resolved.plugins[0].disabled);
}

#[test]
fn plugin_env_override_without_cmd() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "my-tools"

        [plugin.env]
        FOO = "bar"
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert_eq!(resolved.plugins[0].name, "my-tools");
    assert!(resolved.plugins[0].cmd.is_none());
    assert!(!resolved.plugins[0].disabled);
    assert_eq!(
        resolved.plugins[0].env,
        vec![("FOO".to_string(), "bar".to_string())]
    );
}

#[test]
fn plugin_pinned_with_env() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "my-tools"
        cmd = ["/path/to/my-tools"]

        [plugin.env]
        FOO = "bar"
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.plugins.len(), 1);
    assert_eq!(
        resolved.plugins[0].cmd.as_ref().unwrap()[0],
        "/path/to/my-tools"
    );
    assert_eq!(
        resolved.plugins[0].env[0],
        ("FOO".to_string(), "bar".to_string())
    );
}

#[test]
fn plugin_empty_is_skipped() {
    let raw = parse_raw(
        r#"
        [[plugin]]
        name = "empty"
    "#,
    );
    let resolved = resolve(raw).unwrap();
    assert_eq!(
        resolved.plugins.len(),
        0,
        "plugin with no cmd/env and not disabled should be skipped"
    );
}
