//! `kn9t` — launcher: ensures the server is running, then starts the TUI.
//!
//! Usage:
//!   kn9t [model-id]               — start TUI (default)
//!   kn9t chat [--model p/id] <prompt>  — single-turn or REPL via the server
//!   kn9t sessions                 — list sessions
//!   kn9t history [session-id]     — print transcript
//!   kn9t attach  [session-id]     — attach to a running session (REPL)
//!   kn9t status                   — server health (no lease)
//!   kn9t models                   — list configured models
//!   kn9t cost                     — cost analytics
//!   kn9t tools                    — list registered tools
//!   kn9t stop                     — stop server
//!
//! GI-1: external crates only (serde_json). No workspace crate deps.
//! GI-5: no async.

mod bootstrap;
mod chat;
mod cmd_cost;
mod cmd_history;
mod cmd_install_plugins;
mod cmd_models;
mod cmd_sessions;
mod cmd_status;
mod cmd_stop;
mod cmd_tools;
mod http;

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

fn print_help() {
    // Keep in sync with README.md CLI section.
    let v = env!("CARGO_PKG_VERSION");
    println!("kn9t {v} — minimal coding agent (Rust, OS threads, no async)");
    println!();
    println!("Usage:");
    println!("  kn9t                              Launch TUI (default, auto-starts server)");
    println!("  kn9t chat [OPTIONS] [PROMPT]      One-shot or REPL (no PROMPT → REPL)");
    println!("    --model <provider/id>            Model (provider:id or provider/id)");
    println!("    --continue                       REPL on latest session");
    println!("    --json | --format json           JSONL on stdout for autonomous parsing");
    println!("    -h, --help                       Show chat help");
    println!("  kn9t sessions                     List sessions (GET /session)");
    println!("  kn9t history [SESSION_ID]         Show transcript (GET /session/{{id}})");
    println!("  kn9t attach [SESSION_ID]          Attach REPL to session (SSE + lease)");
    println!("  kn9t status                       Server health (GET /health)");
    println!("  kn9t models                       List configured models (GET /models)");
    println!("  kn9t cost [--since MS] [--group-by model|kind|session]  Cost analytics (GET /cost)");
    println!("  kn9t tools                        List registered tools (GET /tools)");
    println!("  kn9t stop                         Graceful shutdown (POST /stop)");
    println!("  kn9t install-plugins [OPTIONS]    Install project plugins to ~/.kn9t/");
    println!("    --from <path>                    Project root (default: cwd)");
    println!("    --no-build                       Skip auto-build; copy existing only");
    println!("    --force                          Overwrite existing plugins");
    println!("    --rebuild                        Force rebuild (implies --force)");
    println!("  kn9t help | --help | -h           Show this help");
    println!("  kn9t --version | -V | version     Show version");
    println!();
    println!("Examples:");
    println!("  kn9t                              # TUI");
    println!("  kn9t chat \"fix the bug\"         # one-shot (text)");
    println!("  kn9t chat --json \"fix the bug\"   # one-shot (JSONL, pipe to jq)");
    println!("  kn9t chat --model opencode-go/deepseek-v4-flash \"hi\"");
    println!("  kn9t chat                         # REPL, Ctrl-D to exit");
    println!("  kn9t chat --continue              # resume latest session");
    println!("  kn9t sessions                     # list");
    println!("  kn9t history                      # latest transcript");
    println!("  kn9t status                       # health + counters");
    println!();
    println!("Config: ~/.kn9t/config.toml  Token: ~/.kn9t/token  Port: ~/.kn9t/port");
    println!("Docs: README.md, DESIGN.md §12, spec/06-server.md, API.md");
}

fn print_version() {
    println!("kn9t {}", env!("CARGO_PKG_VERSION"));
}

fn print_chat_help() {
    println!("kn9t chat — send a prompt or enter REPL");
    println!();
    println!("Usage:");
    println!("  kn9t chat [OPTIONS] [PROMPT...]");
    println!();
    println!("Options:");
    println!("  --model <provider/id>   Model (also provider:id). Default: server default model");
    println!("  --model <provider:id>");
    println!("  --continue              Attach to latest session instead of creating one");
    println!("  --json                  JSONL on stdout (one SSE event per line, parseable)");
    println!("  --format <json|text>    Output format (default text)");
    println!("  -h, --help              Show this help");
    println!();
    println!("Modes:");
    println!("  kn9t chat \"hello\"               One-shot: create session, send, stream, exit");
    println!("  kn9t chat --json \"hello\"        One-shot JSONL: stdout is parseable, stderr stays human");
    println!("  kn9t chat                       REPL: new session, prompt loop (> ), Ctrl-D exits");
    println!("  kn9t chat --continue            REPL: resume latest session");
    println!();
    println!("JSONL (one-shot --json):");
    println!("  kind=session    {{kind, session_id, model}}");
    println!("  kind=prompt     {{kind, text, session_id}}");
    println!("  then raw SSE events: text_delta, thinking_delta, tool_args_delta,");
    println!("  tool_started, tool_progress, tool_finished, message_appended, turn_ended,");
    println!("  approval_request, retry_attempt, turn_status, error, etc. (snake_case, AGENTS.md §12)");
    println!("  Example: kn9t chat --json \"hi\" | jq -c 'select(.kind==\"text_delta\") | .delta'");
    println!("  Example: kn9t chat --json \"hi\" | jq -s 'map(select(.kind==\"tool_started\"))'");
    println!();
    println!("REPL: prompts via POST /session/{{id}}/prompt [lease], streams SSE until TurnEnded.");
    println!("Approval: inline [ No ]/[ Yes ] selector on ApprovalRequest, then POST /approve.");
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help" | "help")
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    // Top-level help / version must NOT start the server.
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        if is_help(cmd) || cmd == "--help" {
            print_help();
            return;
        }
        if matches!(cmd, "--version" | "-V" | "version" | "-v") {
            print_version();
            return;
        }
        // `kn9t --help` style already handled; `kn9t chat --help` handled below.
        if cmd.starts_with('-') && !matches!(cmd, "chat" | "sessions" | "history" | "attach" | "status" | "models" | "cost" | "tools" | "stop" | "help") {
            eprintln!("error: unknown option '{cmd}'");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    }

    // Handle subcommands.
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        match cmd {
            "chat" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    print_chat_help();
                    return;
                }
                let (port, token) = ensure_server();
                chat::run(&args[2..], port, &token);
                return;
            }
            "sessions" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t sessions — list sessions (GET /session)");
                    println!();
                    println!("Usage: kn9t sessions");
                    println!("Options: -h, --help");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_sessions::run(port, &token);
                return;
            }
            "history" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t history — show transcript");
                    println!();
                    println!("Usage: kn9t history [SESSION_ID] [-h|--help]");
                    println!("       kn9t history              # latest session");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_history::run(&args[2..], port, &token);
                return;
            }
            "attach" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t attach — attach REPL to a session");
                    println!();
                    println!("Usage: kn9t attach [SESSION_ID] [-h|--help]");
                    println!("       kn9t attach               # latest");
                    return;
                }
                let (port, token) = ensure_server();
                chat::attach(&args[2..], port, &token);
                return;
            }
            "status" | "health" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t status — server health (GET /health)");
                    println!();
                    println!("Usage: kn9t status [-h|--help]");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_status::run(port, &token);
                return;
            }
            "models" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t models — list configured models (GET /models)");
                    println!();
                    println!("Usage: kn9t models [-h|--help]");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_models::run(port, &token);
                return;
            }
            "cost" | "budget" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t cost — cost analytics (GET /cost, GET /budget)");
                    println!();
                    println!("Usage: kn9t cost [--since MS] [--group-by model|kind|session] [-h|--help]");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_cost::run(&args[2..], port, &token);
                return;
            }
            "tools" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t tools — list registered tools (GET /tools)");
                    println!();
                    println!("Usage: kn9t tools [-h|--help]");
                    return;
                }
                let (port, token) = ensure_server();
                cmd_tools::run(port, &token);
                return;
            }
            "stop" => {
                if args.get(2).map(|s| is_help(s)).unwrap_or(false) {
                    println!("kn9t stop — graceful shutdown (POST /stop)");
                    println!();
                    println!("Usage: kn9t stop [-h|--help]");
                    return;
                }
                cmd_stop::run();
                return;
            }
            "install-plugins" => {
                // Does NOT require server — local-only operation.
                cmd_install_plugins::run(&args[2..]);
                return;
            }
            "help" => {
                print_help();
                return;
            }
            _ => {
                // Unknown command — show help and exit 2 (never fall through to TUI).
                eprintln!("error: unknown command '{cmd}'");
                eprintln!();
                print_help();
                std::process::exit(2);
            }
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
