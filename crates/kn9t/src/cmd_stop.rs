//! `kn9t stop` — request a graceful shutdown of the running server.
//!
//! Sends POST /stop to the server. The server finishes any in-flight turn,
//! then exits cleanly. The port file is removed by the server on exit.

use std::fs;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

fn kn9t_home() -> PathBuf {
    if let Ok(h) = std::env::var("KN9T_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kn9t")
}

pub fn run() {
    let home  = kn9t_home();
    let port  = match fs::read_to_string(home.join("port")).ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
    {
        Some(p) => p,
        None => {
            eprintln!("[kn9t stop] no server running (port file not found)");
            std::process::exit(1);
        }
    };
    let token = match fs::read_to_string(home.join("token")).ok()
        .map(|s| s.trim().to_string())
    {
        Some(t) => t,
        None => {
            eprintln!("[kn9t stop] cannot read token");
            std::process::exit(1);
        }
    };

    let host    = format!("127.0.0.1:{port}");
    let auth    = format!("Bearer {token}");
    let request = format!(
        "POST /stop HTTP/1.0\r\nHost: {host}\r\nAuthorization: {auth}\r\n\
         Content-Length: 0\r\n\r\n"
    );

    let mut stream = match TcpStream::connect(&host) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[kn9t stop] no server running on port {port}");
            std::process::exit(1);
        }
    };
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut resp = String::new();
    BufReader::new(stream).read_to_string(&mut resp).unwrap_or(0);

    if resp.contains("200") {
        eprintln!("[kn9t stop] server stopping (port {port})");
    } else {
        eprintln!("[kn9t stop] unexpected response: {resp}");
        std::process::exit(1);
    }
}
