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
session_header = "x-opencode-session"

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
    // A model override that names neither header nor session must not clear it.
    assert_eq!(merged.session_header, "x-opencode-session");

    // And resolve() must not fail on this shape.
    let resolved = resolve(raw).unwrap();
    assert_eq!(resolved.models.len(), 2);
}

#[test]
fn unknown_api_is_rejected() {
    // A wire format the provider cannot dispatch must fail at load. Accepting it
    // silently would give a model that parses, appears in the picker, and 404s.
    let raw = parse_raw(
        r#"
[provider.gw]
kind = "openai"
base_url = "http://localhost:9/v1"
api_key = "x"

[provider.gw.quirks]
api = "grpc"
"#,
    );
    match resolve(raw) {
        Err(e) => assert!(e.contains("unknown api"), "unexpected error: {e}"),
        Ok(_) => panic!("unknown api must be rejected"),
    }
}

#[test]
fn unknown_api_in_model_override_is_rejected() {
    let raw = parse_raw(
        r#"
[provider.gw]
kind = "openai"
base_url = "http://localhost:9/v1"
api_key = "x"

[[model]]
provider = "gw"
id = "m"
ctx = 1000
max_out = 100
cache = "none"
cache_breakpoints = 0
cache_min_tokens = 0

[model.quirks]
api = "grpc"
"#,
    );
    match resolve(raw) {
        Err(e) => assert!(e.contains("unknown api"), "unexpected error: {e}"),
        Ok(_) => panic!("unknown api override must be rejected"),
    }
}

#[test]
fn responses_api_is_accepted() {
    let raw = parse_raw(
        r#"
[provider.gw]
kind = "openai"
base_url = "http://localhost:9/v1"
api_key = "x"

[provider.gw.quirks]
api = "responses"
"#,
    );
    match resolve(raw) {
        Ok(r) => assert_eq!(r.providers.len(), 1),
        Err(e) => panic!("responses must be accepted: {e}"),
    }
}

#[test]
fn config_discover_false() {
    // C1: discovery used to be unconditional, so a gateway's unusable models were
    // registered anyway -- at a default ctx and price 0 -- and shown in the picker.
    let base = spawn_models_server(r#"{"object":"list","data":[{"id":"discovered-model"}]}"#);

    let toml = |extra: &str| {
        format!(
            r#"
[provider.gw]
kind = "openai"
base_url = "{base}/v1"
api_key = "x"
{extra}

[[model]]
provider = "gw"
id = "declared-model"
ctx = 1000
max_out = 100
cache = "none"
cache_breakpoints = 0
cache_min_tokens = 0
"#
        )
    };

    let off = resolve(parse_raw(&toml("discover = false"))).unwrap();
    let ids: Vec<&str> = off.models.iter().map(|m| m.r#ref.id.as_str()).collect();
    assert!(ids.contains(&"declared-model"));
    assert!(
        !ids.contains(&"discovered-model"),
        "discover = false must not register /models results: {ids:?}"
    );

    let on = resolve(parse_raw(&toml(""))).unwrap();
    let ids: Vec<&str> = on.models.iter().map(|m| m.r#ref.id.as_str()).collect();
    assert!(
        ids.contains(&"discovered-model"),
        "the default must still discover: {ids:?}"
    );
}

/// Serve a canned `/models` body to the next few clients, then stop.
fn spawn_models_server(body: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..4 {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });
    format!("http://{addr}")
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
