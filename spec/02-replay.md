# 02 — `kn9t-provider-replay`

**Crate:** `kn9t-provider-replay`
**Depends on:** `kn9t-provider-core` (for the shared parser it exercises) — see note below.
**DESIGN:** §8.5, §16.
**Build order:** stage 2 of 10. Built **before** any real provider so every later stage
has deterministic, zero-cost, zero-key tests (§16).

> **Dependency note.** DESIGN §16 places replay at stage 2 but PCORE (the shared SSE/parse
> code it replays through) at stage 5. This is not a contradiction: the *raw-bytes fixture
> format and the replay harness* are built at stage 2 against a **minimal inline SSE line
> splitter** that later moves into PCORE. When PCORE lands (05), the replay provider is
> re-pointed at `kn9t-provider-core::sse_lines`/`assemble` (R-RPLY-070). Until then it
> depends only on `kn9t-core` (GI-1 preserved: one workspace dep at a time).

The whole point of this crate: a fixture is the **raw bytes a real provider returned**,
replayed through the **genuine parser**. Decoded-`Chunk` fixtures would bypass every
parser and make the three real bug classes untestable (§16): the `delta_tool_calls`
index bug (§8.6.3), token accounting (§8.6.5), and SSE chunk-boundary buffering.

---

## 1. Fixture format

> **R-RPLY-010 → DESIGN §16, §8.5**
> A fixture MUST be a single file with a small text header, a blank-line separator, and
> then the raw response body **verbatim** (bytes as they came off the socket, including
> SSE `data:` framing, partial UTF-8 across chunk boundaries, and CRLF/LF as sent):
> ```
> kind: raw-sse
> status: 200
> content-type: text/event-stream
> note: two parallel tool calls, batched array shape
>
> <raw body bytes follow, unmodified>
> ```
> The header is `key: value` lines; `kind`, `status`, and `content-type` are MUST. The body
> is opaque to the fixture loader — it is not re-encoded, re-framed, or normalized.
> **Accept:** `cargo test rply::header_parse` — round-trips a header and asserts the body
> byte range is untouched.

> **R-RPLY-015 → DESIGN §16**
> Fixtures MUST also support a **chunked delivery** annotation so SSE boundary buffering is
> testable: an optional `chunks:` header lists byte offsets at which the harness splits the
> body into separate `Read` returns, reproducing a network that delivered a single SSE
> event split across two TCP segments.
> ```
> chunks: 0,37,88     # deliver body[0..37], then [37..88], then [88..]
> ```
> Absent ⇒ the whole body is delivered in one read. This is the only mechanism that
> regression-tests mid-`data:`-line buffering.
> **Accept:** `cargo test rply::chunk_boundary` — a fixture whose `chunks` split falls
> mid-`data:` line parses identically to the same fixture delivered whole.

> **R-RPLY-020 → DESIGN §16**
> Fixtures live under `crates/kn9t-provider-replay/fixtures/<provider>/<name>.fixture`.
> They are checked into the repo and MUST contain no secrets: the recorder (R-RPLY-050)
> redacts `Authorization` and any `api_key` before writing. Bodies are model output, not
> credentials, and are retained verbatim.

---

## 2. The replay provider

> **R-RPLY-030 → DESIGN §8.5, R-CORE-190**
> The crate MUST expose a `ReplayProvider` implementing `kn9t_core::Provider`. Constructed
> from a fixture (or a directory + name), its `stream()` MUST:
> - present the fixture body to the **same parser the real provider of that `kind` uses**
>   (once PCORE/providers exist), yielding the identical `Chunk` sequence a live call would;
> - honor the `chunks:` split so boundary bugs surface;
> - map a non-2xx `status` header to the same `ProvErr` a live call would produce;
> - ignore `Request` contents except where a fixture is explicitly parameterized (it
>   replays a recording; it does not synthesize responses to arbitrary prompts).
> ```rust
> pub struct ReplayProvider { /* kind, header, body, chunks */ }
> impl ReplayProvider {
>     pub fn from_file(path: &Path) -> Result<Self, ProvErr>;
>     pub fn from_fixture(provider: &str, name: &str) -> Result<Self, ProvErr>;
> }
> impl Provider for ReplayProvider { /* name() = header.kind */ }
> ```
> **Accept:** `cargo test rply::yields_expected_chunks` — a known fixture yields a
> `Vec<Chunk>` asserted element-by-element.

> **R-RPLY-035 → DESIGN §8.1**
> When the fixture `status` is 429 or 5xx, `ReplayProvider::stream` MUST return the
> pre-stream `ProvErr` **without** yielding chunks, so retry logic (PCORE §8.1) can be
> tested deterministically. A fixture MAY carry `retry-after:` to exercise backoff parsing.

> **R-RPLY-040 → DESIGN §8.6.6**
> The crate MUST ship fixtures covering each `ProvErr` classification path so RCT's
> truncation policy (03) and the compaction trigger (04) are testable offline:
> a clean `context deadline exceeded` → `StopReason::Length`; raw `prompt is too long` →
> `ContextOverflow`; a stream ending with an unfinished tool call and no `stopReason` →
> `Truncated`; an `ECONNRESET`-shaped truncation of a >50 KB body → `Truncated`.

---

## 3. The recorder

> **R-RPLY-050 → DESIGN §16**
> A `--record` capability MUST capture live provider responses into the fixture format.
> It wraps an inner `Provider`, tees the raw socket bytes to a fixture file (with a header
> filled from the live request/response metadata), and passes chunks through unchanged. It
> records **raw bytes**, before any parsing.
> ```rust
> pub struct RecordingProvider<'a> { inner: &'a dyn Provider, out_dir: PathBuf }
> ```
> The recorder MUST redact `Authorization` and `api_key` (R-RPLY-020). It MUST NOT alter the
> response body.
> **Accept:** manual/integration — record a real call (behind a feature flag / env-gated
> key), then replay the produced fixture and assert byte-identical chunk output.

---

## 4. Re-pointing at PCORE

> **R-RPLY-070 → DESIGN §16, §2.1**
> When stage 5 (`kn9t-provider-core`) exists, the replay provider MUST route its bytes
> through `kn9t_core`… no: through **`kn9t-provider-core::sse_lines` and the per-`kind`
> parser**, replacing the stage-2 inline splitter. After this change, a fixture that passed
> at stage 2 MUST still pass unchanged — proving the inline splitter and the real one agree.
> This is the mechanism by which every provider added later (05, 09, 10) inherits a
> zero-cost regression suite for free: add a fixture, assert its chunk output.
> **Accept:** the stage-2 `rply::*` tests pass again after re-pointing, with no fixture
> edits.

---

## 5. Stage gate

> **R-RPLY-900 → DESIGN §16**
> Stage 2 is **done** when: `ReplayProvider` implements `Provider`; the fixture format
> (header + verbatim body + `chunks` split) loads and replays; the recorder produces a
> replayable fixture; the `ProvErr`-classification fixtures exist; and `cargo test -p
> kn9t-provider-replay` is green with no network access and no API key present in the
> environment. GI-1 holds (one workspace dep).
