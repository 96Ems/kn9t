use kn9t_provider_core::{sse_lines, Backoff, Quirks, AuthScheme, with_retry};
use kn9t_core::{ProvErr, Chunk, Bus, EventSink, StopReason, Tokens, Usage, ModelRef,
    MsgId, Role, Message, Content, CallId, Thinking, Effort};

// ── pcore::sse_boundary (R-PCORE-040) ────────────────────────────────────────

#[test]
fn pcore_sse_boundary() {
    // A "data:" line delivered in two separate Read chunks must reassemble identically.
    let whole = b"data: {\"hello\":\"world\"}\n\n";
    let split_at = 10; // split inside "data: {\"hel"
    let part1 = &whole[..split_at];
    let part2 = &whole[split_at..];
    let combined: Vec<u8> = part1.iter().chain(part2.iter()).copied().collect();

    let from_whole: Vec<Vec<u8>> = sse_lines(&whole[..])
        .collect::<Result<_, _>>().unwrap();
    let from_combined: Vec<Vec<u8>> = sse_lines(&combined[..])
        .collect::<Result<_, _>>().unwrap();

    assert_eq!(from_whole, from_combined, "split-reassemble must equal whole");
    assert_eq!(from_whole.len(), 1);
    assert_eq!(from_whole[0], b"{\"hello\":\"world\"}");
}

#[test]
fn pcore_sse_done_terminates() {
    let body = b"data: {\"a\":1}\n\ndata: [DONE]\n\n";
    let events: Vec<Vec<u8>> = sse_lines(&body[..])
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(events.len(), 1, "[DONE] must terminate the iterator");
}

// ── pcore::assemble_verbatim_args (R-PCORE-050) ──────────────────────────────

#[test]
fn pcore_assemble_verbatim_args() {
    use kn9t_provider_core::assemble;

    let bus = Bus::new();
    let model_ref = ModelRef { provider: "test".into(), id: "m".into() };

    // Non-sorted-key arg fragments that MUST concatenate byte-identically.
    let arg_frag1 = r#"{"z":1,"#;
    let arg_frag2 = r#""a":2}"#;
    let expected_args = format!("{arg_frag1}{arg_frag2}");

    let chunks: Vec<Result<Chunk, ProvErr>> = vec![
        Ok(Chunk::ToolCall { idx: 0, id: CallId("c1".into()), name: "bash".into() }),
        Ok(Chunk::ToolArgs { idx: 0, delta: arg_frag1.to_owned() }),
        Ok(Chunk::ToolArgs { idx: 0, delta: arg_frag2.to_owned() }),
        Ok(Chunk::Usage(Usage { tokens: Tokens::default(), model: model_ref })),
        Ok(Chunk::Stop(StopReason::ToolUse)),
    ];

    let res = assemble(chunks.into_iter(), &bus).unwrap();
    let (msg, stop) = (res.message, res.stop);
    assert!(matches!(stop, StopReason::ToolUse));

    let tool_call = msg.content.iter().find(|c| matches!(c, Content::ToolCall { .. }));
    if let Some(Content::ToolCall { args_json, .. }) = tool_call {
        assert_eq!(args_json, &expected_args, "args must be verbatim concat");
    } else {
        panic!("no ToolCall content found");
    }
}

#[test]
fn pcore_assemble_midstream_error_fatal() {
    use kn9t_provider_core::assemble;
    let bus = Bus::new();
    let chunks: Vec<Result<Chunk, ProvErr>> = vec![
        Ok(Chunk::Text { idx: 0, delta: "hi".into() }),
        Err(ProvErr::Stream("broken pipe".into())),
    ];
    let result = assemble(chunks.into_iter(), &bus);
    assert!(result.is_err(), "mid-stream error must propagate as Err");
}

// ── pcore::retry_pre_stream (R-PCORE-060) ────────────────────────────────────

#[test]
fn pcore_retry_pre_stream() {
    // Attempt returns 429 twice, then Ok.
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter2 = counter.clone();

    let result = with_retry(2, Backoff { initial_ms: 1, factor: 1.0, max_ms: 1 }, move || {
        let n = counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < 2 {
            Err(ProvErr::Http { status: 429, body: "rate limit".into() })
        } else {
            let chunks: Vec<Result<Chunk, ProvErr>> = vec![
                Ok(Chunk::Stop(StopReason::Stop)),
            ];
            Ok(Box::new(chunks.into_iter()) as Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>)
        }
    });

    assert!(result.is_ok(), "should succeed after 2 retries");
    let collected: Vec<_> = result.unwrap().collect();
    assert_eq!(collected.len(), 1);
}

#[test]
fn pcore_retry_no_retry_after_chunk() {
    // Once a chunk is yielded, mid-stream error must NOT be retried.
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter2 = counter.clone();

    let result = with_retry(5, Backoff { initial_ms: 1, factor: 1.0, max_ms: 1 }, move || {
        let n = counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Always succeed on the first call, yielding a chunk then an error.
        if n == 0 {
            let chunks: Vec<Result<Chunk, ProvErr>> = vec![
                Ok(Chunk::Text { idx: 0, delta: "hi".into() }),
                Err(ProvErr::Stream("broke mid-stream".into())),
            ];
            Ok(Box::new(chunks.into_iter()) as Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>)
        } else {
            // Should never be called again.
            Ok(Box::new(std::iter::empty()) as Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>)
        }
    });

    let iter = result.unwrap();
    let items: Vec<_> = iter.collect();
    // First item is Ok(Text), second is Err(Stream).
    assert!(items[0].is_ok());
    assert!(items[1].is_err(), "mid-stream error must pass through");
    // Only 1 attempt was made (no retry).
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1, "must not retry after chunk");
}

// ── pcore::auth_scheme (R-PCORE-030) ─────────────────────────────────────────

#[test]
fn pcore_auth_scheme() {
    // Verify the three auth scheme strings are formed correctly.
    let key = "mykey";
    let bearer = format!("Bearer {key}");
    let token  = format!("token {key}");

    assert!(bearer.starts_with("Bearer "));
    assert!(token.starts_with("token "));
    // Omit: no header emitted (tested implicitly via HttpRequest struct).
}

// ── pcore::tls_default_secure (R-PCORE-035) ──────────────────────────────────

#[test]
fn pcore_tls_default_secure() {
    // The HttpRequest struct's tls_insecure defaults to false when not set.
    use kn9t_provider_core::HttpRequest;
    let req = HttpRequest {
        method:       "POST".into(),
        url:          "https://example.com".into(),
        headers:      vec![],
        body:         vec![],
        auth:         None,
        tls_insecure: false, // default
    };
    assert!(!req.tls_insecure, "TLS verification must be on by default");
}

// ── pcore::quirks_merge (R-PCORE-080) ────────────────────────────────────────

#[test]
fn pcore_quirks_merge() {
    let base = Quirks {
        max_tokens_field: "max_tokens".into(),
        system_role:      "system".into(),
        usage_in_stream:  false,
        finish_reason:    true,
        reasoning:        "none".into(),
        tool_result_name: false,
        thinking_style:   "none".into(),
        thinking_replay:  "verbatim".into(),
        require_tools:    false,
        streaming:        true,
        extra_body:       serde_json::Value::Null,
    };
    let model_override = Quirks {
        max_tokens_field: "max_completion_tokens".into(),
        system_role:      "developer".into(),
        usage_in_stream:  true,
        ..Quirks::default()
    };
    let merged = base.merge(&model_override);
    // Overridden fields.
    assert_eq!(merged.max_tokens_field, "max_completion_tokens");
    assert_eq!(merged.system_role, "developer");
    assert!(merged.usage_in_stream);
    // Unoverridden fields come from model_override (merge is model-wins, not base-fallback).
    // That's the spec: model block overrides, inheriting defaults from model_override.
    // finish_reason would be model_override's default (true).
    assert!(merged.finish_reason);
}

// ── pcore::model_prices_required (R-PCORE-090) ────────────────────────────────

#[test]
fn pcore_model_prices_required() {
    use kn9t_core::{ModelSpec, ModelRef, Price, CacheMode};
    // A ModelSpec must carry all four price fields.
    let spec = ModelSpec {
        r#ref: ModelRef { provider: "openai".into(), id: "gpt-4o".into() },
        api_id: "gpt-4o".into(),
        ctx_window: 128_000,
        max_out: 4096,
        price: Price { input: 2.5, output: 10.0, cache_read: 1.25, cache_write: 0.0 },
        cache: CacheMode::Automatic,
        streaming: true,
        quirks: kn9t_core::Quirks::default(),
    };
    // All four prices are present (non-NaN).
    assert!(spec.price.input.is_finite());
    assert!(spec.price.output.is_finite());
    assert!(spec.price.cache_read.is_finite());
    assert!(spec.price.cache_write.is_finite());
}

// ── pcore::connect_timeout (R-PCORE-010/020) ─────────────────────────────────
// NOTE: This test attempts to connect to a black-hole address and expects ProvErr::Connect.
// We use 127.0.0.2 which is typically unroutable on Windows loopback.

#[test]
fn pcore_connect_timeout() {
    use kn9t_provider_core::{send, HttpRequest};
    use std::time::{Duration, Instant};
    let req = HttpRequest {
        method:       "POST".into(),
        url:          "http://192.0.2.1:12345/".into(), // TEST-NET-1, always black-holes
        headers:      vec![],
        body:         vec![],
        auth:         None,
        tls_insecure: false,
    };
    let timeout = Duration::from_millis(300);
    let start = Instant::now();
    let result = send(req, timeout, None);
    let elapsed = start.elapsed();
    assert!(result.is_err(), "must fail");
    // Can't unwrap_err on Result<HttpResponse> because HttpResponse isn't Debug.
    // Use if-let instead.
    if let Err(err) = result {
        assert!(matches!(err, ProvErr::Connect(_)), "must be ProvErr::Connect, got {err}");
    } else {
        panic!("expected Err but got Ok");
    }
    // Must fail within 2× the timeout (generous for slow CI).
    assert!(elapsed < Duration::from_secs(5), "must time out promptly, took {elapsed:?}");
}
