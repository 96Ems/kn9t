//! `kn9t sessions` — list all sessions from the server.

use serde_json::Value;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;

pub fn run(port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");
    let sessions = get_sessions(&host, &auth);

    // Server wraps list in { "sessions": [...] }.
    let raw_list = if sessions.is_array() {
        sessions.clone()
    } else {
        sessions["sessions"].clone()
    };
    let list = match raw_list.as_array() {
        Some(a) => a,
        None => {
            eprintln!("[kn9t sessions] unexpected response: {sessions}");
            std::process::exit(1);
        }
    };

    if list.is_empty() {
        println!("No sessions.");
        return;
    }

    // Column widths
    let id_w = 26;
    let name_w = 36;
    let seq_w = 6;
    println!(
        "{:<id_w$}  {:<name_w$}  {:>seq_w$}  {}",
        "ID", "NAME", "SEQ", "CWD"
    );
    println!("{}", "-".repeat(id_w + 2 + name_w + 2 + seq_w + 2 + 30));

    // Sort by head_seq descending (most active first).
    let mut sorted: Vec<&Value> = list.iter().collect();
    sorted.sort_by(|a, b| {
        let sa = a["head_seq"].as_i64().unwrap_or(0);
        let sb = b["head_seq"].as_i64().unwrap_or(0);
        sb.cmp(&sa)
    });

    for s in sorted {
        let id = s["id"].as_str().unwrap_or("?");
        let name = s["name"].as_str().unwrap_or("(unnamed)");
        let seq = s["head_seq"].as_i64().unwrap_or(0);
        let cwd = s["cwd"].as_str().unwrap_or("");
        println!(
            "{:<id_w$}  {:<name_w$}  {:>seq_w$}  {}",
            truncate(id, id_w),
            truncate(name, name_w),
            seq,
            truncate(cwd, 50),
        );
    }
}

// ── HTTP GET /session ─────────────────────────────────────────────────────────

fn get_sessions(host: &str, auth: &str) -> Value {
    let request = format!("GET /session HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\r\n");
    let mut stream = TcpStream::connect(host).unwrap_or_else(|e| {
        eprintln!("[kn9t sessions] cannot reach server: {e}");
        std::process::exit(1);
    });
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = String::new();
    BufReader::new(stream)
        .read_to_string(&mut resp)
        .unwrap_or(0);
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    serde_json::from_str(&resp[body_start..]).unwrap_or(Value::Array(vec![]))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", &s[..max.saturating_sub(1)])
}
