//! CancellableReader — wraps a `Read` stream; returns `Interrupted` when `Cancel` fires.
//! See `job/instant-cut.md` — instant cut <1ms on next `read()`.

use kn9t_core::Cancel;
use std::io::{self, Read};

pub struct CancellableReader<R> {
    inner: std::sync::Arc<std::sync::Mutex<R>>,
    cancel: Cancel,
}

impl<R> CancellableReader<R> {
    pub fn new(inner: R, cancel: Cancel) -> Self {
        Self { inner: std::sync::Arc::new(std::sync::Mutex::new(inner)), cancel }
    }
}

impl<R: Read + Send + 'static> Read for CancellableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let len = buf.len();
        // Clone Arc for thread
        let inner = self.inner.clone();
        let cancel = self.cancel.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut tmp = vec![0u8; len];
            let res = inner.lock().unwrap().read(&mut tmp);
            let _ = tx.send((res, tmp));
        });
        loop {
            if cancel.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok((res, tmp)) => {
                    match res {
                        Ok(n) => {
                            buf[..n].copy_from_slice(&tmp[..n]);
                            return Ok(n);
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "reader thread died"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn cancel_reader_interrupts() {
        let data = b"hello world";
        let cancel = Cancel::new();
        let mut r = CancellableReader::new(Cursor::new(data), cancel.clone());
        // before cancel, reads normally
        let mut buf = [0u8; 5];
        assert_eq!(r.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b"hello");
        // after cancel, next read is Interrupted
        cancel.cancel();
        let err = r.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancel_reader_not_cancelled_passes_through() {
        let data = b"abc";
        let cancel = Cancel::new();
        let mut r = CancellableReader::new(Cursor::new(data), cancel);
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "abc");
    }

    #[test]
    fn abort_interrupts_blocking_read_quickly() {
        use std::io::{self, Read};
        use std::time::{Duration, Instant};

        struct BlockingReader;
        impl Read for BlockingReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_secs(10));
                Ok(0)
            }
        }

        let cancel = Cancel::new();
        let cancel_c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel_c.cancel();
        });

        let start = Instant::now();
        let mut r = CancellableReader::new(BlockingReader, cancel);
        let mut buf = [0u8; 1024];
        let err = r.read(&mut buf).unwrap_err();
        let elapsed = start.elapsed();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(elapsed < Duration::from_millis(500), "cancel took too long: {elapsed:?}");
    }
}
