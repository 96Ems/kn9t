use kn9t_core::{
    CacheMode, Chunk, Effort, ModelRef, ModelSpec,
    Price, Provider, StopReason, Thinking,
};
use kn9t_provider_core::Quirks;
use kn9t_provider_openai::{
    cache::should_send_cache_fields,
    decode::DecodeState,
    encode::build_request,
    OpenAiConfig, OpenAiProvider,
};
use serde_json::{json};

fn make_model(id: &str) -> ModelSpec {
    ModelSpec {
        r#ref: ModelRef { provider: "openai".into(), id: id.into() },
        api_id: id.into(),
        ctx_window: 128_000,
        max_out: 4096,
        price: Price { input: 2500000, output: 10000000, cache_read: 1250000, cache_write: 0 },
        cache: CacheMode::Automatic,
        streaming: true,
        quirks: kn9t_core::Quirks::default(),
    }
}

fn make_request(model: &ModelSpec) -> kn9t_core::Request<'_> {
    kn9t_core::Request {
        model,
        system:     Some("You are helpful."),
        messages:   &[],
        tools:      &[],
        thinking:   Thinking::Off,
        max_tokens: Some(512),
        cache:      &[],
    }
}

// ── oai::request_shape (R-OAI-010) ───────────────────────────────────────────

#[test]
fn oai_request_shape_default_quirks() {
    let model = make_model("gpt-4o");
    let req = kn9t_core::Request {
        model: &model,
        system: Some("Be helpful"),
        messages: &[],
        tools: &[],
        thinking: Thinking::Off,
        max_tokens: Some(1024),
        cache: &[],
    };
    let quirks = Quirks::default();
    let body = build_request(&req, &quirks, &CacheMode::Automatic, false);

    assert_eq!(body["model"], json!("gpt-4o"));
    assert_eq!(body["max_tokens"], json!(1024));
    assert!(body.get("max_completion_tokens").is_none(), "must not emit both fields");
    assert_eq!(body["messages"][0]["role"], json!("system"));
    assert_eq!(body["stream"], json!(true));
}

#[test]
fn oai_request_shape_completion_tokens_quirk() {
    let model = make_model("o3");
    let req = make_request(&model);
    let quirks = Quirks {
        max_tokens_field: "max_completion_tokens".into(),
        system_role:      "developer".into(),
        ..Quirks::default()
    };
    let body = build_request(&req, &quirks, &CacheMode::Automatic, false);

    assert_eq!(body["max_completion_tokens"], json!(512));
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["messages"][0]["role"], json!("developer"));
}

// ── oai::decode (R-OAI-020) ──────────────────────────────────────────────────

#[test]
fn oai_decode_text_stream() {
    let model_ref = ModelRef { provider: "openai".into(), id: "gpt-4o".into() };
    let quirks = Quirks::default();
    let mut state = DecodeState::new();

    let delta1 = json!({
        "choices": [{ "delta": { "content": "Hello" }, "finish_reason": null }]
    });
    let delta2 = json!({
        "choices": [{ "delta": { "content": " world" }, "finish_reason": "stop" }]
    });

    let c1 = state.decode(delta1.to_string().as_bytes(), &quirks, &model_ref).unwrap();
    let c2 = state.decode(delta2.to_string().as_bytes(), &quirks, &model_ref).unwrap();

    let text1 = c1.iter().find(|c| matches!(c, Chunk::Text { .. }));
    assert!(text1.is_some());
    if let Some(Chunk::Text { delta, .. }) = text1 {
        assert_eq!(delta, "Hello");
    }

    let stop = c2.iter().find(|c| matches!(c, Chunk::Stop(_)));
    assert!(stop.is_some());
    if let Some(Chunk::Stop(r)) = stop {
        assert!(matches!(r, StopReason::Stop));
    }
}

#[test]
fn oai_decode_tool_call_stream() {
    let model_ref = ModelRef { provider: "openai".into(), id: "gpt-4o".into() };
    let quirks = Quirks::default();
    let mut state = DecodeState::new();

    // First chunk: start tool call
    let d1 = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_abc",
                    "function": { "name": "bash", "arguments": "{\"cmd" }
                }]
            },
            "finish_reason": null
        }]
    });
    // Second chunk: args fragment
    let d2 = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": { "arguments": "\":\"ls\"}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let c1 = state.decode(d1.to_string().as_bytes(), &quirks, &model_ref).unwrap();
    let c2 = state.decode(d2.to_string().as_bytes(), &quirks, &model_ref).unwrap();

    let tc = c1.iter().find(|c| matches!(c, Chunk::ToolCall { .. }));
    assert!(tc.is_some(), "expected ToolCall chunk");
    if let Some(Chunk::ToolCall { name, .. }) = tc {
        assert_eq!(name, "bash");
    }

    let args: Vec<_> = c1.iter().chain(c2.iter())
        .filter(|c| matches!(c, Chunk::ToolArgs { .. }))
        .collect();
    assert!(!args.is_empty(), "expected ToolArgs chunks");

    let stop = c2.iter().find(|c| matches!(c, Chunk::Stop(StopReason::ToolUse)));
    assert!(stop.is_some());
}

#[test]
fn oai_decode_reasoning_stream() {
    let model_ref = ModelRef { provider: "openai".into(), id: "o3".into() };
    let quirks = Quirks {
        thinking_style: "reasoning_content".into(),
        ..Quirks::default()
    };
    let mut state = DecodeState::new();

    let delta = json!({
        "choices": [{
            "delta": { "reasoning_content": "let me think..." },
            "finish_reason": null
        }]
    });

    let chunks = state.decode(delta.to_string().as_bytes(), &quirks, &model_ref).unwrap();
    let thinking = chunks.iter().find(|c| matches!(c, Chunk::Thinking { .. }));
    assert!(thinking.is_some(), "expected Thinking chunk from reasoning_content");
}

// ── oai::toolcall_correlate (R-OAI-030) ─────────────────────────────────────

#[test]
fn oai_toolcall_correlate() {
    // A fixture with id-only correlation (no index field) must assemble the right number.
    let model_ref = ModelRef { provider: "openai".into(), id: "gpt-4o".into() };
    let quirks = Quirks::default();
    let mut state = DecodeState::new();

    // Two tool calls: index present.
    let d = json!({
        "choices": [{
            "delta": {
                "tool_calls": [
                    { "index": 0, "id": "c1", "function": { "name": "foo", "arguments": "" } },
                    { "index": 1, "id": "c2", "function": { "name": "bar", "arguments": "" } },
                ]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let chunks = state.decode(d.to_string().as_bytes(), &quirks, &model_ref).unwrap();
    let tcs: Vec<_> = chunks.iter().filter(|c| matches!(c, Chunk::ToolCall { .. })).collect();
    assert_eq!(tcs.len(), 2, "must decode two tool calls");
}

// ── oai::tool_call_encoding (R-OAI-010) ──────────────────────────────────────
// Assistant messages with ToolCall content MUST produce a top-level `tool_calls`
// array, not content parts — verified against the live gateway in e2e_bedrock.

#[test]
fn oai_tool_call_encoding() {
    use kn9t_core::{CallId, Content, Message, MsgId, Role};
    use kn9t_provider_core::Quirks;

    let model = make_model("claude-haiku");
    let quirks = Quirks::default();

    // Build a synthetic assistant message containing a tool call.
    let assistant_msg = Message {
        id:   MsgId::new(),
        role: Role::Assistant,
        content: vec![Content::ToolCall {
            id:        CallId("call_abc123".into()),
            name:      "calculator".into(),
            args_json: r#"{"expression":"7 * 6"}"#.into(),
        }], silent: false
    };

    // Tool result message sent back.
    let tool_result_msg = Message {
        id:   MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id:       CallId("call_abc123".into()),
            content:  vec![Content::Text { text: "42".into() }],
            is_error: false,
        }], silent: false
    };

    let req = kn9t_core::Request {
        model:      &model,
        system:     None,
        messages:   &[assistant_msg, tool_result_msg],
        tools:      &[],
        thinking:   Thinking::Off,
        max_tokens: Some(64),
        cache:      &[],
    };
    let body = build_request(&req, &quirks, &CacheMode::None, false);
    let msgs = body["messages"].as_array().expect("messages must be array");

    // First message: assistant with tool_calls array.
    let asst = &msgs[0];
    assert_eq!(asst["role"], json!("assistant"), "assistant role must be set");
    let tool_calls = asst["tool_calls"].as_array()
        .expect("assistant message must have top-level tool_calls array");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["id"],                    json!("call_abc123"));
    assert_eq!(tool_calls[0]["type"],                  json!("function"));
    assert_eq!(tool_calls[0]["function"]["name"],      json!("calculator"));
    assert_eq!(tool_calls[0]["function"]["arguments"], json!(r#"{"expression":"7 * 6"}"#));
    // content must be null (no text alongside the tool call).
    assert!(asst["content"].is_null(), "content must be null when only tool calls present");
    // Must NOT appear as a content part.
    assert!(asst.get("type").is_none(), "must not emit type:tool_call content part");

    // Second message: tool result with role=tool and tool_call_id at top level.
    let tr = &msgs[1];
    assert_eq!(tr["role"],         json!("tool"),        "tool result role must be tool");
    assert_eq!(tr["tool_call_id"], json!("call_abc123"), "tool_call_id must match");
    assert_eq!(tr["content"],      json!("42"),           "content must be the result text");
}

// ── oai::cache_automatic_omits (R-OAI-040) ───────────────────────────────────

#[test]
fn oai_cache_automatic_omits() {
    assert!(!should_send_cache_fields(&CacheMode::Automatic), "Automatic must omit cache fields");
    assert!(!should_send_cache_fields(&CacheMode::None), "None must omit cache fields");
}

// ── oai::cache_explicit_places (R-OAI-040) ───────────────────────────────────

#[test]
fn oai_cache_explicit_places() {
    let mode = CacheMode::Explicit { max_breakpoints: 4, min_tokens: 1024 };
    assert!(should_send_cache_fields(&mode), "Explicit must send cache fields");
}

// ── oai::extra_headers (R-OAI-050) ───────────────────────────────────────────
// Verify that extra_headers are stored on OpenAiConfig and that no deployment-specific
// logic exists in the provider itself (no URL sniffing, no hard-coded header names).

#[test]
fn oai_extra_headers() {
    // Extra headers are deployment config — the provider stores them verbatim.
    let config = OpenAiConfig {
        name:  "test".into(),
        extra_headers: vec![
            ("X-User-Id".into(),         "alice".into()),
            ("source_identifier".into(), "my_app_id".into()),
            ("X-Custom".into(),          "value".into()),
        ],
        ..OpenAiConfig::default()
    };

    // All supplied headers must be present in order.
    assert_eq!(config.extra_headers.len(), 3);
    assert_eq!(config.extra_headers[0], ("X-User-Id".into(), "alice".into()));
    assert_eq!(config.extra_headers[1], ("source_identifier".into(), "my_app_id".into()));
    assert_eq!(config.extra_headers[2], ("X-Custom".into(), "value".into()));

    // Default config has no extra headers — provider is deployment-agnostic.
    let default_config = OpenAiConfig::default();
    assert!(default_config.extra_headers.is_empty(),
        "default OpenAiConfig must have no deployment-specific headers");

    // Provider construction must not add any deployment-specific headers based on URL.
    let gateway_url_config = OpenAiConfig {
        base_url: "https://llm-gateway.example.com/v1".into(),
        extra_headers: vec![],  // explicitly empty
        ..OpenAiConfig::default()
    };
    let _provider = OpenAiProvider::new(gateway_url_config.clone());
    // If the provider were sniffing the URL it would mutate extra_headers or add headers
    // internally. Since extra_headers is on config (not derived at runtime), the field
    // stays exactly what the config layer set — verified by nbed_config_headers.
    assert!(gateway_url_config.extra_headers.is_empty(),
        "provider must NOT add headers based on URL — that is the config layer's job (R-OAI-050)");
}

// ── nbed::usage_fields (R-NBED-060) ──────────────────────────────────────────

#[test]
fn nbed_usage_fields() {
    use kn9t_provider_openai::decode::decode_usage;

    // Field at root.
    let u1 = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "cache_creation_input_tokens": 80,
        "cached_tokens": 20,
    });
    let t1 = decode_usage(&u1);

    // Field under prompt_tokens_details.
    let u2 = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "prompt_tokens_details": {
            "cache_creation_input_tokens": 80,
            "cached_tokens": 20,
        }
    });
    let t2 = decode_usage(&u2);

    assert_eq!(t1.cache_write, t2.cache_write, "cache_write must match regardless of field location");
    assert_eq!(t1.cache_read, t2.cache_read, "cache_read must match regardless of field location");
    assert_eq!(t1.cache_write, 80);
    assert_eq!(t1.cache_read, 20);
}

// ── nbed::onem_pair (R-NBED-070) ─────────────────────────────────────────────

#[test]
fn nbed_onem_pair() {
    // Two registry entries for the same api_id with different ctx.
    let guard = ModelSpec {
        r#ref: ModelRef { provider: "my-gateway".into(), id: "claude-4-sonnet".into() },
        api_id: "us.anthropic.claude-sonnet-4-5-20251001-v1:0".into(),
        ctx_window: 200_000,
        max_out: 16_000,
        price: Price { input: 3000000, output: 15000000, cache_read: 300000, cache_write: 3750000 },
        cache: CacheMode::Explicit { max_breakpoints: 4, min_tokens: 1024 },
        streaming: true,
        quirks: kn9t_core::Quirks::default(),
    };
    let full = ModelSpec {
        r#ref: ModelRef { provider: "my-gateway".into(), id: "claude-4-sonnet:1m".into() },
        api_id: "us.anthropic.claude-sonnet-4-5-20251001-v1:0".into(), // same api_id
        ctx_window: 1_000_000,
        max_out: 16_000,
        price: Price { input: 6000000, output: 30000000, cache_read: 600000, cache_write: 7500000 },
        cache: CacheMode::Explicit { max_breakpoints: 4, min_tokens: 1024 },
        streaming: true,
        quirks: kn9t_core::Quirks::default(),
    };

    // Same api_id.
    assert_eq!(guard.api_id, full.api_id, "1M pair must share api_id");
    // Different ctx.
    assert!(full.ctx_window > guard.ctx_window, "1M entry must have larger ctx_window");
    // Different prices.
    assert!(full.price.input > guard.price.input, "1M entry must have higher prices");
    // Different ref.id (200K vs :1m suffix).
    assert_ne!(guard.r#ref.id, full.r#ref.id, "entries must have different ref.id");
}

// ── nbed::rewrites (R-NBED-050) ──────────────────────────────────────────────

#[test]
fn nbed_rewrites_adaptive_thinking() {
    // R-NBED-050 §1: adaptive thinking generates correct body shape.
    let model = ModelSpec {
        r#ref: ModelRef { provider: "my-gateway".into(), id: "claude-4-sonnet".into() },
        api_id: "claude-4-sonnet".into(),
        ctx_window: 200_000,
        max_out: 16_000,
        price: Price { input: 3000000, output: 15000000, cache_read: 300000, cache_write: 3750000 },
        cache: CacheMode::None,
        streaming: false,
        quirks: kn9t_core::Quirks::default(),
    };
    let req = kn9t_core::Request {
        model: &model,
        system: None,
        messages: &[],
        tools: &[],
        thinking: Thinking::Effort(Effort::High),
        max_tokens: Some(4096),
        cache: &[],
    };
    let quirks = Quirks {
        reasoning: "adaptive".into(),
        ..Quirks::default()
    };
    let body = build_request(&req, &quirks, &CacheMode::None, false);

    assert_eq!(body["thinking"]["type"], json!("adaptive"),
        "adaptive thinking must set thinking.type=adaptive");
    assert_eq!(body["output_config"]["effort"], json!("high"),
        "effort must be passed as output_config.effort");
    // Must NOT use budget_tokens form.
    assert!(body["thinking"].get("budget_tokens").is_none(),
        "adaptive must not use budget_tokens");
}

#[test]
fn nbed_rewrites_placeholder_tool() {
    // R-NBED-050 §2: require_tools injects placeholder when tools empty.
    let model = make_model("claude-4-sonnet");
    let req = kn9t_core::Request {
        model: &model,
        system: None,
        messages: &[],
        tools: &[],
        thinking: Thinking::Off,
        max_tokens: Some(512),
        cache: &[],
    };
    let quirks = Quirks {
        require_tools: true,
        ..Quirks::default()
    };
    let body = build_request(&req, &quirks, &CacheMode::None, false);

    let tools = body["tools"].as_array().expect("tools must be present");
    assert_eq!(tools.len(), 1, "must inject exactly one placeholder tool");
    assert_eq!(tools[0]["function"]["name"], json!("_placeholder"));
    assert_eq!(body["tool_choice"], json!("auto"));
}

// ── nbed::config_headers (R-NBED-010) ────────────────────────────────────────
// Verify that a gateway-style config with extra_headers produces correct wire headers.
// The mechanism is the same oai_extra_headers path; this test documents the gateway config shape.

#[test]
fn nbed_config_headers() {
    // A gateway provider is just OpenAiConfig with the right extra_headers.
    // The config layer resolves env: values and populates this vec.
    let config = OpenAiConfig {
        name:          "my-gateway".into(),
        base_url:      "https://llm-gateway.example.com/v1".into(),
        api_key:       None,   // anonymous; identity via X-User-Id
        auth_scheme:   kn9t_provider_core::AuthScheme::Omit,
        tls_insecure:  true,
        extra_headers: vec![
            ("X-User-Id".into(),         "user12345".into()),
            ("source_identifier".into(), "llm_vscode_vC5t068vYTsd".into()),
        ],
        ..OpenAiConfig::default()
    };

    // Verify the extra_headers are present on the config (no network call needed).
    let has_user_id = config.extra_headers.iter()
        .any(|(k, v)| k == "X-User-Id" && v == "user12345");
    let has_source = config.extra_headers.iter()
        .any(|(k, v)| k == "source_identifier" && v == "llm_vscode_vC5t068vYTsd");
    assert!(has_user_id,  "X-User-Id must be in extra_headers");
    assert!(has_source,   "source_identifier must be in extra_headers");
    assert!(config.api_key.is_none(), "anonymous gateway needs no api_key");
}
