//! SSE line splitter: buffers across read boundaries and respects cancel tokens.
//! Checks cancel at each parsed event, not just at read() boundaries.

use kn9t_core::Cancel;
use std::io::{self, BufRead, BufReader, Read};

/// Returns an iterator over complete SSE event payloads (the `data: ...` part),
/// correctly reassembled across chunk boundaries.
///
/// When `cancel` is `Some`, checks `cancel.cancelled` at each parsed
/// event, not just at read() boundaries. This ensures cancel takes effect even
/// when BufReader has buffered multiple events from a single underlying read().
pub fn sse_lines(
    r: impl Read,
    cancel: Option<Cancel>,
) -> impl Iterator<Item = Result<Vec<u8>, io::Error>> {
    SseIter {
        reader: BufReader::new(r),
        cancel,
    }
}

struct SseIter<R: Read> {
    reader: BufReader<R>,
    cancel: Option<Cancel>,
}

impl<R: Read> Iterator for SseIter<R> {
    type Item = Result<Vec<u8>, io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Check cancel at each iteration to catch it even in buffered data.
            if let Some(ref cancel) = self.cancel {
                if cancel.cancelled() {
                    return Some(Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "cancelled",
                    )));
                }
            }

            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Err(e) => return Some(Err(e)),
                Ok(0) => return None, // EOF
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
