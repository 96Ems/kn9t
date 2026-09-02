//! # kn9t-server
//!
//! The HTTP surface for kn9t (stage 06, spec `06-server.md`). One server process,
//! N clients (DESIGN §12). Blocking, thread-per-connection over `tiny_http`; no
//! tokio, no async (GI-5, R-SRV-015).
//!
//! This is the sole crate permitted more than one workspace dependency (GI-1
//! exception): it wires `kn9t-core`, `kn9t-store`, `kn9t-react`, `kn9t-plugin`,
//! and the provider crates, naming the concrete `Store`/`Tool`/`Approver`/`Provider`
//! types (DESIGN §2, §12).
//!
//! Tools are loaded from **external** plugin binaries auto-discovered in
//! `<KN9T_HOME|~/.kn9t>/plugins/` (ADR-0004, R-PLUG2-110), plus any pinned
//! `[[plugin]]` entries from the global config — never from a project-relative
//! `plugins/` directory and not from an in-process crate. This validates the
//! full plugin code path.
//!
//! The public API here is what the binary (`main.rs`) and the acceptance tests
//! both drive: [`ServerHandle::spawn`] binds a listener on an ephemeral port and
//! runs the accept loop on a background thread, returning the bound port so a test
//! client can connect.

pub mod api;
pub mod auth;
pub mod bus;
pub mod config;
pub mod policy;
pub mod http_util;
pub mod lease;
pub mod log;
pub mod router;
pub mod routes;
pub mod spawn;
pub mod sse;
pub mod state;
pub mod system_prompt;
pub mod tools;
pub mod turn;

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub use state::{ServerState, DEFAULT_IDLE_EXIT};

/// A running server: its bound port, a shutdown flag, and the accept-loop join
/// handle. Dropping the handle signals shutdown.
pub struct ServerHandle {
    pub port: u16,
    pub state: Arc<ServerState>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// Bind a `tiny_http` server on `127.0.0.1:0` (ephemeral port) and run the
    /// accept loop on a background thread. Each connection is handled on its own
    /// thread (thread-per-connection, R-SRV-010). Returns once the port is bound.
    ///
    /// The idle-exit watchdog (R-SRV-080) runs on its own thread and flips the
    /// shutdown flag when [`state::IdleTracker::should_exit`] fires.
    pub fn spawn(state: Arc<ServerState>) -> io::Result<ServerHandle> {
        // Bind a std listener first so we know the port deterministically, then
        // hand the socket to tiny_http.
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let port = listener.local_addr()?.port();
        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|e| io::Error::other(format!("tiny_http: {e}")))?;
        let server = Arc::new(server);

        let shutdown = Arc::new(AtomicBool::new(false));

        // Idle-exit watchdog thread (R-SRV-080).
        {
            let state = state.clone();
            let shutdown = shutdown.clone();
            let server = server.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(200));
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if state.idle.should_exit()
                    || state.stop_requested.load(Ordering::SeqCst)
                {
                    shutdown.store(true, Ordering::SeqCst);
                    server.unblock();
                    return;
                }
            });
        }

        let join = {
            let state = state.clone();
            let shutdown = shutdown.clone();
            let server = server.clone();
            std::thread::spawn(move || {
                accept_loop(&server, &state, &shutdown);
            })
        };

        Ok(ServerHandle {
            port,
            state,
            shutdown,
            join: Some(join),
        })
    }

    /// Signal shutdown and wait for the accept loop to stop.
    pub fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            // Unblock any pending recv by dropping via a self-connect is unneeded:
            // the accept loop uses recv_timeout, so it wakes on its own.
            let _ = j.join();
        }
    }

    /// True once the idle-exit watchdog (or an explicit shutdown) has fired.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Block until the server shuts down (idle-exit or external signal).
    pub fn wait(&self) {
        while !self.shutdown.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The accept loop: recv requests with a timeout (so shutdown is observed), and
/// dispatch each on its own thread.
fn accept_loop(
    server: &Arc<tiny_http::Server>,
    state: &Arc<ServerState>,
    shutdown: &Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(req)) => {
                let state = state.clone();
                std::thread::spawn(move || {
                    router::handle(&state, req);
                });
            }
            Ok(None) => continue, // timed out; re-check shutdown
            Err(_) => return,     // server unblocked / closed
        }
    }
}
