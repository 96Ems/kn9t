//! First-run bootstrap — auto-creates `~/.kn9t/` with a config template and token.
//!
//! Call `ensure_home()` once, before any server interaction. It is a no-op if the
//! directory and files already exist. GI-5: no async.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

// ── Config template ───────────────────────────────────────────────────────────

const CONFIG_TEMPLATE: &str = r#"# kn9t configuration — ~/.kn9t/config.toml
#
# This file was auto-generated on first run. Edit it to configure your providers.
# Full documentation: https://github.com/96Ems/kn9t (DESIGN.md §8)
#
# ── Providers ─────────────────────────────────────────────────────────────────
#
# Providers are subprocess plugin binaries.
#
# `binary` is either a bare name — resolved next to the kn9t server executable,
# which is how BUNDLED plugins like kn9t-anthropic are found — or an ABSOLUTE
# PATH, which is how EXTERNAL plugins are found.
#
# kn9t-anthropic (direct Anthropic API) is bundled.
# External plugins (e.g. kn9t-custom-provider for a custom provider gateway) are built separately;
# point `binary` at the absolute path of the compiled executable.
#
# Uncomment and fill in ONE of the blocks below.

# -- Anthropic direct ----------------------------------------------------------
#
# [[provider]]
# id      = "anthropic"
# kind    = "plugin"
# binary  = "kn9t-anthropic"
#
# [provider.anthropic.env]
# ANTHROPIC_API_KEY = "sk-ant-xxxxxxxxxxxxxxxxxxxx"
#
# [[model]]
# provider = "anthropic"
# id       = "claude-opus-4-5"
# label    = "Claude Opus 4.5"
# default  = true

# -- OpenAI-compatible gateway -------------------------------------------------
#
# [[provider]]
# id      = "my-gateway"
# kind    = "openai"
# base_url = "https://llm-gateway.example.com/v1"
#
# [provider.my-gateway.headers]
# X-User-Id = "env:GATEWAY_USER_ID"
#
# [[model]]
# provider = "my-gateway"
# id       = "claude-4-sonnet"

# ── Server ────────────────────────────────────────────────────────────────────
# Optional. Defaults shown — you usually do not need to change these.

# [server]
# port            = 0   # 0 = pick a random free port at startup
# idle_exit_secs  = 5   # seconds of grace after last client disconnects before exiting
#                       # server stays up as long as any client is connected
#                       # set to 0 to disable auto-exit
# log             = "server.log"
"#;

// ── UUID v4 (no deps) ─────────────────────────────────────────────────────────

/// Generate a random UUID v4 from OS entropy. No external crates.
fn random_uuid() -> String {
    // Read 16 bytes from OS CSPRNG.
    let bytes = os_random_bytes::<16>();
    // Set version (4) and variant (10xx) bits per RFC 4122.
    let mut b = bytes;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3],
        b[4], b[5],
        b[6], b[7],
        b[8], b[9],
        b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

#[cfg(unix)]
fn os_random_bytes<const N: usize>() -> [u8; N] {
    use std::fs::File;
    use std::io::Read;
    let mut buf = [0u8; N];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("cannot read /dev/urandom");
    buf
}

#[cfg(windows)]
fn os_random_bytes<const N: usize>() -> [u8; N] {
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            h_algorithm: *mut u8,
            pb_buffer: *mut u8,
            cb_buffer: u32,
            dw_flags: u32,
        ) -> u32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x00000002;
    let mut buf = [0u8; N];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            N as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert_eq!(status, 0, "BCryptGenRandom failed: {status}");
    buf
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Ensure `~/.kn9t/` exists with a config template and server auth token.
///
/// No-op if both `config.toml` and `token` already exist. Prints a first-run
/// message to stderr when files are created. Never fails fatally — if it cannot
/// write, it prints a warning and returns.
pub fn ensure_home(home: &Path) {
    let config_path = home.join("config.toml");
    let token_path  = home.join("token");

    let need_config = !config_path.exists();
    let need_token  = !token_path.exists();

    if !need_config && !need_token {
        return; // fast path — already set up
    }

    // Create the directory if needed.
    if let Err(e) = fs::create_dir_all(home) {
        eprintln!("[kn9t] warning: cannot create {}: {e}", home.display());
        return;
    }

    let stderr = io::stderr();
    let mut err = stderr.lock();

    if need_token {
        let token = random_uuid();
        match fs::write(&token_path, &token) {
            Ok(_) => {}
            Err(e) => {
                writeln!(err, "[kn9t] warning: cannot write token: {e}").ok();
            }
        }
    }

    if need_config {
        match fs::write(&config_path, CONFIG_TEMPLATE) {
            Ok(_) => {
                writeln!(err, "").ok();
                writeln!(err, "  kn9t — first run").ok();
                writeln!(err, "  ─────────────────────────────────────────────────────").ok();
                writeln!(err, "  Created: {}", home.display()).ok();
                writeln!(err, "").ok();
                writeln!(err, "  Next step: edit ~/.kn9t/config.toml and uncomment a").ok();
                writeln!(err, "  [[provider]] block with your API credentials, then").ok();
                writeln!(err, "  run kn9t again.").ok();
                writeln!(err, "  ─────────────────────────────────────────────────────").ok();
                writeln!(err, "").ok();
            }
            Err(e) => {
                writeln!(err, "[kn9t] warning: cannot write config template: {e}").ok();
            }
        }
    }
}

// ── Public helper: home path from env ─────────────────────────────────────────

pub fn kn9t_home_path() -> PathBuf {
    if let Ok(h) = std::env::var("KN9T_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kn9t")
}
