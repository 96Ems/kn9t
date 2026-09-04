//! `kn9t cost` — GET /cost (+ GET /budget summary).

pub fn run(args: &[String], port: u16, server_token: &str) {
    let host = format!("127.0.0.1:{port}");
    let auth = format!("Bearer {server_token}");

    // Parse --since and --group-by
    let mut since: i64 = 0;
    let mut group_by = "model".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" if i + 1 < args.len() => {
                since = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("[kn9t cost] --since expects integer ms epoch");
                    std::process::exit(2);
                });
                i += 2;
            }
            "--group-by" if i + 1 < args.len() => {
                let g = args[i + 1].as_str();
                if !matches!(g, "model" | "kind" | "session") {
                    eprintln!("[kn9t cost] --group-by must be model|kind|session");
                    std::process::exit(2);
                }
                group_by = g.to_string();
                i += 2;
            }
            s if s.starts_with('-') => {
                eprintln!("[kn9t cost] unknown option '{s}'");
                eprintln!("Usage: kn9t cost [--since MS] [--group-by model|kind|session]");
                std::process::exit(2);
            }
            other => {
                eprintln!("[kn9t cost] unexpected arg '{other}'");
                std::process::exit(2);
            }
        }
    }

    let path = format!("/cost?since={since}&group_by={group_by}");
    let resp = crate::http::get_json(&host, &auth, &path, "cost");
    if resp.get("error").is_some() {
        eprintln!("[kn9t cost] error: {resp}");
        std::process::exit(1);
    }

    let total = resp
        .get("total_cost_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let groups = resp.get("groups").and_then(|v| v.as_array());

    println!("cost — since={since} group_by={group_by}");
    println!("total: ${total:.4}");
    if let Some(groups) = groups {
        if groups.is_empty() {
            println!("(no usage rows)");
        } else {
            println!();
            println!(
                "{:<32}  {:>10}  {:>8}  {:>8}",
                "GROUP", "COST_USD", "TOK_IN", "TOK_OUT"
            );
            println!("{}", "-".repeat(64));
            for g in groups {
                let group = g.get("group").and_then(|v| v.as_str()).unwrap_or("?");
                let cost = g.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let tin = g.get("tokens_in").and_then(|v| v.as_i64()).unwrap_or(0);
                let tout = g.get("tokens_out").and_then(|v| v.as_i64()).unwrap_or(0);
                println!("{group:<32}  ${cost:>9.4}  {tin:>8}  {tout:>8}");
            }
        }
    }

    // Budget summary (best-effort).
    let budget = crate::http::get_json(&host, &auth, "/budget", "cost");
    if budget.get("error").is_none() {
        let local = budget
            .get("local_estimate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        println!();
        println!("budget: local_estimate=${local:.4}");
        if let Some(pr) = budget.get("provider_reported").and_then(|v| v.as_f64()) {
            println!(
                "        provider_reported=${pr:.4}  (drift ${:.4})",
                pr - local
            );
        } else {
            println!("        provider_reported: (unavailable)");
        }
    }
}
