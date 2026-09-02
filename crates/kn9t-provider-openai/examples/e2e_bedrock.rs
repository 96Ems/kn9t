//! E2E smoke test: OpenAI-compatible gateway — text turn + full tool-call round trip.
//!
//! Turn 1: "say pong"  → text response.
//! Turn 2: "7 * 6?"   → model calls `calculator` tool → we return 42 → model answers.
//!
//! Usage:
//!   cargo run -p kn9t-provider-openai --example e2e_bedrock
//!
//! Configure via env vars:
//!   E2E_BASE_URL   — gateway base URL  (default: http://localhost:8080/v1)
//!   E2E_API_KEY    — API key, optional (default: empty)
//!   E2E_MODEL_ID   — model id          (default: claude-haiku-4-5)
//!   E2E_USER_ID    — X-User-Id header  (optional, omitted if unset)

use kn9t_core::{
    Cancel, CacheMode, Content, Message, ModelRef, ModelSpec,
    MsgId, Price, Provider, Role, StopReason, Thinking, ToolSpec,
};
use kn9t_provider_core::{assemble, AuthScheme, Quirks};
use kn9t_provider_openai::{OpenAiConfig, OpenAiProvider};
use serde_json::json;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_provider(base_url: &str, api_key: Option<&str>, user_id: Option<&str>) -> OpenAiProvider {
    let extra_headers = if let Some(uid) = user_id {
        vec![("X-User-Id".into(), uid.to_owned())]
    } else {
        vec![]
    };
    let auth_scheme = if api_key.is_some() {
        AuthScheme::Bearer
    } else {
        AuthScheme::Omit
    };
    OpenAiProvider::new(OpenAiConfig {
        name:         "gateway".into(),
        base_url:     base_url.to_owned(),
        api_key:      api_key.map(str::to_owned),
        auth_scheme,
        tls_insecure: false,
        quirks: Quirks {
            finish_reason:   true,
            usage_in_stream: true,
            streaming:       true,
            ..Quirks::default()
        },
        extra_headers,
        ..OpenAiConfig::default()
    })
}

fn make_model(_base_url: &str, model_id: &str) -> ModelSpec {
    ModelSpec {
        r#ref: ModelRef { provider: "gateway".into(), id: model_id.to_owned() },
        api_id:     model_id.to_owned(),
        ctx_window: 200_000,
        max_out:    512,
        price: Price { input: 800000, output: 4000000, cache_read: 80000, cache_write: 1000000 },
        cache:   CacheMode::None,
        streaming: true,
        quirks:  kn9t_core::Quirks::default(),
    }
}

struct NullSink;
impl kn9t_core::EventSink for NullSink {
    fn emit(&self, _: kn9t_core::LiveEvent) {}
}

fn stream_and_assemble(
    provider: &OpenAiProvider,
    model:    &ModelSpec,
    system:   &str,
    messages: &[Message],
    tools:    &[ToolSpec],
    label:    &str,
) -> kn9t_provider_core::AssembleResult {
    let cancel = Cancel::new();
    let req = kn9t_core::Request {
        model,
        system: Some(system),
        messages,
        tools,
        thinking:   Thinking::Off,
        max_tokens: Some(256),
        cache:      &[],
    };
    let chunks = provider.stream(&req, &cancel)
        .unwrap_or_else(|e| { eprintln!("[{label}] FAIL stream: {e:?}"); std::process::exit(1); });
    assemble(chunks, &NullSink)
        .unwrap_or_else(|e| { eprintln!("[{label}] FAIL assemble: {e:?}"); std::process::exit(1); })
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    let base_url = std::env::var("E2E_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080/v1".into());
    let api_key  = std::env::var("E2E_API_KEY").ok()
        .filter(|s| !s.is_empty());
    let model_id = std::env::var("E2E_MODEL_ID")
        .unwrap_or_else(|_| "claude-haiku-4-5".into());
    let user_id  = std::env::var("E2E_USER_ID").ok()
        .filter(|s| !s.is_empty());

    println!("[e2e-bedrock] base_url: {base_url}");
    println!("[e2e-bedrock] model:    {model_id}");

    let provider = make_provider(&base_url, api_key.as_deref(), user_id.as_deref());
    let model    = make_model(&base_url, &model_id);

    // ── Turn 1: plain text ────────────────────────────────────────────────────
    println!("\n[e2e-bedrock] ── turn 1: plain text ──");
    let user_msg = Message {
        id:      MsgId::new(),
        role:    Role::User,
        content: vec![Content::Text { text: "say pong".into() }], silent: false
    };
    let r1 = stream_and_assemble(
        &provider, &model,
        "You are a terse assistant. Respond in one word.",
        &[user_msg],
        &[],
        "turn1",
    );
    let t1_text = r1.message.content.iter()
        .filter_map(|c| if let Content::Text { text } = c { Some(text.as_str()) } else { None })
        .collect::<String>();
    println!("[e2e-bedrock] response: {t1_text:?}");
    println!("[e2e-bedrock] usage:    in={} out={}",
        r1.usage.tokens.input, r1.usage.tokens.output);
    assert!(!t1_text.is_empty(), "turn 1 must return text");
    assert!(matches!(r1.stop, StopReason::Stop), "turn 1 must stop normally");
    println!("[e2e-bedrock] turn 1 PASS");

    // ── Turn 2: tool call → tool result → final answer ────────────────────────
    println!("\n[e2e-bedrock] ── turn 2: tool call round-trip ──");

    let calculator = ToolSpec {
        name:        "calculator".into(),
        description: "Evaluates a simple arithmetic expression and returns the numeric result.".into(),
        schema: json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "The arithmetic expression to evaluate, e.g. \"7 * 6\""
                }
            },
            "required": ["expression"]
        }), hidden: false, effects: vec![], policy: Default::default()
    };

    // Turn 2a: user asks → expect ToolUse stop.
    let user_q = Message {
        id:      MsgId::new(),
        role:    Role::User,
        content: vec![Content::Text { text: "What is 7 * 6? Use the calculator tool.".into() }], silent: false
    };
    let r2a = stream_and_assemble(
        &provider, &model,
        "You are a precise assistant. Always use the calculator tool for arithmetic.",
        &[user_q.clone()],
        &[calculator.clone()],
        "turn2a",
    );
    assert!(matches!(r2a.stop, StopReason::ToolUse), "model must call a tool");

    // Extract the tool call from the assembled message.
    let tool_call = r2a.message.content.iter()
        .find_map(|c| if let Content::ToolCall { id, name, args_json } = c {
            Some((id.clone(), name.clone(), args_json.clone()))
        } else { None })
        .unwrap_or_else(|| { eprintln!("[turn2a] FAIL: no ToolCall in message"); std::process::exit(1); });

    let (call_id, tool_name, args_json) = tool_call;
    println!("[e2e-bedrock] tool call: {tool_name}({args_json})");

    // Parse the expression and evaluate locally.
    let expr = serde_json::from_str::<serde_json::Value>(&args_json)
        .ok()
        .and_then(|v| v.get("expression").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_else(|| { eprintln!("[turn2a] FAIL: cannot parse args"); std::process::exit(1); });

    // Simple inline evaluator for the expected "7 * 6" expression.
    let result = evaluate(&expr);
    println!("[e2e-bedrock] tool result: {expr} = {result}");

    // Turn 2b: send back [user, assistant(tool_call), tool_result] → final text.
    let assistant_msg = r2a.message;  // contains the ToolCall content

    let tool_result_msg = Message {
        id:   MsgId::new(),
        role: Role::Tool,
        content: vec![Content::ToolResult {
            id:       call_id,
            content:  vec![Content::Text { text: result.to_string() }],
            is_error: false,
        }], silent: false
    };

    let r2b = stream_and_assemble(
        &provider, &model,
        "You are a precise assistant. Always use the calculator tool for arithmetic.",
        &[user_q, assistant_msg, tool_result_msg],
        &[calculator],
        "turn2b",
    );
    let t2_text = r2b.message.content.iter()
        .filter_map(|c| if let Content::Text { text } = c { Some(text.as_str()) } else { None })
        .collect::<String>();
    println!("[e2e-bedrock] final answer: {t2_text:?}");
    println!("[e2e-bedrock] usage:        in={} out={}",
        r2b.usage.tokens.input, r2b.usage.tokens.output);

    assert!(!t2_text.is_empty(), "turn 2b must return text");
    assert!(t2_text.contains("42"), "answer must contain 42; got: {t2_text:?}");
    assert!(matches!(r2b.stop, StopReason::Stop), "turn 2b must stop normally");
    println!("[e2e-bedrock] turn 2 PASS");

    println!("\n[e2e-bedrock] ALL PASS");
}

/// Minimal arithmetic evaluator for a * b expressions.
fn evaluate(expr: &str) -> i64 {
    let expr = expr.trim();
    if let Some((a, b)) = expr.split_once('*') {
        let a: i64 = a.trim().parse().unwrap_or(0);
        let b: i64 = b.trim().parse().unwrap_or(0);
        return a * b;
    }
    if let Some((a, b)) = expr.split_once('+') {
        let a: i64 = a.trim().parse().unwrap_or(0);
        let b: i64 = b.trim().parse().unwrap_or(0);
        return a + b;
    }
    if let Some((a, b)) = expr.split_once('-') {
        let a: i64 = a.trim().parse().unwrap_or(0);
        let b: i64 = b.trim().parse().unwrap_or(0);
        return a - b;
    }
    expr.parse().unwrap_or(0)
}
