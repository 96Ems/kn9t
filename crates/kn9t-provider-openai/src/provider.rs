//! R-OAI-010 .. R-OAI-050 — the OpenAI-compatible provider.

use kn9t_provider_core::{
    send, Backoff, HttpRequest, Quirks, AuthScheme, sse_lines,
    CallId, Cancel, Chunk, ModelRef, ProvErr, Provider, Request, Usage,
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
    /// Per-model quirk overrides, keyed by `ModelRef::id` (DESIGN 8.3).
    ///
    /// One gateway commonly fronts models that disagree on wire details -- one
    /// wants `reasoning = "adaptive"`, another `"none"`; one cannot stream. The
    /// provider is instantiated once per provider *name* and resolved that way
    /// (`get_provider(&model.r#ref.provider)`), so a second provider instance is
    /// not an option: it would have to be registered under a different name, and
    /// that name is what lands in `ModelRef`, `ModelChanged` events, and the DB.
    ///
    /// So the override travels with the config and is applied per request via
    /// `quirks_for`. Absent an entry the provider-level `quirks` are used
    /// unchanged, which is the common case.
    pub model_quirks:    std::collections::HashMap<String, Quirks>,
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
            model_quirks:    std::collections::HashMap::new(),
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

    /// Quirks in effect for one request: the per-model override when the config
    /// declared one, else the provider-level set (DESIGN 8.3).
    ///
    /// Keyed on `ModelRef::id` -- the stable config-facing id -- not `api_id`,
    /// which is the wire name and may repeat across entries.
    pub fn quirks_for(&self, model: &ModelRef) -> &Quirks {
        self.config
            .model_quirks
            .get(&model.id)
            .unwrap_or(&self.config.quirks)
    }

    /// Make one streaming attempt; returns the SSE chunk iterator.
    fn attempt(
        &self,
        req: &Request<'_>,
        model_ref: ModelRef,
        cancel: Option<Cancel>,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let body = build_request(req, self.quirks_for(&req.model.r#ref), &req.model.cache, self.config.dump_request);
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
        let mut resp = send(http_req, timeout, cancel)?;

        if resp.status != 200 {
            let body = std::io::read_to_string(&mut resp.body).unwrap_or_default();
            // Truncate to 4k for log/error but keep full error visible.
            let snippet = if body.len() > 4000 { format!("{}…", &body[..4000]) } else { body.clone() };
            eprintln!("[{}] HTTP {} body: {}", self.config.name, resp.status, snippet);
            return Err(ProvErr::Http {
                status: resp.status,
                body,
            });
        }

        let quirks = self.quirks_for(&req.model.r#ref).clone();
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
        self.stream_with_sink(req, cancel, None)
    }

    fn stream_with_sink(
        &self,
        req: &Request,
        cancel: &Cancel,
        sink: Option<&dyn kn9t_provider_core::EventSink>,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        let model_ref = req.model.r#ref.clone();
        let cancel_c = cancel.clone();
        // R-PCORE-060: retry pre-stream errors. Check cancel between retries (instant-cut).
        // With sink, emit RetryAttempt before each sleep so TUI shows retry instead of silent spinner.
        kn9t_provider_core::with_retry_with_sink(3, Backoff::default(), sink, || {
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

    // Reasoning / thinking content, gated on the same quirk the streaming path
    // honours (decode.rs). Without this a `streaming = false` provider silently
    // dropped every thinking block that the streaming path would have emitted.
    if quirks.thinking_style == "reasoning_content" {
        if let Some(text) = msg.get("reasoning_content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                chunks.push(Chunk::Thinking { idx: 0, delta: text.to_owned() });
            }
        }
    }

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
    // Reuse the streaming path's mapping rather than a second inline copy: this one
    // had drifted (no content_filter -> Refusal, and it ignored quirks.finish_reason,
    // so a gateway that always reports "stop" mapped tool calls to Stop).
    let stop = crate::decode::decode_stop(stop_str, has_tools, quirks);
    chunks.push(Chunk::Stop(stop));

    Ok(chunks)
}
