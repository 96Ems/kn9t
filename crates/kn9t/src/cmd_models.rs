//! `kn9t models` — GET /models.

use serde_json::Value;

pub fn run(port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");
    let resp = crate::http::get_json(&host, &auth, "/models", "models");
    if resp.get("error").is_some() {
        eprintln!("[kn9t models] error: {resp}");
        std::process::exit(1);
    }
    let models = resp.get("models").and_then(|v| v.as_array());
    let models = match models {
        Some(a) if !a.is_empty() => a,
        _ => {
            println!("No models configured — edit ~/.kn9t/config.toml");
            return;
        }
    };
    // Header
    println!(
        "{:<16}  {:<28}  {:>8}  {:>8}  {}",
        "PROVIDER", "MODEL", "CTX", "MAX_OUT", "DEFAULT"
    );
    println!("{}", "-".repeat(80));
    for m in models {
        let provider = m.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let ctx = m
            .get("ctx_window")
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let max_out = m
            .get("max_out")
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        let def = if m.get("is_default") == Some(&Value::Bool(true)) {
            "yes"
        } else {
            ""
        };
        println!("{provider:<16}  {id:<28}  {ctx:>8}  {max_out:>8}  {def}");
    }
    // Show auth line if present.
    if let Some(auth) = resp.get("auth") {
        if auth.get("authenticated") == Some(&Value::Bool(false)) {
            if let Some(p) = auth.get("provider").and_then(|v| v.as_str()) {
                eprintln!("\nwarning: provider '{p}' not authenticated (missing api_key/env)");
            }
        }
    }
}
