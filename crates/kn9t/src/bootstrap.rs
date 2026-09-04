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
# Providers are either OpenAI-compatible endpoints (kind = "openai") or
# subprocess plugin binaries (kind = "plugin").
#
# Uncomment ONE provider block below and fill in your credentials.
# You can run multiple providers simultaneously.

# ─────────────────────────────────────────────────────────────────────────────
# OpenCode Go — $10/mo subscription, open-source models (chat-completions)
# https://opencode.ai/go
#
# Limit: $12/5h · $30/week · $60/month
# Get your key at: https://opencode.ai/auth
# ─────────────────────────────────────────────────────────────────────────────

[provider.opencode-go]
kind     = "openai"
base_url = "https://opencode.ai/zen/go/v1"
api_key  = "env:OPENCODE_API_KEY"

[provider.opencode-go.quirks]
usage_in_stream = true

[[model]]
provider         = "opencode-go"
id               = "deepseek-v4-pro"
ctx              = 131072
max_out          = 8192
price_in         = 0.435
price_out        = 0.87
price_cache_read = 0.0036

[[model]]
provider         = "opencode-go"
id               = "deepseek-v4-flash"
ctx              = 131072
max_out          = 8192
price_in         = 0.14
price_out        = 0.28
price_cache_read = 0.0028

[[model]]
provider = "opencode-go"
id       = "kimi-k3"
ctx      = 131072
max_out  = 8192
price_in = 0.60
price_out = 1.80

[[model]]
provider = "opencode-go"
id       = "kimi-k2.7-code"
ctx      = 131072
max_out  = 8192
price_in = 0.60
price_out = 1.80

[[model]]
provider = "opencode-go"
id       = "kimi-k2.6"
ctx      = 131072
max_out  = 8192
price_in = 0.60
price_out = 1.80

[[model]]
provider = "opencode-go"
id       = "glm-5.3-flash"
ctx      = 131072
max_out  = 8192
price_in = 0.10
price_out = 0.40

[[model]]
provider = "opencode-go"
id       = "glm-5.3"
ctx      = 131072
max_out  = 8192
price_in = 1.00
price_out = 2.00

[[model]]
provider = "opencode-go"
id       = "glm-5.2"
ctx      = 131072
max_out  = 8192
price_in = 1.00
price_out = 2.00

[[model]]
provider = "opencode-go"
id       = "mimo-v2.5"
ctx      = 131072
max_out  = 8192
price_in = 0.03
price_out = 0.12

[[model]]
provider = "opencode-go"
id       = "mimo-v2.5-pro"
ctx      = 131072
max_out  = 8192
price_in = 0.10
price_out = 0.40

[[model]]
provider = "opencode-go"
id       = "hy3"
ctx      = 131072
max_out  = 8192
price_in = 0.50
price_out = 1.50

[[model]]
provider = "opencode-go"
id       = "grok-4.6"
ctx      = 131072
max_out  = 8192
price_in = 2.00
price_out = 6.00

[[model]]
provider = "opencode-go"
id       = "minimax-m3"
ctx      = 1048576
max_out  = 16384
price_in = 0.15
price_out = 0.60

[[model]]
provider = "opencode-go"
id       = "minimax-m2.7"
ctx      = 1048576
max_out  = 16384
price_in = 0.15
price_out = 0.60

[[model]]
provider = "opencode-go"
id       = "longcat-2.0"
ctx      = 131072
max_out  = 8192
price_in = 0.10
price_out = 0.40

# ─────────────────────────────────────────────────────────────────────────────
# OpenCode Go — Anthropic-compatible models (Qwen, MiniMax via /messages)
# NOTE: requires a future kn9t-plugin-opencode-ant or similar to speak
# the Anthropic Messages API through the OpenCode Go gateway.
# ─────────────────────────────────────────────────────────────────────────────

# [provider.opencode-go-ant]
# kind     = "openai"   # placeholder — needs Anthropic-compat provider
# base_url = "https://opencode.ai/zen/go"
# api_key  = "env:OPENCODE_API_KEY"

# [[model]]
# provider = "opencode-go-ant"
# id       = "qwen3.8-max"
# ctx      = 131072
# max_out  = 8192
# price_in = 2.00
# price_out = 6.00

# [[model]]
# provider = "opencode-go-ant"
# id       = "qwen3.7-max"
# ctx      = 131072
# max_out  = 8192
# price_in = 2.00
# price_out = 6.00

# ─────────────────────────────────────────────────────────────────────────────
# OpenCode Zen — pay-as-you-go, curated models (chat-completions)
# https://opencode.ai/zen
#
# Same API key as Go. Pay-per-use, auto top-up at $5.
# Get your key at: https://opencode.ai/auth
# ─────────────────────────────────────────────────────────────────────────────

# [provider.opencode-zen]
# kind     = "openai"
# base_url = "https://opencode.ai/zen/v1"
# api_key  = "env:OPENCODE_API_KEY"

# [provider.opencode-zen.quirks]
# usage_in_stream = true

# [[model]]
# provider = "opencode-zen"
# id       = "deepseek-v4-pro"
# ctx      = 131072
# max_out  = 8192
# price_in = 0.435
# price_out = 0.87
# price_cache_read = 0.0036

# [[model]]
# provider = "opencode-zen"
# id       = "deepseek-v4-flash"
# ctx      = 131072
# max_out  = 8192
# price_in = 0.14
# price_out = 0.28
# price_cache_read = 0.0028

# Free models on Zen (no charge)
# [[model]]
# provider = "opencode-zen"
# id       = "deepseek-v4-flash-free"
# ctx      = 131072
# max_out  = 8192

# [[model]]
# provider = "opencode-zen"
# id       = "kimi-k2.5-free"
# ctx      = 131072
# max_out  = 8192

# ─────────────────────────────────────────────────────────────────────────────
# OpenCode Zen — Anthropic-compatible models (Claude, Qwen via /messages)
# NOTE: requires a future kn9t-plugin-opencode-ant or similar.
# ─────────────────────────────────────────────────────────────────────────────

# [provider.opencode-zen-ant]
# kind     = "openai"   # placeholder — needs Anthropic-compat provider
# base_url = "https://opencode.ai/zen"
# api_key  = "env:OPENCODE_API_KEY"

# [[model]]
# provider = "opencode-zen-ant"
# id       = "claude-sonnet-4-5"
# ctx      = 200000
# max_out  = 65536
# price_in = 3.00
# price_out = 15.00
# price_cache_read = 0.30
# price_cache_write = 3.75

# [[model]]
# provider = "opencode-zen-ant"
# id       = "claude-opus-4-6"
# ctx      = 200000
# max_out  = 32768
# price_in = 5.00
# price_out = 25.00
# price_cache_read = 0.50
# price_cache_write = 6.25

# ─────────────────────────────────────────────────────────────────────────────
# OpenCode Zen — OpenAI Responses API models (GPT-5.x)
# NOTE: requires a future kn9t-plugin-opencode-gpt provider.
# These models use the /v1/responses endpoint, not /v1/chat/completions.
# ─────────────────────────────────────────────────────────────────────────────

# [provider.opencode-zen-gpt]
# kind     = "plugin"   # needs dedicated Responses API provider
# binary   = "kn9t-plugin-opencode-gpt"   # future plugin

# [[model]]
# provider = "opencode-zen-gpt"
# id       = "gpt-5.5"
# ctx      = 1048576
# max_out  = 65536
# price_in = 1.25
# price_out = 10.00

# ─────────────────────────────────────────────────────────────────────────────
# Anthropic direct — bundled plugin
# ─────────────────────────────────────────────────────────────────────────────

# [provider.anthropic]
# kind    = "plugin"
# binary  = "kn9t-anthropic"
#
# [provider.anthropic.env]
# ANTHROPIC_API_KEY = "env:ANTHROPIC_API_KEY"
#
# [[model]]
# provider = "anthropic"
# id       = "claude-opus-4-5"
#
# [[model]]
# provider = "anthropic"
# id       = "claude-sonnet-4-5"

# ─────────────────────────────────────────────────────────────────────────────
# Custom provider — external plugin for any gateway
# ─────────────────────────────────────────────────────────────────────────────

# [provider.custom]
# kind    = "plugin"
# binary  = "/absolute/path/to/kn9t-custom-provider"
#
# [provider.custom.env]
# CUSTOM_PROVIDER_URL = "https://your-gateway.example.com"

# ── Plugins ────────────────────────────────────────────────────────────────────
# Tool plugins are auto-discovered from ~/.kn9t/plugins/ at server startup
# (ADR-0004): every executable file in that directory is handshaked as a plugin.
# A project-relative plugins/ directory is NEVER scanned (clone-and-run safety).
#
# Default tools (bash/read/write/edit) are installed here on first run when a
# build of plugins/kn9t-tools is found; you can also drop any plugin binary in
# manually and restart the server.
#
# An explicit [[plugin]] entry (global config only) can override discovery:
# - pin a path:   [[plugin]] name="my-tools" cmd=["/abs/path/to/my-tools"]
#                 → discovered "my-tools" is suppressed; this path is spawned instead
# - inject env:   [[plugin]] name="my-tools"  [plugin.env] FOO="bar"
#                 → env vars are injected when the discovered "my-tools" is spawned
# - disable:      [[plugin]] name="my-tools" enabled=false   (or disabled=true)
#                 → discovered "my-tools" is not spawned at all
#
# Examples:
# [[plugin]]
# name = "my-tools"
# cmd  = ["/absolute/path/to/my-tools"]
# [plugin.env]
# FOO = "bar"
#
# [[plugin]]
# name = "noisy-plugin"
# enabled = false

# ── Server ────────────────────────────────────────────────────────────────────
# Optional. Defaults shown — you usually do not need to change these.

# [server]
# idle_exit_secs  = 1800  # 30 min; set to 0 to disable auto-exit

# ── Policy ────────────────────────────────────────────────────────────────────
# Global only (~/.kn9t/config.toml). Controls bash classification and
# approval prompting (DESIGN §10.1). Absent → ask_on_mutation with built-in
# defaults.

# [policy]
# mode = "ask_on_mutation"   # allow_all | deny_all | readonly | ask_on_mutation

# [policy.bash]
# allow_read = ["rg","grep","find","ls","cat","head","tail","wc","file","stat", ...]
# always_ask = ["rm","mv","cp","chmod","chown","kill","dd","curl","wget","ssh","scp","sh","bash","python","node", ...]
# never      = ["shutdown","reboot","mkfs*","fdisk","sudo"]

# Must come last — every key above belongs to [policy.bash].
# [policy.bash.allow_read_sub]
# git   = ["log","diff","show","status","branch","blame","describe","rev-parse","ls-files","remote","tag"]
# cargo = ["tree","metadata","--version"]
# npm   = ["ls","view","outdated"]

# Persistent always-approvals (written by `scope=always`, never hand-edit `never` entries).
# [policy.approvals]
# always = ["bash:rm -rf /tmp/test"]
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
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15],
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
    let token_path = home.join("token");

    let need_config = !config_path.exists();
    let need_token = !token_path.exists();

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
                writeln!(
                    err,
                    "  ─────────────────────────────────────────────────────"
                )
                .ok();
                writeln!(err, "  Created: {}", home.display()).ok();
                writeln!(err, "").ok();
                writeln!(err, "  Next step: edit ~/.kn9t/config.toml and uncomment a").ok();
                writeln!(err, "  [[provider]] block with your API credentials, then").ok();
                writeln!(err, "  run kn9t again.").ok();
                writeln!(
                    err,
                    "  ─────────────────────────────────────────────────────"
                )
                .ok();
                writeln!(err, "").ok();
            }
            Err(e) => {
                writeln!(err, "[kn9t] warning: cannot write config template: {e}").ok();
            }
        }
    }

    // Install default tool plugins into <home>/plugins on first run (ADR-0004).
    install_default_tools(home);
}

/// Install the default tools plugin (`kn9t-tools`) into `<home>/plugins/`.
///
/// `kn9t-tools` is NOT a workspace member — it is a standalone crate at
/// `plugins/kn9t-tools` in the repo, built separately (its output lands at
/// `plugins/kn9t-tools/target/{debug,release}/`, not `target/`). At bootstrap we
/// cannot assume the binary sits next to the kn9t executable or has been built
/// at all, so we try, in order, and only when the plugins dir is empty (never
/// overwrite something the user installed):
///
/// 1. `<exe dir>/kn9t-tools[.exe]` — a copy placed next to the kn9t binary.
/// 2. `<repo root>/plugins/kn9t-tools/target/{debug,release}/kn9t-tools[.exe]`
///    — the standalone crate's build output, located by walking up from the exe
///    (`cargo run` puts the exe at `<repo>/target/{profile}/kn9t`).
///
/// If none is found we log and continue: server discovery must work regardless of
/// bootstrap (the dir can be populated manually, and a missing plugin is a
/// soft-fail at discovery, not a crash).
fn install_default_tools(home: &Path) {
    let plugins_dir = home.join("plugins");
    if let Err(e) = fs::create_dir_all(&plugins_dir) {
        eprintln!(
            "[kn9t] warning: cannot create {}: {e}",
            plugins_dir.display()
        );
        return;
    }

    // Only populate an empty dir — never clobber user-installed plugins.
    let empty = match fs::read_dir(&plugins_dir) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => true,
    };
    if !empty {
        return;
    }

    let bin_name = format!("kn9t-tools{}", std::env::consts::EXE_SUFFIX);
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        // 1. sibling of the kn9t executable.
        if let Some(d) = exe.parent() {
            candidates.push(d.join(&bin_name));
        }
        // 2. walk up from the exe dir toward the repo root, looking for the
        //    standalone plugin's build output.
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..6 {
            if let Some(d) = dir {
                let base = d.join("plugins").join("kn9t-tools").join("target");
                for profile in ["debug", "release"] {
                    candidates.push(base.join(profile).join(&bin_name));
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
    }

    for cand in &candidates {
        if cand.is_file() {
            let dest = plugins_dir.join(&bin_name);
            match fs::copy(cand, &dest) {
                Ok(_) => {
                    eprintln!(
                        "[kn9t] installed default tools plugin: {} → {}",
                        cand.display(),
                        dest.display()
                    );
                    return;
                }
                Err(e) => {
                    eprintln!(
                        "[kn9t] warning: cannot copy {} to {}: {e}",
                        cand.display(),
                        dest.display()
                    );
                    return;
                }
            }
        }
    }

    eprintln!(
        "[kn9t] no default tools plugin found to install into {}.\n\
         Build it with `cd plugins/kn9t-tools && cargo build`, then copy\n\
         plugins/kn9t-tools/target/debug/kn9t-tools into that directory.",
        plugins_dir.display()
    );
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
