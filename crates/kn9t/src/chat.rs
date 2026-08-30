//! `kn9t chat` subcommand — single-turn CLI over the kn9t-server HTTP/SSE API.
//!
//! Takes the same path as the TUI:
//!   1. POST /session              → session_id
//!   2. POST /session/{id}/lease   → lease token
//!   3. GET  /session/{id}/events  (SSE, background thread)
//!   4. POST /session/{id}/prompt  [X-Lease: <token>]
//!   5. Stream events: print TextDelta to stdout, tool activity to stderr.

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// ── Public entry points ───────────────────────────────────────────────────────

/// `kn9t attach [session-id]` — open SSE on an existing session and enter REPL.
/// If no id, picks the session with the highest head_seq.
pub fn attach(args: &[String], port: u16, server_token: &str) {
    let auth = format!("Bearer {server_token}");
    let host = format!("127.0.0.1:{port}");
    
    // Keep server alive while CLI is running.
    let _attach_stop = spawn_global_attach(&host, &auth);
    
    let session_id = if let Some(id) = args.first().filter(|s| !s.starts_with('-')) {
        id.clone()
    } else {
        resolve_latest_session(&host, &auth)
    };
    eprintln!("[kn9t attach] session: {session_id}");
    repl_loop(&session_id, &host, &auth);
    // _attach_stop dropped here → attach thread stops
}

pub fn run(args: &[String], port: u16, server_token: &str) {
    // ── parse args ──
    let auth = format!("Bearer {server_token}");
    let host = format!("127.0.0.1:{port}");

    // Default: first model from server config.
    let (mut model_provider, mut model_id) = match resolve_default_model(&host, &auth) {
        Some(m) => m,
        None => {
            eprintln!("[kn9t chat] no models configured — add a [[provider]] and [[model]] to ~/.kn9t/config.toml");
            std::process::exit(1);
        }
    };
    let mut do_continue = false;
    let mut prompt_words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" if i + 1 < args.len() => {
                // Strip surrounding quotes if present (shell may leave them).
                let spec = args[i + 1].trim_matches('"').trim_matches('\'');
                // Accept both "provider:model" and "provider/model" formats.
                if let Some((p, m)) = spec.split_once(':') {
                    model_provider = p.to_string();
                    model_id = m.to_string();
                } else if let Some((p, m)) = spec.split_once('/') {
                    model_provider = p.to_string();
                    model_id = m.to_string();
                } else {
                    model_id = spec.to_string();
                }
                i += 2;
            }
            "--continue" => { do_continue = true; i += 1; }
            _ => { prompt_words.push(&args[i]); i += 1; }
        }
    }

    // Keep server alive while CLI is running.
    let _attach_stop = spawn_global_attach(&host, &auth);

    // ── REPL mode: no prompt words, or --continue ──
    if prompt_words.is_empty() || do_continue {
        let session_id = if do_continue {
            resolve_latest_session(&host, &auth)
        } else {
            // New session → REPL
            create_session(&host, &auth, &model_provider, &model_id)
        };
        eprintln!("[kn9t chat] session: {session_id}");
        if !do_continue {
            eprintln!("[kn9t chat] model: {model_provider}:{model_id}");
        }
        repl_loop(&session_id, &host, &auth);
        return;
    }

    // ── One-shot mode ──
    let prompt = prompt_words.join(" ");

    let session_id = create_session(&host, &auth, &model_provider, &model_id);
    eprintln!("[kn9t chat] session: {session_id}");
    eprintln!("[kn9t chat] model: {model_provider}:{model_id}");

    let lease = acquire_lease_with_backoff(&host, &auth, &session_id);

    // Subscribe SSE before sending prompt (no race).
    set_approval_ctx(&host, &auth);
    let rx = subscribe_sse(&host, &auth, &session_id, 0);
    thread::sleep(Duration::from_millis(50));

    eprintln!("[kn9t chat] prompt: {prompt}");
    eprintln!("---");
    post_json(&host, &auth,
        &format!("/session/{session_id}/prompt"),
        &json!({ "text": prompt }), Some(&lease));

    stream_events(rx); // rx dropped here → SSE thread exits → server client_detached
    release_lease(&host, &auth, &session_id, &lease);
}

// ── Default model resolution ─────────────────────────────────────────────────

/// Query GET /models and return (provider, id) for the default model.
fn resolve_default_model(host: &str, auth: &str) -> Option<(String, String)> {
    let request = format!("GET /models HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n");
    let mut stream = TcpStream::connect(host).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).ok()?;
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    let body: Value = serde_json::from_str(&resp[body_start..]).ok()?;
    let models = body.get("models")?.as_array()?;
    // Find the model marked as default.
    for m in models {
        if m.get("is_default") == Some(&Value::Bool(true)) {
            let provider = m.get("provider")?.as_str()?.to_string();
            let id = m.get("id")?.as_str()?.to_string();
            return Some((provider, id));
        }
    }
    // Fallback: first model in list.
    let first = models.first()?;
    let provider = first.get("provider")?.as_str()?.to_string();
    let id = first.get("id")?.as_str()?.to_string();
    Some((provider, id))
}

// ── HTTP helper ───────────────────────────────────────────────────────────────

fn post_json(host: &str, auth: &str, path: &str, body: &Value, lease: Option<&str>) -> Value {
    let body_str = serde_json::to_string(body).unwrap();
    let mut headers = format!(
        "Authorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body_str.len()
    );
    if let Some(l) = lease {
        headers.push_str(&format!("X-Lease: {l}\r\n"));
    }
    let request = format!("POST {path} HTTP/1.0\r\nHost: {host}\r\n{headers}\r\n{body_str}");
    let mut stream = TcpStream::connect(host).unwrap_or_else(|e| {
        eprintln!("[kn9t chat] cannot reach server: {e}");
        std::process::exit(1);
    });
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).unwrap_or(0);
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Null)
}

// ── Approval context (thread-local so the SSE loop can reach host/auth) ──────

use std::cell::RefCell;

thread_local! {
    static APPROVAL_CTX: RefCell<Option<(String, String)>> = RefCell::new(None);
}

fn set_approval_ctx(host: &str, auth: &str) {
    APPROVAL_CTX.with(|ctx| {
        *ctx.borrow_mut() = Some((host.to_string(), auth.to_string()));
    });
}

fn post_approval(host: &str, auth: &str, req_id: &str, allow: bool) {
    let decision = if allow { "allow" } else { "deny" };
    post_json(host, auth, "/approve",
        &json!({ "id": req_id, "decision": decision }), None);
}

/// Render an inline keyboard selector: `[ No ]  [ Yes ]`.
/// Returns true if the user chose Yes.
fn approval_selector(tool: &str, args: &Value) -> bool {
    // Print tool info.
    eprintln!();
    eprintln!("  [approval] {tool}");
    if let Ok(pretty) = serde_json::to_string_pretty(args) {
        for line in pretty.lines().take(10) {
            eprintln!("    {line}");
        }
    }
    eprintln!();

    let mut selected = false; // false = No (safe default), true = Yes

    // Enter raw mode for key capture.
    let _ = terminal::enable_raw_mode();

    loop {
        // Render the two options.
        let (no_hl, yes_hl) = if selected {
            ("  No  ", "[ Yes ]")
        } else {
            ("[ No ]", "  Yes  ")
        };
        eprint!("\r  {no_hl}   {yes_hl}   ←/→ choose · Enter confirm  ");
        let _ = std::io::stderr().flush();

        if let Ok(Event::Key(key)) = event::read() {
            match (key.code, key.modifiers) {
                (KeyCode::Left, _)  | (KeyCode::Char('h'), _) => { selected = false; }
                (KeyCode::Right, _) | (KeyCode::Char('l'), _) => { selected = true; }
                (KeyCode::Enter, _) => { break; }
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    let _ = terminal::disable_raw_mode();
                    eprintln!();
                    std::process::exit(130);
                }
                _ => {}
            }
        }
    }

    let _ = terminal::disable_raw_mode();
    eprintln!("\r  {}", if selected { "→ allowed" } else { "→ denied " });
    selected
}

// ── HTTP GET helper ───────────────────────────────────────────────────────────

fn get_json(host: &str, auth: &str, path: &str) -> Value {
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n"
    );
    let mut stream = TcpStream::connect(host).unwrap_or_else(|e| {
        eprintln!("[kn9t] cannot reach server: {e}");
        std::process::exit(1);
    });
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).unwrap_or(0);
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Null)
}

// ── Global attach (keeps server alive) ────────────────────────────────────────

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Spawn a background thread that connects to /attach and keeps the connection
/// open until the returned flag is set. This keeps the server alive.
fn spawn_global_attach(host: &str, auth: &str) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let host = host.to_string();
    let auth = auth.to_string();
    
    thread::spawn(move || {
        let request = format!(
            "GET /attach HTTP/1.1\r\nHost: {host}\r\nAuthorization: {auth}\r\nConnection: keep-alive\r\n\r\n"
        );
        let mut stream = match TcpStream::connect(&host) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.flush();
        
        // Just keep reading (server sends pings every 30s) until stopped.
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            if line.is_err() {
                break;
            }
        }
    });
    
    stop
}

// ── SSE event loops ───────────────────────────────────────────────────────────

/// One-shot: drain rx until TurnEnded, then return. Consumes rx.
fn stream_events(rx: mpsc::Receiver<String>) {
    stream_events_until_turn_end(&rx);
}

/// REPL: drain rx until TurnEnded, then return (rx reused next turn).
fn stream_events_until_turn_end(rx: &mpsc::Receiver<String>) {
    // call_id → tool name (populated on ToolStarted, used on ToolFinished + MessageAppended)
    let mut active_tools: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // (msg_id, idx) → accumulated args JSON (built from ToolArgsDelta before ToolStarted)
    let mut args_acc: std::collections::HashMap<(String, u32), String> =
        std::collections::HashMap::new();
    // Whether the last character printed to stdout was model text (need a newline before tool output).
    let mut in_text = false;

    loop {
        let raw = match rx.recv() {
            Ok(r) => r,
            Err(_) => break, // sender dropped — SSE ended
        };
        let line = raw.trim();
        if !line.starts_with("data:") { continue; }
        let data = line["data:".len()..].trim();
        if data.is_empty() || data == "ping" { continue; }
        let ev: Value = match serde_json::from_str(data) {
            Ok(v) => v, Err(_) => continue,
        };

        match ev["kind"].as_str().unwrap_or("") {
            "text_delta" => {
                if let Some(text) = ev["delta"].as_str() {
                    in_text = true;
                    print!("{text}");
                    std::io::stdout().flush().ok();
                }
            }

            "tool_args_delta" => {
                let msg_id = ev["msg_id"].as_str().unwrap_or("").to_string();
                let idx    = ev["idx"].as_u64().unwrap_or(0) as u32;
                let delta  = ev["delta"].as_str().unwrap_or("");
                args_acc.entry((msg_id, idx)).or_default().push_str(delta);
            }

            "tool_started" => {
                let call_id  = ev["call_id"].as_str().unwrap_or("?").to_string();
                let name     = ev["name"].as_str().unwrap_or("?").to_string();
                if in_text { eprintln!(); in_text = false; }
                eprintln!("\n[tool] ▶ {name}");
                // Use the last accumulated args block (args arrive in call order).
                let args_json = args_acc.values().last().cloned().unwrap_or_default();
                display_args(&name, &args_json);
                active_tools.insert(call_id, name);
                args_acc.clear();
            }

            "tool_progress" => {
                let note = ev["note"].as_str().unwrap_or("");
                eprintln!("  {note}");
            }

            "tool_finished" => {
                let is_error = ev["is_error"].as_bool().unwrap_or(false);
                let call_id  = ev["call_id"].as_str().unwrap_or("?");
                if is_error {
                    let name = active_tools.get(call_id).map(|s| s.as_str()).unwrap_or("?");
                    eprintln!("[tool] ✗ {name} failed");
                }
                // Successful result text is shown via MessageAppended below.
            }

            "message_appended" => {
                let msg  = &ev["msg"];
                let role = msg["role"].as_str().unwrap_or("");
                // Tool results arrive as role="tool" messages.
                if role == "tool" {
                    if let Some(blocks) = msg["content"].as_array() {
                        for block in blocks {
                            let call_id  = block["id"].as_str().unwrap_or("?");
                            let is_error = block["is_error"].as_bool().unwrap_or(false);
                            let name     = active_tools.get(call_id)
                                .map(|s| s.as_str()).unwrap_or("?");
                            display_result(name, &block["content"], is_error);
                        }
                    }
                }
            }

            "turn_ended" => {
                let stop = ev["stop"].as_str().unwrap_or("stop");
                if in_text { println!(); }
                eprintln!("---");
                eprintln!("[kn9t chat] stop: {stop}");
                break;
            }

            "approval_request" => {
                let req_id   = ev["id"].as_str().unwrap_or("?").to_string();
                let tool     = ev["tool"].as_str().unwrap_or("?");
                let args_val = &ev["args"];
                if in_text { eprintln!(); in_text = false; }
                // host/auth not in scope here — pass via closure over captured strings.
                // We pause SSE consumption; the server blocks until we POST /approve.
                // approval_host/auth are not available in this fn signature, so we
                // use a thread-local trick: stash them in a module-level OnceLock.
                // Simpler: pass them through stream_events_until_turn_end as params.
                // For now we call the standalone approval fn with the host/auth that
                // are stored in APPROVAL_CTX set before the loop.
                let decision = approval_selector(tool, args_val);
                // POST /approve — we need host+auth; stored in thread-local.
                APPROVAL_CTX.with(|ctx| {
                    if let Some((host, auth)) = ctx.borrow().as_ref() {
                        post_approval(host, auth, &req_id, decision);
                    }
                });
            }

            // Silently ignored — not relevant for CLI display.
            "TurnStarted" | "UsageRecorded" | "ThinkingDelta"
            | "ModelChanged" | "SessionForked" | "Compacted" => {}
            _ => {}
        }
    }
}

// ── Tool display ──────────────────────────────────────────────────────────────

/// Pretty-print tool input args to stderr.
fn display_args(name: &str, args_json: &str) {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    match name {
        "bash" => {
            let cmd = args["cmd"].as_str().unwrap_or(args_json);
            eprintln!("  $ {cmd}");
        }
        "read" => {
            let path = args["path"].as_str().unwrap_or("?");
            eprintln!("  {path}");
        }
        "write" => {
            let path    = args["path"].as_str().unwrap_or("?");
            let content = args["content"].as_str().unwrap_or("");
            eprintln!("  {path}");
            for line in content.lines() {
                eprintln!("  + {line}");
            }
        }
        "edit" => {
            let path    = args["filePath"].as_str().unwrap_or("?");
            let old_str = args["oldString"].as_str().unwrap_or("");
            let new_str = args["newString"].as_str().unwrap_or("");
            eprintln!("  {path}");
            unified_diff(old_str, new_str);
        }
        _ => {
            if let Ok(pretty) = serde_json::to_string_pretty(&args) {
                for line in pretty.lines() {
                    eprintln!("  {line}");
                }
            }
        }
    }
}

/// Print a minimal unified diff (removed lines then added lines).
fn unified_diff(old: &str, new: &str) {
    for line in old.lines() { eprintln!("  - {line}"); }
    for line in new.lines() { eprintln!("  + {line}"); }
}

// ── Session helpers ───────────────────────────────────────────────────────────

fn create_session(host: &str, auth: &str, provider: &str, model: &str) -> String {
    let resp = post_json(host, auth, "/session",
        &json!({ "model": { "provider": provider, "id": model } }), None);
    resp["id"].as_str().unwrap_or_else(|| {
        eprintln!("[kn9t chat] create session failed: {resp}");
        std::process::exit(1);
    }).to_string()
}

/// Pick the session with the highest head_seq (most recently active).
pub fn resolve_latest_session(host: &str, auth: &str) -> String {
    let resp = get_json(host, auth, "/session");
    let owned;
    let arr: &Vec<Value> = if resp.is_array() {
        owned = resp.as_array().unwrap().to_vec();
        &owned
    } else if let Some(a) = resp["sessions"].as_array() {
        owned = a.to_vec();
        &owned
    } else {
        eprintln!("[kn9t] error: cannot list sessions");
        std::process::exit(1);
    };
    arr.iter()
        .max_by_key(|s| (
            s["head_seq"].as_i64().unwrap_or(0),
            s["meta"]["created_at"].as_i64()
                .or_else(|| s["created_at"].as_i64())
                .unwrap_or(0),
        ))
        .and_then(|s| s["id"].as_str().or_else(|| s["meta"]["id"].as_str()))
        .unwrap_or_else(|| {
            eprintln!("[kn9t] error: no sessions found");
            std::process::exit(1);
        })
        .to_string()
}

/// Acquire the write lease with exponential backoff (max ~3 s total).
fn acquire_lease_with_backoff(host: &str, auth: &str, session_id: &str) -> String {
    let path    = format!("/session/{session_id}/lease");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut wait = Duration::from_millis(100);
    loop {
        let resp = post_json(host, auth, &path, &json!({}), None);
        if let Some(lease) = resp["lease"].as_str() {
            return lease.to_string();
        }
        if Instant::now() >= deadline {
            eprintln!("[kn9t] error: could not acquire lease: {resp}");
            std::process::exit(1);
        }
        thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_secs(2));
    }
}

fn release_lease(host: &str, auth: &str, session_id: &str, lease: &str) {
    // DELETE /session/{id}/lease — best effort, ignore errors.
    let path    = format!("/session/{session_id}/lease");
    let request = format!(
        "DELETE {path} HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\
         X-Lease: {lease}\r\n\r\n"
    );
    if let Ok(mut s) = TcpStream::connect(host) {
        let _ = s.write_all(request.as_bytes());
    }
}

/// Open an SSE subscription from `from_seq`. Returns the line receiver.
/// The background thread holds the TcpStream open — server counts this as an
/// attached client. When the receiver is dropped (process exits or REPL Ctrl-D),
/// the thread exits, TcpStream closes, server calls `client_detached`.
fn subscribe_sse(host: &str, auth: &str, session_id: &str, from_seq: u64)
    -> mpsc::Receiver<String>
{
    let (tx, rx) = mpsc::sync_channel::<String>(1024);
    let host2    = host.to_string();
    let auth2    = auth.to_string();
    let sid      = session_id.to_string();

    thread::spawn(move || {
        let path    = format!("/session/{sid}/events?from={from_seq}");
        let request = format!(
            "GET {path} HTTP/1.0\r\nHost: {host2}\r\n\
             Authorization: {auth2}\r\nAccept: text/event-stream\r\n\r\n"
        );
        let stream = match TcpStream::connect(&host2) {
            Ok(s) => s,
            Err(e) => { eprintln!("[kn9t] SSE connect: {e}"); return; }
        };
        let mut w = stream.try_clone().unwrap();
        let _ = w.write_all(request.as_bytes());
        let _ = w.flush();
        drop(w);
        // Blocks reading until process exits or tx.send fails (rx dropped).
        // TcpStream drops when this thread exits → server sees EOF → client_detached.
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(l) => { if tx.send(l).is_err() { break; } }
                Err(_) => break,
            }
        }
    });

    rx
}

// ── REPL loop ─────────────────────────────────────────────────────────────────

pub fn repl_loop(session_id: &str, host: &str, auth: &str) {
    set_approval_ctx(host, auth);
    // Subscribe SSE from seq=0 to see full history + live updates.
    let rx = subscribe_sse(host, auth, session_id, 0);
    thread::sleep(Duration::from_millis(50));

    // Drain any existing history silently (until we hit a quiet period).
    // Then enter the interactive loop.
    let stdin = std::io::stdin();

    loop {
        // Print prompt.
        eprint!("> ");
        let _ = std::io::stderr().flush();

        // Read a line from stdin (blocking).
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                // Ctrl-D / EOF — clean exit.
                eprintln!();
                break; // rx dropped when repl_loop returns → SSE thread exits → client_detached
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[kn9t] stdin error: {e}");
                break;
            }
        }

        let prompt = line.trim().to_string();
        if prompt.is_empty() { continue; }

        // Acquire lease (with backoff — another client may be writing).
        let lease = acquire_lease_with_backoff(host, auth, session_id);

        // Send prompt.
        post_json(host, auth,
            &format!("/session/{session_id}/prompt"),
            &json!({ "text": prompt }), Some(&lease));

        // Stream events until TurnEnded.
        stream_events_until_turn_end(&rx);

        // Release lease.
        release_lease(host, auth, session_id, &lease);
    }
}

/// Pretty-print tool result content to stderr (capped at 40 lines).
fn display_result(name: &str, content: &Value, is_error: bool) {
    let label = if is_error { "✗" } else { "✓" };
    eprintln!("[tool] {label} {name}");
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if let Some(text) = block["text"].as_str() {
                let lines: Vec<&str> = text.lines().collect();
                for line in lines.iter().take(40) {
                    eprintln!("  {line}");
                }
                if lines.len() > 40 {
                    eprintln!("  … ({} more lines)", lines.len() - 40);
                }
            }
        }
    }
}
