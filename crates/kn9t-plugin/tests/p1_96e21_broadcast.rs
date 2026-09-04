//! 96E-21 TDD red: plugin events without session_id must never broadcast to all sessions.

use kn9t_core::{EventSink, LiveEvent};
use kn9t_plugin::{
    codec::{write_plugin_msg, PluginMsg},
    NoOpPluginKv, PluginDeclaration, PluginHost,
};
use serde_json::json;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// same in-proc pipe as acceptance.rs
fn channel_pipe() -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    (
        Box::new(ChannelReader {
            rx,
            buf: Vec::new(),
            pos: 0,
        }),
        Box::new(ChannelWriter { tx }),
    )
}
fn make_pipes() -> (
    Box<dyn Read + Send>,
    Box<dyn Write + Send>,
    Box<dyn Read + Send>,
    Box<dyn Write + Send>,
) {
    let (h_read, p_write) = channel_pipe();
    let (p_read, h_write) = channel_pipe();
    (h_read, h_write, p_read, p_write)
}
struct ChannelReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}
impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(d) => {
                    self.buf = d;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
struct ChannelWriter {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
}
impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx
            .send(buf.to_vec())
            .map(|_| buf.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct RecordingSink {
    events: Mutex<Vec<LiveEvent>>,
}
impl EventSink for RecordingSink {
    fn emit(&self, e: LiveEvent) {
        self.events.lock().unwrap().push(e);
    }
}
fn decl(name: &str) -> PluginDeclaration {
    PluginDeclaration {
        name: name.to_string(),
        capabilities: vec![],
        hooks: vec![],
        tools: vec![],
        subscribed_events: vec![],
        provider: None,
    }
}

#[test]
fn broadcast_without_session_id_must_not_leak_to_master() {
    // Master and subagent buses; plugin emits WITHOUT session_id -> must not reach master.
    let (h_read, h_write, p_read, p_write) = make_pipes();
    let host = PluginHost::from_io(h_read, h_write, decl("leaky"), Arc::new(NoOpPluginKv));

    let master_sink = Arc::new(RecordingSink {
        events: Mutex::new(vec![]),
    });
    let child_sink = Arc::new(RecordingSink {
        events: Mutex::new(vec![]),
    });

    // Register both sessions on this thread (set_session + set_bus order matters — host uses TLS).
    host.set_session("master-001");
    host.set_bus(master_sink.clone());
    host.set_session("child-900");
    host.set_bus(child_sink.clone());

    // Keep a reader pretending to be plugin's read side alive so host reader doesn't exit
    let _keep_reader = p_read;

    // Plugin emits an event WITHOUT session_id / sessionId — the bug broadcast to all.
    let mut plugin_writer = p_write;
    let ev = PluginMsg::Event {
        event: json!({"plugin":"leaky","kind":"agents.md","text":"hello"}),
    };
    write_plugin_msg(&mut plugin_writer, &ev).unwrap();
    // Also send a second event with unknown session_id to ensure no broadcast
    let ev2 = PluginMsg::Event {
        event: json!({"plugin":"leaky","kind":"x","session_id":"unknown-999","text":"nope"}),
    };
    write_plugin_msg(&mut plugin_writer, &ev2).unwrap();

    // Give event thread time to route
    std::thread::sleep(Duration::from_millis(200));

    let master_events = master_sink.events.lock().unwrap().len();
    let child_events = child_sink.events.lock().unwrap().len();

    assert_eq!(
        master_events, 0,
        "master must NOT receive untagged plugin event (broadcast leak): got {master_events}"
    );
    assert_eq!(child_events, 0, "child must NOT receive untagged event either (no target) — drop, not broadcast: got {child_events}");
}

#[test]
fn routed_event_with_session_id_goes_only_to_target() {
    let (h_read, h_write, p_read, p_write) = make_pipes();
    let host = PluginHost::from_io(h_read, h_write, decl("leaky2"), Arc::new(NoOpPluginKv));

    let master_sink = Arc::new(RecordingSink {
        events: Mutex::new(vec![]),
    });
    let child_sink = Arc::new(RecordingSink {
        events: Mutex::new(vec![]),
    });

    host.set_session("master-002");
    host.set_bus(master_sink.clone());
    host.set_session("child-901");
    host.set_bus(child_sink.clone());

    let _keep_reader = p_read;
    let mut pw = p_write;
    let ev = PluginMsg::Event {
        event: json!({"plugin":"leaky2","kind":"y","session_id":"master-002","text":"for master"}),
    };
    write_plugin_msg(&mut pw, &ev).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let m = master_sink.events.lock().unwrap().len();
    let c = child_sink.events.lock().unwrap().len();
    assert_eq!(m, 1, "master should receive its targeted event");
    assert_eq!(c, 0, "child must NOT receive master's targeted event");
}

#[test]
fn session_scope_restores_parent_after_child_turn() {
    use kn9t_plugin::{NoOpPluginKv, PluginHost, SessionScope};
    let (h_read, h_write, _p_read, _p_write) = make_pipes();
    let host = PluginHost::from_io(h_read, h_write, decl("scope-test"), Arc::new(NoOpPluginKv));
    host.set_session("parent-001");
    assert_eq!(host.session_id().as_deref(), Some("parent-001"));
    let scope = SessionScope::capture();
    // Simulate child turn overwriting the thread-local (compose_loop sets child)
    host.set_session("child-900");
    assert_eq!(host.session_id().as_deref(), Some("child-900"));
    drop(scope);
    assert_eq!(
        host.session_id().as_deref(),
        Some("parent-001"),
        "SessionScope must restore parent session id"
    );
}
