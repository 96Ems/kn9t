//! Minimal inline SSE line splitter (R-RPLY-015, and the dependency note in
//! `spec/02-replay.md`). This is the **stage-2 stand-in** for
//! `kn9t-provider-core::sse_lines`; at stage 5 it is replaced by PCORE's version
//! (R-RPLY-070) and every fixture that passed here must pass unchanged.
//!
//! It exists to reproduce one real bug class: an SSE `data:` line split across two
//! TCP segments. [`SegmentedReader`] delivers the body in the byte ranges a fixture's
//! `chunks:` header declares, one range per `read()`, and [`sse_lines`] buffers across
//! those reads so a mid-line boundary parses identically to the whole body.

use std::io::{self, Read};

/// Turn a fixture body plus optional `chunks:` start offsets into a `Read` that
/// returns each segment in a separate `read()` (never crossing a declared boundary
/// in a single read), reproducing a network that split one SSE event across TCP
/// segments. With no offsets the whole body is one segment.
pub struct SegmentedReader {
    body: Vec<u8>,
    /// Consecutive `(start, end)` ranges covering the whole body.
    ranges: Vec<(usize, usize)>,
    seg: usize,
    consumed_in_seg: usize,
}

impl SegmentedReader {
    pub fn new(body: Vec<u8>, offsets: &[usize]) -> Self {
        let len = body.len();
        let mut starts: Vec<usize> = offsets.iter().copied().filter(|&o| o < len).collect();
        starts.push(0);
        starts.sort_unstable();
        starts.dedup();
        let mut ranges = Vec::with_capacity(starts.len());
        for i in 0..starts.len() {
            let s = starts[i];
            let e = starts.get(i + 1).copied().unwrap_or(len);
            if e > s {
                ranges.push((s, e));
            }
        }
        if ranges.is_empty() {
            ranges.push((0, len));
        }
        SegmentedReader {
            body,
            ranges,
            seg: 0,
            consumed_in_seg: 0,
        }
    }
}

impl Read for SegmentedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.seg >= self.ranges.len() {
            return Ok(0);
        }
        let (s, e) = self.ranges[self.seg];
        let cur = s + self.consumed_in_seg;
        let remaining = e - cur;
        let n = remaining.min(buf.len());
        buf[..n].copy_from_slice(&self.body[cur..cur + n]);
        self.consumed_in_seg += n;
        if cur + n >= e {
            self.seg += 1;
            self.consumed_in_seg = 0;
        }
        Ok(n)
    }
}

/// Buffered line iterator over any `Read`. Yields each line's bytes with the trailing
/// `\n` and any trailing `\r` stripped. Blank lines are yielded as empty vectors (they
/// are SSE event boundaries). A final line lacking a trailing newline is still yielded.
///
/// This mirrors the shape PCORE's `sse_lines(impl Read) -> Iterator<Result<Vec<u8>,
/// io::Error>>` will have, so re-pointing at stage 5 is a type-compatible swap.
pub fn sse_lines<R: Read>(reader: R) -> SseLines<R> {
    SseLines {
        reader,
        buf: Vec::new(),
        eof: false,
    }
}

pub struct SseLines<R: Read> {
    reader: R,
    buf: Vec<u8>,
    eof: bool,
}

impl<R: Read> Iterator for SseLines<R> {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
                line.pop(); // drop '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(Ok(line));
            }
            if self.eof {
                if self.buf.is_empty() {
                    return None;
                }
                let line = std::mem::take(&mut self.buf);
                return Some(Ok(line));
            }
            let mut tmp = [0u8; 1024];
            match self.reader.read(&mut tmp) {
                Ok(0) => self.eof = true,
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Group raw SSE lines into `data:` event payloads. Consecutive `data:` field lines are
/// joined with `\n` (per the SSE spec); a blank line closes the event. A `data: [DONE]`
/// payload is treated as a terminator and not emitted. This is the stage-2 stand-in for
/// the per-`kind` decode step; for `kind: replay` each payload is exactly one `Chunk`
/// JSON object.
pub fn data_events<I>(lines: I) -> Result<Vec<Vec<u8>>, io::Error>
where
    I: Iterator<Item = io::Result<Vec<u8>>>,
{
    let mut events = Vec::new();
    let mut cur: Option<Vec<u8>> = None;
    for line in lines {
        let line = line?;
        if line.is_empty() {
            if let Some(payload) = cur.take() {
                push_event(&mut events, payload);
            }
            continue;
        }
        if let Some(rest) = strip_data_prefix(&line) {
            let acc = cur.get_or_insert_with(Vec::new);
            if !acc.is_empty() {
                acc.push(b'\n');
            }
            acc.extend_from_slice(rest);
        }
        // Non-data fields (event:, id:, retry:, comments) are ignored at stage 2.
    }
    if let Some(payload) = cur.take() {
        push_event(&mut events, payload);
    }
    Ok(events)
}

fn push_event(events: &mut Vec<Vec<u8>>, payload: Vec<u8>) {
    if payload == b"[DONE]" {
        return;
    }
    events.push(payload);
}

/// Strip a leading `data:` field name and one optional following space.
fn strip_data_prefix(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"data:")?;
    Some(rest.strip_prefix(b" ").unwrap_or(rest))
}
