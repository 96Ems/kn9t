//! Simple file logger for debugging.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// Initialize the log file.
pub fn init(path: &str) {
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        if let Ok(mut guard) = LOG_FILE.lock() {
            *guard = Some(file);
        }
    }
}

/// Log a message.
pub fn log(msg: &str) {
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(ref mut file) = *guard {
            let _ = writeln!(file, "{}", msg);
            let _ = file.flush();
        }
    }
}

/// Log with format.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::log(&format!($($arg)*))
    };
}
