# 05 — `kn9t-provider-core` + `kn9t-provider-openai`

**Crates:** `kn9t-provider-core`, `kn9t-provider-openai`
**Depends on:** `kn9t-provider-core` → `kn9t-core` (GI-1) plus external `ureq`, `rustls`.
`kn9t-provider-openai` → `kn9t-provider-core` (GI-1). The LiteLLM gateway is a
configured instance of `kind = "openai"` (§8.7); all deployment-specific fields live in the
config file only — `kn9t-provider-openai` is gateway-unaware.
**DESIGN:** §2.1, §8, §8.1, §8.2, §8.3, §8.4, §8.4.4, §8.7.
**Build order:** stage 5 of 10. This is where real network I/O first appears.

`kn9t-provider-core` owns four of the five things every provider needs (wire JSON mapping
is the fifth and lives in each provider): HTTP/TLS, SSE line splitting, delta assembly, and
retry. A concrete provider is expected to be ~250 lines (§2.1).

---

# Part A — `kn9t-provider-core`

## A.1 HTTP + TLS

> **R-PCORE-010 → DESIGN §1, §15**
> The crate MUST use a **blocking** HTTP client (`ureq` + `rustls`), no tokio (GI-5). It
> MUST expose a request builder that sets method, URL, headers, body, and a
> **connect-only** timeout, returning a streaming `impl Read` over the response body.
> ```rust
> pub struct HttpRequest { /* method, url, headers, body */ }
> pub fn send(req: HttpRequest, connect_timeout: Duration)
>     -> Result<HttpResponse, ProvErr>;   // HttpResponse: status + headers + Read body
> ```
> **Accept:** `cargo test pcore::connect_timeout` — a connect to a black-hole address fails
> within the timeout as `ProvErr::Connect`, not after the OS default.

> **R-PCORE-020 → DESIGN §8.6, §8.6.6**
> The connect timeout MUST bound **connection establishment only**. Once response headers
> arrive the timer is cleared and the body streams unbounded (a long generation is never
> cut off; §8.6.6). Default 20 s (`connect_timeout_ms`, **SPEC-OPEN** §8.6).

> **R-PCORE-030 → DESIGN §8.6, §8.7**
> Authorization MUST be data, never hardcoded. An `auth_scheme` selects the header form:
> `"bearer"` → `Authorization: Bearer <k>`; `"token"` → `Authorization: token <k>` (custom plugin,
> §8.6); `"omit"` → send no `Authorization` header (anonymous endpoints, §8.7).
> **Accept:** `cargo test pcore::auth_scheme` — each scheme produces the exact header (or
> none).

> **R-PCORE-035 → DESIGN §8.7.5, GI (tls)**
> TLS certificate verification MUST default to **on**. A provider config `tls_insecure =
> true` (§8.7.5) MAY disable it, but doing so MUST log a startup warning. The default when
> the key is absent is `false` (verification on).
> **Accept:** `cargo test pcore::tls_default_secure`.

## A.2 SSE and assembly

> **R-PCORE-040 → DESIGN §8, §16**
> `sse_lines(r: impl Read) -> impl Iterator<Item = Result<Vec<u8>, io::Error>>` MUST split
> an SSE body into events, correctly buffering across `Read`-boundary splits (a `data:` line
> delivered in two chunks). This is the function the replay `chunks:` annotation
> (R-RPLY-015) exercises.
> **Accept:** `cargo test pcore::sse_boundary` — a mid-`data:` split reassembles identically
> to the whole body.

> **R-PCORE-050 → DESIGN §8, R-CORE-180**
> `assemble` MUST fold a `Chunk` iterator into a final `(Message, Usage, StopReason)`,
> emitting `TextDelta`/`ThinkingDelta`/`ToolArgsDelta` transient events through an
> `EventSink` as it goes, and parsing accumulated tool-arg JSON **once at the end** (never
> re-serializing; R-CORE-062):
> ```rust
> pub fn assemble(
>     chunks: impl Iterator<Item = Result<Chunk, ProvErr>>,
>     sink:   &dyn EventSink,
> ) -> Result<(Message, Usage, StopReason), ProvErr>;
> ```
> A mid-stream `Err` MUST propagate (fatal to the turn, §8.1). The assembled
> `Content::ToolCall.args_json` MUST be the raw concatenation of `ToolArgs` deltas.
> **Accept:** `cargo test pcore::assemble_verbatim_args` — non-sorted-key arg fragments
> concatenate byte-identically into the message.

## A.3 Retry

> **R-PCORE-060 → DESIGN §8.1**
> Retry MUST live **inside** `stream()`, before the first chunk is yielded — connection
> errors and HTTP status codes only (`ProvErr::Connect`, `ProvErr::Http` with 429/5xx). Once
> any chunk has reached the sink, a failure is a **hard turn error**, never a silent retry
> (the client has already rendered output). A `RetryProvider` decorator is explicitly
> forbidden (§8.1).
> ```rust
> pub fn with_retry<F>(max: u32, backoff: Backoff, attempt: F)
>     -> Result<Box<dyn Iterator<Item=Result<Chunk,ProvErr>>+Send>, ProvErr>
>     where F: Fn() -> Result<Box<dyn Iterator<Item=Result<Chunk,ProvErr>>+Send>, ProvErr>;
> ```
> Retry MUST honor a `Retry-After` header when present.
> **Accept:** `cargo test pcore::retry_pre_stream` — a replay fixture returning 429 twice
> then 200 succeeds after two retries; a fixture that fails after yielding one chunk returns
> a hard error with no retry.

## A.4 Cache encoding hooks

> **R-PCORE-070 → DESIGN §8.4, §8.4.4**
> `kn9t-provider-core` MUST provide the *scaffolding* for cache encoding but MUST NOT choose
> breakpoints (placement is core, R-CORE-210). It exposes a helper each provider calls to
> map a `&[Cache]` onto its wire form. The **attach-to** rule differs per provider (§8.4.4)
> and lives in the provider, not here.
> **Accept:** covered per-provider (OAI A.6, custom plugin, ANTH).

## A.5 Quirks type (full)

> **R-PCORE-080 → DESIGN §8.2, §8.3**
> The full `Quirks` field set (partly declared in core, R-CORE-095) MUST be honored here.
> Each field is config data (TOML), never URL-sniffed:
> ```
> max_tokens_field : "max_tokens" | "max_completion_tokens"
> system_role      : "system" | "developer"
> usage_in_stream  : bool          # stream_options.include_usage
> finish_reason    : bool          # present in stream, else infer toolUse
> reasoning        : "reasoning_effort" | "budget_tokens" | "adaptive" | "none"
> tool_result_name : bool
> thinking_style   : "reasoning_content" | "tags" | "none"
> thinking_replay  : "verbatim" | "strip"   # from core
> require_tools    : bool          # inject placeholder tool (NBED §8.7.4)
> extra_body       : table         # e.g. LiteLLM metadata passthrough
> ```
> A `[[model]]` quirk block overrides the provider block, merged field-by-field (§8.3).
> **Accept:** `cargo test pcore::quirks_merge` — a model override replaces exactly its named
> fields and inherits the rest.

## A.6 Model registry and prices

> **R-PCORE-090 → DESIGN §8.2, §6.1**
> Models MUST be hand-written config entries carrying `ctx`, `max_out`, and all four prices
> (`price_in`, `price_out`, `price_cache_read`, `price_cache_write`). A generated
> 400-model registry is rejected (§8.2). `/v1/models`-style discovery MAY list ids but never
> supplies prices (§8.7.3), so prices are always local.
> **Accept:** `cargo test pcore::model_prices_required` — a model entry missing a price
> field fails config load.

> **R-PCORE-100 → DESIGN §8.2**
> A `--dump-request` mode MUST print the exact built request body (post-quirks,
> post-cache-encoding) without sending it, so a wrong quirk flag is diagnosable (a wrong
> flag is a runtime 400, not a compile error; §8.2).

---

# Part B — `kn9t-provider-openai`

Covers OpenAI, LiteLLM, Groq, Together, Fireworks, OpenRouter, DeepSeek, xAI, llama.cpp,
Ollama — they differ only by base URL + quirks (§8.5). A LiteLLM gateway is included as a
configured instance (§8.7).

## B.1 Wire mapping

> **R-OAI-010 → DESIGN §8, §8.2**
> The provider MUST build the OpenAI chat-completions request from `Request`, applying
> quirks: `max_tokens` vs `max_completion_tokens`; `system` vs `developer` role;
> `stream_options.include_usage` when `usage_in_stream`; reasoning field per `reasoning`
> quirk; tool-result `name` when `tool_result_name`. It MUST send only fields named in the
> quirk table — **no passthrough of unknown options** (an unrecognized field 400s a strict
> gateway, §8.7.4).
> Assistant messages containing tool calls MUST encode them as a top-level `tool_calls`
> array (not content parts), with `content: null` when no text accompanies them — this is
> the OpenAI wire contract for the turn-2 position of a tool-call round-trip.
> Tool result messages MUST encode as `role: "tool"` with `tool_call_id` at the top level.
> **Accept:** `cargo test oai::request_shape` — golden request bodies for two quirk profiles;
> `cargo test oai::tool_call_encoding` — an assistant message with a ToolCall produces the
> correct `tool_calls` array, not a content part.

> **R-OAI-020 → DESIGN §8**
> The provider MUST decode the OpenAI SSE stream (`choices[0].delta` text,
> `delta.tool_calls[]`, `reasoning_content` when `thinking_style=reasoning_content`) into
> `Chunk`s. When `finish_reason` is absent from the stream (`finish_reason=false` quirk), it
> MUST infer `StopReason::ToolUse` from whether tool calls appeared, else `Stop`.
> **Accept:** `cargo test oai::decode` against replay fixtures for text, tool-call, and
> reasoning streams.

> **R-OAI-030 → DESIGN §8.2**
> Tool-call correlation: when delta `index` is absent, correlate by `id` (§8.2 quirk table).
> **Accept:** `cargo test oai::toolcall_correlate` — a fixture with id-only correlation
> assembles the right number of calls.

## B.2 Cache encoding (OpenAI family)

> **R-OAI-040 → DESIGN §8.4.4**
> Under `CacheMode::Automatic` (OpenAI proper), the provider MUST send **no** cache fields
> at all — an unrecognized cache field is a 400 risk (§8.4.4). Under `Explicit` (LiteLLM/
> OpenRouter passthrough to Anthropic/Bedrock), it MUST attach `cache_control` ephemeral at
> the position each `Cache` names, at the wire level the upstream expects (part-level for
> OpenRouter, message-level for LiteLLM→Anthropic/Bedrock; §8.4.4 table).
> **Accept:** `cargo test oai::cache_automatic_omits` and `oai::cache_explicit_places`.

## B.3 Extra-headers hook

> **R-OAI-050 → DESIGN §8.2, §8.7**
> `OpenAiConfig` MUST expose an `extra_headers: Vec<(String, String)>` field. Every entry
> is appended verbatim to each outgoing request, after `Content-Type` and `Authorization`.
> The provider MUST NOT contain any deployment-specific logic (no URL sniffing, no
> hard-coded header names). Deployment-specific headers (e.g. `X-User-Id`,
> `source_identifier`) are the responsibility of the **config layer** (stage 06), not the
> provider.
> **Accept:** `cargo test oai::extra_headers` — a config with two extra headers sends both
> on the wire, in order, without duplicating built-in headers.

## B.4 LiteLLM gateway

> **R-NBED-010 → DESIGN §8.7**
> A LiteLLM gateway MUST be configured as `kind = "openai"` pointing at the gateway's `/v1`
> base URL. The Converse endpoint MUST NOT be used — `/v1` exposes prompt-cache counters
> (`cached_tokens`, `cache_creation_input_tokens`) that cost/context tracking need (§8.7).
> Deployment-specific headers (`X-User-Id`, `source_identifier`) are supplied via the config-file
> `[provider.X.headers]` table (R-SRV-CFG-020) and injected through R-OAI-050; the provider
> itself is gateway-unaware.
> **Accept:** `cargo test nbed::config_headers` — a gateway config block with a
> `[provider.my-gateway.headers]` table produces the expected headers on the wire.

> **R-NBED-040 → DESIGN §8.7.3**
> `POST /user/usage {}` returns authoritative server-side spend (`max_budget`, `spend`,
> durations, resets). This is ground truth for `GET /budget` (SRV) and reconciliation
> against local estimates (§8.7.3). `/v1/models` lists ids but no prices (R-PCORE-090).

> **R-NBED-050 → DESIGN §8.7.4 (three rewrites)**
> The provider MUST apply exactly these rewrites, gated by quirk/model:
> 1. **Adaptive thinking** (`reasoning = "adaptive"`, per-model §8.3): send
>    `thinking: { type: "adaptive" }` + `output_config: { effort }` with effort
>    `low|medium|high`; NOT the legacy `thinking: { type: "enabled", budget_tokens }`.
>    Sibling models on the same endpoint may still use `reasoning_effort`.
> 2. **Placeholder tool** (`require_tools = true`): adaptive thinking demands a non-empty
>    `tools` array, so a tool-less call (title, compaction) MUST inject a single never-called
>    placeholder tool with `tool_choice: "auto"`, else 400.
> 3. **Non-streaming models** (`streaming = false`): issue a synchronous request and
>    synthesize the `Chunk` sequence from the complete response, so the `Iterator` contract
>    (R-CORE-190) is unchanged downstream.
> **Accept:** `cargo test nbed::rewrites` — one case per rewrite against a golden body /
> replay fixture.

> **R-NBED-060 → DESIGN §8.7.4**
> The provider MUST read cache counters from whichever field is present
> (`cache_creation_input_tokens` at root or under `prompt_tokens_details`), populating
> `Tokens.cache_write`/`cache_read`. It MUST NOT require the AI-SDK-shaped terminal-usage
> chunk or field-stripping workarounds (§8.7.4: `Chunk::Usage` is content-independent).
> **Accept:** `cargo test nbed::usage_fields` — both field placements decode to the same
> `Tokens`.

> **R-NBED-070 → DESIGN §8.7.5 (1M pair)**
> A 1M-context model MUST be registered **twice**, both pointing at the same `api_id`: a
> 200K guardrail entry (compacts early/cheaply) and a `:1m` entry with the full window and
> its own (higher) prices. The 200K figure is an intentional cost guardrail, not a bug.
> Which entry was used MUST be reflected in the write-time price snapshot (STOR R-STOR-070).
> **Accept:** `cargo test nbed::onem_pair` — two registry entries, same `api_id`, different
> `ctx` and prices.

---

## Stage gate

> **R-PCORE-900 / R-OAI-900 / R-NBED-900 → DESIGN §8, §8.7**
> Stage 5 is **done** when: `sse_lines`/`assemble`/retry pass their boundary and pre-stream
> tests; the OpenAI provider decodes text/tool-call/reasoning streams from replay fixtures;
> cache encoding omits under Automatic and places under Explicit; the extra-headers hook
> passes `oai::extra_headers`; the gateway config-headers test passes; the three
> rewrites, dual-field usage decoding, and the 1M pair all pass; `--dump-request` prints a
> correct body; and the replay provider (02) has been re-pointed at `sse_lines`/`assemble`
> (R-RPLY-070) with all stage-2 fixtures still green. GI-1/GI-5 hold. The provider crate
> contains no deployment-specific logic.
