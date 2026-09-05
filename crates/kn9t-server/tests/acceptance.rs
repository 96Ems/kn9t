//! Stage 06 acceptance tests (spec `06-server.md`).
//!
//! Each test spins up a real in-process server on an ephemeral port and drives it
//! over HTTP (R-SRV instructions). Named exactly as the spec's `**Accept:**`
//! clauses: `srv::routes`, `srv::auth_required`, `srv::origin_rejected`,
//! `srv::sse_no_gap_no_dup`, `srv::lease_single_writer`, `srv::spawn_race`,
//! `srv::idle_exit`, `srv::blob_roundtrip`, `srv::autotitle`, `srv::cost_query`,
//! `srv::budget_reports_both`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kn9t_core::{
    Approver, CacheMode, Cancel, Chunk, Content, Event, ModelRef, ModelSpec, MsgId, Price, ProvErr,
    Provider, Quirks, Request, Role, SessionId, StopReason, Store, Tokens, Usage,
};
use kn9t_server::state::ServerState;
use kn9t_server::ServerHandle;

// ── harness ──────────────────────────────────────────────────────────────────

struct Harness {
    handle: ServerHandle,
    token: String,
    port: u16,
    _tmp: tempfile::TempDir,
}

fn temp_store() -> (Arc<kn9t_store::SqliteStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("kn9t.db");
    let store = kn9t_store::SqliteStore::open(&db).unwrap();
    (Arc::new(store), tmp)
}

fn model_spec() -> ModelSpec {
    ModelSpec {
        r#ref: ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        },
        api_id: "m1".into(),
        ctx_window: 128_000,
        max_out: 4096,
        price: Price {
            input: 1000000,
            output: 2000000,
            cache_read: 500000,
            cache_write: 0,
        },
        cache: CacheMode::Automatic,
        streaming: true,
        quirks: Quirks::default(),
    }
}

fn start(state: Arc<ServerState>) -> Harness {
    let token = state.token.clone();
    let (store_tmp, tmp) = (state.clone(), tempfile::tempdir().unwrap());
    let _ = store_tmp;
    let handle = ServerHandle::spawn(state).unwrap();
    let port = handle.port;
    Harness {
        handle,
        token,
        port,
        _tmp: tmp,
    }
}

/// Build a state with a fresh temp store and a random token.
fn fresh_state() -> (Arc<ServerState>, tempfile::TempDir) {
    let (store, tmp) = temp_store();
    let token = kn9t_server::auth::generate_token();
    let spec = model_spec();
    let mut state = ServerState::new(store, token, Default::default(), Vec::new())
        .with_default_model(spec.clone());
    state.model_registry = vec![spec];
    let state = Arc::new(state);
    (state, tmp)
}

/// Spin up a harness with a fresh store (keeps the tempdir alive for the test).
fn harness() -> (Harness, tempfile::TempDir) {
    let (state, tmp) = fresh_state();
    (start(state), tmp)
}

// ── minimal HTTP client (avoids ureq quirks with custom methods/headers) ──────

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Make a raw HTTP/1.1 request and read the full (non-SSE) response.
fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    parse_response(&raw)
}

/// Authorized JSON request helper.
fn req_auth(
    h: &Harness,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: serde_json::Value,
) -> Resp {
    let auth = format!("Bearer {}", h.token);
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", &auth)];
    headers.extend_from_slice(extra_headers);
    let body_bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap()
    };
    request(h.port, method, path, &headers, &body_bytes)
}

fn parse_response(raw: &[u8]) -> Resp {
    let split = find_subslice(raw, b"\r\n\r\n").unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = if split + 4 <= raw.len() {
        raw[split + 4..].to_vec()
    } else {
        Vec::new()
    };
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            l.split_once(": ")
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        })
        .collect();
    Resp {
        status,
        headers,
        body,
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Create a session and return its id.
fn make_session(h: &Harness) -> String {
    let r = req_auth(
        h,
        "POST",
        "/session",
        &[],
        serde_json::json!({ "cwd": "." }),
    );
    assert_eq!(
        r.status,
        200,
        "create session: {}",
        String::from_utf8_lossy(&r.body)
    );
    r.json()["id"].as_str().unwrap().to_owned()
}

/// Acquire the lease and return the holder token.
fn acquire_lease(h: &Harness, id: &str) -> String {
    let r = req_auth(
        h,
        "POST",
        &format!("/session/{id}/lease"),
        &[],
        serde_json::Value::Null,
    );
    assert_eq!(r.status, 200);
    r.json()["lease"].as_str().unwrap().to_owned()
}

// ── acceptance tests — named srv::* per spec ─────────────────────────────────

mod srv {
    use super::*;

    // ── srv::routes (R-SRV-010) ───────────────────────────────────────────────────

    #[test]
    fn routes() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;

        // Each documented route exists with the right method + lease semantics.
        // GET /session
        assert_eq!(
            req_auth(&h, "GET", "/session", &[], serde_json::Value::Null).status,
            200
        );
        // GET /session/{id}
        assert_eq!(
            req_auth(
                &h,
                "GET",
                &format!("/session/{id}"),
                &[],
                serde_json::Value::Null
            )
            .status,
            200
        );
        // POST /session/{id}/fork
        let fork = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/fork"),
            &[],
            serde_json::json!({ "origin_seq": 0, "reason": "fork" }),
        );
        assert_eq!(
            fork.status,
            200,
            "fork: {}",
            String::from_utf8_lossy(&fork.body)
        );

        // Lease-required routes WITHOUT the lease header → 409.
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/prompt"),
                &[],
                serde_json::json!({ "text": "hi" })
            )
            .status,
            409
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/steer"),
                &[],
                serde_json::json!({ "text": "hi" })
            )
            .status,
            409
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/abort"),
                &[],
                serde_json::Value::Null
            )
            .status,
            409
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/model"),
                &[],
                serde_json::json!({ "provider": "test", "id": "m1" })
            )
            .status,
            409
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                "/approve",
                &[],
                serde_json::json!({ "id": 1, "decision": "allow" })
            )
            .status,
            409
        );

        // WITH the lease header → accepted (200).
        let lease_hdr: [(&str, &str); 1] = [("X-Lease", lh)];
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/steer"),
                &lease_hdr,
                serde_json::json!({ "text": "hi" })
            )
            .status,
            200
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/abort"),
                &lease_hdr,
                serde_json::Value::Null
            )
            .status,
            200
        );
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/model"),
                &lease_hdr,
                serde_json::json!({ "provider": "test", "id": "m1" })
            )
            .status,
            200
        );
        let approve_hdr: [(&str, &str); 2] = [("X-Lease", lh), ("X-Lease-Session", &id)];
        assert_eq!(
            req_auth(
                &h,
                "POST",
                "/approve",
                &approve_hdr,
                serde_json::json!({ "id": 1, "decision": "allow" })
            )
            .status,
            200
        );

        // Blob, models, cost, budget.
        assert_eq!(
            req_auth(&h, "GET", "/models", &[], serde_json::Value::Null).status,
            200
        );
        assert_eq!(
            req_auth(
                &h,
                "GET",
                "/cost?group_by=model",
                &[],
                serde_json::Value::Null
            )
            .status,
            200
        );
        assert_eq!(
            req_auth(&h, "GET", "/budget", &[], serde_json::Value::Null).status,
            200
        );

        // DELETE /session/{id}/lease then DELETE /session/{id}.
        // Use a fresh, unforked session: R-STOR-160 forbids deleting a fork origin,
        // and `id` above is now the origin of `fork`.
        assert_eq!(
            req_auth(
                &h,
                "DELETE",
                &format!("/session/{id}/lease"),
                &lease_hdr,
                serde_json::Value::Null
            )
            .status,
            200
        );
        let victim = make_session(&h);
        assert_eq!(
            req_auth(
                &h,
                "DELETE",
                &format!("/session/{victim}"),
                &[],
                serde_json::Value::Null
            )
            .status,
            200
        );

        // Unknown route → 404.
        assert_eq!(
            req_auth(&h, "GET", "/nonesuch", &[], serde_json::Value::Null).status,
            404
        );

        h.handle.shutdown();
    }

    /// 96E-18 — durable appends echo on the SSE bus after commit, exactly once.
    ///
    /// Regression guard for 96E-12: `EventSink` is transient-only, so `MessageAppended`
    /// emitted by the loop/routes reach SSE observers solely through the store
    /// after-append observer installed in `ServerState::new`. Without it the TUI never
    /// sees assistant/tool messages live (no tool cards, streamed text dropped on the
    /// next turn start). The prompt route previously published manually; now the
    /// observer is the single publisher — this test fails if either side regresses
    /// (missing echo, or duplicate echo).
    #[test]
    fn p1_96e18_durable_appends_echo_on_sse_bus() {
        let (state, _tmp) = fresh_state();
        let h = start(state.clone());
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);

        // Subscribe BEFORE the prompt so the echo lands in our ring.
        let sub = state.buses.subscribe(&id, 1024);

        let lease_hdr: [(&str, &str); 1] = [("X-Lease", &lease)];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({ "text": "hello bus" }),
        );
        assert_eq!(
            r.status,
            200,
            "prompt: {}",
            String::from_utf8_lossy(&r.body)
        );
        let expected_seq = r.json()["seq"].as_u64().expect("prompt returns seq");

        // Drain what the bus delivered (prompt echo is synchronous with the HTTP
        // response: the observer publishes before the route returns).
        let mut frames = Vec::new();
        while let Some(ev) = sub.try_recv() {
            frames.push(ev);
        }

        let echoed: Vec<_> = frames
            .iter()
            .filter(|e| matches!(e, Event::MessageAppended { .. }))
            .map(|e| e.seq())
            .collect();
        assert_eq!(
            echoed,
            vec![Some(expected_seq)],
            "MessageAppended echo must appear exactly once with the store seq, got {} frames: {:?}",
            frames.len(),
            frames.iter().map(|e| e.seq()).collect::<Vec<_>>()
        );
        let Event::MessageAppended { seq, msg } = &frames
            .iter()
            .find(|e| matches!(e, Event::MessageAppended { .. }))
            .expect("echo present")
        else {
            unreachable!()
        };
        assert!(
            *seq == expected_seq,
            "echo must carry the store seq, got {seq}"
        );
        let has_text = msg
            .content
            .iter()
            .any(|c| matches!(c, Content::Text { text } if text.contains("hello bus")));
        assert!(has_text, "echo must carry the appended message");

        h.handle.shutdown();
    }

    // ── srv::sse_owns_lease — attached reader keeps its lease past idle ────────────

    /// Regression: a client that holds the write lease but only *reads* (sits on the
    /// event stream without writing) used to idle-lose its lease after the timeout,
    /// so its next `prompt` 409'd even though it was still connected — "the server
    /// stops responding after idle". The owning SSE stream (`?lease=<holder>`) now
    /// keeps the lease warm on every heartbeat (DESIGN §12.6).
    #[test]
    fn sse_owns_lease_survives_idle() {
        // Short lease idle + fast heartbeat so the test runs in well under a second.
        std::env::set_var("KN9T_SSE_HEARTBEAT_MS", "40");
        let (store, tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let spec = model_spec();
        let mut state = ServerState::new(store, token, Default::default(), Vec::new())
            .with_default_model(spec.clone())
            .with_lease_idle(Duration::from_millis(150));
        state.model_registry = vec![spec];
        let state = Arc::new(state);
        let h = start(state);
        let _tmp = tmp;

        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);

        // Open the owning SSE stream on a background thread and keep it connected.
        // Passing ?lease=<holder> makes this stream the lease owner.
        let sse_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sse_thread = {
            let port = h.port;
            let token = h.token.clone();
            let id = id.clone();
            let lease = lease.clone();
            let stop = sse_stop.clone();
            std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                let req = format!(
                    "GET /session/{id}/events?from=0&lease={lease} HTTP/1.1\r\n\
                 Host: 127.0.0.1\r\n\
                 Authorization: Bearer {token}\r\n\
                 Connection: keep-alive\r\n\r\n"
                );
                stream.write_all(req.as_bytes()).unwrap();
                stream.flush().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .unwrap();
                // Read (and discard) frames/heartbeats until told to stop; this keeps
                // the connection alive so the server touches the lease each heartbeat.
                let mut buf = [0u8; 512];
                while !stop.load(Ordering::SeqCst) {
                    match stream.read(&mut buf) {
                        Ok(0) => break, // server closed
                        Ok(_) => {}     // frame or heartbeat — keep going
                        Err(_) => {}    // read timeout — loop and re-check stop
                    }
                }
            })
        };

        // Idle far longer than the 150ms lease timeout while only the SSE stream is
        // active (no writes). Several heartbeats (40ms) fire in this window.
        std::thread::sleep(Duration::from_millis(500));

        // The prompt must still succeed: the owning stream kept the lease warm.
        let lease_hdr: [(&str, &str); 1] = [("X-Lease", &lease)];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({ "text": "still here after idle" }),
        );
        assert_eq!(
        r.status, 200,
        "prompt after idle must succeed because the SSE stream owns+refreshes the lease; got {}: {}",
        r.status, String::from_utf8_lossy(&r.body)
    );

        sse_stop.store(true, Ordering::SeqCst);
        h.handle.shutdown();
        let _ = sse_thread.join();
        std::env::remove_var("KN9T_SSE_HEARTBEAT_MS");
    }

    // ── srv::auth_required (R-SRV-020) ────────────────────────────────────────────

    #[test]
    fn auth_required() {
        let (h, _tmp) = harness();
        // No Authorization header → 401.
        let r = request(h.port, "GET", "/session", &[], b"");
        assert_eq!(r.status, 401);
        // Wrong token → 401.
        let r = request(
            h.port,
            "GET",
            "/session",
            &[("Authorization", "Bearer wrong")],
            b"",
        );
        assert_eq!(r.status, 401);
        // Correct token → 200.
        let auth = format!("Bearer {}", h.token);
        let r = request(h.port, "GET", "/session", &[("Authorization", &auth)], b"");
        assert_eq!(r.status, 200);

        // R-SRV-020: token file is written 0600 where supported.
        let tmp = tempfile::tempdir().unwrap();
        let tok_path = tmp.path().join("token");
        kn9t_server::auth::write_token(&tok_path, "abc123").unwrap();
        assert_eq!(std::fs::read_to_string(&tok_path).unwrap(), "abc123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&tok_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token file must be 0600");
        }

        h.handle.shutdown();
    }

    // ── srv::origin_rejected (R-SRV-030) ──────────────────────────────────────────

    #[test]
    fn origin_rejected() {
        let (h, _tmp) = harness();
        let auth = format!("Bearer {}", h.token);
        // Any request with an Origin header is rejected, even with a valid token.
        let r = request(
            h.port,
            "GET",
            "/session",
            &[
                ("Authorization", &auth),
                ("Origin", "https://evil.example.com"),
            ],
            b"",
        );
        assert_eq!(r.status, 403);
        // Without Origin → fine.
        let r = request(h.port, "GET", "/session", &[("Authorization", &auth)], b"");
        assert_eq!(r.status, 200);
        h.handle.shutdown();
    }

    // ── srv::sse_no_gap_no_dup (R-SRV-040, R-SRV-050) ─────────────────────────────

    #[test]
    fn sse_no_gap_no_dup() {
        let (state, _tmp) = fresh_state();

        // Create a session with a couple of durable events already committed.
        let id = SessionId::new();
        kn9t_store::create_session(&state.store, &id, ".", &model_spec().r#ref).unwrap();
        let e1 = mk_msg("first");
        let seq1 = state.store.append(&id, e1.clone()).unwrap(); // seq 1

        // The client attaches from seq 0. Step 1: subscribe FIRST.
        let sub = state
            .buses
            .subscribe(&id.0, kn9t_server::sse::SSE_RING_CAPACITY);

        // Simulate an event committed DURING the attach window (after subscribe, before
        // the durable backlog read): it must be delivered exactly once.
        let e2 = mk_msg("during-window");
        let seq2 = state.store.append(&id, e2.clone()).unwrap(); // seq 2
                                                                 // The server echoes durable appends onto the bus (as the routes/loop do).
        state.buses.publish(&id.0, with_seq(&e2, seq2));

        // Also push a transient delta on the bus during the window (must forward once).
        state.buses.publish(
            &id.0,
            Event::TextDelta {
                msg_id: MsgId::new(),
                idx: 0,
                delta: "UNIQUEDELTA".into(),
            },
        );

        // Steps 2–4: build the attach prelude (durable replay + dedup flush).
        let prelude = kn9t_server::sse::build_attach_prelude(&state.store, &id.0, 0, &sub);

        // Count occurrences of each message's marker text across all emitted frames.
        let all: String = prelude.frames.join("");
        assert_eq!(count(&all, "first"), 1, "seq1 must appear exactly once");
        assert_eq!(
            count(&all, "during-window"),
            1,
            "gap event delivered exactly once"
        );
        assert_eq!(
            count(&all, "UNIQUEDELTA"),
            1,
            "transient forwarded exactly once"
        );
        assert_eq!(prelude.head_seq, seq2, "watermark = current head_seq");
        let _ = seq1;

        // R-SRV-050: live_messages partial is surfaced on attach.
        state
            .store
            .upsert_live_message(
                &id,
                &MsgId::new(),
                "assistant",
                &[Content::Text {
                    text: "partial-live".into(),
                }],
            )
            .unwrap();
        let sub2 = state.buses.subscribe(&id.0, 16);
        let prelude2 = kn9t_server::sse::build_attach_prelude(&state.store, &id.0, 0, &sub2);
        let all2: String = prelude2.frames.join("");
        assert!(all2.contains("live_partial"), "live_partial frame present");
        assert!(all2.contains("partial-live"), "partial text surfaced");
    }

    fn mk_msg(marker: &str) -> Event {
        Event::MessageAppended {
            seq: 0,
            msg: kn9t_core::Message {
                id: MsgId::new(),
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: marker.into(),
                }],
                silent: false,
            },
        }
    }

    /// Rewrite a MessageAppended's seq (mirrors what the store returns to the bus echo).
    fn with_seq(e: &Event, seq: u64) -> Event {
        match e {
            Event::MessageAppended { msg, .. } => Event::MessageAppended {
                seq,
                msg: msg.clone(),
            },
            other => other.clone(),
        }
    }

    fn count(hay: &str, needle: &str) -> usize {
        hay.matches(needle).count()
    }

    // ── srv::lease_single_writer (R-SRV-060) ──────────────────────────────────────

    #[test]
    fn lease_single_writer() {
        let (h, _tmp) = harness();
        let id = make_session(&h);

        // First acquire succeeds.
        let holder_a = acquire_lease(&h, &id);

        // Second acquire (no takeover) → 409.
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/lease"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(
            r.status, 409,
            "second acquire without takeover must be busy"
        );

        // A writes fine.
        let a_hdr: [(&str, &str); 1] = [("X-Lease", &holder_a)];
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/steer"),
                &a_hdr,
                serde_json::json!({ "text": "a" })
            )
            .status,
            200
        );

        // Takeover steals the lease.
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/lease?takeover=1"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 200);
        let holder_b = r.json()["lease"].as_str().unwrap().to_owned();
        assert_ne!(holder_a, holder_b);

        // A's writes now 409; B's succeed.
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/steer"),
                &a_hdr,
                serde_json::json!({ "text": "a2" })
            )
            .status,
            409,
            "prior holder must 409 after takeover"
        );
        let b_hdr: [(&str, &str); 1] = [("X-Lease", &holder_b)];
        assert_eq!(
            req_auth(
                &h,
                "POST",
                &format!("/session/{id}/steer"),
                &b_hdr,
                serde_json::json!({ "text": "b" })
            )
            .status,
            200
        );

        h.handle.shutdown();
    }

    // ── srv::spawn_race (R-SRV-070) ───────────────────────────────────────────────

    #[test]
    fn spawn_race() {
        let tmp = tempfile::tempdir().unwrap();
        let port_path = tmp.path().join("port");
        let lock_path = tmp.path().join("spawn.lock");

        // Two clients start together; a shared spawn counter proves exactly one server
        // is spawned. The spawn_fn stands in for a detached `kn9t serve`: it starts an
        // in-process server (a real listener) and writes the port file.
        let spawn_count = Arc::new(AtomicUsize::new(0));
        // Keep the spawned server(s) alive for the duration.
        let servers: Arc<std::sync::Mutex<Vec<ServerHandle>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let mk_spawn = |count: Arc<AtomicUsize>,
                        servers: Arc<std::sync::Mutex<Vec<ServerHandle>>>,
                        pp: std::path::PathBuf| {
            move || -> std::io::Result<()> {
                count.fetch_add(1, Ordering::SeqCst);
                let (store, tmp) = temp_store();
                std::mem::forget(tmp); // keep db alive for the test
                let token = kn9t_server::auth::generate_token();
                let state = Arc::new(ServerState::new(
                    store,
                    token,
                    Default::default(),
                    Vec::new(),
                ));
                let handle = ServerHandle::spawn(state)?;
                kn9t_server::spawn::write_port(&pp, handle.port)?;
                servers.lock().unwrap().push(handle);
                Ok(())
            }
        };

        let c1 = spawn_count.clone();
        let s1 = servers.clone();
        let pp1 = port_path.clone();
        let lp1 = lock_path.clone();
        let c2 = spawn_count.clone();
        let s2 = servers.clone();
        let pp2 = port_path.clone();
        let lp2 = lock_path.clone();

        let t1 = std::thread::spawn(move || {
            kn9t_server::spawn::ensure_server(
                &pp1,
                &lp1,
                mk_spawn(c1, s1, pp1.clone()),
                Duration::from_secs(5),
            )
        });
        let t2 = std::thread::spawn(move || {
            kn9t_server::spawn::ensure_server(
                &pp2,
                &lp2,
                mk_spawn(c2, s2, pp2.clone()),
                Duration::from_secs(5),
            )
        });

        let r1 = t1.join().unwrap().unwrap();
        let r2 = t2.join().unwrap().unwrap();

        // Both clients connected to the SAME port; exactly one server was spawned.
        assert_eq!(r1.port, r2.port, "both clients see the same server");
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "exactly one server spawned"
        );

        // Stale port file → respawn. Point the port file at a closed socket.
        let dead_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port() // listener drops → port closed
        };
        kn9t_server::spawn::write_port(&port_path, dead_port).unwrap();
        let before = spawn_count.load(Ordering::SeqCst);
        let c3 = spawn_count.clone();
        let s3 = servers.clone();
        let pp3 = port_path.clone();
        let r3 = kn9t_server::spawn::ensure_server(
            &port_path,
            &lock_path,
            mk_spawn(c3, s3, pp3),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_ne!(
            r3.port, dead_port,
            "stale port triggers respawn to a live port"
        );
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            before + 1,
            "stale port respawns exactly once"
        );

        for h in servers.lock().unwrap().drain(..) {
            h.shutdown();
        }
    }

    // ── srv::idle_exit (R-SRV-080) ────────────────────────────────────────────────

    #[test]
    fn idle_exit() {
        // A server with a very short idle window and no clients/turns exits.
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let state = Arc::new(
            ServerState::new(store, token, Default::default(), Vec::new())
                .with_idle_exit(Duration::from_millis(300)),
        );
        let handle = ServerHandle::spawn(state).unwrap();
        // No attached client, no running turn: should exit after the idle window.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !handle.is_shutdown() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(handle.is_shutdown(), "idle server must self-exit");

        // A server WITH an attached client does not exit within the same window.
        let (store2, _tmp2) = temp_store();
        let token2 = kn9t_server::auth::generate_token();
        let state2 = Arc::new(
            ServerState::new(store2, token2, Default::default(), Vec::new())
                .with_idle_exit(Duration::from_millis(300)),
        );
        let handle2 = ServerHandle::spawn(state2).unwrap();
        let id = {
            let auth = format!("Bearer {}", token_of(&handle2));
            let r = request(
                handle2.port,
                "POST",
                "/session",
                &[("Authorization", &auth)],
                br#"{"cwd":"."}"#,
            );
            r.status // touch to keep server used
        };
        let _ = id;
        // Attach an SSE client on a background thread (holds an idle-keeping ref).
        handle2.state.idle.client_attached();
        std::thread::sleep(Duration::from_millis(800));
        assert!(
            !handle2.is_shutdown(),
            "server with an attached client must not exit"
        );
        handle2.state.idle.client_detached();
        handle2.shutdown();
    }

    fn token_of(h: &ServerHandle) -> String {
        h.state.token.clone()
    }

    // ── srv::stop_route (POST /stop) ──────────────────────────────────────────────

    #[test]
    fn stop_route() {
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let state = Arc::new(ServerState::new(
            store,
            token,
            Default::default(),
            Vec::new(),
        ));
        let h = ServerHandle::spawn(state).unwrap();
        let auth = format!("Bearer {}", h.state.token);

        // POST /stop returns 200 and sets stop_requested.
        let resp = request(h.port, "POST", "/stop", &[("Authorization", &auth)], &[]);
        assert_eq!(resp.status, 200);

        // Server shuts down promptly.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !h.is_shutdown() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(h.is_shutdown(), "server must shut down after POST /stop");
    }

    // ── srv::keepalive_detects_dropped_client ─────────────────────────────────────
    // Verifies that the SSE keepalive ping causes `client_detached` to be called
    // when a client closes its SSE connection, allowing the idle-exit watchdog
    // to fire.

    #[test]
    fn keepalive_detects_dropped_client() {
        use std::io::BufRead;

        // Short heartbeat so the test doesn't wait 15s for disconnect detection.
        std::env::set_var("KN9T_SSE_HEARTBEAT_MS", "200");

        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let spec = model_spec();
        // Very short idle window: exits quickly after last client detaches.
        let mut state = ServerState::new(store, token, Default::default(), Vec::new())
            .with_default_model(spec.clone())
            .with_idle_exit(Duration::from_millis(300));
        state.model_registry = vec![spec];
        let h = ServerHandle::spawn(Arc::new(state)).unwrap();
        let auth = format!("Bearer {}", h.state.token);

        // Create a session so the server has state; /attach itself is session-agnostic.
        let _sess = {
            let body = br#"{"cwd":"."}"#;
            let r = request(
                h.port,
                "POST",
                "/session",
                &[
                    ("Authorization", &auth),
                    ("Content-Type", "application/json"),
                ],
                body,
            );
            let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            v["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no id in: {}", String::from_utf8_lossy(&r.body)))
                .to_string()
        };

        // Open a real SSE TCP connection to the GLOBAL attach endpoint — this is the one
        // that calls client_attached() (R-SRV-081). Session SSE (/session/{id}/events)
        // deliberately does NOT affect attached_clients, so it cannot be used here.
        let sse_req = format!(
            "GET /attach HTTP/1.0\r\n\
         Host: 127.0.0.1:{}\r\n\
         Authorization: {auth}\r\n\
         Accept: text/event-stream\r\n\r\n",
            h.port
        );
        let mut sse_stream = TcpStream::connect(format!("127.0.0.1:{}", h.port)).unwrap();
        sse_stream.write_all(sse_req.as_bytes()).unwrap();
        sse_stream.flush().unwrap();

        // Read the HTTP response head to confirm the connection is live.
        let mut reader = std::io::BufReader::new(sse_stream.try_clone().unwrap());
        let mut head = String::new();
        reader.read_line(&mut head).unwrap();
        assert!(head.contains("200"), "SSE must return 200, got: {head}");

        // Server now has 1 attached client.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            h.state.idle.attached_count(),
            1,
            "server must count SSE client"
        );

        // Drop the SSE stream — OS closes the TCP socket.
        drop(sse_stream);
        drop(reader);

        // Server detects disconnect via keepalive write failure.
        // KN9T_SSE_HEARTBEAT_MS=200 is set so we don't wait 15s.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.state.idle.attached_count() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(
            h.state.idle.attached_count(),
            0,
            "client_detached must be called after socket close"
        );

        // And server must exit after the idle window.
        let deadline2 = std::time::Instant::now() + Duration::from_secs(2);
        while !h.is_shutdown() && std::time::Instant::now() < deadline2 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            h.is_shutdown(),
            "server must idle-exit after last client disconnects"
        );
    }

    // ── srv::blob_roundtrip (R-SRV-090) ───────────────────────────────────────────

    #[test]
    fn blob_roundtrip() {
        let (h, _tmp) = harness();
        let payload = b"the quick brown fox";

        // PUT returns a hash.
        let put = request(
            h.port,
            "POST",
            "/blob",
            &[
                ("Authorization", &format!("Bearer {}", h.token)),
                ("Content-Type", "text/plain"),
            ],
            payload,
        );
        assert_eq!(put.status, 200);
        let hash = put.json()["hash"].as_str().unwrap().to_owned();
        assert_eq!(put.json()["mime"].as_str().unwrap(), "text/plain");

        // GET returns identical bytes with the ETag + immutable cache.
        let get = req_auth(
            &h,
            "GET",
            &format!("/blob/{hash}"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(get.status, 200);
        assert_eq!(get.body, payload);
        assert_eq!(get.header("ETag").unwrap(), format!("\"{hash}\""));
        assert_eq!(get.header("Cache-Control").unwrap(), "immutable");

        // A second PUT of the same bytes reuses the row (same hash).
        let put2 = request(
            h.port,
            "POST",
            "/blob",
            &[
                ("Authorization", &format!("Bearer {}", h.token)),
                ("Content-Type", "text/plain"),
            ],
            payload,
        );
        assert_eq!(put2.json()["hash"].as_str().unwrap(), hash);

        // Verify only one row exists.
        let count: i64 = h
            .handle
            .state
            .store
            .query_one(
                "SELECT COUNT(*) FROM blobs WHERE hash=?1",
                &[&hash.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "second put reuses the row");

        // Unknown hash → 404.
        let miss = req_auth(&h, "GET", "/blob/deadbeef", &[], serde_json::Value::Null);
        assert_eq!(miss.status, 404);

        h.handle.shutdown();
    }

    // ── srv::autotitle (R-SRV-100) ────────────────────────────────────────────────

    /// A fake provider that returns a fixed title, and a switch to fail.
    struct FakeProvider {
        fail: bool,
        title: String,
    }

    impl Provider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn stream(
            &self,
            _req: &Request,
            _cancel: &Cancel,
        ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
            if self.fail {
                return Err(ProvErr::Connect("simulated failure".into()));
            }
            let chunks = vec![
                Ok(Chunk::Text {
                    idx: 0,
                    delta: self.title.clone(),
                }),
                Ok(Chunk::Usage(Usage {
                    tokens: Tokens {
                        input: 12,
                        output: 4,
                        ..Default::default()
                    },
                    model: ModelRef {
                        provider: "test".into(),
                        id: "m1".into(),
                    },
                })),
                Ok(Chunk::Stop(StopReason::Stop)),
            ];
            Ok(Box::new(chunks.into_iter()))
        }
    }

    #[test]
    fn autotitle() {
        // A nameless session with one assistant message gets a title + a title usage row.
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let provider: Arc<dyn Provider> = Arc::new(FakeProvider {
            fail: false,
            title: "Fix the parser".into(),
        });
        let state = Arc::new(
            ServerState::new(store, token, Default::default(), Vec::new())
                .with_default_model(model_spec())
                .with_provider(provider),
        );

        let id = SessionId::new();
        kn9t_store::create_session(&state.store, &id, ".", &model_spec().r#ref).unwrap();
        // A user message then an assistant message (the "first assistant turn").
        state
            .store
            .append(&id, mk_user("please fix the parser bug"))
            .unwrap();
        state.store.append(&id, mk_msg("done")).unwrap();

        kn9t_server::routes::session::maybe_autotitle(&state, &id);

        let name: Option<String> = state
            .store
            .query_one(
                "SELECT name FROM sessions WHERE id=?1",
                &[&id.0.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            name.as_deref(),
            Some("Fix the parser"),
            "title set after turn 1"
        );

        // A usage row with kind=title exists.
        let title_usage: i64 = state
            .store
            .query_one(
                "SELECT COUNT(*) FROM usage WHERE session_id=?1 AND kind='title'",
                &[&id.0.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title_usage, 1, "a title usage row is recorded");

        // A provider failure leaves name null with no error.
        let (store2, _tmp2) = temp_store();
        let token2 = kn9t_server::auth::generate_token();
        let failing: Arc<dyn Provider> = Arc::new(FakeProvider {
            fail: true,
            title: String::new(),
        });
        let state2 = Arc::new(
            ServerState::new(store2, token2, Default::default(), Vec::new())
                .with_default_model(model_spec())
                .with_provider(failing),
        );
        let id2 = SessionId::new();
        kn9t_store::create_session(&state2.store, &id2, ".", &model_spec().r#ref).unwrap();
        state2.store.append(&id2, mk_user("hello")).unwrap();
        state2.store.append(&id2, mk_msg("hi")).unwrap();
        kn9t_server::routes::session::maybe_autotitle(&state2, &id2); // must not panic
        let name2: Option<String> = state2
            .store
            .query_one(
                "SELECT name FROM sessions WHERE id=?1",
                &[&id2.0.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name2, None, "provider failure leaves name null");

        // A session created WITH a name is not re-titled.
        let (store3, _tmp3) = temp_store();
        let token3 = kn9t_server::auth::generate_token();
        let p3: Arc<dyn Provider> = Arc::new(FakeProvider {
            fail: false,
            title: "Should not apply".into(),
        });
        let state3 = Arc::new(
            ServerState::new(store3, token3, Default::default(), Vec::new())
                .with_default_model(model_spec())
                .with_provider(p3),
        );
        let id3 = SessionId::new();
        kn9t_store::create_session(&state3.store, &id3, ".", &model_spec().r#ref).unwrap();
        state3
            .store
            .execute_raw(
                "UPDATE sessions SET name='Given' WHERE id=?1",
                &[&id3.0.as_str()],
            )
            .unwrap();
        state3.store.append(&id3, mk_user("q")).unwrap();
        state3.store.append(&id3, mk_msg("a")).unwrap();
        kn9t_server::routes::session::maybe_autotitle(&state3, &id3);
        let name3: Option<String> = state3
            .store
            .query_one(
                "SELECT name FROM sessions WHERE id=?1",
                &[&id3.0.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            name3.as_deref(),
            Some("Given"),
            "a supplied name suppresses auto-titling"
        );
    }

    fn mk_user(text: &str) -> Event {
        Event::MessageAppended {
            seq: 0,
            msg: kn9t_core::Message {
                id: MsgId::new(),
                role: Role::User,
                content: vec![Content::Text { text: text.into() }],
                silent: false,
            },
        }
    }

    // ── srv::cost_query (R-SRV-110) ───────────────────────────────────────────────

    #[test]
    fn cost_query() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let sid = SessionId(id.clone());

        // Record two usage rows via the store (as the loop would). The store
        // recomputes cost from tokens × price_snapshot / 1e6 (R-STOR-070), ignoring
        // the event's own cost_usd; expected total = 0.0002 + 0.00036 = 0.00056.
        h.handle
            .state
            .store
            .append(&sid, usage_event("test", "m1", 100, 50, 0.0))
            .unwrap();
        h.handle
            .state
            .store
            .append(&sid, usage_event("test", "m1", 200, 80, 0.0))
            .unwrap();

        // group_by=model
        let r = req_auth(
            &h,
            "GET",
            "/cost?since=0&group_by=model",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 200);
        let j = r.json();
        let total = j["total_cost_usd"].as_f64().unwrap();
        assert!(
            (total - 0.00056).abs() < 1e-9,
            "total cost aggregated, got {total}"
        );
        let groups = j["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["group"].as_str().unwrap(), "m1");

        // group_by=kind
        let rk = req_auth(
            &h,
            "GET",
            "/cost?since=0&group_by=kind",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(rk.status, 200);
        assert_eq!(
            rk.json()["groups"].as_array().unwrap()[0]["group"]
                .as_str()
                .unwrap(),
            "main"
        );

        // Bad group_by → 400.
        let bad = req_auth(
            &h,
            "GET",
            "/cost?group_by=nonsense",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(bad.status, 400);

        h.handle.shutdown();
    }

    fn usage_event(provider: &str, model: &str, tin: u32, tout: u32, _cost_ignored: f64) -> Event {
        Event::UsageRecorded {
            seq: 0,
            provider: provider.into(),
            model: model.into(),
            kind: kn9t_core::UsageKind::Main,
            tokens: Tokens {
                input: tin,
                output: tout,
                ..Default::default()
            },
            price_snapshot: Price {
                input: 1000000,
                output: 2000000,
                cache_read: 0,
                cache_write: 0,
            },
            // The store recomputes cost from tokens × price (R-STOR-070); this field is
            // intentionally ignored by the projection.
            cost_micros: 0,
            cost_usd: 0.0,
            estimated: false,
        }
    }

    // ── srv::budget_reports_both (R-SRV-120) ──────────────────────────────────────

    #[test]
    fn budget_reports_both() {
        // With a provider-reported figure injected, /budget returns both.
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let state = Arc::new(
            ServerState::new(store, token, Default::default(), Vec::new())
                .with_default_model(model_spec())
                .with_provider_budget(3.50),
        );
        // Add a local usage row so local_estimate is nonzero. Cost is recomputed by
        // the store as (100·1 + 50·2)/1e6 = 0.0002 (R-STOR-070).
        let id = SessionId::new();
        kn9t_store::create_session(&state.store, &id, ".", &model_spec().r#ref).unwrap();
        state
            .store
            .append(&id, usage_event("test", "m1", 100, 50, 0.0))
            .unwrap();

        let h = start(state);
        let r = req_auth(&h, "GET", "/budget", &[], serde_json::Value::Null);
        assert_eq!(r.status, 200);
        let j = r.json();
        let local = j["local_estimate"].as_f64().unwrap();
        assert!(
            (local - 0.0002).abs() < 1e-9,
            "local estimate present, got {local}"
        );
        assert_eq!(
            j["provider_reported"].as_f64().unwrap(),
            3.50,
            "provider-reported present"
        );

        // Without a provider figure, only local_estimate is present.
        let (h2, _tmp2) = harness();
        let r2 = req_auth(&h2, "GET", "/budget", &[], serde_json::Value::Null);
        assert!(
            r2.json().get("provider_reported").is_none(),
            "no provider figure → omitted"
        );
        assert!(r2.json().get("local_estimate").is_some());

        h.handle.shutdown();
        h2.handle.shutdown();
    }

    // ── session response bodies ───────────────────────────────────────────────────

    #[test]
    fn create_session_body() {
        let (h, _tmp) = harness();
        let r = req_auth(
            &h,
            "POST",
            "/session",
            &[],
            serde_json::json!({
                "name": "my-session",
                "cwd": "."
            }),
        );
        assert_eq!(r.status, 200);
        let j = r.json();
        assert!(j["id"].as_str().is_some(), "id must be a string");
        assert_eq!(j["name"].as_str(), Some("my-session"));
        assert!(j["cwd"].as_str().is_some(), "cwd present");
        assert!(j["model"].is_object(), "model object present");
        assert!(j["model"]["provider"].as_str().is_some());
        assert!(j["model"]["id"].as_str().is_some());
        h.handle.shutdown();
    }

    #[test]
    fn list_sessions_body() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let r = req_auth(&h, "GET", "/session", &[], serde_json::Value::Null);
        assert_eq!(r.status, 200);
        let j = r.json();
        let sessions = j["sessions"].as_array().expect("sessions array");
        assert!(
            sessions.iter().any(|s| s["id"].as_str() == Some(&id)),
            "created session must appear in list"
        );
        // F5: created_at must be the schema-pinned ISO8601 string (millis normalized
        // at the boundary), not a raw INTEGER the client would misread.
        for s in sessions {
            if let Some(ca) = s["created_at"].as_str() {
                let yyyy = ca.split('-').next().unwrap_or("");
                assert!(
                    yyyy.len() == 4 && yyyy.starts_with('2'),
                    "created_at must be ISO8601 (YYYY-...), got {ca:?}"
                );
            }
        }
        h.handle.shutdown();
    }

    // ── snapshot_body ─────────────────────────────────────────────────────────────

    #[test]
    fn snapshot_body() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let r = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 200);
        let j = r.json();
        assert!(j["meta"]["id"].as_str().is_some(), "meta.id present");
        // F5: meta.created_at normalized to ISO8601 at the boundary.
        if let Some(ca) = j["meta"]["created_at"].as_str() {
            assert!(
                ca.contains('T') && ca.ends_with('Z'),
                "meta.created_at must be ISO8601 (YYYY-MM-DDTHH:MM:SSZ), got {ca:?}"
            );
        }
        assert!(j["head_seq"].is_number(), "head_seq is a number");
        assert!(j["ctx_tokens"].is_number(), "ctx_tokens present");
        assert!(j["cost_usd"].is_number(), "cost_usd present");
        assert!(j["model"].is_object(), "model object present");
        assert!(j["transcript"].is_array(), "transcript array present");
        // snapshot on unknown id → 404
        let r2 = req_auth(
            &h,
            "GET",
            "/session/nonexistent",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r2.status, 404);
        h.handle.shutdown();
    }

    // ── unknown fields → 400 (Phase 2: deny_unknown_fields contract) ──────────────

    #[test]
    fn unknown_field_is_400() {
        let (h, _tmp) = harness();

        // POST /session with an unknown field → 400, not a silent ignore (F6).
        let r = req_auth(
            &h,
            "POST",
            "/session",
            &[],
            serde_json::json!({
                "cwd": ".", "bogus_field": 1
            }),
        );
        assert_eq!(
            r.status,
            400,
            "unknown create field: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.json()["error"].as_str(), Some("bad_json"));

        // Wrong-typed field (object where string expected) → 400.
        let r0 = req_auth(
            &h,
            "POST",
            "/session",
            &[],
            serde_json::json!({
                "cwd": { "not": "a string" }
            }),
        );
        assert_eq!(r0.status, 400, "mistyped create field must 400");

        // Lease-required body routes: prompt / approve / fork with unknown fields → 400.
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let lease_hdr: [(&str, &str); 1] = [("X-Lease", lh)];
        let r2 = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({ "text": "hi", "unexpected": true }),
        );
        assert_eq!(r2.status, 400, "unknown prompt field must 400");

        let hdrs: [(&str, &str); 2] = [("X-Lease", lh), ("X-Lease-Session", &id)];
        let r3 = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({ "id": 1, "decision": "allow", "nope": 1 }),
        );
        assert_eq!(r3.status, 400, "unknown approve field must 400");

        // Mistyped approve field (string where u64 expected) → 400.
        let r4 = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({ "id": "not-a-number", "decision": "allow" }),
        );
        assert_eq!(r4.status, 400, "mistyped approve field must 400");

        // Enum-invalid decision → 400 (validated, not default-deny).
        let r5 = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({ "id": 1, "decision": "maybe" }),
        );
        assert_eq!(r5.status, 400, "unknown decision enum must 400");

        // Fork with an invalid reason → 400.
        let r6 = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/fork"),
            &[],
            serde_json::json!({ "origin_seq": 0, "reason": "sideways" }),
        );
        assert_eq!(r6.status, 400, "unknown fork reason must 400");

        // Valid bodies still pass (regression guard for the stricter parsing).
        let r7 = req_auth(
            &h,
            "POST",
            "/session",
            &[],
            serde_json::json!({ "cwd": "." }),
        );
        assert_eq!(r7.status, 200, "valid empty-ish create body still 200");

        h.handle.shutdown();
    }

    // ── fork semantics ────────────────────────────────────────────────────────────

    #[test]
    fn fork_creates_new_session() {
        let (h, _tmp) = harness();
        let origin = make_session(&h);

        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{origin}/fork"),
            &[],
            serde_json::json!({ "origin_seq": 0, "reason": "rewind" }),
        );
        assert_eq!(r.status, 200, "fork: {}", String::from_utf8_lossy(&r.body));
        let j = r.json();
        let fork_id = j["id"].as_str().expect("fork returns id");
        assert_ne!(fork_id, origin, "fork must have a different id");

        // Fork appears in the session list.
        let list = req_auth(&h, "GET", "/session", &[], serde_json::Value::Null);
        let sessions = list.json()["sessions"].as_array().unwrap().clone();
        assert!(
            sessions.iter().any(|s| s["id"].as_str() == Some(fork_id)),
            "forked session must appear in list"
        );

        // Forked session can acquire a lease independently.
        let fork_lease = acquire_lease(&h, fork_id);
        assert!(!fork_lease.is_empty());

        h.handle.shutdown();
    }

    #[test]
    fn fork_unknown_session_404() {
        let (h, _tmp) = harness();
        let r = req_auth(
            &h,
            "POST",
            "/session/nosuchid/fork",
            &[],
            serde_json::json!({ "origin_seq": 0, "reason": "rewind" }),
        );
        assert_eq!(r.status, 404);
        h.handle.shutdown();
    }

    // ── delete session ────────────────────────────────────────────────────────────

    #[test]
    fn delete_session() {
        let (h, _tmp) = harness();
        let id = make_session(&h);

        // Must exist first.
        assert_eq!(
            req_auth(
                &h,
                "GET",
                &format!("/session/{id}"),
                &[],
                serde_json::Value::Null
            )
            .status,
            200
        );

        // Delete it.
        let r = req_auth(
            &h,
            "DELETE",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(
            r.status,
            200,
            "delete: {}",
            String::from_utf8_lossy(&r.body)
        );

        // Now gone.
        assert_eq!(
            req_auth(
                &h,
                "GET",
                &format!("/session/{id}"),
                &[],
                serde_json::Value::Null
            )
            .status,
            404
        );

        h.handle.shutdown();
    }

    #[test]
    fn delete_unknown_session_404() {
        let (h, _tmp) = harness();
        let r = req_auth(
            &h,
            "DELETE",
            "/session/nosuchid",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 404);
        h.handle.shutdown();
    }

    // ── prompt appends a message ──────────────────────────────────────────────────

    #[test]
    fn prompt_appends_user_message() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let lease_hdr = [("X-Lease", lh)];

        // Snapshot before.
        let before = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        let before_seq = before["head_seq"].as_u64().unwrap();

        // Send prompt.
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({ "text": "hello world" }),
        );
        assert_eq!(
            r.status,
            200,
            "prompt: {}",
            String::from_utf8_lossy(&r.body)
        );
        let j = r.json();
        assert_eq!(j["accepted"].as_bool(), Some(true), "accepted field");
        assert!(j["seq"].as_u64().is_some(), "seq field present");

        // Snapshot after — head_seq advanced and transcript has the user message.
        let after = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        let after_seq = after["head_seq"].as_u64().unwrap();
        assert!(after_seq > before_seq, "head_seq must advance after prompt");

        let transcript = after["transcript"].as_array().unwrap();
        let has_user_msg = transcript.iter().any(|m| {
            m["role"].as_str() == Some("user")
                && m["content"]
                    .as_array()
                    .map(|c| c.iter().any(|b| b["text"].as_str() == Some("hello world")))
                    .unwrap_or(false)
        });
        assert!(
            has_user_msg,
            "user message with 'hello world' must be in transcript"
        );

        h.handle.shutdown();
    }

    // ── set_model ─────────────────────────────────────────────────────────────────

    #[test]
    fn set_model_updates_session() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let lease_hdr = [("X-Lease", lh)];

        // Switch to the second model in the registry (haiku → sonnet or vice-versa).
        let models_r = req_auth(&h, "GET", "/models", &[], serde_json::Value::Null);
        let models = models_r.json()["models"].as_array().unwrap().clone();
        // Pick any model different from the current default (or same — just verify 200).
        let target = &models[0];
        let provider = target["provider"].as_str().unwrap();
        let mid = target["id"].as_str().unwrap();

        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/model"),
            &lease_hdr,
            serde_json::json!({ "provider": provider, "id": mid }),
        );
        assert_eq!(
            r.status,
            200,
            "set_model: {}",
            String::from_utf8_lossy(&r.body)
        );

        // Snapshot shows the new model.
        let snap = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        assert_eq!(snap["model"]["id"].as_str(), Some(mid));

        h.handle.shutdown();
    }

    // ── models response body ──────────────────────────────────────────────────────

    #[test]
    fn models_body() {
        let (h, _tmp) = harness();
        let r = req_auth(&h, "GET", "/models", &[], serde_json::Value::Null);
        assert_eq!(r.status, 200);
        let j = r.json();
        let models = j["models"].as_array().expect("models array");
        assert!(!models.is_empty(), "at least one model configured");
        for m in models {
            assert!(m["id"].as_str().is_some(), "each model has id");
            assert!(m["provider"].as_str().is_some(), "each model has provider");
            assert!(
                m["ctx_window"].as_u64().is_some(),
                "each model has ctx_window"
            );
        }
        // auth block present
        assert!(j["auth"].is_object(), "auth block present");
        h.handle.shutdown();
    }

    // ── lease response body ───────────────────────────────────────────────────────

    #[test]
    fn lease_body() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/lease"),
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 200);
        let j = r.json();
        let holder = j["lease"].as_str().expect("lease field is a string");
        assert!(!holder.is_empty(), "holder is non-empty");
        assert_eq!(
            j["session"].as_str(),
            Some(id.as_str()),
            "session echoed back"
        );
        h.handle.shutdown();
    }

    // ── steer ─────────────────────────────────────────────────────────────────────

    #[test]
    fn steer_appends_system_message() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let lease_hdr = [("X-Lease", lh)];

        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/steer"),
            &lease_hdr,
            serde_json::json!({ "text": "be concise" }),
        );
        assert_eq!(r.status, 200, "steer: {}", String::from_utf8_lossy(&r.body));

        // Steering message appears in transcript.
        let snap = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        let transcript = snap["transcript"].as_array().unwrap();
        let has_steer = transcript.iter().any(|m| {
            m["content"]
                .as_array()
                .map(|c| c.iter().any(|b| b["text"].as_str() == Some("be concise")))
                .unwrap_or(false)
        });
        assert!(has_steer, "steering text must appear in transcript");
        h.handle.shutdown();
    }

    // ── abort ─────────────────────────────────────────────────────────────────────

    #[test]
    fn abort_returns_200() {
        // Abort on a session with no running turn is a no-op but must return 200.
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let lease_hdr = [("X-Lease", lh)];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/abort"),
            &lease_hdr,
            serde_json::Value::Null,
        );
        assert_eq!(r.status, 200);
        h.handle.shutdown();
    }

    // ── approve (no-pending is a no-op 200) ──────────────────────────────────────

    #[test]
    fn approve_no_pending_is_ok() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;
        let hdrs = [("X-Lease", lh), ("X-Lease-Session", id.as_str())];
        // No pending approval with id=9999 — server must return 200 (idempotent).
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({ "id": 9999, "decision": "allow" }),
        );
        assert_eq!(
            r.status,
            200,
            "approve no-pending: {}",
            String::from_utf8_lossy(&r.body)
        );
        h.handle.shutdown();
    }

    // ── approve: blocks on Ask, HardDeny never prompts ─────────────────────────

    /// Dummy bash tool that just returns ok.
    struct DummyBashTool {
        spec: kn9t_core::ToolSpec,
    }
    impl kn9t_core::Tool for DummyBashTool {
        fn spec(&self) -> &kn9t_core::ToolSpec {
            &self.spec
        }
        fn execute(
            &self,
            _args: &serde_json::Value,
            _ctx: &kn9t_core::ToolCtx,
            _cancel: &kn9t_core::Cancel,
        ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            Ok(kn9t_core::ToolOutput {
                content: vec![kn9t_core::Content::Text { text: "ok".into() }],
                details: None,
                is_error: false,
            })
        }
    }
    fn dummy_bash_registry() -> kn9t_core::ToolRegistry {
        let spec = kn9t_core::ToolSpec {
            name: "bash".into(),
            description: "run shell".into(),
            schema: serde_json::json!({"type":"object"}),
            hidden: false,
            effects: vec![kn9t_core::Effect {
                field: "cmd".into(),
                kind: kn9t_core::EffectKind::Shell,
            }],
            policy: kn9t_core::ToolPolicy {
                pattern_field: Some("cmd".into()),
                default_policy: kn9t_core::DefaultPolicy::Ask,
                builtin_allow: vec!["cat *".into(), "ls *".into()],
                builtin_deny: vec!["sudo *".into()],
            },
        };
        kn9t_core::ToolRegistry::from_tools(vec![
            Arc::new(DummyBashTool { spec }) as Arc<dyn kn9t_core::Tool>
        ])
    }

    /// ADR-0008 -- a hook host that answers `Ask` for one tool, standing in for a policy plugin.
    ///
    /// Since ADR-0008 the server has no opinion about risk: only a plugin can raise an `Ask`.
    /// These tests exercise the surviving *mechanism* (emit `ApprovalRequest`, block the turn,
    /// resolve via `POST /approve`, honour the scope), so they inject the verdict in-process
    /// rather than spawning a Python subprocess.
    struct AskingHooks {
        tool: String,
    }
    impl kn9t_core::HookHost for AskingHooks {
        fn before_tool_call(
            &self,
            tool: &str,
            _args: &serde_json::Value,
            _cwd: &std::path::Path,
        ) -> kn9t_core::HookVeto {
            if tool == self.tool {
                kn9t_core::HookVeto::Ask {
                    reason: "test policy asks".into(),
                }
            } else {
                kn9t_core::HookVeto::Allow
            }
        }
        // Everything else is pass-through: this host exists only to raise the Ask.
        fn after_tool_call(
            &self,
            _tool: &str,
            _args: &serde_json::Value,
            result: Vec<kn9t_core::Content>,
        ) -> Vec<kn9t_core::Content> {
            result
        }
        fn before_request(
            &self,
            msgs: Vec<kn9t_core::Message>,
            _model: &kn9t_core::ModelRef,
            _system: Option<&str>,
        ) -> Vec<kn9t_core::Message> {
            msgs
        }
        fn should_stop_after_turn(
            &self,
            _stop: kn9t_core::StopReason,
            _usage: &kn9t_core::Usage,
            _turn: u32,
        ) -> bool {
            false
        }
        fn prepare_next_turn(
            &self,
            _stop: kn9t_core::StopReason,
            _usage: &kn9t_core::Usage,
        ) -> kn9t_core::NextTurnPatch {
            kn9t_core::NextTurnPatch::default()
        }
        fn get_steering(&self) -> Vec<kn9t_core::Message> {
            Vec::new()
        }
        fn get_followup(&self) -> Vec<kn9t_core::Message> {
            Vec::new()
        }
        fn get_api_key(&self, _provider: &str) -> Option<String> {
            None
        }
    }

    /// ADR-0008 -- a hook host that refuses one tool outright, standing in for a policy plugin
    /// that decided the call is never acceptable (the old `HardDeny`).
    struct DenyingHooks {
        tool: String,
    }
    impl kn9t_core::HookHost for DenyingHooks {
        fn before_tool_call(
            &self,
            tool: &str,
            _args: &serde_json::Value,
            _cwd: &std::path::Path,
        ) -> kn9t_core::HookVeto {
            if tool == self.tool {
                kn9t_core::HookVeto::Deny {
                    reason: "test policy refuses".into(),
                }
            } else {
                kn9t_core::HookVeto::Allow
            }
        }
        fn after_tool_call(
            &self,
            _tool: &str,
            _args: &serde_json::Value,
            result: Vec<kn9t_core::Content>,
        ) -> Vec<kn9t_core::Content> {
            result
        }
        fn before_request(
            &self,
            msgs: Vec<kn9t_core::Message>,
            _model: &kn9t_core::ModelRef,
            _system: Option<&str>,
        ) -> Vec<kn9t_core::Message> {
            msgs
        }
        fn should_stop_after_turn(
            &self,
            _stop: kn9t_core::StopReason,
            _usage: &kn9t_core::Usage,
            _turn: u32,
        ) -> bool {
            false
        }
        fn prepare_next_turn(
            &self,
            _stop: kn9t_core::StopReason,
            _usage: &kn9t_core::Usage,
        ) -> kn9t_core::NextTurnPatch {
            kn9t_core::NextTurnPatch::default()
        }
        fn get_steering(&self) -> Vec<kn9t_core::Message> {
            Vec::new()
        }
        fn get_followup(&self) -> Vec<kn9t_core::Message> {
            Vec::new()
        }
        fn get_api_key(&self, _provider: &str) -> Option<String> {
            None
        }
    }

    /// Provider that emits one bash tool call with the given cmd, then a final stop.
    struct OneToolProvider {
        cmd: String,
        calls: Arc<Mutex<u32>>,
    }
    impl kn9t_core::Provider for OneToolProvider {
        fn name(&self) -> &str {
            "one-tool"
        }
        fn stream(
            &self,
            _req: &kn9t_core::Request,
            _cancel: &kn9t_core::Cancel,
        ) -> Result<
            Box<dyn Iterator<Item = Result<kn9t_core::Chunk, kn9t_core::ProvErr>> + Send>,
            kn9t_core::ProvErr,
        > {
            let n = {
                let mut c = self.calls.lock().unwrap();
                let n = *c;
                *c += 1;
                n
            };
            if n == 0 {
                // First turn iteration: emit the tool call
                let args = serde_json::json!({"cmd": self.cmd}).to_string();
                let chunks = vec![
                    Ok(kn9t_core::Chunk::ToolCall {
                        idx: 0,
                        id: kn9t_core::CallId("call_1".into()),
                        name: "bash".into(),
                    }),
                    Ok(kn9t_core::Chunk::ToolArgs {
                        idx: 0,
                        delta: args,
                    }),
                    Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                        tokens: kn9t_core::Tokens {
                            input: 10,
                            output: 5,
                            ..Default::default()
                        },
                        model: ModelRef {
                            provider: "one-tool".into(),
                            id: "m1".into(),
                        },
                    })),
                    Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::ToolUse)),
                ];
                Ok(Box::new(chunks.into_iter()))
            } else {
                // Second iteration: final answer (no more tools)
                let chunks = vec![
                    Ok(kn9t_core::Chunk::Text {
                        idx: 0,
                        delta: "done".into(),
                    }),
                    Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                        tokens: kn9t_core::Tokens {
                            input: 10,
                            output: 5,
                            ..Default::default()
                        },
                        model: ModelRef {
                            provider: "one-tool".into(),
                            id: "m1".into(),
                        },
                    })),
                    Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::Stop)),
                ];
                Ok(Box::new(chunks.into_iter()))
            }
        }
    }

    #[test]
    fn approve_resolves_blocked_policy() {
        // ADR-0008: a plugin `Ask` must emit ApprovalRequest and block until POST /approve.
        // This is the mechanism the ADR keeps in the server: bus + condvar + /approve.
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let calls = Arc::new(Mutex::new(0u32));
        let provider: Arc<dyn kn9t_core::Provider> = Arc::new(OneToolProvider {
            cmd: "rm -rf /tmp/test".into(),
            calls: calls.clone(),
        });
        let tools = dummy_bash_registry();
        // ADR-0008: the Ask comes from the policy plugin, so inject one in-process.
        let mut state = kn9t_server::state::ServerState::new(store, token, tools, Vec::new())
            .with_hooks_override(Arc::new(AskingHooks {
                tool: "bash".into(),
            }))
            .with_default_model(model_spec())
            .with_provider(provider);
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);
        let h = start(state.clone());
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);

        // Subscribe BEFORE the prompt so we don't miss the transient ApprovalRequest.
        let sub = state.buses.subscribe(&id, 64);

        // Prompt that triggers a tool call the injected policy hook asks about.
        let lease_hdr = [("X-Lease", lease.as_str())];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({"text":"do rm"}),
        );
        assert_eq!(
            r.status,
            200,
            "prompt: {}",
            String::from_utf8_lossy(&r.body)
        );

        // Wait for ApprovalRequest on the bus (transient, not stored).
        // The turn emits TurnStarted, MessageAppended etc before the tool batch,
        // so loop until we see ApprovalRequest.
        let approval_id = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut found = None;
            while std::time::Instant::now() < deadline {
                if let Some(ev) = sub.recv_timeout(Duration::from_millis(200)) {
                    match ev {
                        Event::ApprovalRequest { id, tool, .. } if tool == "bash" => {
                            found = Some(id);
                            break;
                        }
                        _ => continue, // ignore other transients
                    }
                }
            }
            found.expect("ApprovalRequest must be emitted for Ask")
        };

        // Turn should still be running (blocked on approval).
        assert!(
            kn9t_server::turn::is_turn_running(&state, &id),
            "turn must be blocked on Ask"
        );

        // Resolve via POST /approve (allow).
        let hdrs = [
            ("X-Lease", lease.as_str()),
            ("X-Lease-Session", id.as_str()),
        ];
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({"id": approval_id.0, "decision":"allow"}),
        );
        assert_eq!(
            r.status,
            200,
            "approve: {}",
            String::from_utf8_lossy(&r.body)
        );

        // Wait for turn to finish.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while kn9t_server::turn::is_turn_running(&state, &id)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !kn9t_server::turn::is_turn_running(&state, &id),
            "turn must have unblocked after approve"
        );

        // Transcript must contain the tool result (dummy bash returned ok) and final assistant text.
        let snap = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        let transcript = snap["transcript"].as_array().unwrap();
        let has_tool_result = transcript.iter().any(|m| {
            m["content"]
                .as_array()
                .map(|c| c.iter().any(|b| b["type"].as_str() == Some("tool_result")))
                .unwrap_or(false)
        });
        assert!(
            has_tool_result,
            "tool_result must appear after approval, transcript={}",
            serde_json::to_string_pretty(&snap).unwrap()
        );

        h.handle.shutdown();
    }

    #[test]
    fn approve_hard_deny_no_prompt() {
        // ADR-0008: a plugin `Deny` must NOT prompt and must not block -- there is nothing to
        // ask, the call is simply refused.
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let calls = Arc::new(Mutex::new(0u32));
        let provider: Arc<dyn kn9t_core::Provider> = Arc::new(OneToolProvider {
            cmd: "sudo rm -rf /".into(),
            calls: calls.clone(),
        });
        let tools = dummy_bash_registry();
        // ADR-0008: an outright refusal now comes from the plugin, not a server-side classifier.
        let mut state = kn9t_server::state::ServerState::new(store, token, tools, Vec::new())
            .with_hooks_override(Arc::new(DenyingHooks {
                tool: "bash".into(),
            }))
            .with_default_model(model_spec())
            .with_provider(provider);
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);
        let h = start(state.clone());
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let sub = state.buses.subscribe(&id, 64);

        let lease_hdr = [("X-Lease", lease.as_str())];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &lease_hdr,
            serde_json::json!({"text":"try sudo"}),
        );
        assert_eq!(r.status, 200);

        // Wait a bit for turn to finish — it must NOT have emitted ApprovalRequest.
        std::thread::sleep(Duration::from_millis(500));
        // Drain any events; there must be no ApprovalRequest.
        let mut saw_approval = false;
        while let Some(ev) = sub.try_recv() {
            if matches!(ev, Event::ApprovalRequest { .. }) {
                saw_approval = true;
            }
        }
        assert!(!saw_approval, "plugin Deny must not emit ApprovalRequest");

        // Turn should have finished already (not blocked).
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while kn9t_server::turn::is_turn_running(&state, &id)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !kn9t_server::turn::is_turn_running(&state, &id),
            "plugin Deny must not block"
        );

        // The denied tool should be a is_error tool_result.
        let snap = req_auth(
            &h,
            "GET",
            &format!("/session/{id}"),
            &[],
            serde_json::Value::Null,
        )
        .json();
        let transcript = snap["transcript"].as_array().unwrap();
        let has_error_result = transcript.iter().any(|m| {
            m["content"]
                .as_array()
                .map(|c| c.iter().any(|b| b["is_error"].as_bool() == Some(true)))
                .unwrap_or(false)
        });
        assert!(
            has_error_result,
            "plugin Deny must produce is_error tool_result"
        );

        h.handle.shutdown();
    }

    // ── policy: config [policy] demonstrably changes behaviour ─────────────────

    // ── approve scope handling (Phase 1.5) ─────────────────────────────────────

    #[test]
    fn approve_unknown_decision_is_400() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let hdrs = [
            ("X-Lease", lease.as_str()),
            ("X-Lease-Session", id.as_str()),
        ];
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({"id": 123, "decision": "bogus"}),
        );
        assert_eq!(
            r.status,
            400,
            "unknown decision must be 400, got {} body {}",
            r.status,
            String::from_utf8_lossy(&r.body)
        );
        assert!(String::from_utf8_lossy(&r.body).contains("unknown decision"));
        h.handle.shutdown();
    }

    #[test]
    fn approve_unknown_scope_is_400() {
        let (h, _tmp) = harness();
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let hdrs = [
            ("X-Lease", lease.as_str()),
            ("X-Lease-Session", id.as_str()),
        ];
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({"id": 123, "decision": "allow", "scope": "bogus"}),
        );
        assert_eq!(r.status, 400, "unknown scope must be 400");
        h.handle.shutdown();
    }

    #[test]
    fn approve_always_writes_config() {
        // Verify scope=always persists to config.toml under [policy.approvals]
        use kn9t_server::policy::{ApprovalCache, ApprovalRegistry, InteractiveApprover};
        let (store, _store_tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let config_tmp = tempfile::tempdir().unwrap();
        let config_path = config_tmp.path().join("config.toml");
        // Ensure file exists empty
        std::fs::write(&config_path, "").unwrap();
        let cache = Arc::new(ApprovalCache::new(config_path.clone()));
        let registry = Arc::new(ApprovalRegistry::new());
        let approver: Arc<dyn kn9t_core::Approver> = Arc::new(InteractiveApprover::with_cache(
            registry.clone(),
            cache.clone(),
        ));
        let tools = dummy_bash_registry();
        let mut state = ServerState::new(store, token, tools, Vec::new());
        state.approval_registry = registry.clone();
        state.approval_cache = cache.clone();
        let calls = Arc::new(Mutex::new(0u32));
        // Use a Repeating provider that emits same rm command each turn (even calls)
        struct RepeatingProvider {
            cmd: String,
            calls: Arc<Mutex<u32>>,
        }
        impl kn9t_core::Provider for RepeatingProvider {
            fn name(&self) -> &str {
                "repeat"
            }
            fn stream(
                &self,
                req: &kn9t_core::Request,
                _cancel: &kn9t_core::Cancel,
            ) -> Result<
                Box<dyn Iterator<Item = Result<kn9t_core::Chunk, kn9t_core::ProvErr>> + Send>,
                kn9t_core::ProvErr,
            > {
                // Title request (autotitle) has max_tokens 16 — return plain text, not a tool call
                if req.max_tokens == Some(16) {
                    return Ok(Box::new(
                        vec![
                            Ok(kn9t_core::Chunk::Text {
                                idx: 0,
                                delta: "title".into(),
                            }),
                            Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                                tokens: kn9t_core::Tokens::default(),
                                model: kn9t_core::ModelRef {
                                    provider: "repeat".into(),
                                    id: "m1".into(),
                                },
                            })),
                            Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::Stop)),
                        ]
                        .into_iter(),
                    ));
                }
                let n = {
                    let mut c = self.calls.lock().unwrap();
                    let n = *c;
                    *c += 1;
                    n
                };
                if n % 2 == 0 {
                    let args = serde_json::json!({"cmd": self.cmd}).to_string();
                    let chunks = vec![
                        Ok(kn9t_core::Chunk::ToolCall {
                            idx: 0,
                            id: kn9t_core::CallId("call_1".into()),
                            name: "bash".into(),
                        }),
                        Ok(kn9t_core::Chunk::ToolArgs {
                            idx: 0,
                            delta: args,
                        }),
                        Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                            tokens: kn9t_core::Tokens {
                                input: 10,
                                output: 5,
                                ..Default::default()
                            },
                            model: kn9t_core::ModelRef {
                                provider: "repeat".into(),
                                id: "m1".into(),
                            },
                        })),
                        Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::ToolUse)),
                    ];
                    Ok(Box::new(chunks.into_iter()))
                } else {
                    let chunks = vec![
                        Ok(kn9t_core::Chunk::Text {
                            idx: 0,
                            delta: "done".into(),
                        }),
                        Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                            tokens: kn9t_core::Tokens {
                                input: 10,
                                output: 5,
                                ..Default::default()
                            },
                            model: kn9t_core::ModelRef {
                                provider: "repeat".into(),
                                id: "m1".into(),
                            },
                        })),
                        Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::Stop)),
                    ];
                    Ok(Box::new(chunks.into_iter()))
                }
            }
        }
        let provider: Arc<dyn kn9t_core::Provider> = Arc::new(RepeatingProvider {
            cmd: "rm -rf /tmp/always_test".into(),
            calls: calls.clone(),
        });
        state = state
            .with_approver(approver)
            .with_hooks_override(Arc::new(AskingHooks {
                tool: "bash".into(),
            }))
            .with_default_model(model_spec())
            .with_provider(provider);
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);
        let h = start(state.clone());
        let sid = make_session(&h);
        let lease = acquire_lease(&h, &sid);
        let sub = state.buses.subscribe(&sid, 64);
        let lease_hdr = [("X-Lease", lease.as_str())];
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{sid}/prompt"),
            &lease_hdr,
            serde_json::json!({"text":"do rm"}),
        );
        assert_eq!(r.status, 200);
        // Wait for ApprovalRequest
        let approval_id = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut found = None;
            while std::time::Instant::now() < deadline {
                if let Some(ev) = sub.recv_timeout(Duration::from_millis(200)) {
                    if let Event::ApprovalRequest { id, tool, .. } = ev {
                        if tool == "bash" {
                            found = Some(id);
                            break;
                        }
                    }
                }
            }
            found.expect("ApprovalRequest must appear")
        };
        // Approve with scope always (new schema)
        let hdrs = [
            ("X-Lease", lease.as_str()),
            ("X-Lease-Session", sid.as_str()),
        ];
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &hdrs,
            serde_json::json!({"id": approval_id.0, "decision": "allow", "scope": "always"}),
        );
        assert_eq!(
            r.status,
            200,
            "approve always: {}",
            String::from_utf8_lossy(&r.body)
        );
        // Wait for turn to finish
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while kn9t_server::turn::is_turn_running(&state, &sid)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!kn9t_server::turn::is_turn_running(&state, &sid));
        // Config file must contain the fingerprint
        let cfg_text = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            cfg_text.contains("always"),
            "config must contain always approvals, got: {}",
            cfg_text
        );
        assert!(
            cfg_text.contains("rm -rf /tmp/always_test"),
            "config must contain fingerprint, got: {}",
            cfg_text
        );
        // In-memory cache must have persistent entry
        let fp = "bash:rm -rf /tmp/always_test";
        assert!(
            state.approval_cache.has_persistent(fp),
            "persistent cache must contain {}",
            fp
        );
        // Second prompt with same command should NOT emit ApprovalRequest (cached)
        let sub2 = state.buses.subscribe(&sid, 64);
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{sid}/prompt"),
            &lease_hdr,
            serde_json::json!({"text":"do rm again"}),
        );
        assert_eq!(r.status, 200);
        std::thread::sleep(Duration::from_millis(800));
        let mut saw = false;
        while let Some(ev) = sub2.try_recv() {
            if matches!(ev, Event::ApprovalRequest { .. }) {
                saw = true;
            }
        }
        assert!(
            !saw,
            "second identical command with always scope must not prompt"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while kn9t_server::turn::is_turn_running(&state, &sid)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!kn9t_server::turn::is_turn_running(&state, &sid));
        // Legacy decision "always" also maps to always (F4)
        // Test legacy path via direct record: send decision "always" without scope
        let config_tmp2 = tempfile::tempdir().unwrap();
        let config_path2 = config_tmp2.path().join("config.toml");
        std::fs::write(&config_path2, "").unwrap();
        let cache2 = Arc::new(ApprovalCache::new(config_path2.clone()));
        let registry2 = Arc::new(ApprovalRegistry::new());
        // Simulate a pending approval and resolve with legacy decision "always"
        // Use a dummy slot
        // Create a manual approval id and meta via the approver path
        // Simpler: just test server route accepts legacy
        let (store2, _t2) = temp_store();
        let token2 = kn9t_server::auth::generate_token();
        let mut state2 = ServerState::new(store2, token2, dummy_bash_registry(), Vec::new());
        state2.approval_registry = registry2.clone();
        state2.approval_cache = cache2.clone();
        let approver2: Arc<dyn kn9t_core::Approver> = Arc::new(InteractiveApprover::with_cache(
            registry2.clone(),
            cache2.clone(),
        ));
        state2 = state2
            .with_approver(approver2)
            .with_hooks_override(Arc::new(AskingHooks {
                tool: "bash".into(),
            }))
            .with_default_model(model_spec())
            .with_provider(Arc::new(RepeatingProvider {
                cmd: "rm -rf /tmp/legacy".into(),
                calls: Arc::new(Mutex::new(0)),
            }));
        state2.model_registry = vec![model_spec()];
        let state2 = Arc::new(state2);
        let h2 = start(state2.clone());
        let sid2 = make_session(&h2);
        let lease2 = acquire_lease(&h2, &sid2);
        let sub2 = state2.buses.subscribe(&sid2, 64);
        let r = req_auth(
            &h2,
            "POST",
            &format!("/session/{sid2}/prompt"),
            &[("X-Lease", lease2.as_str())],
            serde_json::json!({"text":"do rm"}),
        );
        assert_eq!(r.status, 200);
        let aid = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut found = None;
            while std::time::Instant::now() < deadline {
                if let Some(ev) = sub2.recv_timeout(Duration::from_millis(200)) {
                    if let Event::ApprovalRequest { id, .. } = ev {
                        found = Some(id);
                        break;
                    }
                }
            }
            found.unwrap()
        };
        let r = req_auth(
            &h2,
            "POST",
            "/approve",
            &[
                ("X-Lease", lease2.as_str()),
                ("X-Lease-Session", sid2.as_str()),
            ],
            serde_json::json!({"id": aid.0, "decision": "always"}),
        );
        assert_eq!(r.status, 200, "legacy always must be 200 not deny");
        // Should have persisted to cache2's file? But cache2 path is config_path2, not config_path, and state2's cache is cache2, so write goes to config_path2
        std::thread::sleep(Duration::from_millis(300));
        // Wait turn
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while kn9t_server::turn::is_turn_running(&state, &sid2)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            cache2.has_persistent("bash:rm -rf /tmp/legacy"),
            "legacy always must persist"
        );
        h.handle.shutdown();
        h2.handle.shutdown();
    }

    #[test]
    fn approve_session_caches() {
        use kn9t_server::policy::{ApprovalCache, ApprovalRegistry, InteractiveApprover};
        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let config_tmp = tempfile::tempdir().unwrap();
        let config_path = config_tmp.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        let cache = Arc::new(ApprovalCache::new(config_path.clone()));
        let registry = Arc::new(ApprovalRegistry::new());
        let approver: Arc<dyn kn9t_core::Approver> = Arc::new(InteractiveApprover::with_cache(
            registry.clone(),
            cache.clone(),
        ));
        let tools = dummy_bash_registry();
        let mut state = ServerState::new(store, token, tools, Vec::new());
        state.approval_registry = registry.clone();
        state.approval_cache = cache.clone();
        let calls = Arc::new(Mutex::new(0u32));
        struct Rep {
            cmd: String,
            calls: Arc<Mutex<u32>>,
        }
        impl kn9t_core::Provider for Rep {
            fn name(&self) -> &str {
                "rep"
            }
            fn stream(
                &self,
                req: &kn9t_core::Request,
                _cancel: &kn9t_core::Cancel,
            ) -> Result<
                Box<dyn Iterator<Item = Result<kn9t_core::Chunk, kn9t_core::ProvErr>> + Send>,
                kn9t_core::ProvErr,
            > {
                if req.max_tokens == Some(16) {
                    return Ok(Box::new(
                        vec![
                            Ok(kn9t_core::Chunk::Text {
                                idx: 0,
                                delta: "title".into(),
                            }),
                            Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                                tokens: kn9t_core::Tokens::default(),
                                model: kn9t_core::ModelRef {
                                    provider: "rep".into(),
                                    id: "m1".into(),
                                },
                            })),
                            Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::Stop)),
                        ]
                        .into_iter(),
                    ));
                }
                let n = {
                    let mut c = self.calls.lock().unwrap();
                    let n = *c;
                    *c += 1;
                    n
                };
                if n % 2 == 0 {
                    let args = serde_json::json!({"cmd": self.cmd}).to_string();
                    Ok(Box::new(
                        vec![
                            Ok(kn9t_core::Chunk::ToolCall {
                                idx: 0,
                                id: kn9t_core::CallId("call_1".into()),
                                name: "bash".into(),
                            }),
                            Ok(kn9t_core::Chunk::ToolArgs {
                                idx: 0,
                                delta: args,
                            }),
                            Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                                tokens: kn9t_core::Tokens::default(),
                                model: kn9t_core::ModelRef {
                                    provider: "rep".into(),
                                    id: "m1".into(),
                                },
                            })),
                            Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::ToolUse)),
                        ]
                        .into_iter(),
                    ))
                } else {
                    Ok(Box::new(
                        vec![
                            Ok(kn9t_core::Chunk::Text {
                                idx: 0,
                                delta: "done".into(),
                            }),
                            Ok(kn9t_core::Chunk::Usage(kn9t_core::Usage {
                                tokens: kn9t_core::Tokens::default(),
                                model: kn9t_core::ModelRef {
                                    provider: "rep".into(),
                                    id: "m1".into(),
                                },
                            })),
                            Ok(kn9t_core::Chunk::Stop(kn9t_core::StopReason::Stop)),
                        ]
                        .into_iter(),
                    ))
                }
            }
        }
        let provider: Arc<dyn kn9t_core::Provider> = Arc::new(Rep {
            cmd: "rm -rf /tmp/session_test".into(),
            calls: calls.clone(),
        });
        state = state
            .with_approver(approver)
            .with_hooks_override(Arc::new(AskingHooks {
                tool: "bash".into(),
            }))
            .with_default_model(model_spec())
            .with_provider(provider);
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);
        let h = start(state.clone());
        let sid = make_session(&h);
        let lease = acquire_lease(&h, &sid);
        let sub = state.buses.subscribe(&sid, 64);
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{sid}/prompt"),
            &[("X-Lease", lease.as_str())],
            serde_json::json!({"text":"first"}),
        );
        assert_eq!(r.status, 200);
        let aid = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut found = None;
            while std::time::Instant::now() < deadline {
                if let Some(ev) = sub.recv_timeout(Duration::from_millis(200)) {
                    if let Event::ApprovalRequest { id, .. } = ev {
                        found = Some(id);
                        break;
                    }
                }
            }
            found.expect("first ApprovalRequest")
        };
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &[
                ("X-Lease", lease.as_str()),
                ("X-Lease-Session", sid.as_str()),
            ],
            serde_json::json!({"id": aid.0, "decision": "allow", "scope": "session"}),
        );
        assert_eq!(r.status, 200);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while kn9t_server::turn::is_turn_running(&state, &sid)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!kn9t_server::turn::is_turn_running(&state, &sid));
        assert!(state
            .approval_cache
            .has_session(&sid, "bash:rm -rf /tmp/session_test"));
        // Second prompt same command should be auto-allowed (no ApprovalRequest)
        let sub2 = state.buses.subscribe(&sid, 64);
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{sid}/prompt"),
            &[("X-Lease", lease.as_str())],
            serde_json::json!({"text":"second"}),
        );
        assert_eq!(r.status, 200);
        std::thread::sleep(Duration::from_millis(800));
        let mut saw = false;
        while let Some(ev) = sub2.try_recv() {
            if matches!(ev, Event::ApprovalRequest { .. }) {
                saw = true;
            }
        }
        assert!(
            !saw,
            "session scope must cache, second prompt must not emit ApprovalRequest"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while kn9t_server::turn::is_turn_running(&state, &sid)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!kn9t_server::turn::is_turn_running(&state, &sid));
        // Different session should still prompt (session scope is per-session)
        let sid2 = make_session(&h);
        let lease2 = acquire_lease(&h, &sid2);
        let sub3 = state.buses.subscribe(&sid2, 64);
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{sid2}/prompt"),
            &[("X-Lease", lease2.as_str())],
            serde_json::json!({"text":"third in other session"}),
        );
        assert_eq!(r.status, 200);
        let mut found = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(ev) = sub3.recv_timeout(Duration::from_millis(200)) {
                if let Event::ApprovalRequest { id, .. } = ev {
                    found = Some(id);
                    break;
                }
            }
        }
        assert!(
            found.is_some(),
            "other session must still prompt (session scope is isolated)"
        );
        // Clean up: resolve that pending so turn doesn't hang
        let aid2 = found.unwrap();
        let r = req_auth(
            &h,
            "POST",
            "/approve",
            &[
                ("X-Lease", lease2.as_str()),
                ("X-Lease-Session", sid2.as_str()),
            ],
            serde_json::json!({"id": aid2.0, "decision": "deny", "scope": "once"}),
        );
        assert_eq!(r.status, 200);
        std::thread::sleep(Duration::from_millis(300));
        h.handle.shutdown();
    }

    // ── abort_then_prompt_race ────────────────────────────────────────────────────

    /// A provider that emits a tool call and blocks until signaled.
    /// This simulates a slow stream where the user aborts mid-stream.
    struct SlowToolProvider {
        started: Arc<std::sync::atomic::AtomicBool>,
        proceed: Arc<std::sync::Barrier>,
    }

    impl Provider for SlowToolProvider {
        fn name(&self) -> &str {
            "slow-tool"
        }
        fn stream(
            &self,
            _req: &Request,
            cancel: &Cancel,
        ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
            // Signal that stream started
            self.started.store(true, Ordering::SeqCst);

            // Wait at barrier (simulates slow stream)
            self.proceed.wait();

            // Check if cancelled
            if cancel.cancelled() {
                // Return simple text + aborted
                let chunks = vec![
                    Ok(Chunk::Text {
                        idx: 0,
                        delta: "partial".into(),
                    }),
                    Ok(Chunk::Usage(Usage {
                        tokens: Tokens {
                            input: 50,
                            output: 5,
                            ..Default::default()
                        },
                        model: ModelRef {
                            provider: "slow-tool".into(),
                            id: "m1".into(),
                        },
                    })),
                    Ok(Chunk::Stop(StopReason::Aborted)),
                ];
                return Ok(Box::new(chunks.into_iter()));
            }

            // Normal completion with tool call
            let chunks = vec![
                Ok(Chunk::ToolCall {
                    idx: 0,
                    id: kn9t_core::CallId("call_1".into()),
                    name: "bash".into(),
                }),
                Ok(Chunk::ToolArgs {
                    idx: 0,
                    delta: r#"{"command":"echo test"}"#.into(),
                }),
                Ok(Chunk::Usage(Usage {
                    tokens: Tokens {
                        input: 50,
                        output: 20,
                        ..Default::default()
                    },
                    model: ModelRef {
                        provider: "slow-tool".into(),
                        id: "m1".into(),
                    },
                })),
                Ok(Chunk::Stop(StopReason::ToolUse)),
            ];
            Ok(Box::new(chunks.into_iter()))
        }
    }

    /// Test that prompt is blocked while a turn is running.
    /// Without the fix, a new prompt sent immediately after abort can corrupt the transcript.
    #[test]
    fn abort_then_prompt_race() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let proceed = Arc::new(std::sync::Barrier::new(2));

        let provider: Arc<dyn Provider> = Arc::new(SlowToolProvider {
            started: started.clone(),
            proceed: proceed.clone(),
        });

        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let state = Arc::new(
            ServerState::new(store.clone(), token.clone(), Default::default(), Vec::new())
                .with_default_model(model_spec())
                .with_provider(provider),
        );

        let h = start(state.clone());
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;

        // Send first prompt - this will trigger the slow provider
        let r1 = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &[("X-Lease", lh)],
            serde_json::json!({ "text": "run a command" }),
        );
        assert_eq!(r1.status, 200, "first prompt accepted");

        // Wait for turn to start
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !started.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(started.load(Ordering::SeqCst), "turn must start");

        // Now abort while the turn is "running"
        let r_abort = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/abort"),
            &[("X-Lease", lh)],
            serde_json::Value::Null,
        );
        assert_eq!(r_abort.status, 200, "abort accepted");

        // BUG TEST: Without fix, the server accepts a new prompt while the first turn
        // is still running. This causes the user message to be appended before
        // the first turn completes, potentially corrupting the transcript.
        //
        // The expected behavior is that the server should either:
        // 1. Block until the first turn completes, OR
        // 2. Return an error indicating a turn is still running (409 Conflict)
        //
        // Send second prompt BEFORE releasing the barrier (while first turn blocked)
        let r2 = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &[("X-Lease", lh)],
            serde_json::json!({ "text": "please use read" }),
        );

        // The bug is that this succeeds even though a turn is running
        // With the fix, this should return 409 or block
        let is_conflict = r2.status == 409;

        // Now let the first turn complete
        proceed.wait();

        // Give turns time to complete
        std::thread::sleep(Duration::from_millis(200));

        // Check state.idle.running_turns() via /status endpoint
        let status = req_auth(&h, "GET", "/status", &[], serde_json::Value::Null);
        let _running = status.json()["running_turns"].as_u64().unwrap_or(0);

        // Wait for all turns to finish
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.idle.running_turns() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }

        // With the fix, prompt() should return 409 when a turn is running
        assert!(
            is_conflict,
            "prompt while turn running should return 409 Conflict, got {}",
            r2.status
        );
        assert_eq!(r2.json()["error"].as_str(), Some("turn_running"));

        h.handle.shutdown();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "plugin reload harness needs a POSIX shell script as the dummy plugin binary"
    )]
    fn plugin_reload() {
        // Build a dummy plugin that declares one tool and replies instantly.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("reload-tools");
        write_dummy_plugin(&bin, "reload-tools", "reload_tool");

        let (store, _tmp_store) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let kv: Arc<dyn kn9t_core::PluginKv> = store.clone() as Arc<dyn kn9t_core::PluginKv>;
        let host = kn9t_plugin::PluginHost::spawn(&bin, &[], kv)
            .expect("spawn dummy plugin for reload test");
        let host = Arc::new(host);
        let tools = {
            let t = kn9t_server::tools::extract_tools_public(&host);
            kn9t_core::ToolRegistry::from_tools(t)
        };
        let mut state = ServerState::new(store.clone(), token.clone(), tools, vec![host.clone()]);
        state.set_plugin_spawn(
            "reload-tools".to_string(),
            vec![bin.to_string_lossy().into_owned()],
            vec![],
        );
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);
        let h = start(state);

        // First reload — no in-flight, should re-handshake and re-register.
        let r = req_auth(
            &h,
            "POST",
            "/plugin/reload-tools/reload",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(
            r.status,
            200,
            "first reload: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.json()["reloaded"].as_str().unwrap(), "reload-tools");
        // Tools count after reload should still be 1 (the same plugin re-registered).
        assert_eq!(r.json()["tools"].as_u64().unwrap(), 1);

        // Second reload should also succeed (idempotent).
        let r2 = req_auth(
            &h,
            "POST",
            "/plugin/reload-tools/reload",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(
            r2.status,
            200,
            "second reload: {}",
            String::from_utf8_lossy(&r2.body)
        );

        // Unknown plugin → 404.
        let r3 = req_auth(
            &h,
            "POST",
            "/plugin/unknown/reload",
            &[],
            serde_json::Value::Null,
        );
        assert_eq!(r3.status, 404, "unknown plugin reload should 404");

        h.handle.shutdown();
    }

    // helper for plugin_reload — writes a minimal handshake plugin as a shell script (unix only)
    // For windows, the test is skipped (requires .exe).
    #[cfg(unix)]
    fn write_dummy_plugin(path: &std::path::Path, name: &str, tool: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
        "#!/bin/sh\nIFS= read -r _host_hello\nprintf '%s\\n' '{{\"t\":\"hello\",\"name\":\"{name}\",\"capabilities\":[\"streaming\",\"cancelable\"],\"tools\":[{{\"name\":\"{tool}\",\"description\":\"dummy\",\"schema\":{{\"type\":\"object\"}},\"parallel_safe\":false}}]}}'\nwhile IFS= read -r _line; do :; done\n"
    );
        std::fs::write(path, script).expect("write dummy plugin");
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o111)).unwrap();
    }
    // On Windows the test above is #[ignore]d (the dummy plugin is a POSIX shell
    // script), so this stub only has to satisfy the compiler - it is never called.
    #[cfg(windows)]
    fn write_dummy_plugin(_path: &std::path::Path, _name: &str, _tool: &str) {
        unreachable!("plugin_reload is #[ignore]d on Windows");
    }

    // ── 96E-17: plugin → host API ops (host_api capability) ─────────────────────

    struct StubProvider {
        text: String,
    }
    impl Provider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }
        fn stream(
            &self,
            _req: &Request<'_>,
            _cancel: &Cancel,
        ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
            let chunks: Vec<Result<Chunk, ProvErr>> = vec![
                Ok(Chunk::Text {
                    idx: 0,
                    delta: self.text.clone(),
                }),
                Ok(Chunk::Usage(Usage {
                    tokens: Tokens {
                        input: 10,
                        output: 20,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    model: ModelRef {
                        provider: "test".into(),
                        id: "m1".into(),
                    },
                })),
                Ok(Chunk::Stop(StopReason::Stop)),
            ];
            Ok(Box::new(chunks.into_iter()))
        }
    }

    struct AllowAllApprover;
    impl Approver for AllowAllApprover {
        fn request(
            &self,
            _call: &kn9t_core::ToolCall,
            _cwd: &std::path::Path,
            _reason: &str,
            _ctx: &kn9t_core::ApprovalCtx,
        ) -> kn9t_core::Decision {
            kn9t_core::Decision::Allow
        }
    }

    /// A trivial registry tool for tool_execute.
    struct EchoTool;
    impl kn9t_core::Tool for EchoTool {
        fn spec(&self) -> &kn9t_core::ToolSpec {
            static SPEC: std::sync::OnceLock<kn9t_core::ToolSpec> = std::sync::OnceLock::new();
            SPEC.get_or_init(|| kn9t_core::ToolSpec {
                name: "echo_tool".into(),
                description: "echo".into(),
                schema: serde_json::json!({"type":"object"}),
                hidden: false,
                effects: vec![],
                policy: Default::default(),
            })
        }
        fn execute(
            &self,
            args: &serde_json::Value,
            _ctx: &kn9t_core::ToolCtx,
            _cancel: &Cancel,
        ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            Ok(kn9t_core::ToolOutput {
                content: vec![Content::Text {
                    text: format!("echo:{}", args["msg"]),
                }],
                details: None,
                is_error: false,
            })
        }
    }

    #[test]
    fn p1_96e17_host_api_ops_session_read_provider_complete_tool_execute() {
        use kn9t_plugin::HostApi as _;
        use kn9t_server::host_api::ServerHostApi;

        let (store, _tmp) = temp_store();
        let spec = model_spec();
        let mut state = ServerState::new(
            store.clone(),
            "test-token".into(),
            Default::default(),
            Vec::new(),
        )
        .with_default_model(spec.clone())
        .with_provider(Arc::new(StubProvider {
            text: "hello from stub".into(),
        }));
        state.model_registry = vec![spec.clone()];
        let state = Arc::new(state);
        {
            // Approver must not block the test on interactive approval.
            *state.approver.write().unwrap() = Arc::new(AllowAllApprover);
            // A registry tool for tool_execute.
            let mut reg = state.tools.lock().unwrap();
            reg.push(Arc::new(EchoTool));
        }

        let sid = SessionId::new();
        let model_ref = ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        };
        kn9t_store::create_session(&store, &sid, "/cwd", &model_ref).unwrap();

        store
            .append(
                &sid,
                Event::MessageAppended {
                    seq: 0,
                    msg: kn9t_core::Message {
                        id: MsgId::new(),
                        role: Role::User,
                        content: vec![Content::Text {
                            text: "first".to_string(),
                        }],
                        silent: false,
                    },
                },
            )
            .unwrap();
        store
            .append(
                &sid,
                Event::MessageAppended {
                    seq: 0,
                    msg: kn9t_core::Message {
                        id: MsgId::new(),
                        role: Role::Assistant,
                        content: vec![Content::ToolCall {
                            id: kn9t_core::CallId("t1".into()),
                            name: "echo_tool".into(),
                            args_json: "{\"msg\":\"hi\"}".into(),
                        }],
                        silent: false,
                    },
                },
            )
            .unwrap();

        let api = ServerHostApi {
            state: state.clone(),
        };
        let sid_str = sid.0.clone();

        // ── session_read ─────────────────────────────────────────────────────────
        let r = api
            .handle(
                "plug",
                Some(&sid_str),
                "session_read",
                &serde_json::json!({"session": sid_str, "end": 99}),
            )
            .unwrap();
        let msgs = r["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "both appended messages readable");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_call");
        assert_eq!(msgs[1]["content"][0]["id"], "t1");

        // ── provider_complete ────────────────────────────────────────────────────
        let r = api
            .handle(
                "plug",
                Some(&sid_str),
                "provider_complete",
                &serde_json::json!({
                    "session": sid_str,
                    "messages": [{
                        "id": "u1",
                        "role": "user",
                        "content": [{"type": "text", "text": "hi"}],
                        "silent": false
                    }]
                }),
            )
            .unwrap();
        assert_eq!(r["content"][0]["text"], "hello from stub");
        assert_eq!(r["stop"], "stop");
        assert_eq!(r["usage"]["input"], 10);
        assert_eq!(r["usage"]["output"], 20);

        // Usage recorded in the session as Subagent (honest budget accounting).
        let kinds = store
            .query_strings("SELECT kind FROM usage WHERE session_id=?1", &[&sid_str])
            .unwrap();
        assert!(
            kinds.iter().any(|k| k == "subagent"),
            "provider_complete usage must be recorded as subagent, got {kinds:?}"
        );

        // ── tool_execute (with policy) ───────────────────────────────────────────
        let r = api
        .handle(
            "plug",
            Some(&sid_str),
            "tool_execute",
            &serde_json::json!({"session": sid_str, "name": "echo_tool", "args": {"msg": "bonjour"}}),
        )
        .unwrap();
        assert_eq!(r["is_error"], false);
        assert_eq!(r["content"][0]["text"], "echo:\"bonjour\"");

        // Unknown op → Err, unknown tool → Err.
        assert!(api
            .handle("plug", Some(&sid_str), "nope", &serde_json::json!({}))
            .is_err());
        assert!(api
            .handle(
                "plug",
                Some(&sid_str),
                "tool_execute",
                &serde_json::json!({"name": "nope"})
            )
            .is_err());

        // ── tool_list (child toolset composition) ────────────────────────────────
        let r = api
            .handle("plug", Some(&sid_str), "tool_list", &serde_json::json!({}))
            .unwrap();
        let tools = r["tools"].as_array().unwrap();
        assert!(
            tools.iter().any(|t| t == "echo_tool"),
            "registry names listed, got {tools:?}"
        );
    }

    #[test]
    fn p1_96e17_session_fork_and_prompt_spawns_a_real_child() {
        use kn9t_plugin::HostApi as _;
        use kn9t_server::host_api::ServerHostApi;

        // A child session is just a forked session running a turn (R-PLUG-110).
        let (store, _tmp) = temp_store();
        let spec = model_spec();
        let mut state = ServerState::new(
            store.clone(),
            "test-token".into(),
            Default::default(),
            Vec::new(),
        )
        .with_default_model(spec.clone())
        .with_provider(Arc::new(StubProvider {
            text: "child says hi".into(),
        }));
        state.model_registry = vec![spec.clone()];
        {
            *state.approver.write().unwrap() = Arc::new(AllowAllApprover);
        }
        let state = Arc::new(state);
        let api = ServerHostApi {
            state: state.clone(),
        };

        let parent = SessionId::new();
        let model_ref = ModelRef {
            provider: "test".into(),
            id: "m1".into(),
        };
        kn9t_store::create_session(&store, &parent, "/cwd", &model_ref).unwrap();

        // 1. fork a bare child (no transcript inheritance).
        let r = api
            .handle(
                "plug",
                Some(&parent.0.as_str()),
                "session_fork",
                &serde_json::json!({
                    "session": parent.0, "copy_events": false, "budget_usd": 10.0,
                }),
            )
            .unwrap();
        let child_id = r["session"].as_str().unwrap().to_string();

        // The child row is a subagent fork with the budget captured.
        let (reason, budget): (String, Option<f64>) = store
            .query_one(
                "SELECT fork_reason, budget_remaining_usd FROM sessions WHERE id=?1",
                &[&child_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(reason, "subagent");
        assert_eq!(budget, Some(10.0));

        // 2. run one synchronous turn on the child.
        let r = api
            .handle(
                "plug",
                Some(&child_id),
                "session_prompt",
                &serde_json::json!({
                    "session": child_id, "text": "summarize the span",
                }),
            )
            .unwrap();
        assert_eq!(r["result"], "child says hi");

        // The child has its OWN transcript (task + assistant) and its OWN usage.
        let msgs: Vec<String> = store
            .query_strings(
                "SELECT role FROM messages WHERE session_id=?1 ORDER BY seq",
                &[&child_id],
            )
            .unwrap();
        assert_eq!(
            msgs,
            vec!["user", "assistant"],
            "child transcript lives in its session"
        );
        let usage: Vec<String> = store
            .query_strings("SELECT kind FROM usage WHERE session_id=?1", &[&child_id])
            .unwrap();
        assert!(
            !usage.is_empty(),
            "child usage is recorded in the child session"
        );

        // 3. budget is enforced: a fork with a tiny budget errors when exceeded.
        let r = api
            .handle(
                "plug",
                Some(&parent.0.as_str()),
                "session_fork",
                &serde_json::json!({
                    "session": parent.0, "copy_events": false, "budget_usd": 0.0,
                }),
            )
            .unwrap();
        let tight = r["session"].as_str().unwrap().to_string();
        let err = api
            .handle(
                "plug",
                Some(&tight),
                "session_prompt",
                &serde_json::json!({
                    "session": tight, "text": "will blow the budget",
                }),
            )
            .expect_err("zero budget must fail");
        assert!(err.contains("budget"), "budget error surfaced, got {err}");
    }

    // ── steer_during_tool_execution ───────────────────────────────────────────────
    // Regression test: steering during tool execution must not corrupt the transcript.
    // Without fix: [assistant:tool_use] -> [user:steer] -> [tool:tool_result] = INVALID
    // The API must reject the steer while a turn is running (409 turn_running).

    /// A provider that emits a tool call. The tool itself will block.
    struct ToolCallProvider {
        call_count: Arc<Mutex<u32>>,
    }

    impl Provider for ToolCallProvider {
        fn name(&self) -> &str {
            "tool-call"
        }
        fn stream(
            &self,
            req: &Request,
            _cancel: &Cancel,
        ) -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr> {
            // Title request (autotitle) - return plain text
            if req.max_tokens == Some(16) {
                return Ok(Box::new(
                    vec![
                        Ok(Chunk::Text {
                            idx: 0,
                            delta: "title".into(),
                        }),
                        Ok(Chunk::Usage(Usage {
                            tokens: Tokens::default(),
                            model: ModelRef {
                                provider: "tool-call".into(),
                                id: "m1".into(),
                            },
                        })),
                        Ok(Chunk::Stop(StopReason::Stop)),
                    ]
                    .into_iter(),
                ));
            }

            let n = {
                let mut c = self.call_count.lock().unwrap();
                let v = *c;
                *c += 1;
                v
            };

            if n == 0 {
                // First call: emit tool_use for slow_tool
                let chunks = vec![
                    Ok(Chunk::ToolCall {
                        idx: 0,
                        id: kn9t_core::CallId("call_slow".into()),
                        name: "slow_tool".into(),
                    }),
                    Ok(Chunk::ToolArgs {
                        idx: 0,
                        delta: r#"{"delay_ms": 500}"#.into(),
                    }),
                    Ok(Chunk::Usage(Usage {
                        tokens: Tokens {
                            input: 10,
                            output: 5,
                            ..Default::default()
                        },
                        model: ModelRef {
                            provider: "tool-call".into(),
                            id: "m1".into(),
                        },
                    })),
                    Ok(Chunk::Stop(StopReason::ToolUse)),
                ];
                Ok(Box::new(chunks.into_iter()))
            } else {
                // Subsequent: just text
                let chunks = vec![
                    Ok(Chunk::Text {
                        idx: 0,
                        delta: "done".into(),
                    }),
                    Ok(Chunk::Usage(Usage {
                        tokens: Tokens::default(),
                        model: ModelRef {
                            provider: "tool-call".into(),
                            id: "m1".into(),
                        },
                    })),
                    Ok(Chunk::Stop(StopReason::Stop)),
                ];
                Ok(Box::new(chunks.into_iter()))
            }
        }
    }

    /// A slow tool that blocks for a configurable duration.
    struct SlowTool {
        started: Arc<std::sync::atomic::AtomicBool>,
    }

    impl kn9t_core::Tool for SlowTool {
        fn spec(&self) -> &kn9t_core::ToolSpec {
            Box::leak(Box::new(kn9t_core::ToolSpec {
                name: "slow_tool".into(),
                description: "A tool that sleeps".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "delay_ms": {"type": "integer"}
                    }
                }),
                hidden: false,
                effects: vec![],
                policy: Default::default(),
            }))
        }

        fn execute(
            &self,
            args: &serde_json::Value,
            _ctx: &kn9t_core::ToolCtx,
            cancel: &Cancel,
        ) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> {
            self.started.store(true, Ordering::SeqCst);
            let delay = args["delay_ms"].as_u64().unwrap_or(100);
            // Sleep in small increments checking cancel
            let start = std::time::Instant::now();
            while start.elapsed().as_millis() < delay as u128 {
                if cancel.cancelled() {
                    return Ok(kn9t_core::ToolOutput {
                        content: vec![Content::Text {
                            text: "cancelled".into(),
                        }],
                        details: None,
                        is_error: true,
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(kn9t_core::ToolOutput {
                content: vec![Content::Text {
                    text: "slept".into(),
                }],
                details: None,
                is_error: false,
            })
        }
    }

    #[test]
    fn steer_during_tool_execution_appends_after_tool_result() {
        // Regression test for transcript corruption when steering during tool execution.
        // BUG: steer appends directly to store, creating invalid sequence:
        //   [assistant:tool_use] -> [user:steer] -> [tool:tool_result]
        // FIX: steer must be buffered and appended AFTER tool_result:
        //   [assistant:tool_use] -> [tool:tool_result] -> [user:steer]
        
        let tool_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let call_count = Arc::new(Mutex::new(0u32));

        let provider: Arc<dyn Provider> = Arc::new(ToolCallProvider {
            call_count: call_count.clone(),
        });

        let slow_tool: Arc<dyn kn9t_core::Tool> = Arc::new(SlowTool {
            started: tool_started.clone(),
        });
        let mut tools = kn9t_core::ToolRegistry::new();
        tools.push(slow_tool);

        let (store, _tmp) = temp_store();
        let token = kn9t_server::auth::generate_token();
        let mut state =
            ServerState::new(store.clone(), token.clone(), tools, Vec::new())
                .with_default_model(model_spec())
                .with_provider(provider);
        state.model_registry = vec![model_spec()];
        let state = Arc::new(state);

        let h = start(state.clone());
        let id = make_session(&h);
        let lease = acquire_lease(&h, &id);
        let lh: &str = &lease;

        // Send prompt - will trigger tool_use then slow_tool execution
        let r = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/prompt"),
            &[("X-Lease", lh)],
            serde_json::json!({ "text": "run slow tool" }),
        );
        assert_eq!(r.status, 200, "prompt accepted");

        // Wait for tool to start executing
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !tool_started.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(tool_started.load(Ordering::SeqCst), "tool must start");

        // Small delay to ensure we're solidly in the tool execution phase
        std::thread::sleep(Duration::from_millis(50));

        // Steer while the tool is executing - should succeed (200)
        let r_steer = req_auth(
            &h,
            "POST",
            &format!("/session/{id}/steer"),
            &[("X-Lease", lh)],
            serde_json::json!({ "text": "change direction" }),
        );
        assert_eq!(
            r_steer.status, 200,
            "steer must be accepted, got {}: {}",
            r_steer.status,
            String::from_utf8_lossy(&r_steer.body)
        );

        // Wait for turn to complete
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while kn9t_server::turn::is_turn_running(&state, &id)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }

        // Verify transcript integrity
        // CORRECT order: [user:prompt] [assistant:tool_use] [tool:tool_result] [user:steer] ...
        // WRONG order:   [user:prompt] [assistant:tool_use] [user:steer] [tool:tool_result] ...
        let roles: Vec<String> = store
            .query_strings(
                "SELECT role FROM messages WHERE session_id=?1 ORDER BY seq",
                &[&id],
            )
            .unwrap();
        
        // The critical invariant: no user message between assistant (tool_use) and tool (tool_result)
        let has_corruption = roles.windows(3).any(|w| w == ["assistant", "user", "tool"]);
        
        assert!(
            !has_corruption,
            "BUG: transcript corrupted - user message between tool_use and tool_result. Got {:?}",
            roles
        );

        h.handle.shutdown();
    }
} // mod srv
