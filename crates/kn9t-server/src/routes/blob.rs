//! R-SRV-090 — blob roundtrip (DESIGN §12.7, R-STOR-140).
//!
//! `POST /blob` computes SHA-256, stores once via the store (dedup by content
//! hash), returns `{hash, mime}`. `GET /blob/{hash}` returns bytes with
//! `ETag: "<hash>"` and `Cache-Control: immutable` (blobs are content-addressed and
//! never change).

use std::sync::Arc;

use crate::http_util::{BytesResp, JsonResp, Reply};
use crate::state::ServerState;

/// Guess a mime type from the declared Content-Type, defaulting to
/// `application/octet-stream`. A caller that knows better sets Content-Type.
fn resolve_mime(content_type: Option<&str>) -> String {
    match content_type {
        Some(ct) if !ct.is_empty() && ct != "application/x-www-form-urlencoded" => {
            // Strip any `; charset=...` parameter.
            ct.split(';').next().unwrap_or(ct).trim().to_owned()
        }
        _ => "application/octet-stream".to_owned(),
    }
}

/// `POST /blob` → `{hash, mime}`. A second put of identical bytes reuses the row
/// (store dedup, R-STOR-140).
pub fn put(state: &Arc<ServerState>, bytes: Vec<u8>, content_type: Option<&str>) -> JsonResp {
    if bytes.is_empty() {
        return JsonResp::error(400, "empty_blob", "blob body is empty");
    }
    let mime = resolve_mime(content_type);
    match state.store.put_blob(&bytes, &mime) {
        Ok(hash) => JsonResp::ok(serde_json::json!({ "hash": hash, "mime": mime })),
        Err(e) => JsonResp::error(500, "store_error", &e.0),
    }
}

/// `GET /blob/{hash}` → bytes with ETag + immutable cache.
pub fn get(state: &Arc<ServerState>, hash: &str) -> Reply {
    match state.store.get_blob(hash) {
        Ok(Some((bytes, mime))) => Reply::Bytes(BytesResp {
            status: 200,
            bytes,
            content_type: mime,
            headers: vec![
                ("ETag".into(), format!("\"{hash}\"")),
                ("Cache-Control".into(), "immutable".into()),
            ],
        }),
        Ok(None) => Reply::Json(JsonResp::error(404, "not_found", "no such blob")),
        Err(e) => Reply::Json(JsonResp::error(500, "store_error", &e.0)),
    }
}
