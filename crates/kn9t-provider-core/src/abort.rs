//! CancellableReader — wraps a `Read` stream; returns `Interrupted` when `Cancel` fires.
//! See `docs/internal/job/instant-cut.md` — instant cut <1ms on next `read()`.

use kn9t_core::Cancel;
use kn9t_macros::safe_unwrap;
use std::io::{self, Read};

pub struct CancellableReader<R> {
    inner: std::sync::Arc<std::sync::Mutex<R>>,
    cancel: Cancel,
}

impl<R> CancellableReader<R> {
    pub fn new(inner: R, cancel: Cancel) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
            cancel,
        }
    }
}

impl<R: Read + Send + 'static> Read for CancellableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "cancelled",
            ));
        }
        let len = buf.len();
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut tmp = vec![0u8; len];
            let res = safe_unwrap!(inner.lock()).read(&mut tmp);
            let _ = tx.send((res, tmp));
        });
        loop {
            if cancel.cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "cancelled",
                ));
            }
            match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok((res, tmp)) => match res {
                    Ok(n) => {
                        buf[..n].copy_from_slice(&tmp[..n]);
                        return Ok(n);
                    }
                    Err(e) => return Err(e),
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "reader thread died",
                    ))
                }
            }
        }
    }
}

