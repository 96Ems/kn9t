//! [`ReplayProvider`] — a [`kn9t_provider_core::Provider`] backed by a fixture (R-RPLY-030/035).
//!
//! It presents the fixture body to a parser, honoring the `chunks:` delivery split, and
//! yields the identical `Chunk` sequence a live call would. For a non-2xx `status` it
//! returns the pre-stream `ProvErr` a live call would produce, without yielding chunks
//! (R-RPLY-035), so retry logic (05) is testable offline.
//!
//! ## Two fixture families
//! - **`kind: replay`** — the stage-2 *native* format: the body is one `Chunk`-JSON
//!   object per SSE `data:` event. This is what stages 03/04 consume to drive the loop
//!   deterministically before any real parser exists. R-CORE-180 explicitly sanctions
//!   decoded-chunk fixtures for the replay crate's own use.
//! - **raw real-provider bytes** (`kind: raw-sse`, `openai`, ...) — retained
//!   verbatim and parsed through the *genuine* per-`kind` parser once PCORE/providers
//!   land (R-RPLY-070). At stage 2 there is no such parser, so replaying a raw fixture
//!   of a not-yet-implemented kind is a `ProvErr::Decode` rather than a wrong guess.

use std::fs;
use std::path::{Path, PathBuf};

use kn9t_provider_core::{Cancel, Chunk, ProvErr, Provider, Request, sse_lines};

use crate::fixture::Fixture;
// SegmentedReader stays local — a replay-specific delivery helper. `data_events`
// is not needed here (see the note at the SegmentedReader call below: each yielded
// Vec<u8> is already the JSON payload); it remains a public re-export from lib.rs.
// sse_lines is now the REAL kn9t-provider-core implementation (R-RPLY-070).
use crate::sse::SegmentedReader;

/// A provider that replays a recorded fixture.
pub struct ReplayProvider {
    fixture: Fixture,
}

impl ReplayProvider {
    /// Load a fixture from an explicit path.
    pub fn from_file(path: &Path) -> Result<Self, ProvErr> {
        let raw = fs::read(path)
            .map_err(|e| ProvErr::Connect(format!("replay: cannot read {}: {e}", path.display())))?;
        let fixture = Fixture::parse(&raw)?;
        Ok(ReplayProvider { fixture })
    }

    /// Load `crates/kn9t-provider-replay/fixtures/<provider>/<name>.fixture`, resolved
    /// relative to this crate's manifest dir (R-RPLY-020).
    pub fn from_fixture(provider: &str, name: &str) -> Result<Self, ProvErr> {
        Self::from_file(&fixtures_dir().join(provider).join(format!("{name}.fixture")))
    }

    /// Construct directly from an in-memory fixture (used by the recorder round-trip and
    /// tests that build a fixture on the fly).
    pub fn from_fixture_struct(fixture: Fixture) -> Self {
        ReplayProvider { fixture }
    }

    /// The parsed fixture (test/introspection accessor).
    pub fn fixture(&self) -> &Fixture {
        &self.fixture
    }

    /// Decode the body into the `Chunk` sequence, honoring the `chunks:` delivery split.
    /// Separated from `stream()` so tests can assert the vector directly.
    pub fn chunks(&self) -> Result<Vec<Chunk>, ProvErr> {
        // Pre-stream status classification takes precedence (R-RPLY-035).
        if let Some(err) = self.status_error() {
            return Err(err);
        }
        match self.fixture.kind.as_str() {
            "replay" => self.decode_native(),
            other => Err(ProvErr::Decode(format!(
                "replay: no stage-2 parser for kind {other:?}; raw fixtures of real \
                 providers are parsed once PCORE/that provider exists (R-RPLY-070)"
            ))),
        }
    }

    /// Native `kind: replay` decode: each SSE `data:` event is one `Chunk` JSON object,
    /// delivered through the segmented reader + inline SSE splitter so boundary bugs
    /// surface identically whether or not `chunks:` is present.
    fn decode_native(&self) -> Result<Vec<Chunk>, ProvErr> {
        // R-RPLY-070: route through kn9t-provider-core::sse_lines (the real splitter).
        // pcore's sse_lines already strips the `data: ` prefix and handles [DONE];
        // each yielded Vec<u8> is directly the JSON payload — no data_events wrapper needed.
        let reader = SegmentedReader::new(self.fixture.body.clone(), &self.fixture.chunks);
        let mut out = Vec::new();
        for item in sse_lines(reader) {
            let payload = item
                .map_err(|e| ProvErr::Stream(format!("replay: io while splitting sse: {e}")))?;
            let chunk: Chunk = serde_json::from_slice(&payload).map_err(|e| {
                ProvErr::Decode(format!("replay: bad chunk json: {e}"))
            })?;
            out.push(chunk);
        }
        Ok(out)
    }

    /// The terminal `ProvErr` a native `kind: replay` stream ends with, declared by an
    /// optional `terminal-error:` header (R-RPLY-040). This lets stages 03/04 exercise the
    /// truncation ladder and compaction trigger deterministically *before* the real
    /// per-`kind` classifiers exist (05/09). The raw-byte twin fixtures carry the actual
    /// wire form (`prompt is too long`, an unfinished tool call, ...) and must classify to
    /// the same `ProvErr` once parsed for real — the R-RPLY-070 agreement guarantee.
    ///
    /// Recognized values: `context_overflow`, `truncated`, `stream:<msg>`, `decode:<msg>`.
    /// `context deadline exceeded` is **not** an error — it is a clean `StopReason::Length`
    /// and appears as a normal `stop` chunk in the body, so it needs no header.
    pub fn terminal_error(&self) -> Option<ProvErr> {
        let raw = self.fixture.header("terminal-error")?;
        let (tag, rest) = match raw.split_once(':') {
            Some((t, r)) => (t.trim(), r.trim()),
            None => (raw.trim(), ""),
        };
        Some(match tag {
            "context_overflow" => ProvErr::ContextOverflow,
            "truncated" => ProvErr::Truncated,
            "stream" => ProvErr::Stream(rest.to_string()),
            "decode" => ProvErr::Decode(rest.to_string()),
            other => ProvErr::Decode(format!("replay: unknown terminal-error {other:?}")),
        })
    }

    /// Map a non-2xx `status` to the pre-stream `ProvErr` a live call would raise
    /// (R-RPLY-035). 2xx ⇒ `None`.
    fn status_error(&self) -> Option<ProvErr> {
        let s = self.fixture.status;
        if (200..300).contains(&s) {
            return None;
        }
        let body = String::from_utf8_lossy(&self.fixture.body).into_owned();
        Some(ProvErr::Http { status: s, body })
    }
}

impl Provider for ReplayProvider {
    fn name(&self) -> &str {
        &self.fixture.kind
    }

    fn stream(
        &self,
        _req: &Request,
        _cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        // A fixture replays a recording; it ignores `Request` contents (R-RPLY-030).
        // The returned iterator must be `'static`, so decode eagerly and hand back an
        // owning iterator. (Stage 2 fixtures are small; laziness buys nothing here.)
        let chunks = self.chunks()?;
        let items: Vec<Result<Chunk, ProvErr>> = match self.terminal_error() {
            // A mid-stream terminal error is yielded *after* the good chunks (R-RPLY-040):
            // the loop sees partial content then a fatal `ProvErr`, exactly like the wire.
            Some(err) => chunks
                .into_iter()
                .map(Ok)
                .chain(std::iter::once(Err(err)))
                .collect(),
            None => chunks.into_iter().map(Ok).collect(),
        };
        Ok(Box::new(items.into_iter()))
    }
}

/// `.../crates/kn9t-provider-replay/fixtures`.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}
