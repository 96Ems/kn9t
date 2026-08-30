//! `kn9t` — launcher: ensures the server is running, then starts the TUI.
//!
//! Usage:
//!   kn9t [model-id]               — start TUI (default)
//!   kn9t chat [--model p/id] <prompt>  — single-turn or REPL via the server
//!   kn9t sessions                 — list sessions
//!   kn9t history [session-id]     — print transcript
//!   kn9t attach  [session-id]     — attach to a running session (REPL)
//!
//! GI-1: external crates only (serde_json). No workspace crate deps.
//! GI-5: no async.

mod bootstrap;
mod chat;
mod cmd_sessions;
mod cmd_history;
mod cmd_stop;

use std::env;
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// ── Paths ─────────────────────────────────────────────────────────────────────

fn kn9t_home() -> PathBuf { bootstrap::kn9t_home_path() }

fn token_path() -> PathBuf { kn9t_home().join("token") }
fn port_path()  -> PathBuf { kn9t_home().join("port") }

// ── Server liveness ───────────────────────────────────────────────────────────

fn port_alive(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(300),
    ).is_ok()
}

fn read_port() -> Option<u16> {
    fs::read_to_string(port_path()).ok()?.trim().parse().ok()
}

fn read_token() -> Option<String> {
    fs::read_to_string(token_path()).ok().map(|s| s.trim().to_owned())
}

// ── Server spawn ──────────────────────────────────────────────────────────────

fn sibling_bin(name: &str) -> PathBuf {
    let mut p = env::current_exe().unwrap_or_else(|_| PathBuf::from(name));
    p.pop();
    p.push(name);
    if cfg!(windows) { p.set_extension("exe"); }
    p
}

fn spawn_server() -> std::io::Result<()> {
    let bin  = sibling_bin("kn9t-server");
    let log  = kn9t_home().join("server.log");
    let file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&log)
        .unwrap_or_else(|_| {
            #[cfg(windows)]
            return std::fs::File::open("NUL").unwrap();
            #[cfg(not(windows))]
            return std::fs::File::open("/dev/null").unwrap();
        });
    let file2 = file.try_clone()?;
    Command::new(&bin)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(file2)
        .spawn()
        .map_err(|e| std::io::Error::new(
            e.kind(),
            format!("cannot launch {}: {e}\nHint: run `cargo build -p kn9t-server` first",
                bin.display()),
        ))?;
    Ok(())
}

fn wait_for_server(timeout: Duration) -> Option<u16> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(port) = read_port() {
            if port_alive(port) { return Some(port); }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

// ── Server ensure ─────────────────────────────────────────────────────────────

fn ensure_server() -> (u16, String) {
    // Bootstrap: create ~/.kn9t/ + config template + token on first run.
    bootstrap::ensure_home(&kn9t_home());
    let port = match read_port() {
        Some(p) if port_alive(p) => {
            eprintln!("[kn9t] reusing server on port {p}");
            p
        }
        _ => {
            eprintln!("[kn9t] starting server…");
            if let Err(e) = spawn_server() {
                eprintln!("[kn9t] error: {e}");
                std::process::exit(1);
            }
            match wait_for_server(Duration::from_secs(15)) {
                Some(p) => { eprintln!("[kn9t] server ready on port {p}"); p }
                None => {
                    eprintln!("[kn9t] error: server did not start within 15s");
                    std::process::exit(1);
                }
            }
        }
    };
    let token = match read_token() {
        Some(t) => t,
        None => {
            eprintln!("[kn9t] error: cannot read {}", token_path().display());
            std::process::exit(1);
        }
    };
    (port, token)
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    // Handle subcommands.
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        match cmd {
            "chat" => {
                let (port, token) = ensure_server();
                chat::run(&args[2..], port, &token);
                return;
            }
            "sessions" => {
                let (port, token) = ensure_server();
                cmd_sessions::run(port, &token);
                return;
            }
            "history" => {
                let (port, token) = ensure_server();
                cmd_history::run(&args[2..], port, &token);
                return;
            }
            "attach" => {
                let (port, token) = ensure_server();
                chat::attach(&args[2..], port, &token);
                return;
            }
            "stop" => {
                cmd_stop::run();
                return;
            }
            _ => {} // Unknown arg — fall through to TUI
        }
    }

    // Default: launch TUI.
    // Model selection is handled in TUI via GET /models (auto-discovery from providers).
    let (port, token) = ensure_server();

    let tui_bin = sibling_bin("kn9t-tui");
    let status = Command::new(&tui_bin)
        .env("KN9T_URL",   format!("http://127.0.0.1:{port}"))
        .env("KN9T_TOKEN", &token)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("[kn9t] cannot launch {}: {e}\nHint: run `cargo build -p kn9t-tui` first",
                tui_bin.display());
            std::process::exit(1);
        });

    std::process::exit(status.code().unwrap_or(0));
}
