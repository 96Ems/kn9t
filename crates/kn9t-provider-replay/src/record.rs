//! `--record` capability (R-RPLY-050): capture provider responses into the fixture
//! format so they can be replayed offline.
//!
//! ## Stage-2 scope (see `spec/02-replay.md` dependency note + CHANGELOG)
//! A faithful `--record` tees the **raw socket bytes** before any parsing. At stage 2
//! there is no HTTP transport yet (that seam is `kn9t-provider-core`, stage 5): the only
//! byte source is a [`Provider`], which already yields decoded [`Chunk`]s. So the
//! stage-2 recorder writes a **native `kind: replay`** fixture by re-encoding the chunk
//! stream as SSE `data:` lines — a genuinely replayable fixture whose chunk output is
//! byte-identical on reload. When PCORE lands, the raw-byte tee attaches at the socket
//! and records real-provider `kind`s verbatim; [`redact_header_value`] is the redaction
//! it will use (R-RPLY-020). This crate never alters a recorded body.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kn9t_provider_core::{Cancel, Chunk, ProvErr, Provider, Request};

use crate::fixture::Fixture;

/// Wraps an inner provider and writes fixtures under `out_dir`.
pub struct RecordingProvider<'a> {
    pub inner: &'a dyn Provider,
    pub out_dir: PathBuf,
}

impl<'a> RecordingProvider<'a> {
    pub fn new(inner: &'a dyn Provider, out_dir: impl Into<PathBuf>) -> Self {
        RecordingProvider {
            inner,
            out_dir: out_dir.into(),
        }
    }

    /// Drive the inner provider for `req`, tee every chunk through unchanged, and write a
    /// native `kind: replay` fixture named `<name>.fixture` under `out_dir/replay/`.
    /// Returns the fixture path. Chunks are returned too, so a caller can assert the tee
    /// did not alter them.
    pub fn record(
        &self,
        req: &Request,
        cancel: &Cancel,
        name: &str,
    ) -> Result<(PathBuf, Vec<Chunk>), ProvErr> {
        let mut chunks = Vec::new();
        for item in self.inner.stream(req, cancel)? {
            chunks.push(item?);
        }
        let fixture = fixture_from_chunks(&chunks)?;
        let dir = self.out_dir.join("replay");
        fs::create_dir_all(&dir)
            .map_err(|e| ProvErr::Connect(format!("record: mkdir {}: {e}", dir.display())))?;
        let path = dir.join(format!("{name}.fixture"));
        fs::write(&path, serialize_fixture(&fixture))
            .map_err(|e| ProvErr::Connect(format!("record: write {}: {e}", path.display())))?;
        Ok((path, chunks))
    }
}

impl Provider for RecordingProvider<'_> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn stream(
        &self,
        req: &Request,
        cancel: &Cancel,
    ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
        // Pass-through; recording is driven explicitly via `record()` so the raw stream
        // is never buffered on the hot path.
        self.inner.stream(req, cancel)
    }
}

/// Build an in-memory native fixture from a decoded chunk sequence.
fn fixture_from_chunks(chunks: &[Chunk]) -> Result<Fixture, ProvErr> {
    Ok(Fixture {
        kind: "replay".to_string(),
        status: 200,
        content_type: "text/event-stream".to_string(),
        chunks: Vec::new(),
        extra: vec![("note".to_string(), "recorded from chunk stream".to_string())],
        body: encode_chunks_sse(chunks)?,
    })
}

/// Encode chunks as `kind: replay` SSE body: one `data: <chunk-json>` line per chunk,
/// each event terminated by a blank line, closed with `data: [DONE]`.
pub fn encode_chunks_sse(chunks: &[Chunk]) -> Result<Vec<u8>, ProvErr> {
    let mut body = Vec::new();
    for chunk in chunks {
        let json = serde_json::to_vec(chunk)
            .map_err(|e| ProvErr::Decode(format!("record: encode chunk: {e}")))?;
        body.extend_from_slice(b"data: ");
        body.extend_from_slice(&json);
        body.extend_from_slice(b"\n\n");
    }
    body.extend_from_slice(b"data: [DONE]\n\n");
    Ok(body)
}

/// Serialize a fixture back to its on-disk bytes: header lines, blank line, verbatim body.
pub fn serialize_fixture(f: &Fixture) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("kind: {}\n", f.kind));
    out.push_str(&format!("status: {}\n", f.status));
    out.push_str(&format!("content-type: {}\n", f.content_type));
    if !f.chunks.is_empty() {
        let list = f
            .chunks
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!("chunks: {list}\n"));
    }
    for (k, v) in &f.extra {
        out.push_str(&format!("{k}: {}\n", redact_header_value(k, v)));
    }
    out.push('\n');
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&f.body);
    bytes
}

/// Redact credentials before writing (R-RPLY-020): `Authorization` and any key whose name
/// contains `api_key` (case-insensitive) become `<redacted>`. Bodies are never touched.
pub fn redact_header_value(key: &str, value: &str) -> String {
    let k = key.to_ascii_lowercase();
    if k == "authorization" || k.contains("api_key") || k.contains("api-key") {
        "<redacted>".to_string()
    } else {
        value.to_string()
    }
}

/// Write raw response bytes to a fixture file with a caller-supplied header, redacting
/// credential headers. This is the entry point the stage-5 raw-byte tee uses; exposed now
/// so the format has one writer.
pub fn write_raw_fixture(
    path: &Path,
    kind: &str,
    status: u16,
    content_type: &str,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> io::Result<()> {
    let fixture = Fixture {
        kind: kind.to_string(),
        status,
        content_type: content_type.to_string(),
        chunks: Vec::new(),
        extra: extra_headers.to_vec(),
        body: body.to_vec(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_fixture(&fixture))
}
