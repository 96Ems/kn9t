//! Small HTTP helpers over `tiny_http`: JSON responses, body reading, query
//! parsing, and header lookup. Keeps the route handlers terse.

use tiny_http::{Header, Request, Response};

/// A JSON response body + status, before it is turned into a `tiny_http::Response`.
pub struct JsonResp {
    pub status: u16,
    pub body: String,
    /// Extra `(name, value)` headers (e.g. ETag).
    pub headers: Vec<(String, String)>,
}

impl JsonResp {
    pub fn new(status: u16, value: serde_json::Value) -> Self {
        JsonResp {
            status,
            body: serde_json::to_string(&value).unwrap_or_else(|_| "{}".into()),
            headers: Vec::new(),
        }
    }
    pub fn ok(value: serde_json::Value) -> Self {
        Self::new(200, value)
    }
    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self::new(
            status,
            serde_json::json!({ "error": code, "message": message }),
        )
    }
    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }
}

/// A binary response (blob GET).
pub struct BytesResp {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
}

/// Either a JSON or a binary reply.
pub enum Reply {
    Json(JsonResp),
    Bytes(BytesResp),
}

impl From<JsonResp> for Reply {
    fn from(j: JsonResp) -> Self {
        Reply::Json(j)
    }
}
impl From<BytesResp> for Reply {
    fn from(b: BytesResp) -> Self {
        Reply::Bytes(b)
    }
}

/// Read the full request body into a byte vector.
pub fn read_body(req: &mut Request) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = req.as_reader().read_to_end(&mut buf);
    buf
}

/// Read + parse the request body as JSON (empty body → `null`).
pub fn read_json(req: &mut Request) -> serde_json::Value {
    let bytes = read_body(req);
    if bytes.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Look up a header value (case-insensitive on field name).
pub fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Split `path?query` and return the query string (without `?`).
pub fn query_of(url: &str) -> &str {
    match url.split_once('?') {
        Some((_, q)) => q,
        None => "",
    }
}

/// The path component (before any `?`).
pub fn path_of(url: &str) -> &str {
    match url.split_once('?') {
        Some((p, _)) => p,
        None => url,
    }
}

/// Parse a single query parameter value (URL-decoded, minimal).
pub fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(url_decode(v));
            }
        } else if pair == key {
            return Some(String::new());
        }
    }
    None
}

/// Minimal percent-decoding (`%XX` and `+` → space).
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(h << 4 | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Respond to a `tiny_http::Request` with a [`Reply`].
pub fn respond(req: Request, reply: Reply) {
    match reply {
        Reply::Json(j) => {
            let mut resp = Response::from_string(j.body)
                .with_status_code(j.status)
                .with_header(header_kv("Content-Type", "application/json"));
            for (k, v) in &j.headers {
                resp = resp.with_header(header_kv(k, v));
            }
            let _ = req.respond(resp);
        }
        Reply::Bytes(b) => {
            let mut resp = Response::from_data(b.bytes)
                .with_status_code(b.status)
                .with_header(header_kv("Content-Type", &b.content_type));
            for (k, v) in &b.headers {
                resp = resp.with_header(header_kv(k, v));
            }
            let _ = req.respond(resp);
        }
    }
}

fn header_kv(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes())
        .unwrap_or_else(|_| Header::from_bytes(&b"X-Invalid"[..], &b"1"[..]).unwrap())
}
