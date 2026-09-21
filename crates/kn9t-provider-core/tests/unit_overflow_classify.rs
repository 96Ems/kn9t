//! B2 — HTTP error → `ProvErr::ContextOverflow` classification.
//!
//! `ProvErr::ContextOverflow` is load-bearing: it is the only signal that reaches
//! `Attempt::ContextOverflow` and therefore the only way the ReAct loop can react to a
//! prompt that no longer fits (R-RCT-080/090, compaction re-plan). Before this test the
//! variant existed in core and was matched in `kn9t-react/src/exec.rs`, but **no provider
//! ever produced it** — every real "context length exceeded" arrived as
//! `ProvErr::Http { status: 400, .. }`, which the loop classifies as a hard failure. The
//! compaction machinery was unreachable from a real overflow and the session died instead
//! of recovering.
//!
//! The classifier is a pure function so the wire strings the providers actually send can be
//! pinned here, rather than asserted through a socket.

#![allow(clippy::unwrap_used)]

use kn9t_provider_core::retry::is_context_overflow;

/// Every one of these is a real error body from a provider that means "your prompt does not
/// fit". They arrive on 400 (OpenAI-compatible, Anthropic) or 413 (some gateways).
#[test]
fn real_overflow_bodies_classify_as_context_overflow() {
    let cases: &[(u16, &str)] = &[
        // OpenAI / OpenAI-compatible gateways.
        (
            400,
            r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 131204 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        ),
        (
            400,
            r#"{"error":{"message":"context length exceeded","type":"invalid_request_error"}}"#,
        ),
        (
            400,
            r#"{"error":{"message":"Please reduce the length of the messages or completion.","code":"context_length_exceeded"}}"#,
        ),
        // The bare error code, which some gateways return without prose.
        (400, r#"{"error":{"code":"context_length_exceeded"}}"#),
        // Anthropic.
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 205000 tokens > 200000 maximum"}}"#,
        ),
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 199000 + 8000 > 200000"}}"#,
        ),
        // "too many tokens" phrasing.
        (400, r#"{"error":{"message":"too many tokens in request"}}"#),
        // Payload-too-large gateways.
        (
            413,
            r#"{"error":{"message":"maximum context length is 32768 tokens"}}"#,
        ),
        // Case must not matter — gateways rewrite casing freely.
        (400, r#"{"error":{"message":"CONTEXT LENGTH EXCEEDED"}}"#),
        (400, r#"{"error":{"message":"Prompt Is Too Long"}}"#),
        // Local runtimes / self-hosted gateways say "context window" instead of
        // "context length" (llama.cpp, vLLM wrappers, llama_index).
        (
            400,
            r#"{"error":{"message":"The prompt size exceeds the context window size and cannot be processed"}}"#,
        ),
        (
            400,
            r#"{"error":{"message":"input is too long for the context window"}}"#,
        ),
    ];

    for (status, body) in cases {
        assert!(
            is_context_overflow(*status, body),
            "expected ContextOverflow for status={status} body={body}"
        );
    }
}

/// Errors that are emphatically NOT overflow. Misclassifying any of these would send the
/// loop into a compaction re-plan that cannot help — and, worse, would mask a real auth or
/// rate-limit failure behind "context overflow".
#[test]
fn unrelated_errors_do_not_classify_as_context_overflow() {
    let cases: &[(u16, &str)] = &[
        (
            401,
            r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#,
        ),
        (
            429,
            r#"{"error":{"message":"Rate limit reached for gpt-4 in organization org-x","code":"rate_limit_exceeded"}}"#,
        ),
        (500, r#"{"error":{"message":"internal server error"}}"#),
        (
            503,
            r#"{"error":{"message":"The engine is currently overloaded, please try again later"}}"#,
        ),
        (
            400,
            r#"{"error":{"message":"Invalid value for 'tool_choice'","type":"invalid_request_error"}}"#,
        ),
        (
            404,
            r#"{"error":{"message":"The model `gpt-5-turbo` does not exist"}}"#,
        ),
        // "max_tokens" alone is a normal parameter complaint, not an overflow.
        (
            400,
            r#"{"error":{"message":"max_tokens must be greater than 0"}}"#,
        ),
        // "context window" mentioned descriptively, with no overflow claim. Matching the
        // phrase bare would send the loop compacting over an unrelated parameter error.
        (
            400,
            r#"{"error":{"message":"This model has a context window of 8192 tokens; set 'n_ctx' explicitly."}}"#,
        ),
        (400, ""),
    ];

    for (status, body) in cases {
        assert!(
            !is_context_overflow(*status, body),
            "expected NOT ContextOverflow for status={status} body={body}"
        );
    }
}

/// A 200 is never an overflow by status alone — the streaming path handles in-band error
/// frames separately, and treating a success as overflow would loop the turn.
#[test]
fn success_status_is_never_overflow() {
    assert!(!is_context_overflow(
        200,
        r#"{"error":{"message":"context length exceeded"}}"#
    ));
}

/// A rate limit that happens to mention tokens must stay a rate limit: 429 is retryable,
/// overflow is not, and confusing them turns a recoverable throttle into a dead turn.
#[test]
fn token_rate_limit_is_not_overflow() {
    let body = r#"{"error":{"message":"Rate limit reached: you requested too many tokens per minute (TPM)","code":"rate_limit_exceeded"}}"#;
    assert!(!is_context_overflow(429, body));
}
