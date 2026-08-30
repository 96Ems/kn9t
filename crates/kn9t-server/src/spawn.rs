//! R-SRV-070 — client-side auto-spawn of the server (DESIGN §12.2).
//!
//! Any client (TUI, `kn9t -p`) that finds nothing listening spawns a detached
//! `kn9t serve`:
//! 1. take an exclusive lock on `~/.kn9t/spawn.lock` (else two clients racing both
//!    spawn a server);
//! 2. spawn detached, poll for `~/.kn9t/port`, connect;
//! 3. a port file pointing at a closed socket is stale → delete and respawn;
//! 4. release the lock.
//!
//! An in-process server for `-p` is forbidden — the second wiring path §2 exists
//! to prevent (DESIGN §12.2).

use std::fs::{File, OpenOptions};
use std::io;
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::auth;

/// An OS advisory file lock held for the duration of a spawn attempt. Uses
/// `flock` on Unix and an exclusive-create `.held` marker spin on Windows.
pub struct SpawnLock {
    _file: File,
    #[cfg(unix)]
    fd: i32,
    #[cfg(not(unix))]
    held_path: std::path::PathBuf,
}

impl SpawnLock {
    /// Acquire the exclusive spawn lock, blocking until it is available.
    pub fn acquire(path: &Path) -> io::Result<SpawnLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        Self::lock_exclusive(path, file)
    }

    #[cfg(unix)]
    fn lock_exclusive(_path: &Path, file: File) -> io::Result<SpawnLock> {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        // LOCK_EX (2). Blocking.
        let rc = unsafe { flock(fd, 2) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SpawnLock { _file: file, fd })
    }

    #[cfg(not(unix))]
    fn lock_exclusive(path: &Path, file: File) -> io::Result<SpawnLock> {
        // Windows has no flock. Use a `.held` marker created with exclusive-create
        // semantics as an advisory mutex; spin until we win it. A stale marker
        // (holder crashed) is reclaimed after a grace period based on its mtime.
        let held_path = path.with_extension("held");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match OpenOptions::new().create_new(true).write(true).open(&held_path) {
                Ok(_) => {
                    return Ok(SpawnLock { _file: file, held_path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // Reclaim a stale marker whose holder likely died.
                    if let Ok(meta) = std::fs::metadata(&held_path) {
                        if let Ok(modified) = meta.modified() {
                            if modified.elapsed().unwrap_or_default() > Duration::from_secs(60) {
                                let _ = std::fs::remove_file(&held_path);
                                continue;
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "spawn lock contended for too long",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for SpawnLock {
    fn drop(&mut self) {
        // LOCK_UN (8).
        unsafe {
            flock(self.fd, 8);
        }
    }
}

#[cfg(not(unix))]
impl Drop for SpawnLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.held_path);
    }
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Read the port from `~/.kn9t/port`, if present and parseable.
pub fn read_port(path: &Path) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    s.trim().parse::<u16>().ok()
}

/// True if a TCP connection to `127.0.0.1:port` succeeds (server is listening).
pub fn is_listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(200),
    )
    .is_ok()
}

/// Result of ensuring a server is up.
pub struct Connected {
    pub port: u16,
}

/// R-SRV-070 — ensure a server is listening, spawning one if necessary. `spawn_fn`
/// launches the detached server process (injected so tests can spawn an in-process
/// thread-server rather than a real binary). Returns the live port.
///
/// The whole sequence runs under the spawn lock so two racing clients yield exactly
/// one server.
pub fn ensure_server<F>(
    port_path: &Path,
    lock_path: &Path,
    spawn_fn: F,
    poll_timeout: Duration,
) -> io::Result<Connected>
where
    F: FnOnce() -> io::Result<()>,
{
    // Fast path: a live port file with a listening socket needs no lock.
    if let Some(p) = read_port(port_path) {
        if is_listening(p) {
            return Ok(Connected { port: p });
        }
    }

    // Contended path: take the lock, re-check (another client may have won the
    // race and spawned while we waited), then spawn if still needed.
    let _lock = SpawnLock::acquire(lock_path)?;

    if let Some(p) = read_port(port_path) {
        if is_listening(p) {
            return Ok(Connected { port: p }); // someone else spawned while we waited
        }
        // Stale port file (socket closed): delete and respawn (R-SRV-070 step 3).
        let _ = std::fs::remove_file(port_path);
    }

    spawn_fn()?;

    // Poll for the port file + a listening socket.
    let deadline = Instant::now() + poll_timeout;
    loop {
        if let Some(p) = read_port(port_path) {
            if is_listening(p) {
                return Ok(Connected { port: p });
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "server did not come up within poll timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // lock released on drop (step 4)
}

/// Write the listening port to `~/.kn9t/port` (server side, on bind).
pub fn write_port(path: &Path, port: u16) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, port.to_string())
}

/// Re-export for callers that build default paths.
pub use auth::{port_path, spawn_lock_path};
