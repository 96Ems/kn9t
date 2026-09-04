//! `kn9t tools` — GET /tools.

pub fn run(port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");
    let resp = crate::http::get_json(&host, &auth, "/tools", "tools");
    if resp.get("error").is_some() {
        eprintln!("[kn9t tools] error: {resp}");
        std::process::exit(1);
    }
    let tools = resp.get("tools").and_then(|v| v.as_array());
    match tools {
        Some(arr) if !arr.is_empty() => {
            println!("{:<22}  {}", "TOOL", "DESCRIPTION");
            println!("{}", "-".repeat(72));
            for t in arr {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let hidden = t.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
                let tag = if hidden { " (hidden)" } else { "" };
                // Truncate desc for table.
                let d = if desc.len() > 52 {
                    format!("{}…", &desc[..51])
                } else {
                    desc.to_string()
                };
                println!("{name:<22}  {d}{tag}");
            }
        }
        _ => {
            println!("No tools registered.");
        }
    }
}
