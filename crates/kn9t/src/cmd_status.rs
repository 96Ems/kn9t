//! `kn9t status` — server health (GET /health, no lease).
//!
//! Shows whether the server is up, how long idle, how many SSE clients
//! and running turns. Useful as `kn9t status` did not exist before and
//! previously fell through to the TUI.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use serde_json::Value;

pub fn run(port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");

    let health = get_json(&host, &auth, "/health");
    if health.is_null() {
        eprintln!("[kn9t status] no response from server on port {port}");
        std::process::exit(1);
    }
    if health.get("error").is_some() {
        eprintln!("[kn9t status] error: {health}");
        std::process::exit(1);
    }

    let ok = health["ok"].as_bool().unwrap_or(false);
    let idle_secs = health["idle_secs"].as_u64().unwrap_or(0);
    let attached = health["attached_clients"].as_u64().unwrap_or(0);
    let running = health["running_turns"].as_u64().unwrap_or(0);

    println!("kn9t server — port {port}");
    println!("  ok:               {}", if ok { "yes" } else { "no" });
    println!("  idle:             {}s", idle_secs);
    println!("  attached clients: {attached}");
    println!("  running turns:    {running}");
    println!();

    // Also show a one-line summary from /models and /session so `status`
    // is actually useful without running three commands.
    let models = get_json(&host, &auth, "/models");
    if let Some(arr) = models.get("models").and_then(|v| v.as_array()) {
        println!("  models: {} configured", arr.len());
        for m in arr.iter().take(5) {
            let p = m.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let def = if m.get("is_default") == Some(&Value::Bool(true)) { " (default)" } else { "" };
            println!("    - {p}/{id}{def}");
        }
        if arr.len() > 5 {
            println!("    … ({} more)", arr.len() - 5);
        }
    }

    let sessions = get_json(&host, &auth, "/session");
    let count = sessions.get("sessions")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .or_else(|| sessions.as_array().map(|a| a.len()))
        .unwrap_or(0);
    println!("  sessions: {count}");
}

fn get_json(host: &str, auth: &str, path: &str) -> Value {
    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n");
    let mut stream = match TcpStream::connect(host) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[kn9t status] cannot reach server: {e}");
            std::process::exit(1);
        }
    };
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).unwrap_or(0);
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Null)
}
