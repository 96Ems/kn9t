//! xtask — schema-first code generation for kn9t (ADR-0005, Phase 2).
//!
//! `cargo run -p xtask -- generate` reads `schema/http.json` + `schema/plugin.json`
//! and regenerates, from the schema as the single source of truth:
//!
//! | output | notes |
//! |---|---|
//! | `crates/kn9t-server/src/api.rs` | typed request structs with `#[serde(deny_unknown_fields)]` — a mistyped field is a **400**, not a silent ignore |
//! | `crates/kn9t-tui/src/write` | wire mirrors, GI-6-clean (serde only, no kn9t-* dep) |
//! | `API.md` | human-readable contract docs — never hand-edited again |
//! | `schema/generated/go_types.go` | Go client stubs (for `plugins/kn9t-agents-md`) |
//! | `schema/generated/python_types.py` | Python client stubs (for `plugins/kn9t-mcp`) |
//!
//! The generator is **idempotent**: consecutive runs produce byte-identical output.
//! `scripts/check-schema.sh` (installed in the pre-commit hook) fails on any drift
//! between the schema and the committed generated files.
//!
//! This crate is a **dev/tool** dependency (DESIGN §15 budget intact: no new *runtime*
//! deps anywhere). `preserve_order` on its serde_json is a generator-only convenience.

mod gen_markdown;
mod gen_server;
mod gen_stubs;
mod gen_wire;
mod schema;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("generate") => match generate() {
            Ok(()) => {
                println!("xtask generate: schema -> api.rs, wire.rs, API.md, Go/Python stubs (idempotent)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("xtask generate failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("--check") => match check() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("xtask: unknown subcommand '{other}' (expected 'generate')");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("usage: cargo run -p xtask -- generate");
            ExitCode::FAILURE
        }
    }
}

/// Locate the workspace root (directory containing the workspace `Cargo.toml`).
fn repo_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join("Cargo.toml").is_file() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    PathBuf::from(".")
}

/// Load and structurally validate both schemas.
fn load() -> Result<(serde_json::Value, serde_json::Value), String> {
    let root = repo_root();
    let http: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schema/http.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("schema/http.json: {e}"))?;
    let plugin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schema/plugin.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("schema/plugin.json: {e}"))?;

    if http.get("routes").and_then(|r| r.as_array()).is_none() {
        return Err("schema/http.json: missing 'routes' array".into());
    }
    if http.get("sse").is_none() {
        return Err("schema/http.json: missing 'sse' section".into());
    }
    if plugin.get("host_to_plugin").is_none() || plugin.get("plugin_to_host").is_none() {
        return Err("schema/plugin.json: missing 'host_to_plugin' / 'plugin_to_host'".into());
    }
    Ok((http, plugin))
}

/// Entry for `generate`: load, validate, regenerate every output in place.
fn generate() -> Result<(), String> {
    let (http, plugin) = load()?;
    let root = repo_root();

    gen_server::write(&root, &http)?;
    gen_wire::write(&root, &http)?;
    gen_markdown::write(&root, &http, &plugin)?;
    gen_stubs::write(&root, &http, &plugin)?;
    Ok(())
}

/// Entry for `--check`: verify every committed output is byte-identical to what the
/// generator would produce right now (used by scripts/check-schema.sh in pre-commit).
fn check() -> Result<(), String> {
    let (http, plugin) = load()?;
    let root = repo_root();
    let mut failures = 0;

    let mut expected: Vec<(PathBuf, String)> = Vec::new();
    expected.push((root.join("crates/kn9t-server/src/api.rs"), gen_server::generate(&http)?));
    expected.push((root.join("crates/kn9t-tui/src/wire.rs"), gen_wire::generate(&http)?));
    expected.push((root.join("API.md"), gen_markdown::generate(&http, &plugin)?));
    let (go, py) = gen_stubs::generate(&http, &plugin)?;
    expected.push((root.join("schema/generated/go_types.go"), go));
    expected.push((root.join("schema/generated/python_types.py"), py));

    for (path, want) in &expected {
        match std::fs::read_to_string(path) {
            Ok(have) if &have == want => {}
            Ok(_) => {
                eprintln!("DRIFT: {} differs from schema-generated output", path.display());
                failures += 1;
            }
            Err(_) => {
                eprintln!(
                    "DRIFT: {} missing (run `cargo run -p xtask -- generate`)",
                    path.display()
                );
                failures += 1;
            }
        }
    }
    if failures > 0 {
        Err(format!("{failures} generated file(s) drifted from the schema"))
    } else {
        println!("xtask --check: all generated outputs match the schema");
        Ok(())
    }
}