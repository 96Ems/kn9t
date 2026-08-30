//! R-STOR-100, R-STOR-110 — plan_request and compaction boundary.

use kn9t_core::{
    breakpoints, CompactSpan, Content, Message, MsgId, RequestPlan, Role, SeqRange, SessionId,
    StoreErr,
};
use rusqlite::params;

use crate::db::SqliteStore;

/// SPEC-OPEN compaction threshold: 0.80 × ctx_window
const COMPACT_THRESHOLD: f64 = 0.80;

pub fn plan_request(store: &SqliteStore, session: &SessionId) -> Result<RequestPlan, StoreErr> {
    let sid = session.0.clone();
    let model_spec = store.get_model_spec_for_session(&sid);

    struct MsgRow {
        seq: u64,
        role: String,
        content_json: String,
        est_tokens: i64,
    }

    // Scope the lock to just the query - release before resolve_image_blobs
    // to avoid deadlock (get_blob also needs the lock).
    let rows: Vec<MsgRow> = {
        let conn = store.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT seq, role, content, est_tokens FROM messages \
             WHERE session_id=?1 ORDER BY seq",
        ).map_err(|e| StoreErr(format!("plan prepare: {e}")))?;
        let mut out = Vec::new();
        let mut result = stmt.query(params![sid])
            .map_err(|e| StoreErr(format!("plan query: {e}")))?;
        while let Some(r) = result.next().map_err(|e| StoreErr(format!("plan row: {e}")))? {
            out.push(MsgRow {
                seq:          r.get::<_, i64>(0).unwrap_or(0) as u64,
                role:         r.get(1).unwrap_or_default(),
                content_json: r.get(2).unwrap_or_default(),
                est_tokens:   r.get(3).unwrap_or(0),
            });
        }
        out
    }; // conn lock released here

    // Now safe to call resolve_image_blobs (which calls get_blob -> needs lock)
    let messages: Vec<Message> = rows.iter().map(|r| {
        let content: Vec<Content> = serde_json::from_str(&r.content_json).unwrap_or_default();
        // Resolve blob references to inline base64 for provider compatibility.
        let content = resolve_image_blobs(store, content);
        Message { id: MsgId::new(), role: parse_role(&r.role), content, silent: false }
    }).collect();

    let total_est: i64 = rows.iter().map(|r| r.est_tokens).sum();
    let seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();

    let cache = model_spec.as_ref()
        .map(|s| breakpoints(&messages, &s.cache))
        .unwrap_or_default();

    let compact = model_spec.as_ref().and_then(|spec| {
        let threshold = (spec.ctx_window as f64 * COMPACT_THRESHOLD) as i64;
        if total_est >= threshold && messages.len() >= 2 {
            Some(compact_span(&seqs, &messages))
        } else {
            None
        }
    });

    Ok(RequestPlan { system: None, messages, tools: vec![], cache, compact })
}

fn parse_role(s: &str) -> Role {
    match s {
        "system"    => Role::System,
        "assistant" => Role::Assistant,
        "tool"      => Role::Tool,
        _           => Role::User,
    }
}

/// Resolve `Content::Image` blob references (`sha256:...`) to inline base64 data URIs.
/// This makes images compatible with all providers (OpenAI, Anthropic, etc.).
fn resolve_image_blobs(store: &SqliteStore, content: Vec<Content>) -> Vec<Content> {
    use base64::Engine;
    
    content.into_iter().map(|c| {
        match c {
            Content::Image { sha256, mime } => {
                // Extract hash from "sha256:<hex>" format.
                let hash = sha256.strip_prefix("sha256:").unwrap_or(&sha256);
                eprintln!("[resolve_image_blobs] resolving hash={}", hash);
                
                // Try to load blob data from store.
                match store.get_blob(hash) {
                    Ok(Some((data, stored_mime))) => {
                        let mime = if mime.is_empty() { stored_mime } else { mime };
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                        eprintln!("[resolve_image_blobs] resolved: {} bytes -> {} base64 chars", data.len(), b64.len());
                        Content::Image {
                            sha256: format!("data:{};base64,{}", mime, b64),
                            mime,
                        }
                    }
                    Ok(None) => {
                        eprintln!("[resolve_image_blobs] blob not found for hash={}", hash);
                        Content::Image { sha256, mime }
                    }
                    Err(e) => {
                        eprintln!("[resolve_image_blobs] error loading blob: {:?}", e);
                        Content::Image { sha256, mime }
                    }
                }
            }
            other => other,
        }
    }).collect()
}

/// R-STOR-110 — pick the oldest ~half, snap boundary to avoid orphaned ToolCall/Result pairs.
pub fn compact_span(seqs: &[u64], messages: &[Message]) -> CompactSpan {
    let n = messages.len();
    let mut cut = n / 2;
    loop {
        if cut >= n { break; }
        if has_orphan_tool_call(&messages[..cut]) { cut += 1; } else { break; }
    }
    let replaced_msgs = messages[..cut].to_vec();
    let start_seq = seqs.first().copied().unwrap_or(1);
    let end_seq   = if cut > 0 { seqs[cut - 1] } else { start_seq };
    CompactSpan {
        replaced: SeqRange { start: start_seq, end: end_seq },
        messages: replaced_msgs,
    }
}

/// True if any ToolCall in `msgs` has no matching ToolResult in `msgs`.
pub fn has_orphan_tool_call(msgs: &[Message]) -> bool {
    for m in msgs {
        for c in &m.content {
            if let Content::ToolCall { id, .. } = c {
                let has_result = msgs.iter().any(|m2| {
                    m2.content.iter().any(|c2| {
                        matches!(c2, Content::ToolResult { id: rid, .. } if rid == id)
                    })
                });
                if !has_result { return true; }
            }
        }
    }
    false
}
