//! Session lifecycle + write routes (R-SRV-010, R-SRV-060, R-SRV-100).
//!
//! Create/list/snapshot/fork/delete, the write lease, and the writer commands
//! (`prompt`/`steer`/`abort`/`model`/`approve`). `prompt` drives a ReAct turn on a
//! background thread, publishing events to the session bus; after the first
//! assistant turn of a nameless session it issues one cheap title call
//! (R-SRV-100), best-effort.

use std::sync::Arc;

use kn9t_core::{
    Content, Event, ForkReason, Message, ModelRef, MsgId, Role, SessionId, Store,
};

use crate::http_util::JsonResp;
use crate::state::ServerState;
use crate::turn;

/// `POST /session` — create; body `{cwd, model?, name?}`.
pub fn create(state: &Arc<ServerState>, body: serde_json::Value) -> JsonResp {
    let cwd = body
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| state.cwd.to_str().unwrap_or("."))
        .to_owned();
    let name = body.get("name").and_then(|n| n.as_str()).map(|s| s.to_owned());

    // Resolve the model: body-supplied ref, else server default.
    let model_ref = match resolve_model_ref(state, &body) {
        Ok(m) => m,
        Err(e) => return JsonResp::error(400, "bad_model", &e),
    };

    let id = SessionId::new();
    if let Err(e) = kn9t_store::create_session(&state.store, &id, &cwd, &model_ref) {
        return JsonResp::error(500, "store_error", &e.0);
    }

    // A `name` supplied at creation suppresses auto-titling (R-SRV-100). Persist it.
    if let Some(n) = &name {
        let _ = state.store.execute_raw(
            "UPDATE sessions SET name=?1 WHERE id=?2",
            &[&n.as_str(), &id.0.as_str()],
        );
    }

    JsonResp::ok(serde_json::json!({
        "id": id.0,
        "cwd": cwd,
        "name": name,
        "model": { "provider": model_ref.provider, "id": model_ref.id },
    }))
}

fn resolve_model_ref(state: &Arc<ServerState>, body: &serde_json::Value) -> Result<ModelRef, String> {
    if let Some(m) = body.get("model") {
        if let (Some(p), Some(i)) = (
            m.get("provider").and_then(|x| x.as_str()),
            m.get("id").and_then(|x| x.as_str()),
        ) {
            return Ok(ModelRef { provider: p.into(), id: i.into() });
        }
    }
    state
        .default_model
        .as_ref()
        .map(|s| s.r#ref.clone())
        .ok_or_else(|| "no model supplied and no server default".into())
}

/// `GET /session` — list sessions.
pub fn list(state: &Arc<ServerState>) -> JsonResp {
    // Include created_at for TUI date grouping (ISO 8601 format from SQLite).
    let sql = "SELECT json_object('id', id, 'name', name, 'cwd', cwd, 'head_seq', head_seq, \
               'created_at', created_at) FROM sessions ORDER BY created_at DESC";
    let strings = state.store.query_strings(sql, &[]).unwrap_or_default();
    let sessions: Vec<serde_json::Value> = strings
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    JsonResp::ok(serde_json::json!({ "sessions": sessions }))
}

/// `GET /session/{id}` — snapshot `{meta, head_seq, transcript}`.
pub fn snapshot(state: &Arc<ServerState>, id: &str) -> JsonResp {
    let sid = SessionId(id.to_owned());
    let snap = match state.store.snapshot(&sid) {
        Ok(s) => s,
        Err(e) => return JsonResp::error(404, "not_found", &e.0),
    };

    // Meta.
    let meta_sql = "SELECT json_object('id', id, 'name', name, 'cwd', cwd, \
                    'created_at', created_at) FROM sessions WHERE id=?1";
    let meta: serde_json::Value = state
        .store
        .query_strings(meta_sql, &[&id])
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    // Transcript: the message projection in seq order.
    // Include silent flag so TUI can filter out silent messages on reload.
    let msg_sql = "SELECT json_object('seq', seq, 'role', role, 'content', json(content), \
                   'silent', CASE WHEN silent THEN json('true') ELSE json('false') END) \
                   FROM messages WHERE session_id=?1 ORDER BY seq";
    let transcript: Vec<serde_json::Value> = state
        .store
        .query_strings(msg_sql, &[&id])
        .unwrap_or_default()
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();

    JsonResp::ok(serde_json::json!({
        "meta": meta,
        "head_seq": snap.head_seq,
        "ctx_tokens": snap.ctx_tokens,
        "cost_usd": snap.cost_usd,
        "model": { "provider": snap.model.provider, "id": snap.model.id },
        "transcript": transcript,
    }))
}

/// `POST /session/{id}/fork` — `{origin_seq, reason}` → new session id.
pub fn fork(state: &Arc<ServerState>, id: &str, body: serde_json::Value) -> JsonResp {
    let origin = SessionId(id.to_owned());
    let origin_seq = body.get("origin_seq").and_then(|s| s.as_u64()).unwrap_or(0);
    let reason = match body.get("reason").and_then(|r| r.as_str()) {
        Some("rewind") => ForkReason::Rewind,
        Some("subagent") => ForkReason::Subagent,
        Some("tree") => ForkReason::Tree,
        _ => ForkReason::Fork,
    };
    // 404 if origin session doesn't exist.
    let cwd: Option<String> = state
        .store
        .query_one("SELECT cwd FROM sessions WHERE id=?1", &[&id], |r| r.get(0))
        .ok();
    let cwd = match cwd {
        Some(c) => c,
        None => return JsonResp::error(404, "not_found", "session not found"),
    };

    let new_id = SessionId::new();
    match kn9t_store::fork_session(&state.store, &origin, &new_id, origin_seq, reason, None, &cwd) {
        Ok(()) => JsonResp::ok(serde_json::json!({ "id": new_id.0 })),
        Err(e) => JsonResp::error(400, "fork_failed", &e.0),
    }
}

/// `DELETE /session/{id}`.
pub fn delete(state: &Arc<ServerState>, id: &str) -> JsonResp {
    // 404 if session doesn't exist.
    let exists: bool = state.store
        .query_one("SELECT 1 FROM sessions WHERE id=?1", &[&id], |_| Ok(1i64))
        .is_ok();
    if !exists {
        return JsonResp::error(404, "not_found", "session not found");
    }
    let sid = SessionId(id.to_owned());
    match state.store.delete_session(&sid) {
        Ok(()) => {
            state.buses.drop_session(id);
            JsonResp::ok(serde_json::json!({ "deleted": id }))
        }
        Err(e) => JsonResp::error(400, "delete_failed", &e.0),
    }
}

/// `POST /session/{id}/lease?takeover=1` — acquire write lease (R-SRV-060). The
/// minted holder token is returned; the client presents it on every write via the
/// `X-Lease` header.
pub fn lease_acquire(state: &Arc<ServerState>, id: &str, takeover: bool) -> JsonResp {
    use crate::lease::AcquireResult;
    match state.leases.acquire(id, takeover) {
        AcquireResult::Granted(holder) => {
            JsonResp::ok(serde_json::json!({ "lease": holder, "session": id }))
        }
        AcquireResult::Busy => JsonResp::error(409, "session_busy", "another client holds the lease"),
    }
}

/// `DELETE /session/{id}/lease` — release (only the holder may).
pub fn lease_release(state: &Arc<ServerState>, id: &str, holder: Option<&str>) -> JsonResp {
    match holder {
        Some(h) if state.leases.release(id, h) => {
            JsonResp::ok(serde_json::json!({ "released": id }))
        }
        _ => JsonResp::error(409, "session_busy", "you do not hold this lease"),
    }
}

/// `POST /session/{id}/prompt` — `{text, blobs:[sha256]}` [lease required].
/// Appends the user message and runs a turn on a background thread.
///
/// Returns 409 Conflict if a turn is already running for this session. The client
/// should wait for the turn to complete (via SSE TurnEnded event) before sending
/// another prompt. This prevents transcript corruption when the user aborts a turn
/// and immediately sends a new prompt before the abort completes.
pub fn prompt(state: &Arc<ServerState>, id: &str, body: serde_json::Value) -> JsonResp {
    eprintln!("[prompt] session={} body_keys={:?}", id, body.as_object().map(|o| o.keys().collect::<Vec<_>>()));
    
    // Check if a turn is already running for this session.
    // This prevents the race condition where abort + immediate new prompt
    // causes the user message to be appended before tool_results, corrupting
    // the transcript and causing "tool_use ids without tool_result blocks" errors.
    if turn::is_turn_running(id) {
        eprintln!("[prompt] turn already running, rejecting");
        return JsonResp::error(409, "turn_running",
            "A turn is already running for this session. Wait for it to complete before sending another prompt.");
    }

    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("").to_owned();
    let blobs: Vec<String> = body
        .get("blobs")
        .and_then(|b| b.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect())
        .unwrap_or_default();
    // Support inline base64 images from TUI (data:image/png;base64,...)
    let images: Vec<String> = body
        .get("images")
        .and_then(|b| b.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect())
        .unwrap_or_default();
    
    eprintln!("[prompt] text={} chars, blobs={}, images={}", text.len(), blobs.len(), images.len());

    let sid = SessionId(id.to_owned());

    // Build the user message: text + any image refs.
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(Content::Text { text });
    }
    // Handle blob references (pre-uploaded via /blob endpoint).
    for hash in &blobs {
        // Resolve mime from the stored blob for the content ref.
        let mime = state
            .store
            .get_blob(hash)
            .ok()
            .flatten()
            .map(|(_, m)| m)
            .unwrap_or_else(|| "application/octet-stream".into());
        content.push(Content::Image {
            sha256: format!("sha256:{hash}"),
            mime,
        });
    }
    // Handle inline base64 images (from clipboard paste).
    for (i, data_uri) in images.iter().enumerate() {
        eprintln!("[prompt] parsing image {} ({} chars)", i, data_uri.len());
        // Store as blob and get hash.
        if let Some((mime, data)) = parse_data_uri(data_uri) {
            eprintln!("[prompt] image {} parsed: mime={} data={} bytes", i, mime, data.len());
            match state.store.put_blob(&data, &mime) {
                Ok(hash) => {
                    eprintln!("[prompt] image {} stored: hash={}", i, hash);
                    content.push(Content::Image {
                        sha256: format!("sha256:{hash}"),
                        mime,
                    });
                }
                Err(e) => {
                    eprintln!("[prompt] Failed to store inline image: {:?}", e);
                }
            }
        }
    }
    let msg = Message { id: MsgId::new(), role: Role::User, content, silent: false };
    eprintln!("[prompt] built message with {} content parts", msg.content.len());

    let seq = match state.store.append(&sid, Event::MessageAppended { seq: 0, msg: msg.clone() }) {
        Ok(s) => {
            eprintln!("[prompt] appended message, seq={}", s);
            s
        }
        Err(e) => {
            eprintln!("[prompt] append failed: {:?}", e);
            return JsonResp::error(404, "not_found", &e.0);
        }
    };
    // Echo the durable event on the bus for attached observers.
    state
        .buses
        .publish(id, Event::MessageAppended { seq, msg });
    eprintln!("[prompt] published to bus, spawning turn");

    // Run the turn asynchronously (background OS thread; GI-5: no async runtime).
    turn::spawn_turn(state.clone(), sid);
    eprintln!("[prompt] turn spawned, returning OK");

    JsonResp::ok(serde_json::json!({ "accepted": true, "seq": seq }))
}

/// `POST /session/{id}/steer` — `{text}` [lease required]. Queues a steering
/// message that the running/next turn folds in.
pub fn steer(state: &Arc<ServerState>, id: &str, body: serde_json::Value) -> JsonResp {
    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("").to_owned();
    let sid = SessionId(id.to_owned());
    let msg = Message {
        id: MsgId::new(),
        role: Role::User,
        content: vec![Content::Text { text }],
        silent: false,
    };
    match state.store.append(&sid, Event::MessageAppended { seq: 0, msg: msg.clone() }) {
        Ok(seq) => {
            state.buses.publish(id, Event::MessageAppended { seq, msg });
            JsonResp::ok(serde_json::json!({ "steered": true, "seq": seq }))
        }
        Err(e) => JsonResp::error(404, "not_found", &e.0),
    }
}

/// `POST /session/{id}/abort` — cancel the running turn [lease required].
pub fn abort(state: &Arc<ServerState>, id: &str) -> JsonResp {
    turn::abort(state, id);
    JsonResp::ok(serde_json::json!({ "aborted": id }))
}

/// `POST /session/{id}/model` — `{provider, id}` [lease required]. Appends a
/// `ModelChanged` durable event.
pub fn set_model(state: &Arc<ServerState>, id: &str, body: serde_json::Value) -> JsonResp {
    eprintln!("[set_model] session={} body={}", id, body);
    let (provider, model_id) = match (
        body.get("provider").and_then(|x| x.as_str()),
        body.get("id").and_then(|x| x.as_str()),
    ) {
        (Some(p), Some(i)) => (p.to_owned(), i.to_owned()),
        _ => {
            eprintln!("[set_model] ERROR: provider and id required");
            return JsonResp::error(400, "bad_model", "provider and id required");
        }
    };
    eprintln!("[set_model] provider={} model_id={}", provider, model_id);
    let sid = SessionId(id.to_owned());
    let model = ModelRef { provider, id: model_id };
    match state
        .store
        .append(&sid, Event::ModelChanged { seq: 0, model: model.clone() })
    {
        Ok(seq) => {
            eprintln!("[set_model] SUCCESS seq={}", seq);
            state.buses.publish(id, Event::ModelChanged { seq, model });
            JsonResp::ok(serde_json::json!({ "model_set": true, "seq": seq }))
        }
        Err(e) => {
            eprintln!("[set_model] ERROR: {}", e.0);
            JsonResp::error(404, "not_found", &e.0)
        }
    }
}

/// `POST /approve` — `{id, decision, scope}` [lease required]. Records an approval
/// decision that the tool-dispatch policy consults. In v1 the decision is signaled
/// to the running turn via the approval registry.
pub fn approve(state: &Arc<ServerState>, body: serde_json::Value) -> JsonResp {
    let approval_id = body.get("id").and_then(|x| x.as_u64());
    let decision = body.get("decision").and_then(|d| d.as_str()).unwrap_or("deny");
    match approval_id {
        Some(aid) => {
            turn::record_approval(state, aid, decision == "allow");
            JsonResp::ok(serde_json::json!({ "approved": aid, "decision": decision }))
        }
        None => JsonResp::error(400, "bad_approval", "approval id required"),
    }
}

/// Test/helper: force the auto-title flow for a session's first assistant turn.
/// Exposed for the acceptance test and used internally by the turn runner.
pub fn maybe_autotitle(state: &Arc<ServerState>, session: &SessionId) {
    turn::maybe_autotitle(state, session);
}

/// Parse a data URI (e.g. "data:image/png;base64,iVBOR...") into (mime, bytes).
fn parse_data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    // Format: data:[<mediatype>][;base64],<data>
    let uri = uri.strip_prefix("data:")?;
    let (header, data) = uri.split_once(',')?;
    
    let is_base64 = header.ends_with(";base64");
    let mime = if is_base64 {
        header.strip_suffix(";base64")?
    } else {
        header
    };
    
    if is_base64 {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
        Some((mime.to_string(), bytes))
    } else {
        // URL-encoded data (rare for images).
        None
    }
}
