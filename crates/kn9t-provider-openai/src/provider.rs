//! R-OAI-010 .. R-OAI-050 — the OpenAI-compatible provider.

use kn9t_provider_core::{
    send, with_retry, Backoff, HttpRequest, Quirks, AuthScheme, sse_lines,
    CallId, Cancel, Chunk, ModelRef, ProvErr, Provider, Request, StopReason, Usage,
};
use std::time::Duration;

use crate::decode::DecodeState;
use crate::encode::build_request;

/// R-PCORE-090/R-OAI-010 — config for one OpenAI-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiConfig {
    pub name:            String,
    pub base_url:        String,
    pub api_key:         Option<String>,
    pub auth_scheme:     AuthScheme,
    pub quirks:          Quirks,
    /// Connect timeout (ms).
    pub connect_timeout_ms: u64,
    /// R-PCORE-035: skip TLS cert verification (logs warning on construction).
    pub tls_insecure:    bool,
    /// If true, print request body before sending (R-PCORE-100).
    pub dump_request:    bool,
    /// R-OAI-050: deployment-specific headers injected verbatim on every request.
    /// Resolved by the config layer (stage 06); the provider is deployment-unaware.
    pub extra_headers:   Vec<(String, String)>,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        OpenAiConfig {
            name:            "openai".into(),
            base_url:        "https://api.openai.com/v1".into(),
            api_key:         None,
            auth_scheme:     AuthScheme::Bearer,
            quirks:          Quirks::default(),
            connect_timeout_ms: 20_000,
            tls_insecure:    false,
            dump_request:    false,
            extra_headers:   Vec::new(),
        }
    }
}

/// R-OAI-010 — OpenAI-compatible provider. Covers OpenAI, LiteLLM, Groq, Together,
/// Fireworks, OpenRouter, DeepSeek, xAI, llama.cpp, Ollama, and any configured gateway
/// (§8.5/§8.7). Deployment-specific headers are injected via `extra_headers` (R-OAI-050).
pub struct OpenAiProvider {
    config: OpenAiConfig,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Self {
        if config.tls_insecure {
            eprintln!(
                "[kn9t] WARNING: TLS certificate verification disabled for provider '{}' (R-PCORE-035)",
                config.name
            );
        }
        OpenAiProvider { config }
    }

    /// Build the Authorization header value.
    fn auth(&self) -> Option<(AuthScheme, String)> {
        self.config.api_key.as_ref().map(|k| (self.config.auth_scheme.clone(), k.clone()))
    }

    /// R-OAI-050: build outgoing headers — Content-Type first, then extra_headers verbatim.
    fn build_headers(&self) -> Vec<(String, String)> {
        let mut h = vec![("Content-Type".into(), "application/json".into())];
        h.extend(self.config.extra_headers.iter().cloned());
        h
    }

    /// Make one streaming attempt; returns the SSE chunk iterator.
    fn attempt(
        &self,
        req: &Request<'_>,
        model_ref: ModelRef,
        cancel: Option<Cancel>,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let body = build_request(req, &self.config.quirks, &req.model.cache, self.config.dump_request);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| ProvErr::Connect(format!("serialize: {e}")))?;

        let url = format!("{}/chat/completions", self.config.base_url);
        
        // Log the request for debugging.
        eprintln!("[{}] POST {} model={}", self.config.name, url, req.model.api_id);
        let http_req = HttpRequest {
            method:       "POST".into(),
            url:          url.clone(),
            headers:      self.build_headers(),
            body:         body_bytes,
            auth:         self.auth(),
            tls_insecure: self.config.tls_insecure,
        };

        let timeout = Duration::from_millis(self.config.connect_timeout_ms);
        let resp = send(http_req, timeout, cancel)?;

        if resp.status != 200 {
            return Err(ProvErr::Http {
                status: resp.status,
                body:   String::new(),
            });
        }

        let quirks = self.config.quirks.clone();
        let streaming = quirks.streaming;

        if streaming {
            // SSE streaming path.
            let mut state = DecodeState::new();
            let lines = sse_lines(resp.body);
            let iter = lines.flat_map(move |line_res| {
                match line_res {
                    Err(e)    => vec![Err(ProvErr::Stream(e.to_string()))],
                    Ok(bytes) => {
                        match state.decode(&bytes, &quirks, &model_ref) {
                            Ok(chunks) => chunks.into_iter().map(Ok).collect(),
                            Err(e)     => vec![Err(e)],
                        }
                    }
                }
            });
            Ok(Box::new(iter))
        } else {
            // R-NBED-050 §3: non-streaming — read full response, synthesize chunks.
            let body_str = std::io::read_to_string(resp.body)
                .map_err(|e| ProvErr::Stream(e.to_string()))?;
            let v: serde_json::Value = serde_json::from_str(&body_str)
                .map_err(|e| ProvErr::Decode(e.to_string()))?;
            let chunks = synthesize_chunks(&v, &quirks, &model_ref)?;
            Ok(Box::new(chunks.into_iter().map(Ok)))
        }
    }
}

impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn stream(
        &self,
        req: &Request,
        cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let model_ref = req.model.r#ref.clone();
        let cancel_c = cancel.clone();
        // R-PCORE-060: retry pre-stream errors. Check cancel between retries (instant-cut).
        with_retry(3, Backoff::default(), || {
            if cancel_c.cancelled() {
                return Err(ProvErr::Stream("cancelled".into()));
            }
            self.attempt(req, model_ref.clone(), Some(cancel_c.clone()))
        })
    }
}

/// Synthesize Chunk sequence from a non-streaming response (R-NBED-050 §3).
fn synthesize_chunks(
    v: &serde_json::Value,
    quirks: &Quirks,
    model_ref: &ModelRef,
) -> Result<Vec<Chunk>, ProvErr> {
    let mut chunks = Vec::new();

    // Usage.
    if let Some(usage) = v.get("usage") {
        let tokens = crate::decode::decode_usage(usage);
        chunks.push(Chunk::Usage(Usage { tokens, model: model_ref.clone() }));
    }

    let choices = v.get("choices")
        .and_then(|c| c.as_array())
        .ok_or_else(|| ProvErr::Decode("no choices".into()))?;
    if choices.is_empty() {
        return Err(ProvErr::Decode("empty choices".into()));
    }

    let msg = &choices[0]["message"];
    let mut has_tools = false;

    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            chunks.push(Chunk::Text { idx: 0, delta: text.to_owned() });
        }
    }

    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        has_tools = !tcs.is_empty();
        for (i, tc) in tcs.iter().enumerate() {
            let idx = i as u32;
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_owned();
            let name = tc.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or("").to_owned();
            let args = tc.pointer("/function/arguments").and_then(|a| a.as_str()).unwrap_or("").to_owned();
            chunks.push(Chunk::ToolCall { idx, id: CallId(id), name });
            chunks.push(Chunk::ToolArgs { idx, delta: args });
        }
    }

    let stop_str = choices[0].get("finish_reason").and_then(|r| r.as_str()).unwrap_or("stop");
    let stop = match stop_str {
        "tool_calls" => StopReason::ToolUse,
        "length"     => StopReason::Length,
        _            => if has_tools { StopReason::ToolUse } else { StopReason::Stop },
    };
    chunks.push(Chunk::Stop(stop));

    Ok(chunks)
}
