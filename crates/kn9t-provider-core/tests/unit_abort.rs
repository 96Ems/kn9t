use kn9t_core::Cancel;
use kn9t_provider_core::abort::CancellableReader;
use std::io::{self, Read};

#[test]
fn cancel_reader_interrupts() {
    let data = b"hello world";
    let cancel = Cancel::new();
    let mut r = CancellableReader::new(std::io::Cursor::new(data), cancel.clone());
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
    let mut r = CancellableReader::new(std::io::Cursor::new(data), cancel);
    let mut out = String::new();
    r.read_to_string(&mut out).unwrap();
    assert_eq!(out, "abc");
}

#[test]
fn abort_interrupts_blocking_read_quickly() {
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
    assert!(
        elapsed < Duration::from_millis(500),
        "cancel took too long: {elapsed:?}"
    );
}

#[test]
fn abort_interrupts_http_sse_quickly() {
    use kn9t_provider_core::{send, HttpRequest};
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
    let mut lines = kn9t_provider_core::sse::sse_lines(resp.body, None);
    eprintln!("test: reading first line");
    let first = lines.next().expect("first").expect("ok");
    eprintln!("test: first line ok {:?}", first);
    assert_eq!(first, b"{\"x\":1}");
    eprintln!("test: reading second line (should block then cancel)");
    let second = lines.next();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(800),
        "cancel took too long: {elapsed:?}"
    );
    if let Some(Err(e)) = second {
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionAborted);
    } else {
        assert!(elapsed < Duration::from_millis(800));
    }
    // Don't join server, let it timeout
    let _ = server;
}
