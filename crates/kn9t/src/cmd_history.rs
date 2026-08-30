//! `kn9t history [session-id]` — print the full transcript of a session.
//! If no id is given, uses the session with the highest head_seq (most active).

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

pub fn run(args: &[String], port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");

    // Resolve session id.
    let session_id = if let Some(id) = args.first().filter(|s| !s.starts_with('-')) {
        id.clone()
    } else {
        // Pick the most recently active session.
        match latest_session(&host, &auth) {
            Some(id) => id,
            None => {
                eprintln!("[kn9t history] no sessions found");
                std::process::exit(1);
            }
        }
    };

    eprintln!("[kn9t history] session: {session_id}");

    let session = get_session(&host, &auth, &session_id);
    // Server returns transcript under "transcript" key.
    let transcript = if session["transcript"].is_array() {
        &session["transcript"]
    } else if session["messages"].is_array() {
        &session["messages"]
    } else {
        eprintln!("[kn9t history] no transcript in session response");
        return;
    };

    for msg in transcript.as_array().unwrap() {
        print_message(msg);
    }
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

fn get_json(host: &str, auth: &str, path: &str) -> Value {
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n"
    );
    let mut stream = TcpStream::connect(host).unwrap_or_else(|e| {
        eprintln!("[kn9t history] cannot reach server: {e}");
        std::process::exit(1);
    });
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).unwrap_or(0);
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Null)
}

fn get_session(host: &str, auth: &str, id: &str) -> Value {
    get_json(host, auth, &format!("/session/{id}"))
}

fn latest_session(host: &str, auth: &str) -> Option<String> {
    let resp = get_json(host, auth, "/session");
    // Server wraps in { "sessions": [...] } or returns array directly.
    let arr = if resp.is_array() {
        resp.as_array()?.to_vec()
    } else {
        resp["sessions"].as_array()?.to_vec()
    };
    arr.iter()
        .max_by_key(|s| {
            (
                s["head_seq"].as_i64().unwrap_or(0),
                s["meta"]["created_at"].as_i64()
                    .or_else(|| s["created_at"].as_i64())
                    .unwrap_or(0),
            )
        })
        .and_then(|s| s["id"].as_str().or_else(|| s["meta"]["id"].as_str()))
        .map(|s| s.to_string())
}

// ── Transcript printer ────────────────────────────────────────────────────────

fn print_message(msg: &Value) {
    let role = msg["role"].as_str().unwrap_or("?");
    match role {
        "user" => {
            println!("\n\x1b[1;34m[user]\x1b[0m");
            print_content_blocks(&msg["content"]);
        }
        "assistant" => {
            println!("\n\x1b[1;32m[assistant]\x1b[0m");
            print_content_blocks(&msg["content"]);
        }
        "tool" => {
            if let Some(blocks) = msg["content"].as_array() {
                for block in blocks {
                    let call_id  = block["id"].as_str().unwrap_or("?");
                    let is_error = block["is_error"].as_bool().unwrap_or(false);
                    let label    = if is_error { "\x1b[31m✗\x1b[0m" } else { "\x1b[32m✓\x1b[0m" };
                    println!("\n\x1b[1;33m[tool result]\x1b[0m {label} (call {call_id})");
                    print_content_blocks(&block["content"]);
                }
            }
        }
        other => {
            println!("\n[{other}]");
            print_content_blocks(&msg["content"]);
        }
    }
}

fn print_content_blocks(content: &Value) {
    match content {
        Value::String(s) => println!("{s}"),
        Value::Array(blocks) => {
            for block in blocks {
                match block["type"].as_str().unwrap_or("") {
                    "text" => {
                        if let Some(text) = block["text"].as_str() {
                            println!("{text}");
                        }
                    }
                    "tool_use" => {
                        let name = block["name"].as_str().unwrap_or("?");
                        let id   = block["id"].as_str().unwrap_or("?");
                        println!("\x1b[33m[tool call]\x1b[0m {name} ({id})");
                        if let Ok(pretty) = serde_json::to_string_pretty(&block["input"]) {
                            for line in pretty.lines().take(40) {
                                println!("  {line}");
                            }
                        }
                    }
                    "thinking" => {
                        // Collapse thinking blocks to a summary line.
                        println!("\x1b[2m[thinking …]\x1b[0m");
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
