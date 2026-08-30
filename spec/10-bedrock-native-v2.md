# 10 — `kn9t-provider-bedrock` (native) + `kn9t-provider-gemini` — v2

**Crates:** `kn9t-provider-bedrock`, `kn9t-provider-gemini`
**Depends on:** each on `kn9t-provider-core` (GI-1).
**DESIGN:** §8.5, §8.4.4, §18.12, §18.13.
**Build order:** stage 10 of 10. **Scope: v2.** These are **out of the v1 acceptance gates**
(README §8). The LiteLLM gateway (05) covers Bedrock models in v1, doing
SigV4 server-side; native Bedrock is a performance/independence upgrade, not a capability
gate.

---

# Part A — native Bedrock

## A.1 SigV4 and transport

> **R-BEDN-010 → DESIGN §8.5**
> The provider MUST sign requests with AWS **SigV4** (access key / secret / session token /
> region), without pulling a tokio-based AWS SDK (GI-5). A minimal SigV4 signer over the
> blocking client (PCORE) is preferred; if an AWS crate is used it MUST be a
> blocking/sync-compatible one that does not violate the §15 dependency budget — this
> tradeoff is itself a v2 decision, recorded here as **SPEC-OPEN (BEDN transport)**.

> **R-BEDN-020 → DESIGN §8.5**
> The provider MUST decode the Bedrock streaming response framed as
> `application/vnd.amazon.eventstream` (binary event framing: prelude + headers + payload +
> CRCs), turning each event into `Chunk`s. This binary framing — not JSON SSE — is why it is
> its own crate and Pi's largest file (47 KB, §8.5).
> **Accept (v2):** `cargo test bedn::eventstream_decode` against a recorded binary-frame
> fixture (the R-RPLY fixture format stores the raw bytes verbatim, so binary framing is
> captured unchanged).

## A.2 Cache encoding (native `cachePoint`)

> **R-BEDN-030 → DESIGN §8.4.4**
> Native Bedrock's cache marker is a **separate `cachePoint` element appended to the content
> array**, NOT an attribute on an existing block (§8.4.4). The encoder MUST express it as an
> appended element; an "attribute on the last block" design cannot represent it and is
> forbidden here. Max 4 breakpoints (v2).
> **Accept (v2):** `cargo test bedn::cachepoint_appended`.

## A.3 Usage

> **R-BEDN-040 → DESIGN §8.4.3**
> The provider MUST populate `Tokens.cache_read`/`cache_write` from Bedrock's cache usage
> fields, so tiered cost (STOR R-STOR-070) is correct. Providers reporting neither leave
> them zero (R-CORE-100).

---

# Part B — Gemini

## B.1 Cached content model

> **R-GEM-010 → DESIGN §8.4.4**
> Gemini caching is **not** breakpoint-based: it uses a **separate cached-content resource**,
> TTL-billed, referenced by handle (§8.4.4 table, `CacheMode::None` for the breakpoint path).
> The provider MUST NOT emit `Cache` breakpoints to Gemini; instead it manages a
> cached-content resource lifecycle (create/refer/expire). This is a different model and is
> **SPEC-OPEN (GEM caching)** pending v2 design.

> **R-GEM-020 → DESIGN §8.5**
> The provider MUST map `Request` to Gemini's `generateContent` streaming shape (roles,
> parts, tool/function-call blocks) and decode responses into `Chunk`s, populating `Usage`.
> **Accept (v2):** `cargo test gem::decode` against replay fixtures.

---

# Part C — deferred Anthropic cache options

> **R-BEDN-050 → DESIGN §18.12**
> Anthropic's **1-hour cache TTL** opt-in (2x write cost) is v2. When added it is a per-model
> flag affecting only `price_cache_write` and the marker's TTL attribute; it MUST reuse the
> existing placement (R-CORE-210) with no new code path in core.

> **R-BEDN-060 → DESIGN §18.13**
> Anthropic's top-level **automatic `cache_control`** (self-managing breakpoint consuming one
> of four slots) is v2 and was rejected for v1 because it is a fourth code path (400 on
> legacy Bedrock, unavailable on the custom plugin; §18.13). If adopted it is a per-provider mode, not a
> change to `breakpoints()`.

---

## Stage gate (v2)

> **R-BEDN-900 / R-GEM-900 → DESIGN §8.5**
> Stage 10 is **done (v2)** when: native Bedrock signs with SigV4, decodes eventstream
> framing, and appends `cachePoint`; Gemini maps/decodes and manages cached-content; both
> populate cache-tiered usage; and both pass through the ReAct loop via replay fixtures.
> None of these gate the v1 release; the LiteLLM gateway (05) is the v1 path.
