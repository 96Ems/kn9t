//! R-PCORE-040 — SSE line splitter. Buffers across Read-boundary splits.

use std::io::{self, BufRead, BufReader, Read};

/// Returns an iterator over complete SSE event payloads (the `data: ...` part),
/// correctly reassembled across chunk boundaries.
pub fn sse_lines(r: impl Read) -> impl Iterator<Item = Result<Vec<u8>, io::Error>> {
    SseIter {
        reader: BufReader::new(r),
    }
}

struct SseIter<R: Read> {
    reader: BufReader<R>,
}

impl<R: Read> Iterator for SseIter<R> {
    type Item = Result<Vec<u8>, io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Err(e)                         => return Some(Err(e)),
                Ok(0)                          => return None, // EOF
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if let Some(payload) = trimmed.strip_prefix("data: ") {
                        if payload == "[DONE]" {
                            return None;
                        }
                        return Some(Ok(payload.as_bytes().to_vec()));
                    }
                    // Skip comment, empty lines, event:, id:, retry: lines
                }
            }
        }
    }
}
