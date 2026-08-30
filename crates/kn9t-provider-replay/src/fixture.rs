//! Fixture format: header + blank line + verbatim body (R-RPLY-010/015/020).
//!
//! A fixture is the **raw bytes a real provider returned** (SSE framing, CRLF/LF,
//! partial UTF-8 across chunk boundaries — all preserved), preceded by a small
//! `key: value` text header. The loader never re-encodes, re-frames, or normalizes
//! the body: it only locates the byte range after the first blank line and hands it
//! back untouched.

use kn9t_provider_core::ProvErr;

/// The parsed header plus the untouched body byte range.
///
/// `kind`, `status`, and `content_type` are MUST fields (R-RPLY-010). Everything else
/// is retained in `extra` in file order so a fixture can annotate freely
/// (`note:`, `retry-after:`, `terminal-error:`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    pub kind: String,
    pub status: u16,
    pub content_type: String,
    /// Byte offsets, relative to the body start, at which the harness splits delivery
    /// into separate reads (R-RPLY-015). Empty ⇒ one read.
    pub chunks: Vec<usize>,
    /// All other header lines, in order, as `(key, value)`.
    pub extra: Vec<(String, String)>,
    /// The response body, verbatim.
    pub body: Vec<u8>,
}

impl Fixture {
    /// Parse a whole fixture file's bytes. The header is ASCII `key: value` lines
    /// terminated by the first blank line (`\n\n` or `\r\n\r\n`); everything after is
    /// the body, byte-for-byte.
    pub fn parse(raw: &[u8]) -> Result<Fixture, ProvErr> {
        let sep = find_blank_line(raw)
            .ok_or_else(|| ProvErr::Decode("fixture: missing blank-line header separator".into()))?;
        let header_bytes = &raw[..sep.header_end];
        let body = raw[sep.body_start..].to_vec();

        let header_text = std::str::from_utf8(header_bytes)
            .map_err(|_| ProvErr::Decode("fixture: header is not UTF-8".into()))?;

        let mut kind: Option<String> = None;
        let mut status: Option<u16> = None;
        let mut content_type: Option<String> = None;
        let mut chunks: Vec<usize> = Vec::new();
        let mut extra: Vec<(String, String)> = Vec::new();

        for line in header_text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let (key, value) = split_header(line).ok_or_else(|| {
                ProvErr::Decode(format!("fixture: malformed header line: {line:?}"))
            })?;
            match key {
                "kind" => kind = Some(value.to_string()),
                "status" => {
                    status = Some(value.parse().map_err(|_| {
                        ProvErr::Decode(format!("fixture: bad status {value:?}"))
                    })?)
                }
                "content-type" => content_type = Some(value.to_string()),
                "chunks" => chunks = parse_chunks(value)?,
                _ => extra.push((key.to_string(), value.to_string())),
            }
        }

        Ok(Fixture {
            kind: kind.ok_or_else(|| ProvErr::Decode("fixture: missing `kind`".into()))?,
            status: status.ok_or_else(|| ProvErr::Decode("fixture: missing `status`".into()))?,
            content_type: content_type
                .ok_or_else(|| ProvErr::Decode("fixture: missing `content-type`".into()))?,
            chunks,
            extra,
            body,
        })
    }

    /// Look up a non-canonical header value (from `extra`) by key.
    pub fn header(&self, key: &str) -> Option<&str> {
        self.extra
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

struct BlankLine {
    /// Start of the blank line (exclusive end of the header text).
    header_end: usize,
    /// Start of the body (after the blank line's terminator).
    body_start: usize,
}

/// Find the first blank line, tolerating both `\n\n` and `\r\n\r\n`. `header_end` is the
/// start of the blank line (so `raw[..header_end]` is the header text, including the last
/// header line's newline — harmless for `str::lines`); `body_start` is just past the
/// blank line's `\n`.
fn find_blank_line(raw: &[u8]) -> Option<BlankLine> {
    let mut i = 0;
    let mut line_start = 0;
    while i < raw.len() {
        if raw[i] == b'\n' {
            // Content of the line that just ended, minus a trailing '\r'.
            let mut end = i;
            if end > line_start && raw[end - 1] == b'\r' {
                end -= 1;
            }
            if end == line_start {
                return Some(BlankLine {
                    header_end: line_start,
                    body_start: i + 1,
                });
            }
            line_start = i + 1;
        }
        i += 1;
    }
    None
}

fn split_header(line: &str) -> Option<(&str, &str)> {
    let idx = line.find(':')?;
    let key = line[..idx].trim();
    let value = line[idx + 1..].trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

fn parse_chunks(value: &str) -> Result<Vec<usize>, ProvErr> {
    // Allow a trailing `# comment`.
    let value = value.split('#').next().unwrap_or("").trim();
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for tok in value.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        out.push(
            tok.parse()
                .map_err(|_| ProvErr::Decode(format!("fixture: bad chunk offset {tok:?}")))?,
        );
    }
    Ok(out)
}
