//! Session lifecycle + write routes (R-SRV-010, R-SRV-060, R-SRV-100).
//!
//! Create/list/snapshot/fork/delete, the write lease, and the writer commands
//! (`prompt`/`steer`/`abort`/`model`/`approve`). `prompt` drives a ReAct turn on a
//! background thread, publishing events to the session bus; after the first
//! assistant turn of a nameless session it issues one cheap title call
//! (R-SRV-100), best-effort.
//!
//! Request bodies are **typed** — the router deserializes into `crate::api` structs
//! (schema-generated, `deny_unknown_fields`), so an unknown/mistyped field is a
//! 400, never a silent ignore (F6).

use std::sync::Arc;

use kn9t_core::{Content, Event, ForkReason, Message, ModelRef, MsgId, Role, SessionId, Store};

use crate::api;
use crate::http_util::{millis_to_iso, JsonResp};
use crate::state::ServerState;
use crate::turn;

/// `POST /session` — create; body `{cwd?, model?, name?}`.
pub fn create(state: &Arc<ServerState>, req: api::CreateSessionReq) -> JsonResp {
    let cwd = req
        .cwd
        .unwrap_or_else(|| state.cwd.to_str().unwrap_or(".").to_owned());
    let name = req.name;

    // Resolve the model: body-supplied ref, else server default.
    let model_ref = if let Some(m) = req.model {
        ModelRef {
            provider: m.provider,
            id: m.id,
        }
    } else {
        match state.default_model.as_ref() {
            Some(s) => s.r#ref.clone(),
            None => {
                return JsonResp::error(400, "bad_model", "no model supplied and no server default")
            }
        }
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

/// `GET /session` — list sessions.
///
/// F5: `created_at` is stored as INTEGER millis; the schema pins it as an ISO8601
/// string, so the boundary normalizes here (`millis_to_iso`).
pub fn list(state: &Arc<ServerState>) -> JsonResp {
    let sql = "SELECT json_object('id', id, 'name', name, 'cwd', cwd, 'head_seq', head_seq, \
               'created_at', created_at) FROM sessions ORDER BY created_at DESC";
    let strings = state.store.query_strings(sql, &[]).unwrap_or_default();
    let sessions: Vec<serde_json::Value> = strings
        .iter()
        .filter_map(|s| {
            let mut v: serde_json::Value = serde_json::from_str(s).ok()?;
            if let Some(ms) = v.get("created_at").and_then(|c| c.as_i64()) {
                v["created_at"] = serde_json::Value::String(millis_to_iso(ms));
            }
            Some(v)
        })
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

    // Meta. `created_at` normalized to ISO8601 at the boundary (F5).
    let meta_sql = "SELECT json_object('id', id, 'name', name, 'cwd', cwd, \
                    'created_at', created_at) FROM sessions WHERE id=?1";
    let mut meta: serde_json::Value = state
        .store
        .query_strings(meta_sql, &[&id])
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    if let Some(ms) = meta.get("created_at").and_then(|c| c.as_i64()) {
        meta["created_at"] = serde_json::Value::String(millis_to_iso(ms));
    }

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

/// `POST /session/{id}/fork` — `{origin_seq?, reason?}` → new session id.
/// `reason` is validated against the schema enum (unknown → 400).
pub fn fork(state: &Arc<ServerState>, id: &str, req: api::ForkReq) -> JsonResp {
    let origin = SessionId(id.to_owned());
    let origin_seq = req.origin_seq.unwrap_or(0);
    let reason = match req.reason.as_deref() {
        Some("fork") => ForkReason::Fork,
        Some("rewind") => ForkReason::Rewind,
        Some("subagent") => ForkReason::Subagent,
        Some("tree") => ForkReason::Tree,
        Some(other) => {
            return JsonResp::error(
                400,
                "bad_reason",
                &format!("reason must be one of fork|rewind|subagent|tree, got '{other}'"),
            )
        }
        None => ForkReason::Fork,
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
    match kn9t_store::fork_session(
        &state.store,
        &origin,
        &new_id,
        origin_seq,
        reason,
        None,
        &cwd,
    ) {
        Ok(()) => JsonResp::ok(serde_json::json!({ "id": new_id.0 })),
        Err(e) => JsonResp::error(400, "fork_failed", &e.0),
    }
}

/// `DELETE /session/{id}`.
pub fn delete(state: &Arc<ServerState>, id: &str) -> JsonResp {
    // 404 if session doesn't exist.
    let exists: bool = state
        .store
        .query_one("SELECT 1 FROM sessions WHERE id=?1", &[&id], |_| Ok(1i64))
        .is_ok();
    if !exists {
        return JsonResp::error(404, "not_found", "session not found");
    }
    let sid = SessionId(id.to_owned());
    match state.store.delete_session(&sid) {
        Ok(()) => {
            state.buses.drop_session(id);
            state.ui_pages.clear_session(id);
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
        AcquireResult::Busy => {
            JsonResp::error(409, "session_busy", "another client holds the lease")
        }
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

/// `POST /session/{id}/prompt` — `{text?, blobs?, images?}` [lease required].
/// Appends the user message and runs a turn on a background thread.
///
/// Returns 409 Conflict if a turn is already running for this session. The client
/// should wait for the turn to complete (via SSE TurnEnded event) before sending
/// another prompt. This prevents transcript corruption when the user aborts a turn
/// and immediately sends a new prompt before the abort completes.
///
/// F12: debug scaffolding moved from `eprintln!` to `crate::log!`.
pub fn prompt(state: &Arc<ServerState>, id: &str, req: api::PromptReq) -> JsonResp {
    let text = req.text.unwrap_or_default();
    let blobs = req.blobs.unwrap_or_default();
    let images = req.images.unwrap_or_default();
    crate::log!(
        "[prompt] session={} text={} chars, blobs={}, images={}",
        id,
        text.len(),
        blobs.len(),
        images.len()
    );

    // Check if a turn is already running for this session.
    // This prevents the race condition where abort + immediate new prompt
    // causes the user message to be appended before tool_results, corrupting
    // the transcript and causing "tool_use ids without tool_result blocks" errors.
    if turn::is_turn_running(state, id) {
        crate::log!("[prompt] turn already running, rejecting");
        return JsonResp::error(409, "turn_running",
            "A turn is already running for this session. Wait for it to complete before sending another prompt.");
    }

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
        if let Some((mime, data)) = parse_data_uri(data_uri) {
            crate::log!(
                "[prompt] image {i} parsed: mime={mime} data={} bytes",
                data.len()
            );
            match state.store.put_blob(&data, &mime) {
                Ok(hash) => {
                    crate::log!("[prompt] image {i} stored: hash={hash}");
                    content.push(Content::Image {
                        sha256: format!("sha256:{hash}"),
                        mime,
                    });
                }
                Err(e) => {
                    crate::log!("[prompt] failed to store inline image: {e:?}");
                }
            }
        }
    }
    let msg = Message {
        id: MsgId::new(),
        role: Role::User,
        content,
        silent: false,
    };

    let seq = match state.store.append(
        &sid,
        Event::MessageAppended {
            seq: 0,
            msg: msg.clone(),
        },
    ) {
        Ok(s) => {
            crate::log!("[prompt] appended message, seq={s}");
            s
        }
        Err(e) => {
            crate::log!("[prompt] append failed: {:?}", e);
            return JsonResp::error(404, "not_found", &e.0);
        }
    };
    // 96E-18: the durable SSE echo happens in the store after-append observer
    // (ServerState::new), not here — one publisher, no duplicates.

    // Run the turn asynchronously (background OS thread; GI-5: no async runtime).
    turn::spawn_turn(state.clone(), sid);

    JsonResp::ok(serde_json::json!({ "accepted": true, "seq": seq }))
}

/// `POST /session/{id}/steer` — `{text}` [lease required]. Queues a steering
/// message that the running/next turn folds in.
///
/// If a turn is running: the message is queued in-memory and will be drained by
/// `get_steering()` AFTER tool_results. This prevents transcript corruption:
/// `[tool_use] -> [tool_result] -> [steer]` instead of the invalid
/// `[tool_use] -> [steer] -> [tool_result]`.
///
/// If no turn is running: the message is appended immediately to the store
/// (original behavior).
pub fn steer(state: &Arc<ServerState>, id: &str, req: api::SteerReq) -> JsonResp {
    let text = req.text;
    let sid = SessionId(id.to_owned());
    let msg = Message {
        id: MsgId::new(),
        role: Role::User,
        content: vec![Content::Text { text }],
        silent: false,
    };

    // If a turn is running, queue the message to prevent transcript corruption.
    // The message will be drained by get_steering() AFTER tool_results.
    if turn::is_turn_running(state, id) {
        state.queue_steering(id, msg);
        return JsonResp::ok(serde_json::json!({ "steered": true, "queued": true }));
    }

    // No turn running: append immediately (original behavior).
    match state.store.append(
        &sid,
        Event::MessageAppended {
            seq: 0,
            msg: msg.clone(),
        },
    ) {
        Ok(seq) => {
            // 96E-18: SSE echo via the store after-append observer.
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
pub fn set_model(state: &Arc<ServerState>, id: &str, req: api::SetModelReq) -> JsonResp {
    let model = ModelRef {
        provider: req.provider,
        id: req.id,
    };
    crate::log!(
        "[set_model] session={id} provider={} id={}",
        model.provider,
        model.id
    );
    let sid = SessionId(id.to_owned());
    match state.store.append(
        &sid,
        Event::ModelChanged {
            seq: 0,
            model: model.clone(),
        },
    ) {
        Ok(seq) => {
            crate::log!("[set_model] success seq={seq}");
            // 96E-18: SSE echo via the store after-append observer.
            JsonResp::ok(serde_json::json!({ "model_set": true, "seq": seq }))
        }
        Err(e) => {
            crate::log!("[set_model] error: {}", e.0);
            JsonResp::error(404, "not_found", &e.0)
        }
    }
}

/// `POST /session/{id}/tools` — `{disabled: [String]}` [lease required].
/// Sets the full list of tools DISABLED for this session. Appends a `ToolsToggled`
/// durable event. Blocking is enforced at tool-execution time (the provider still
/// sees every tool spec so the cache prefix is unchanged).
///
/// The response includes `reenabled`: tools that were disabled and are no longer.
/// The server also stores them in `pending_reactivation` so the next `spawn_turn`
/// can inject a one-shot `<system-reminder>` informing the agent.
pub fn set_tools(state: &Arc<ServerState>, id: &str, req: api::SetToolsReq) -> JsonResp {
    let sid = SessionId(id.to_owned());

    // Compute reenabled = (old disabled) - (new disabled).
    let old_disabled: std::collections::HashSet<String> = state
        .store
        .snapshot(&sid)
        .map(|s| s.disabled_tools.into_iter().collect())
        .unwrap_or_default();
    let new_disabled: std::collections::HashSet<String> = req.disabled.iter().cloned().collect();
    let reenabled: Vec<String> = old_disabled.difference(&new_disabled).cloned().collect();

    crate::log!(
        "[set_tools] session={id} disabled={:?} reenabled={:?}",
        req.disabled,
        reenabled
    );

    // Append the durable event (full replacement: last ToolsToggled wins on replay).
    match state.store.append(
        &sid,
        Event::ToolsToggled {
            seq: 0,
            disabled: req.disabled,
        },
    ) {
        Ok(seq) => {
            // Store reenabled for the next turn's one-shot reminder.
            if !reenabled.is_empty() {
                let mut map = state
                    .pending_reactivation
                    .lock()
                    .expect("pending_reactivation poisoned");
                map.insert(id.to_owned(), reenabled.iter().cloned().collect());
            }
            let reenabled_json: Vec<serde_json::Value> = reenabled
                .into_iter()
                .map(serde_json::Value::String)
                .collect();
            JsonResp::ok(serde_json::json!({
                "tools_set": true,
                "seq": seq,
                "reenabled": reenabled_json,
            }))
        }
        Err(e) => {
            crate::log!("[set_tools] error: {}", e.0);
            JsonResp::error(404, "not_found", &e.0)
        }
    }
}

/// `POST /approve` — `{id, decision, scope}` [lease required]. Records an approval
/// decision that the tool-dispatch policy consults. In v1 the decision is signaled
/// to the running turn via the approval registry. `scope` is `once` (default),
/// `session`, or `always` (writes back to `~/.kn9t/config.toml`). The TUI's legacy
/// `"always"` decision is treated as `decision=allow, scope=always` for compat.
/// Unknown `decision` or `scope` values are 400 (not default-deny) — validation
/// lives in `turn::resolve_approval` and is surfaced verbatim.
pub fn approve(state: &Arc<ServerState>, req: api::ApproveReq) -> JsonResp {
    let decision = req.decision.as_str();
    let scope = req.scope.as_deref();
    match turn::resolve_approval(state, req.id, decision, scope) {
        Ok(_) => JsonResp::ok(serde_json::json!({
            "approved": req.id,
            "decision": decision,
            "scope": scope.unwrap_or(if decision == "always" { "always" } else { "once" }),
        })),
        Err(e) => JsonResp::error(400, "bad_approval", &e),
    }
}

/// `POST /session/{id}/rename` — `{name}` action endpoint (no PATCH).
/// Updates `sessions.name` and emits `TitleChanged` so the TUI can update
/// the sidebar. A manual rename suppresses auto-titling (R-SRV-100 already
/// checks for an existing name before title-calling).
pub fn rename(state: &Arc<ServerState>, id: &str, req: api::RenameReq) -> JsonResp {
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return JsonResp::error(400, "bad_name", "name must be non-empty");
    }
    if name.len() > 80 {
        return JsonResp::error(400, "bad_name", "name must be <= 80 chars");
    }
    // 404 if session doesn't exist.
    let exists: bool = state
        .store
        .query_one("SELECT 1 FROM sessions WHERE id=?1", &[&id], |_| Ok(1i64))
        .is_ok();
    if !exists {
        return JsonResp::error(404, "not_found", "session not found");
    }
    if let Err(e) = state.store.execute_raw(
        "UPDATE sessions SET name=?1 WHERE id=?2",
        &[&name.as_str(), &id],
    ) {
        return JsonResp::error(500, "store_error", &e.0);
    }
    // Notify SSE subscribers.
    state.buses.publish(
        id,
        Event::TitleChanged {
            title: name.clone(),
        },
    );
    JsonResp::ok(serde_json::json!({ "id": id, "name": name }))
}

/// `POST /session/{id}/compact` — manually trigger compaction [lease required].
/// The engine already exists (`exec.rs:139 run_compaction`); previously only
/// reachable via automatic threshold. This endpoint forces a compaction over
/// the oldest half of messages. If a provider is available it summarizes via
/// the model; otherwise a deterministic local summary is used so the endpoint
/// works offline and the test needs no network.
pub fn compact(state: &Arc<ServerState>, id: &str) -> JsonResp {
    let sid = SessionId(id.to_owned());
    // 404 if session missing.
    let exists: bool = state
        .store
        .query_one("SELECT 1 FROM sessions WHERE id=?1", &[&id], |_| Ok(1i64))
        .is_ok();
    if !exists {
        return JsonResp::error(404, "not_found", "session not found");
    }

    // Try automatic compaction via plan_request first (threshold-based).
    // If none, force a span over the oldest half.
    let plan = match state.store.plan_request(&sid) {
        Ok(p) => p,
        Err(e) => return JsonResp::error(500, "store_error", &e.0),
    };

    let compact_span = if let Some(span) = plan.compact {
        span
    } else {
        // Force compaction if we have at least 2 messages, else error.
        if plan.messages.len() < 2 {
            return JsonResp::error(400, "nothing_to_compact", "not enough messages to compact");
        }
        // Replicate `plan::compact_span` logic here without needing private helper.
        let mut cut = plan.messages.len() / 2;
        // Avoid orphaned ToolCall: if the cut leaves a ToolCall without result, extend.
        loop {
            if cut >= plan.messages.len() {
                break;
            }
            let has_orphan = plan.messages[..cut].iter().any(|m| {
                m.content.iter().any(|c| matches!(c, Content::ToolCall { id, .. } if {
                    let has_result = plan.messages[..cut].iter().any(|m2| {
                        m2.content.iter().any(|c2| matches!(c2, Content::ToolResult { id: rid, .. } if rid == id))
                    });
                    !has_result
                }))
            });
            if has_orphan {
                cut += 1;
            } else {
                break;
            }
        }
        // Need seqs to build SeqRange — fetch from DB ordering.
        // Use json_object so query_strings (which reads column 0 as String) works for INTEGER.
        let seqs: Vec<u64> = {
            let sql =
                "SELECT json_object('seq', seq) FROM messages WHERE session_id=?1 ORDER BY seq";
            state
                .store
                .query_strings(sql, &[&id])
                .unwrap_or_default()
                .iter()
                .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .filter_map(|v| v.get("seq").and_then(|x| x.as_u64()))
                .collect()
        };
        // Fallback: if seqs unavailable, use 1..cut
        let start = seqs.first().copied().unwrap_or(1);
        let end = if cut > 0 {
            seqs.get(cut - 1).copied().unwrap_or(cut as u64)
        } else {
            start
        };
        kn9t_core::CompactSpan {
            replaced: kn9t_core::SeqRange { start, end },
            messages: plan.messages[..cut].to_vec(),
        }
    };

    // Build a summary message. Prefer provider summarization if a provider is
    // available; otherwise use a deterministic local summary so the endpoint
    // is testable offline.
    let summary = if let (Some(provider), Some(model)) = (
        state.provider.clone().or_else(|| {
            state
                .default_model
                .as_ref()
                .and_then(|m| state.get_provider(&m.r#ref.provider))
        }),
        state
            .default_model
            .clone()
            .or_else(|| state.store.get_model_spec_for_session(id)),
    ) {
        // Try provider summarize (best-effort, 16 max_tokens, short timeout via Cancel).
        let mut msgs = compact_span.messages.clone();
        msgs.push(Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text {
                text: "Summarize the conversation so far, preserving decisions, file paths, and open tasks, so it can replace the older messages.".to_string(),
            }],
            silent: false,
        });
        let req = kn9t_core::Request {
            model: &model,
            system: Some("You produce only a concise summary, nothing else."),
            messages: &msgs,
            tools: &[],
            thinking: kn9t_core::Thinking::Off,
            max_tokens: Some(256),
            cache: &[],
        };
        let cancel = kn9t_core::Cancel::new();
        match provider.stream(&req, &cancel) {
            Ok(stream) => {
                let mut text = String::new();
                for item in stream {
                    if let Ok(kn9t_core::Chunk::Text { delta, .. }) = item {
                        text.push_str(&delta);
                    }
                }
                let t = text.trim().to_string();
                if t.is_empty() {
                    deterministic_summary(&compact_span.messages)
                } else {
                    Message {
                        id: MsgId::new(),
                        role: Role::Assistant,
                        content: vec![Content::Text { text: t }],
                        silent: false,
                    }
                }
            }
            Err(_) => deterministic_summary(&compact_span.messages),
        }
    } else {
        deterministic_summary(&compact_span.messages)
    };

    let event = Event::Compacted {
        seq: 0,
        replaced: compact_span.replaced.clone(),
        summary,
    };
    let seq = match state.store.append(&sid, event.clone()) {
        Ok(s) => s,
        Err(e) => return JsonResp::error(500, "store_error", &e.0),
    };
    // 96E-18: SSE echo via the store after-append observer (seq-stamped there).
    JsonResp::ok(serde_json::json!({ "compacted": true, "seq": seq, "message": "compacted" }))
}

fn deterministic_summary(msgs: &[Message]) -> Message {
    let excerpt: String = msgs
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let snippet: String = excerpt.chars().take(200).collect();
    let text = if snippet.is_empty() {
        format!("Summary of {} earlier messages.", msgs.len())
    } else {
        format!("Summary of {} earlier messages: {}", msgs.len(), snippet)
    };
    Message {
        id: MsgId::new(),
        role: Role::Assistant,
        content: vec![Content::Text { text }],
        silent: false,
    }
}

/// `GET /session/{id}/export` — full transcript + events dump (replaces TUI
/// `/export` placeholder that printed "planned for a future release").
pub fn export_session(state: &Arc<ServerState>, id: &str) -> JsonResp {
    let sid = SessionId(id.to_owned());
    // 404 if session missing.
    let exists: bool = state
        .store
        .query_one("SELECT 1 FROM sessions WHERE id=?1", &[&id], |_| Ok(1i64))
        .is_ok();
    if !exists {
        return JsonResp::error(404, "not_found", "session not found");
    }
    // Meta.
    let meta_sql = "SELECT json_object('id', id, 'name', name, 'cwd', cwd, 'created_at', created_at) FROM sessions WHERE id=?1";
    let mut meta: serde_json::Value = state
        .store
        .query_strings(meta_sql, &[&id])
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    if let Some(ms) = meta.get("created_at").and_then(|c| c.as_i64()) {
        meta["created_at"] = serde_json::Value::String(crate::http_util::millis_to_iso(ms));
    }
    // Transcript (message projection).
    let msg_sql = "SELECT json_object('seq', seq, 'role', role, 'content', json(content), 'silent', CASE WHEN silent THEN json('true') ELSE json('false') END) FROM messages WHERE session_id=?1 ORDER BY seq";
    let transcript: Vec<serde_json::Value> = state
        .store
        .query_strings(msg_sql, &[&id])
        .unwrap_or_default()
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    // Events (raw durable log).
    let ev_sql = "SELECT payload FROM events WHERE session_id=?1 ORDER BY seq";
    let events: Vec<serde_json::Value> = state
        .store
        .query_strings(ev_sql, &[&id])
        .unwrap_or_default()
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    // Also include snapshot for convenience.
    let _ = sid;
    JsonResp::ok(
        serde_json::json!({ "id": id, "meta": meta, "transcript": transcript, "events": events }),
    )
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
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        Some((mime.to_string(), bytes))
    } else {
        // URL-encoded data (rare for images).
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_to_iso_known_epoch() {
        // 0 ms → Unix epoch.
        assert_eq!(millis_to_iso(0), "1970-01-01T00:00:00Z");
        // 1_757_001_600_000 ms = 2025-09-04T16:00:00Z.
        assert_eq!(millis_to_iso(1_757_001_600_000), "2025-09-04T16:00:00Z");
    }

    #[test]
    fn millis_to_iso_rounds_and_clamps() {
        // Mid-day + sub-second: seconds dropped, Z appended.
        assert_eq!(millis_to_iso(1_757_005_200_500), "2025-09-04T17:00:00Z");
        // Negative (pre-1970) uses floor division (proleptic Gregorian).
        assert_eq!(millis_to_iso(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn millis_to_iso_leap_year() {
        // 2024-02-29T00:00:00Z = 1_709_164_800_000 ms.
        assert_eq!(millis_to_iso(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }
}
