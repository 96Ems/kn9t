//! The tool-call payload must carry the session's `cwd`.
//!
//! `RemoteTool::execute` sent `{tool, args, session}` and nothing else, so the SDK's
//! `ToolCallCtx::cwd` was always `None` and a plugin tool fell back to its own process
//! directory -- the one the *server* was started in, shared by every session at once.
//!
//! The symptom was specific enough to mislead: hooks *do* receive `cwd` (the host sets it per
//! turn), so `AGENTS.md` loaded from the correct project while `bash pwd` printed the server's
//! directory. Everything driven by hooks looked right; only tools were wrong.
//!
//! `ToolCtx::cwd` is authoritative -- the ReAct loop takes it from `RunParams`, which the
//! server fills from the session row -- so the fix is to forward it instead of letting the
//! plugin guess.

#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use kn9t_core::{Bus, CallId, Cancel, Tool, ToolCtx, ToolSpec};
use kn9t_plugin::codec::{write_plugin_msg, HostMsg, PluginDeclaration, PluginMsg};
use kn9t_plugin::{NoOpPluginKv, PluginHost, RemoteTool};
use serde_json::json;

// ── in-memory pipes (same shape as acceptance.rs, kept local to this file) ────

struct ChannelReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}
impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(data) => {
                    self.buf = data;
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

fn probe_spec() -> ToolSpec {
    ToolSpec {
        name: "probe".into(),
        description: String::new(),
        schema: json!({ "type": "object" }),
        hidden: false,
        effects: vec![],
        policy: Default::default(),
    }
}

// ── harness ──────────────────────────────────────────────────────────────────

/// Drive one tool call through the host and return the payload the plugin received.
fn payload_for_tool_call(cwd: &std::path::Path) -> serde_json::Value {
    let (h_read, h_write, p_read, p_write) = make_pipes();

    let seen = Arc::new(Mutex::new(serde_json::Value::Null));
    let seen_clone = seen.clone();

    // Mock plugin: capture the invocation payload, then answer so `execute` returns.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(p_read);
        let mut writer = p_write;
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let msg: HostMsg = match serde_json::from_str(line.trim_end()) {
            Ok(m) => m,
            Err(_) => return,
        };
        if let HostMsg::Hook { id, payload, .. } = msg {
            *seen_clone.lock().unwrap() = payload;
            let _ = write_plugin_msg(
                &mut writer,
                &PluginMsg::Result {
                    id,
                    body: json!({
                        "content": [{"type": "text", "text": "ok"}],
                        "is_error": false
                    }),
                },
            );
        }
    });

    let host = Arc::new(PluginHost::from_io(
        h_read,
        h_write,
        decl("t"),
        Arc::new(NoOpPluginKv),
    ));

    let tool = RemoteTool::new(probe_spec(), host);
    let ctx = ToolCtx {
        cwd: cwd.to_path_buf(),
        read: Arc::new(Mutex::new(std::collections::HashMap::new())),
        bus: Arc::new(Bus::new()),
        call_id: CallId("c1".into()),
    };
    let _ = tool.execute(&json!({}), &ctx, &Cancel::new());

    // Bind before returning: the guard must drop inside this scope.
    let captured = seen.lock().unwrap().clone();
    captured
}

// ── tests ────────────────────────────────────────────────────────────────────

/// The bug: `cwd` was simply absent from the payload.
#[test]
fn a_tool_call_payload_carries_the_session_cwd() {
    let raw = if cfg!(windows) {
        r"C:\work\project-a"
    } else {
        "/work/project-a"
    };
    let payload = payload_for_tool_call(std::path::Path::new(raw));

    let got = payload.get("cwd").and_then(|v| v.as_str());
    assert!(
        got.is_some(),
        "the tool payload has no `cwd`, so every plugin tool falls back to the server's own \
         directory: `bash pwd` and relative paths then ignore the session. payload was {payload}"
    );
    assert_eq!(
        got.unwrap(),
        raw,
        "the payload must carry the session's cwd verbatim"
    );
}

/// The fields that were already there must keep working.
#[test]
fn the_existing_payload_fields_are_untouched() {
    let raw = if cfg!(windows) { r"C:\tmp" } else { "/tmp" };
    let payload = payload_for_tool_call(std::path::Path::new(raw));

    assert_eq!(
        payload.get("tool").and_then(|v| v.as_str()),
        Some("probe"),
        "tool name still present"
    );
    assert!(payload.get("args").is_some(), "args still present");
    assert!(
        payload.as_object().unwrap().contains_key("session"),
        "session key still present, so a plugin can tell 'no session' from 'field missing'"
    );
}

/// A path with spaces and backslashes must survive the wire intact -- Windows paths are the
/// common case, and a mangled one silently sends the tool somewhere else.
#[test]
fn a_windows_style_cwd_survives_the_wire() {
    let raw = if cfg!(windows) {
        r"C:\Users\some one\My Project"
    } else {
        "/home/some one/My Project"
    };
    let payload = payload_for_tool_call(std::path::Path::new(raw));
    assert_eq!(payload.get("cwd").and_then(|v| v.as_str()), Some(raw));
}
