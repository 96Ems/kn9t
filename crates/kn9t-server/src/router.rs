//! R-SRV-010 — the HTTP surface router (DESIGN §12.1).
//!
//! Central dispatch: auth (R-SRV-020) and Origin rejection (R-SRV-030) are applied
//! to **every** request before routing. `[lease required]` routes check the write
//! lease and 409 otherwise (R-SRV-060). Unknown routes 404.
//!
//! The SSE route (`GET /session/{id}/events`) needs the raw request to hijack the
//! socket, so it is handled inline; every other route returns a `Reply`.

use std::sync::Arc;

use tiny_http::{Method, Request};

use crate::api;
use crate::auth;
use crate::http_util::{
    header, parse_json, path_of, query_of, query_param, read_body, respond, JsonResp, Reply,
};
use crate::routes;
use crate::sse;
use crate::state::ServerState;

/// A parsed path into segments (no leading empty element).
fn segments(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Which lease-required write routes exist (R-SRV-010: `[lease required]`).
fn is_lease_required(method: &Method, segs: &[&str]) -> bool {
    match (method, segs) {
        (Method::Post, ["session", _, action]) => {
            matches!(*action, "prompt" | "steer" | "abort" | "model" | "compact")
        }
        (Method::Post, ["approve"]) => true,
        _ => false,
    }
}

/// Handle one connection's request end-to-end. This is the single entry point the
/// server main loop and the tests both drive.
pub fn handle(state: &Arc<ServerState>, mut req: Request) {
    state.idle.touch();

    // R-SRV-030: reject any cross-origin request outright. A browser `fetch`
    // attaches an `Origin` header; a native client does not.
    if header(&req, "Origin").is_some() {
        return respond(
            req,
            Reply::Json(JsonResp::error(403, "origin_rejected", "cross-origin requests are refused")),
        );
    }

    // R-SRV-020: mandatory bearer auth on every request.
    let authed = match header(&req, "Authorization").and_then(auth::parse_bearer) {
        Some(tok) => auth::token_matches(&state.token, tok),
        None => false,
    };
    if !authed {
        return respond(
            req,
            Reply::Json(JsonResp::error(401, "unauthorized", "missing or invalid bearer token")),
        );
    }

    let method = req.method().clone();
    let url = req.url().to_owned();
    let path = path_of(&url).to_owned();
    let query = query_of(&url).to_owned();
    let segs = segments(&path);

    // SSE attach is special: it hijacks the socket. Handle before the generic
    // lease/reply path.
    if method == Method::Get {
        if let ["session", id, "events"] = segs.as_slice() {
            return handle_sse(state, req, id, &query);
        }
        // Global attach — keeps server alive without session subscription.
        if segs.as_slice() == ["attach"] {
            return handle_global_attach(state, req);
        }
    }

    // R-SRV-060: enforce the write lease on lease-required routes. The holder token
    // arrives in the `X-Lease` header (minted by POST /lease).
    if is_lease_required(&method, &segs) {
        let session = lease_session_for(&segs);
        let holder = header(&req, "X-Lease").map(|s| s.to_owned());
        let ok = match (&session, &holder) {
            (Some(sid), Some(h)) => state.leases.holds(sid, h),
            // /approve has no session in the path; require a valid holder for ANY
            // session the client claims via X-Lease-Session.
            (None, Some(h)) => {
                match header(&req, "X-Lease-Session") {
                    Some(sid) => state.leases.holds(sid, h),
                    None => false,
                }
            }
            _ => false,
        };
        if !ok {
            return respond(
                req,
                Reply::Json(JsonResp::error(409, "session_busy", "write lease required")),
            );
        }
    }

    let reply = route(state, &mut req, &method, &segs, &query);
    respond(req, reply);
}

/// The session id a lease-required route applies to.
fn lease_session_for(segs: &[&str]) -> Option<String> {
    match segs {
        ["session", id, _] => Some((*id).to_owned()),
        _ => None,
    }
}

/// Dispatch a non-SSE route to its handler, returning a [`Reply`].
fn route(
    state: &Arc<ServerState>,
    req: &mut Request,
    method: &Method,
    segs: &[&str],
    query: &str,
) -> Reply {
    match (method, segs) {
        // ── sessions ──
        (Method::Post, ["session"]) => match parse_json::<api::CreateSessionReq>(req) {
            Ok(body) => routes::session::create(state, body).into(),
            Err(e) => e.into(),
        },
        (Method::Get, ["session"]) => routes::session::list(state).into(),
        (Method::Get, ["session", id]) => routes::session::snapshot(state, id).into(),
        (Method::Post, ["session", id, "fork"]) => {
            match parse_json::<api::ForkReq>(req) {
                Ok(body) => routes::session::fork(state, id, body).into(),
                Err(e) => e.into(),
            }
        }
        (Method::Delete, ["session", id]) => routes::session::delete(state, id).into(),
        (Method::Post, ["session", id, "lease"]) => {
            let takeover = query_param(query, "takeover").as_deref() == Some("1");
            routes::session::lease_acquire(state, id, takeover).into()
        }
        (Method::Delete, ["session", id, "lease"]) => {
            let holder = header(req, "X-Lease").map(|s| s.to_owned());
            routes::session::lease_release(state, id, holder.as_deref()).into()
        }
        (Method::Post, ["session", id, "prompt"]) => {
            match parse_json::<api::PromptReq>(req) {
                Ok(body) => routes::session::prompt(state, id, body).into(),
                Err(e) => e.into(),
            }
        }
        (Method::Post, ["session", id, "steer"]) => {
            match parse_json::<api::SteerReq>(req) {
                Ok(body) => routes::session::steer(state, id, body).into(),
                Err(e) => e.into(),
            }
        }
        (Method::Post, ["session", id, "abort"]) => routes::session::abort(state, id).into(),
        (Method::Post, ["session", id, "model"]) => {
            crate::log!("POST /session/{}/model", id);
            match parse_json::<api::SetModelReq>(req) {
                Ok(body) => routes::session::set_model(state, id, body).into(),
                Err(e) => e.into(),
            }
        }
        (Method::Post, ["approve"]) => match parse_json::<api::ApproveReq>(req) {
            Ok(body) => routes::session::approve(state, body).into(),
            Err(e) => e.into(),
        },
        (Method::Post, ["session", id, "rename"]) => {
            match parse_json::<api::RenameReq>(req) {
                Ok(body) => routes::session::rename(state, id, body).into(),
                Err(e) => e.into(),
            }
        }
        (Method::Post, ["session", id, "compact"]) => routes::session::compact(state, id).into(),
        (Method::Get, ["session", id, "export"]) => routes::session::export_session(state, id).into(),
        (Method::Get, ["tools"]) => routes::tools::list(state).into(),

        // ── blobs ──
        (Method::Post, ["blob"]) => {
            let ct = header(req, "Content-Type").map(|s| s.to_owned());
            let body = read_body(req);
            routes::blob::put(state, body, ct.as_deref()).into()
        }
        (Method::Get, ["blob", hash]) => routes::blob::get(state, hash),

        // ── models / cost / budget ──
        (Method::Get, ["models"]) => routes::models::list(state).into(),
        (Method::Get, ["cost"]) => routes::cost::query(state, query).into(),
        (Method::Get, ["budget"]) => routes::cost::budget(state).into(),
        
        // ── preferences ──
        (Method::Get, ["pref", key]) => routes::pref::get(state, key).into(),
        (Method::Put, ["pref", key]) => {
            let body = read_body(req);
            let value = String::from_utf8_lossy(&body).to_string();
            routes::pref::set(state, key, &value).into()
        }
        
        // ── policy info (ADR-0008: plugin decides, server routes) ──
        (Method::Get, ["policy"]) => routes::policy::get_state(state).into(),

        // ── server control ──
        (Method::Post, ["stop"]) => {
            state.stop_requested.store(true, std::sync::atomic::Ordering::SeqCst);
            crate::log!("stop requested via POST /stop");
            JsonResp::ok(serde_json::json!({"ok": true})).into()
        }
        (Method::Get, ["health"]) => {
            JsonResp::ok(serde_json::json!({
                "ok": true,
                "idle_secs": state.idle.last_activity_elapsed().as_secs(),
                "attached_clients": state.idle.attached_count(),
                "running_turns": state.idle.running_turns(),
            })).into()
        }

        // ── plugin hot-reload (R-PLUG2-100) ──
        (Method::Post, ["plugin", name, "reload"]) => routes::plugin::reload(state, name),

        // ── 96E-28 generic interaction ──
        (Method::Post, ["ui-respond"]) => match parse_json::<api::UiRespondReq>(req) {
            Ok(body) => routes::interaction::respond(state, body).into(),
            Err(e) => e.into(),
        },
        
        // ── unknown ──
        _ => JsonResp::error(404, "not_found", "no such route").into(),
    }
}

/// R-SRV-040/050 — SSE attach. Subscribe first, replay durable + live_partial,
/// dedup-flush buffer, then live loop. Holds the write lock for none of it.
///
/// If the client passes `?lease={holder}` (the token it acquired via POST /lease),
/// this stream *owns* that lease: it keeps it warm on every heartbeat and releases
/// it when the stream ends (DESIGN §12.6). This is what stops an attached client
/// that reads for >5 min without writing from silently idle-losing its lease and
/// then getting a 409 on its next prompt.
fn handle_sse(state: &Arc<ServerState>, req: Request, session: &str, query: &str) {
    let from: u64 = query_param(query, "from")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let lease_holder = query_param(query, "lease");

    crate::log!("SSE attach: session={} from={} lease={:?}", session, from, lease_holder.is_some());

    // Step 1: subscribe FIRST (buffer everything from now on).
    let sub = state.buses.subscribe(session, sse::SSE_RING_CAPACITY);

    // Steps 2–4: build the ordered prelude (durable replay, live_partial, dedup).
    let prelude = sse::build_attach_prelude(&state.store, session, from, &sub);

    // Note: session SSE does NOT increment attached_clients.
    // Only /attach does that (one per client process).
    crate::log!("SSE session connect: session={}", session);

    let mut writer = req.into_writer();
    // Keep the owning lease warm on every heartbeat while this stream lives.
    let mut on_alive = || {
        if let Some(holder) = &lease_holder {
            state.leases.touch(session, holder);
        }
    };
    let write_res = (|| -> std::io::Result<()> {
        sse::write_sse_head(&mut writer)?;
        for frame in &prelude.frames {
            writer.write_all(frame.as_bytes())?;
        }
        writer.flush()?;
        sse::run_live_loop(&mut writer, &sub, &mut on_alive)
    })();

    if let Err(ref e) = write_res {
        crate::log!("SSE session error: {}", e);
    }
    // The stream owned the lease; its end releases it (DESIGN §12.6). A no-op if
    // the holder no longer matches (e.g. another client took over).
    if let Some(holder) = &lease_holder {
        state.leases.release(session, holder);
    }
    crate::log!("SSE session disconnect: session={}", session);
}

/// Global attach endpoint — keeps server alive without subscribing to a session.
/// The client connects here at startup and stays connected until it exits.
/// Server sends periodic heartbeats; client disconnect triggers idle-exit check.
fn handle_global_attach(state: &Arc<ServerState>, req: Request) {
    crate::log!("global attach");
    
    // Mark client as attached (keeps server alive, R-SRV-080).
    state.idle.client_attached();
    crate::log!("global attached: clients={}", state.idle.attached_count());
    
    let mut writer = req.into_writer();
    let write_res = (|| -> std::io::Result<()> {
        sse::write_sse_head(&mut writer)?;
        
        // Send initial hello event.
        writer.write_all(b"event: hello\ndata: {}\n\n")?;
        writer.flush()?;
        
        // Heartbeat loop — a write failure is how we detect a dead client, which is
        // what eventually drives idle-exit. Use the shared `heartbeat_interval()` so the
        // period is configurable via KN9T_SSE_HEARTBEAT_MS (default 15s) rather than a
        // hardcoded 30s; session SSE already does this, and tests depend on it.
        loop {
            std::thread::sleep(sse::heartbeat_interval());
            writer.write_all(b"event: ping\ndata: {}\n\n")?;
            writer.flush()?;
        }
    })();
    
    if let Err(ref e) = write_res {
        crate::log!("global attach error: {}", e);
    }
    
    // Detached: client gone.
    state.idle.client_detached();
    crate::log!("global detached: clients={}", state.idle.attached_count());
}

use std::io::Write;
