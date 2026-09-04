//! SSE parser shared by all provider plugins.
//!
//! Buffers until `\n\n`, extracts `event:` and `data:` fields per block.
//! A block may be split across multiple network chunks — we buffer on `\n\n`.
//!
//! This module is intentionally dependency-free (only `std::io`). It is
//! re-exported at the crate root so plugin authors can write:
//!
//! ```rust
//! use kn9t_plugin_sdk::{SseEvent, SseReader};
//! ```

use std::io::{BufRead, BufReader, Read};

/// One parsed SSE event carrying both the event name and its data payload.
///
/// The `event` field corresponds to the `event:` line in the SSE block;
/// `data` is the concatenation of all `data:` lines in the same block,
/// joined by `\n` when multiple `data:` lines are present.
#[derive(Debug)]
pub struct SseEvent {
    /// The value of the `event:` field, e.g. `"completion"` or `"done"`.
    pub event: String,
    /// The value of the `data:` field(s). Multiple `data:` lines are joined
    /// with `\n`.
    pub data: String,
}

/// Iterator over SSE events from any [`Read`] source.
///
/// Wraps the reader in a [`BufReader`] and yields one [`SseEvent`] per
/// blank-line-terminated SSE block. Lines with unknown field names are
/// silently ignored per the SSE specification.
///
/// # Example
///
/// ```rust
/// use kn9t_plugin_sdk::{SseEvent, SseReader};
///
/// let raw = b"event: completion\ndata: {\"text\":\"hello\"}\n\n\
///             event: done\ndata: {}\n\n";
///
/// let mut reader = SseReader::new(raw.as_slice());
///
/// let e1 = reader.next().unwrap().unwrap();
/// assert_eq!(e1.event, "completion");
/// assert!(e1.data.contains("hello"));
///
/// let e2 = reader.next().unwrap().unwrap();
/// assert_eq!(e2.event, "done");
///
/// assert!(reader.next().is_none());
/// ```
pub struct SseReader<R: Read> {
    inner: BufReader<R>,
}

impl<R: Read> SseReader<R> {
    /// Wrap `r` in a buffered SSE reader.
    pub fn new(r: R) -> Self {
        SseReader {
            inner: BufReader::new(r),
        }
    }
}

impl<R: Read> Iterator for SseReader<R> {
    type Item = Result<SseEvent, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut event_name = String::new();
        let mut data_buf = String::new();
        let mut saw_field = false;

        loop {
            let mut line = String::new();
            match self.inner.read_line(&mut line) {
                Ok(0) => {
                    // EOF — flush if we have something
                    if saw_field {
                        return Some(Ok(SseEvent {
                            event: event_name,
                            data: data_buf,
                        }));
                    }
                    return None;
                }
                Err(e) => return Some(Err(format!("SSE read error: {e}"))),
                Ok(_) => {}
            }

            let line = line.trim_end_matches(|c| c == '\r' || c == '\n');

            if line.is_empty() {
                // Blank line = block separator
                if saw_field {
                    return Some(Ok(SseEvent {
                        event: event_name,
                        data: data_buf,
                    }));
                }
                // Empty block — keep going
                continue;
            }

            saw_field = true;

            if let Some(val) = line.strip_prefix("event:") {
                event_name = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("data:") {
                if !data_buf.is_empty() {
                    data_buf.push('\n');
                }
                data_buf.push_str(val.trim());
            }
            // Lines without a colon or with unknown fields are ignored per SSE spec.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_two_events() {
        let raw = b"event: completion\ndata: {\"deltaText\":\"hel\"}\n\n\
                    event: completion\ndata: {\"deltaText\":\"lo\"}\n\n\
                    event: done\ndata: {}\n\n";
        let mut r = SseReader::new(raw.as_slice());
        let e1 = r.next().unwrap().unwrap();
        assert_eq!(e1.event, "completion");
        assert!(e1.data.contains("hel"));
        let e2 = r.next().unwrap().unwrap();
        assert_eq!(e2.event, "completion");
        assert!(e2.data.contains("lo"));
        let e3 = r.next().unwrap().unwrap();
        assert_eq!(e3.event, "done");
    }

    #[test]
    fn split_across_chunks() {
        // Simulate a block split across two lines of the same field (multi-data lines).
        let raw = b"event: completion\ndata: {\"a\":1}\ndata: {\"b\":2}\n\n";
        let mut r = SseReader::new(raw.as_slice());
        let e = r.next().unwrap().unwrap();
        // data_buf = "{\"a\":1}\n{\"b\":2}" — both lines present
        assert!(e.data.contains("a"));
        assert!(e.data.contains("b"));
    }
}
