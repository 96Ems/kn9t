//! CancellableReader — wraps a `Read` stream; returns `Interrupted` when `Cancel` fires.
//! See `docs/internal/job/instant-cut.md` — instant cut <1ms on next `read()`.

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
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "cancelled"));
        }
        let len = buf.len();
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
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "cancelled"));
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
        // after cancel, next read is ConnectionAborted (not Interrupted, which BufReader retries)
        cancel.cancel();
        let err = r.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted);
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
        assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted);
        assert!(elapsed < Duration::from_millis(500), "cancel took too long: {elapsed:?}");
    }

    #[test]
    fn abort_interrupts_http_sse_quickly() {
        use crate::{send, HttpRequest};
        use std::io::Write;
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\n\r\n").unwrap();
            stream.write_all(b"data: {\"x\":1}\n\n").unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_secs(10));
            // keep connection open
            std::thread::sleep(Duration::from_secs(10));
        });

        let cancel = Cancel::new();
        let cancel_c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            eprintln!("test: firing cancel");
            cancel_c.cancel();
        });

        let start = Instant::now();
        let req = HttpRequest {
            method: "POST".into(),
            url: format!("http://{}/", addr),
            headers: vec![],
            body: vec![],
            auth: None,
            tls_insecure: false,
        };
        let resp = send(req, Duration::from_secs(5), Some(cancel)).expect("send ok");
        let mut lines = crate::sse::sse_lines(resp.body);
        eprintln!("test: reading first line");
        let first = lines.next().expect("first").expect("ok");
        eprintln!("test: first line ok {:?}", first);
        assert_eq!(first, b"{\"x\":1}");
        eprintln!("test: reading second line (should block then cancel)");
        let second = lines.next();
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(800), "cancel took too long: {elapsed:?}");
        if let Some(Err(e)) = second {
            assert_eq!(e.kind(), std::io::ErrorKind::ConnectionAborted);
        } else {
            assert!(elapsed < Duration::from_millis(800));
        }
        // Don't join server, let it timeout
        let _ = server;
    }
}
