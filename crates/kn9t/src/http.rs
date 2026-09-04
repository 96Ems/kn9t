//! 96E-36 — the CLI's one HTTP client.
//!
//! Seven commands previously carried their own `TcpStream` helper with its own request
//! formatting, its own bearer header, and its own response parsing. Six copies of `get_json`
//! had already drifted: three spellings of the connect-error branch, two of the `format!`,
//! and per-command `eprintln!` prefixes. None were covered by a test, because
//! `crates/kn9t/` had none.
//!
//! Deliberately still zero third-party dependencies (the crate takes only `serde_json` and
//! `crossterm`). The requests are `HTTP/1.0` against a loopback server we ship ourselves, so
//! the response is always identity-encoded and closed by EOF — no chunked decoding, no
//! keep-alive framing, no redirects. `ureq` would buy correctness we do not need here and
//! cost a dependency in the one crate that has almost none; if the CLI ever talks to
//! something it did not spawn, that trade flips.
//!
//! **Dependency duplicates** (`cargo tree -d --workspace`, 13 pairs) — documenting rather
//! than reducing, since none are this module's to fix:
//! - `crossterm` ×3: **0.27 is this crate's alone**, 0.28 via `ratatui`, 0.29 via a git patch
//!   (eitsupi's `feature/windows-vt-input`, upstream PR #1030, Windows bracketed paste).
//!   Bumping the CLI to 0.29 would collapse this to two; it is a separate change because the
//!   CLI's `crossterm` use is raw-mode/key handling, not rendering.
//! - `ureq` ×2: v3 via provider-core/server, **v2 via `kn9t-tui`**. This crate takes neither.
//! - `base64`, `png`, `miniz_oxide`, `getrandom`, `bitflags`, `syn`, `unicode-width`,
//!   `hashbrown` (×3), `webpki-roots`, `windows-sys`: all transitive, no direct edges here.
//!
//! Track PR #1030's merge so the git patch and the third `crossterm` can be dropped together.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;

use serde_json::Value;

/// Connect, or exit(1) with a message naming the command that failed.
///
/// The CLI's contract is "print why and stop", not "return an error nobody reads" — every
/// former copy of this did exactly that, just with a different prefix each time. `who` keeps
/// that per-command wording (`[kn9t cost]`, `[kn9t status]`, …).
fn connect(host: &str, who: &str) -> TcpStream {
    match TcpStream::connect(host) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[kn9t {who}] cannot reach server: {e}");
            std::process::exit(1);
        }
    }
}

/// Split `\r\n\r\n` and parse the body. `Value::Null` on any malformed response, matching
/// the previous behaviour: these commands render "unknown"/empty rather than panicking when
/// the server answers something unexpected.
fn parse_body(resp: &str) -> Value {
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Null)
}

/// Read the whole response to EOF. `HTTP/1.0` with no keep-alive means EOF *is* the
/// terminator, so a short read cannot silently truncate a large transcript.
fn read_all(stream: TcpStream) -> String {
    let mut resp = String::new();
    let mut r = BufReader::new(stream);
    // Bytes first, then lossy UTF-8: a transcript can carry invalid sequences and
    // `read_to_string` would discard the entire response rather than the bad bytes.
    let mut buf = Vec::new();
    if r.read_to_end(&mut buf).is_ok() {
        resp = String::from_utf8_lossy(&buf).into_owned();
    }
    resp
}

/// `GET path`, parsed as JSON.
pub fn get_json(host: &str, auth: &str, path: &str, who: &str) -> Value {
    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n");
    let mut stream = connect(host, who);
    if stream.write_all(request.as_bytes()).is_err() || stream.flush().is_err() {
        eprintln!("[kn9t {who}] connection closed while sending request");
        std::process::exit(1);
    }
    parse_body(&read_all(stream))
}

/// `POST path` with a JSON body, parsed as JSON. `lease` adds `X-Lease` when the caller
/// holds the session write lease.
pub fn post_json(host: &str, auth: &str, path: &str, body: &Value, lease: Option<&str>, who: &str) -> Value {
    let body_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    let mut headers = format!(
        "Authorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body_str.len()
    );
    if let Some(l) = lease {
        headers.push_str(&format!("X-Lease: {l}\r\n"));
    }
    let request = format!("POST {path} HTTP/1.0\r\nHost: {host}\r\n{headers}\r\n{body_str}");
    let mut stream = connect(host, who);
    if stream.write_all(request.as_bytes()).is_err() || stream.flush().is_err() {
        eprintln!("[kn9t {who}] connection closed while sending request");
        std::process::exit(1);
    }
    parse_body(&read_all(stream))
}

/// Subscribe to a session's SSE stream, one raw line per message.
///
/// The reader thread owns the socket: when it exits the `TcpStream` drops, the server sees
/// EOF and fires `client_detached`. Sending stops as soon as the receiver is dropped.
pub fn subscribe_sse(host: &str, auth: &str, session_id: &str, from_seq: u64) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::sync_channel::<String>(1024);
    let host2 = host.to_string();
    let auth2 = auth.to_string();
    let sid = session_id.to_string();

    thread::spawn(move || {
        let path = format!("/session/{sid}/events?from={from_seq}");
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host2}\r\n\
             Authorization: {auth2}\r\nAccept: text/event-stream\r\n\r\n"
        );
        let stream = match TcpStream::connect(&host2) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[kn9t] SSE connect: {e}");
                return;
            }
        };
        let mut w = match stream.try_clone() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[kn9t] SSE clone: {e}");
                return;
            }
        };
        let _ = w.write_all(request.as_bytes());
        let _ = w.flush();
        drop(w);
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot HTTP/1.0 server: accepts one connection, records the request it received,
    /// replies with `body`, and closes. Closing is what terminates the response, which is the
    /// same contract the real server uses for these routes.
    fn stub(body: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("addr").to_string();
        let h = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            // Read just the head; enough to assert on the request line and headers.
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let resp = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}"
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
            req
        });
        (addr, h)
    }

    #[test]
    fn get_json_parses_body_and_sends_bearer() {
        let (addr, h) = stub(r#"{"ok":true,"n":3}"#);
        let v = get_json(&addr, "Bearer tok-123", "/health", "test");
        let req = h.join().expect("stub thread");

        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["n"], serde_json::json!(3));
        assert!(req.starts_with("GET /health HTTP/1.0\r\n"), "request line: {req:?}");
        assert!(req.contains("Authorization: Bearer tok-123\r\n"), "auth header missing: {req:?}");
    }

    #[test]
    fn post_json_sends_body_content_length_and_lease() {
        let (addr, h) = stub(r#"{"accepted":1}"#);
        let body = serde_json::json!({"prompt":"hi"});
        let v = post_json(&addr, "Bearer t", "/session/s1/prompt", &body, Some("lease-9"), "test");
        let req = h.join().expect("stub thread");

        assert_eq!(v["accepted"], serde_json::json!(1));
        assert!(req.starts_with("POST /session/s1/prompt HTTP/1.0\r\n"), "request line: {req:?}");
        assert!(req.contains("Content-Type: application/json\r\n"), "content-type: {req:?}");
        assert!(req.contains("X-Lease: lease-9\r\n"), "lease header: {req:?}");
        // Content-Length must match the serialized body, or the server blocks waiting for more.
        let expected = serde_json::to_string(&body).unwrap();
        assert!(
            req.contains(&format!("Content-Length: {}\r\n", expected.len())),
            "content-length: expected {} for body {expected:?}; request was {req:?}",
            expected.len()
        );
        assert!(req.ends_with(&expected), "body not sent last: {req:?}");
    }

    #[test]
    fn post_json_omits_lease_header_when_none() {
        let (addr, h) = stub("{}");
        post_json(&addr, "Bearer t", "/x", &serde_json::json!({}), None, "test");
        let req = h.join().expect("stub thread");
        assert!(!req.contains("X-Lease"), "lease header must be absent: {req:?}");
    }

    /// A non-JSON response yields `Value::Null` rather than panicking: these commands render
    /// "unknown" instead of dying when the server answers with an error page.
    #[test]
    fn malformed_body_is_null_not_panic() {
        let (addr, h) = stub("not json at all");
        let v = get_json(&addr, "Bearer t", "/health", "test");
        let _ = h.join();
        assert_eq!(v, Value::Null);
    }

    /// The header/body split must be found even when the body itself contains \r\n\r\n.
    #[test]
    fn body_containing_blank_line_is_parsed() {
        let (addr, h) = stub("{\"text\":\"a\\r\\n\\r\\nb\"}");
        let v = get_json(&addr, "Bearer t", "/x", "test");
        let _ = h.join();
        assert_eq!(v["text"], serde_json::json!("a\r\n\r\nb"));
    }

    /// Invalid UTF-8 in a transcript must not discard the whole response: `read_to_string`
    /// would have returned Err and left the body empty.
    #[test]
    fn invalid_utf8_does_not_discard_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let h = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let mut resp = b"HTTP/1.0 200 OK\r\n\r\n{\"t\":\"".to_vec();
            resp.push(0xff); // lone continuation byte: not valid UTF-8
            resp.extend_from_slice(b"\"}");
            let _ = sock.write_all(&resp);
        });
        let v = get_json(&addr, "Bearer t", "/x", "test");
        let _ = h.join();
        // Lossy decoding replaces the bad byte, so the JSON still parses.
        assert!(v["t"].is_string(), "expected a string, got {v:?}");
    }

    #[test]
    fn sse_yields_lines_until_server_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let h = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let _ = sock.write_all(
                b"HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"a\":1}\ndata: {\"b\":2}\n",
            );
            let _ = sock.flush();
            req
        });

        let rx = subscribe_sse(&addr, "Bearer t", "s1", 7);
        let mut data = Vec::new();
        while let Ok(line) = rx.recv() {
            if line.starts_with("data:") {
                data.push(line);
            }
        }
        let req = h.join().expect("stub thread");

        assert!(req.starts_with("GET /session/s1/events?from=7 HTTP/1.0\r\n"), "request: {req:?}");
        assert!(req.contains("Accept: text/event-stream\r\n"), "accept header: {req:?}");
        assert_eq!(data.len(), 2, "expected 2 data lines, got {data:?}");
        assert!(data[0].contains("\"a\":1"));
        assert!(data[1].contains("\"b\":2"));
    }
}
