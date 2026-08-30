//! R-SRV-020, R-SRV-030 — mandatory auth and cross-origin rejection.
//!
//! The token is a random 32-byte hex string written to `~/.kn9t/token` (mode 0600
//! on Unix) at startup; the listening port to `~/.kn9t/port`. Every request MUST
//! carry `Authorization: Bearer <token>` (else 401). Any request carrying an
//! `Origin` header is rejected (403) so a webpage `fetch` cannot drive the agent
//! (DESIGN §12.5).

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The kn9t config directory, `~/.kn9t`, overridable by `KN9T_HOME` (tests).
pub fn kn9t_home() -> PathBuf {
    if let Ok(h) = std::env::var("KN9T_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kn9t")
}

pub fn token_path() -> PathBuf {
    kn9t_home().join("token")
}
pub fn port_path() -> PathBuf {
    kn9t_home().join("port")
}
pub fn spawn_lock_path() -> PathBuf {
    kn9t_home().join("spawn.lock")
}

/// A random 32-byte token rendered as 64 hex chars. Not cryptographic-grade RNG
/// (splitmix64 over wall-clock nanos + address entropy) but ample for a
/// loopback-only, single-user localhost guard (DESIGN §12.5); the security
/// property is *possession of the file*, which is 0600-protected.
pub fn generate_token() -> String {
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678);
    // Mix in a stack address for extra per-process entropy.
    let stack = &seed as *const u64 as u64;
    seed ^= stack.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    if seed == 0 {
        seed = 0x0123_4567_89AB_CDEF;
    }
    let mut out = String::with_capacity(64);
    for _ in 0..4 {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out
}

/// Write the token to `path` with mode 0600 on Unix.
pub fn write_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, token)?;
    set_mode_0600(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &Path) -> io::Result<()> {
    // Windows: no 0600 equivalent; the token file lives in the user profile
    // directory. R-SRV-020 mandates 0600 "where the OS supports it".
    Ok(())
}

/// Extract the bearer token from an `Authorization: Bearer <token>` header value.
pub fn parse_bearer(header_value: &str) -> Option<&str> {
    let v = header_value.trim();
    let rest = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))?;
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Constant-time-ish equality over equal-length tokens (avoids trivial early-out
/// timing leaks; both are 64 hex chars in practice).
pub fn token_matches(expected: &str, given: &str) -> bool {
    if expected.len() != given.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(given.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}
