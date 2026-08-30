# 09 — `kn9t-anthropic` (provider plugin)

**Binary:** `internal-plugins/kn9t-anthropic` (bundled)
**Depends on:** `kn9t-plugin-sdk` only (zero workspace deps — GI-1 satisfied by design).
**Host bridge:** `RemoteProvider` in `kn9t-plugin` (its one workspace dep is `kn9t-core`).
**DESIGN:** §8.5, §8.4.2, §8.4.3, §8.4.4, §13.8, Q26, Q31.
**Build order:** stage 9 of 10.

**Decision (Q26/Q31):** providers ship as subprocess plugin binaries using
`kn9t-plugin-sdk`'s `PluginProvider` trait. The host sends a `hook:"provider_complete"`
call carrying the serialised `Request`; the plugin streams `chunk` messages (one per
`kn9t_core::Chunk` variant) then a final `done` with stop + usage. `RemoteProvider` adapts
this into the `Provider` trait the ReAct loop expects. Benefits: hot-reload, no workspace
dep bloat, and the full provider-plugin code path is exercised in production.

Anthropic is **not** OpenAI-shaped. Every requirement below is a **silent failure** if
wrong — that is the reason to specify it.

> **Vendor-specific providers live in their own repositories.** Only `kn9t-anthropic` is
> bundled here, to keep one worked example of a non-OpenAI provider plugin in-tree. Any
> other provider — including partner or gateway-specific ones — is an *external* plugin: a
> standalone crate outside this workspace, registered through `~/.kn9t/config.toml` with an
> absolute `binary` path. See `plugins/README.md` and R-CP-005-style externality rules (external plugin pattern).

---

## 1. Protocol

> **R-ANTH-010 → DESIGN §8.5, §8**
> The provider MUST implement Anthropic's Messages API: `content_block_delta` /
> `input_json_delta` decoded into `Chunk`s (text, thinking, tool-call, tool-args), `usage`
> into `Tokens`, and stop reasons into `StopReason`. Auth via `auth_scheme` (x-api-key
> header form as Anthropic requires) plus the `anthropic-version` header.
> **Accept:** `cargo test anth::decode` against replay fixtures for text, thinking, and
> tool-call streams.

## 2. Thinking signatures

> **R-ANTH-020 → DESIGN §4.2, §8.4.2**
> During tool loops the provider MUST replay prior `Content::Thinking` blocks **verbatim,
> with `signature` intact** (`thinking_replay = verbatim`). An altered signature is a 400,
> and a replayed block counts as input tokens when read from cache. Stripping thinking
> silently changes the prefix and invalidates the message cache every turn (§4.2).
> **Accept:** `cargo test anth::thinking_verbatim` — a replayed thinking block's signature
> is byte-identical to what was received.

## 3. Cache encoding (message-level)

> **R-ANTH-030 → DESIGN §8.4.4, §8.4.1**
> The provider MUST attach `"cache_control": {"type":"ephemeral"}` at **message level**
> (§8.4.4 table) for each `Cache` position in `Request::cache`. It MUST treat the `&[Cache]`
> slice as **priority order, not positional** (R-CORE-210, §8.4.1): each entry is placed
> independently; the encoder MUST NOT assume a monotonic walk over content blocks.
> **Accept:** `cargo test anth::cache_priority_order` — the `[assistant, user]` case
> (breakpoints `[System, AfterMessage(1), AfterMessage(0)]`) attaches markers to the correct
> messages despite descending positions.

> **R-ANTH-040 → DESIGN §8.4.2.2, §8.4.3**
> The provider MUST honor per-model `min_tokens` from the model's `CacheMode::Explicit`
> (512 Opus 5 … 4096 Haiku 4.5, §8.4.2.2), placing at most `max_breakpoints` (4) markers. It
> MUST report `cache_creation_input_tokens` and `cache_read_input_tokens` into
> `Tokens.cache_write`/`cache_read` so the tiered cost (STOR R-STOR-070) is correct;
> `input_tokens` is the after-breakpoint remainder (§8.4.3), not the full prompt.
> **Accept:** `cargo test anth::usage_partition` — `input + cache_read + cache_write` equals
> the real context, and a cache-effective turn has small `input`.


## 4. Stage gate

> **R-ANTH-900 → DESIGN §8.5**
> Stage 9 is **done** when Anthropic passes decode, verbatim thinking signatures,
> priority-order message-level cache placement, and the usage partition; and it is
> exercised end-to-end through the ReAct loop via replay fixtures. GI-1/GI-5 hold.
>
> External provider plugins carry their own gates in their own repositories; they are not
> part of this repository's acceptance criteria.
