//! Simple append-only file logger for the server process.
//! Writes timestamped lines to `~/.kn9t/server.log`.
//! No external deps — just std::fs and std::sync.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Open (or create) the log file. Call once at startup.
pub fn init(path: &PathBuf) {
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(path) {
        *LOG.lock().unwrap() = Some(f);
    }
}

/// Write one log line: `[HH:MM:SS] <msg>\n` to file and stderr.
pub fn write(msg: &str) {
    let ts = timestamp();
    let line = format!("[{ts}] {msg}\n");
    eprint!("{line}");
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 86400;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let s = s % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Log macro — mirrors eprintln! but also writes to the log file.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(&format!($($arg)*))
    };
}
