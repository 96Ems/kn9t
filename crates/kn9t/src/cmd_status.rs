//! `kn9t status` — server health (GET /health, no lease).
//!
//! Shows whether the server is up, how long idle, how many SSE clients
//! and running turns. Useful as `kn9t status` did not exist before and
//! previously fell through to the TUI.

use serde_json::Value;

pub fn run(port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");

    let health = crate::http::get_json(&host, &auth, "/health", "status");
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
    let models = crate::http::get_json(&host, &auth, "/models", "status");
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

    let sessions = crate::http::get_json(&host, &auth, "/session", "status");
    let count = sessions.get("sessions")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .or_else(|| sessions.as_array().map(|a| a.len()))
        .unwrap_or(0);
    println!("  sessions: {count}");
}

