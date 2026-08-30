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

/// Parse a request body into a typed struct. The target type carries
/// `#[serde(deny_unknown_fields)]` (generated `crate::api` types), so a malformed,
/// mistyped, or **unknown** field is a 400 — never a silent ignore (F6).
pub fn parse_json<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T, JsonResp> {
    let bytes = read_body(req);
    if bytes.is_empty() {
        return Err(JsonResp::error(
            400,
            "bad_json",
            "request body required for this route",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| {
        JsonResp::error(400, "bad_json", &format!("invalid request body: {e}"))
    })
}

/// Convert milliseconds-since-UNIX-epoch to an ISO8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`). The store persists `created_at`/`ts` as INTEGER
/// millis (R-STOR, `as_millis()`); this is the boundary normalization to the
/// schema's `format: date-time` string (F5). Proleptic Gregorian via
/// Hinnant's civil-from-days — no chrono/time dependency (DESIGN §15).
pub fn millis_to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400); // seconds within the day

    // Howard Hinnant's civil_from_days (days → y/m/d), valid for the whole
    // i64 days range; matches the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Read the full request body into a byte vector.

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
