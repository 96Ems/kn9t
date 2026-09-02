//! Acceptance tests for kn9t-plugin (Stage 08).
//!
//! Tests live in `mod plug` so `cargo test plug::handshake` etc. work.

use kn9t_plugin::codec::{write_plugin_msg, HostMsg, PluginDeclaration, PluginMsg};
use kn9t_core::{Content, HookName, HookVeto, Message, ModelRef, MsgId, Role, StopReason, Tokens, Usage};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── shared helpers ────────────────────────────────────────────────────────────

fn channel_pipe() -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    (Box::new(ChannelReader { rx, buf: Vec::new(), pos: 0 }), Box::new(ChannelWriter { tx }))
}

fn make_pipes() -> (
    Box<dyn Read + Send>, Box<dyn Write + Send>,
    Box<dyn Read + Send>, Box<dyn Write + Send>,
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
                Ok(data) => { self.buf = data; self.pos = 0; }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct ChannelWriter { tx: std::sync::mpsc::SyncSender<Vec<u8>> }
impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx.send(buf.to_vec()).map(|_| buf.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn decl(name: &str, hooks: Vec<HookName>) -> PluginDeclaration {
    PluginDeclaration { name: name.to_string(), capabilities: vec![], hooks, tools: vec![], subscribed_events: vec![], provider: None }
}

fn test_usage() -> Usage {
    Usage {
        tokens: Tokens::default(),
        model: ModelRef { provider: "test".to_string(), id: "m".to_string() },
    }
}

fn test_message(text: &str) -> Message {
    Message {
        id: MsgId::new(), role: Role::User,
        content: vec![Content::Text { text: text.to_string() }],
        silent: false,
    }
}

/// Spawn a plugin thread that reads N hook requests and replies with the given bodies.
fn spawn_plugin_multi(
    plugin_read: Box<dyn Read + Send>,
    plugin_write: Box<dyn Write + Send>,
    replies: Vec<serde_json::Value>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(plugin_read);
        let mut writer = plugin_write;
        for reply_body in replies {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
            let msg: HostMsg = match serde_json::from_str(line.trim_end()) {
                Ok(m) => m, Err(_) => break,
            };
            let id = match &msg { HostMsg::Hook { id, .. } => *id, _ => continue };
            let reply = PluginMsg::Result { id, body: reply_body };
            if write_plugin_msg(&mut writer, &reply).is_err() { break; }
        }
    });
}

fn spawn_plugin_responder(
    plugin_read: Box<dyn Read + Send>,
    plugin_write: Box<dyn Write + Send>,
    reply_body: serde_json::Value,
) {
    spawn_plugin_multi(plugin_read, plugin_write, vec![reply_body]);
}

// ── tests ─────────────────────────────────────────────────────────────────────

mod plug {
    use super::*;
    use kn9t_plugin::{
        codec::{read_host_msg, write_host_msg},
        config::{filter_configs, PluginConfig},
        ComposedHookHost, NoOpPluginKv, PluginHost, SpawnTool,
    };
    use kn9t_core::{HookHost, Tool};
    use std::path::Path;

    // ── R-PLUG-040: handshake ─────────────────────────────────────────────────

    #[test]
    fn handshake() {
        let (h_read, h_write, p_read, p_write) = make_pipes();

        let plugin_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            let msg = read_host_msg(&mut reader).expect("plugin: read hello");
            match &msg {
                HostMsg::Hello { proto, kn9t } => {
                    assert_eq!(*proto, 1);
                    assert!(!kn9t.is_empty());
                }
                _ => panic!("expected Hello"),
            }
            let reply = PluginMsg::Hello {
                name: "redact".to_string(),
                capabilities: vec![],
                hooks: vec!["after_tool_call".to_string()],
                tools: vec![],
                events: vec!["MessageAppended".to_string()],
                provider: None,
            };
            write_plugin_msg(&mut writer, &reply).expect("plugin: write hello");
        });

        let mut host_writer = h_write;
        let mut host_reader = BufReader::new(h_read);
        let hello = HostMsg::Hello { proto: 1, kn9t: "0.1.0".to_string() };
        write_host_msg(&mut host_writer, &hello).expect("host: write hello");
        let reply = kn9t_plugin::codec::read_plugin_msg(&mut host_reader)
            .expect("host: read plugin hello");
        match reply {
            PluginMsg::Hello { name, hooks, tools, events, .. } => {
                assert_eq!(name, "redact");
                assert_eq!(hooks, vec!["after_tool_call"]);
                assert!(tools.is_empty());
                assert_eq!(events, vec!["MessageAppended"]);
            }
            _ => panic!("expected Hello reply"),
        }
        plugin_handle.join().unwrap();
    }

    // ── R-PLUG-040 (real spawn): PluginHost::spawn() against kn9t-tools binary ──

    /// plug::spawn_real — PluginHost::spawn() performs the hello/hello handshake
    /// against a real subprocess binary (kn9t-tools), registers its declared tools,
    /// and the host can send Shutdown cleanly.
    #[test]
    fn spawn_real() {
        // Build-time artifact location, NOT runtime plugin discovery (ADR-0004):
        // the server scans only ~/.kn9t/plugins/; here we locate the cargo output.
        // kn9t-tools is a standalone crate (plugins/kn9t-tools) — build it with
        // `cd plugins/kn9t-tools && cargo build`.
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let name = format!("kn9t-tools{ext}");
        // CARGO_MANIFEST_DIR = <repo>/crates/kn9t-plugin; ../.. = <repo>.
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };

        let external = workspace.join("plugins/kn9t-tools").join("target").join(profile).join(&name);
        let legacy = workspace.join("target").join(profile).join(&name);
        let installed = {
            let home = std::env::var("KN9T_HOME").map(std::path::PathBuf::from).ok()
                .or_else(|| {
                    std::env::var("HOME").ok()
                        .map(|h| std::path::PathBuf::from(h).join(".kn9t"))
                });
            match home {
                Some(h) => h.join("plugins").join(&name),
                None => external.clone(),
            }
        };

        let bin_path = [&external, &legacy, &installed]
            .iter()
            .find(|p| p.is_file())
            .map(|p| (*p).clone())
            .unwrap_or(external);

        if !bin_path.exists() {
            eprintln!("skip plug::spawn_real — binary not found at {}", bin_path.display());
            eprintln!("run `cd plugins/kn9t-tools && cargo build` first");
            return;
        }

        let host = PluginHost::spawn(&bin_path, &[], Arc::new(NoOpPluginKv))
            .expect("spawn must succeed");

        // kn9t-tools declares bash, read, edit tools.
        assert!(!host.declaration.name.is_empty(), "plugin must declare a name");
        assert!(
            host.declaration.tools.iter().any(|t| t.name == "bash"),
            "kn9t-tools must declare the bash tool"
        );
        assert!(
            host.declaration.tools.iter().any(|t| t.name == "read"),
            "kn9t-tools must declare the read tool"
        );
        assert!(
            host.declaration.is_streaming(),
            "kn9t-tools must declare streaming capability"
        );

        // Clean shutdown.
        host.shutdown();
    }

    // ── R-PLUG-060: hook surface ──────────────────────────────────────────────

    #[test]
    fn hook_surface() {
        // before_tool_call → allow
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"action": "allow"}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::BeforeToolCall]), Arc::new(NoOpPluginKv));
            let result = host.before_tool_call("bash", &json!({"cmd": "ls"}), Path::new("/"));
            assert!(matches!(result, HookVeto::Allow));
        }
        // after_tool_call → keep
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"action": "keep"}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::AfterToolCall]), Arc::new(NoOpPluginKv));
            let result = host.after_tool_call("bash", &json!({}), vec![Content::Text { text: "hi".to_string() }]);
            assert_eq!(result.len(), 1);
        }
        // before_request → keep
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"action": "keep"}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::BeforeRequest]), Arc::new(NoOpPluginKv));
            let msgs = vec![test_message("hi")];
            let model = ModelRef { provider: "t".to_string(), id: "m".to_string() };
            let result = host.before_request(msgs.clone(), &model, None);
            assert_eq!(result.len(), 1);
        }
        // should_stop_after_turn → continue
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"action": "continue"}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::ShouldStopAfterTurn]), Arc::new(NoOpPluginKv));
            assert!(!host.should_stop_after_turn(StopReason::Stop, &test_usage(), 1));
        }
        // prepare_next_turn → keep
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"action": "keep"}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::PrepareNextTurn]), Arc::new(NoOpPluginKv));
            let patch = host.prepare_next_turn(StopReason::Stop, &test_usage());
            assert!(patch.model.is_none() && patch.thinking.is_none());
        }
        // get_steering → empty
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"messages": []}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));
            assert!(host.get_steering().is_empty());
        }
        // get_followup → empty
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"messages": []}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::GetFollowup]), Arc::new(NoOpPluginKv));
            assert!(host.get_followup().is_empty());
        }
        // get_api_key → null
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            spawn_plugin_responder(p_read, p_write, json!({"key": null}));
            let host = PluginHost::from_io(h_read, h_write, decl("p", vec![HookName::GetApiKey]), Arc::new(NoOpPluginKv));
            assert!(host.get_api_key("openai").is_none());
        }
    }

    // ── R-PLUG-070: composition (real subprocess binaries) ────────────────────

    fn test_plugin_bin() -> std::path::PathBuf {
        let ext = if cfg!(windows) { ".exe" } else { "" };
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug")
            .join(format!("kn9t-test-plugin{ext}"))
    }

    #[test]
    fn composition() {
        let bin = test_plugin_bin();
        if !bin.exists() {
            eprintln!("skip plug::composition — build kn9t-test-plugin first");
            return;
        }

        // ── Pipeline: after_tool_call — A replaces, B sees A's output ──
        {
            let host_a = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "after_tool_call"),
                    ("TEST_PLUGIN_REPLY", r#"{"action":"replace","content":[{"type":"text","text":"from_a"}]}"#),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-a")
            );
            let host_b = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "after_tool_call"),
                    ("TEST_PLUGIN_REPLY", r#"{"action":"keep"}"#),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-b")
            );
            let composed = ComposedHookHost::new(vec![host_a, host_b]);
            let original = vec![Content::Text { text: "original".to_string() }];
            let result = composed.after_tool_call("bash", &json!({}), original);
            // A replaced → "from_a"; B kept it → still "from_a"
            assert_eq!(result.len(), 1);
            match &result[0] {
                Content::Text { text } => assert_eq!(text, "from_a"),
                _ => panic!("expected text"),
            }
        }

        // ── Veto: before_tool_call — first deny wins, B not reached ──
        {
            let host_a = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "before_tool_call"),
                    ("TEST_PLUGIN_REPLY", r#"{"action":"deny","reason":"blocked"}"#),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-a")
            );
            let host_b = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "before_tool_call"),
                    ("TEST_PLUGIN_REPLY", r#"{"action":"allow"}"#),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-b")
            );
            let composed = ComposedHookHost::new(vec![host_a, host_b]);
            let result = composed.before_tool_call("bash", &json!({}), Path::new("/"));
            assert!(matches!(result, HookVeto::Deny { .. }), "expected Deny");
        }

        // ── Collect: get_steering — concat two plugins in order ──
        {
            let msg_a_val = serde_json::to_value(test_message("steer_a")).unwrap();
            let msg_b_val = serde_json::to_value(test_message("steer_b")).unwrap();
            let reply_a = format!(r#"{{"messages":[{}]}}"#, msg_a_val);
            let reply_b = format!(r#"{{"messages":[{}]}}"#, msg_b_val);

            let host_a = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "get_steering"),
                    ("TEST_PLUGIN_REPLY", &reply_a),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-a")
            );
            let host_b = Arc::new(
                PluginHost::spawn(&bin, &[
                    ("TEST_PLUGIN_HOOK",  "get_steering"),
                    ("TEST_PLUGIN_REPLY", &reply_b),
                ], Arc::new(NoOpPluginKv)).expect("spawn plugin-b")
            );
            let composed = ComposedHookHost::new(vec![host_a, host_b]);
            let result = composed.get_steering();
            assert_eq!(result.len(), 2, "two steering messages collected");
        }
    }

    // ── R-PLUG-080: timeouts (real subprocess binary) ─────────────────────────

    #[test]
    fn timeout() {
        use kn9t_plugin::host::default_timeout;

        // Default timeout values (spec R-PLUG-080).
        assert_eq!(default_timeout(HookName::BeforeToolCall),      Duration::from_millis(30_000));
        assert_eq!(default_timeout(HookName::AfterToolCall),        Duration::from_millis(2_000));
        assert_eq!(default_timeout(HookName::BeforeRequest),        Duration::from_millis(2_000));
        assert_eq!(default_timeout(HookName::ShouldStopAfterTurn),  Duration::from_millis(1_000));
        assert_eq!(default_timeout(HookName::PrepareNextTurn),       Duration::from_millis(1_000));
        assert_eq!(default_timeout(HookName::GetSteering),          Duration::from_millis(500));
        assert_eq!(default_timeout(HookName::GetFollowup),          Duration::from_millis(500));
        assert_eq!(default_timeout(HookName::GetApiKey),            Duration::from_millis(5_000));

        let bin = test_plugin_bin();
        if !bin.exists() {
            eprintln!("skip plug::timeout (real subprocess) — build kn9t-test-plugin first");
            // Fall back to the in-process pipe stub so the timeout value assertions
            // above still act as a gate.
            let (h_read, h_write, _p_read, _p_write) = make_pipes();
            let host = PluginHost::from_io(h_read, h_write, decl("slow", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));
            let start = Instant::now();
            let result = host.get_steering();
            let elapsed = start.elapsed();
            assert!(result.is_empty(), "failure posture: empty on timeout");
            assert!(elapsed < Duration::from_millis(600), "timeout too slow: {:?}", elapsed);
            return;
        }

        // Real subprocess that sleeps 5 000 ms — far beyond the 500 ms get_steering
        // timeout. The host must cut it at budget and return the failure posture (empty).
        let host = PluginHost::spawn(&bin, &[
            ("TEST_PLUGIN_HOOK",     "get_steering"),
            ("TEST_PLUGIN_SLEEP_MS", "5000"),
            ("TEST_PLUGIN_REPLY",    r#"{"messages":[]}"#),
        ], Arc::new(NoOpPluginKv)).expect("spawn slow plugin");

        let start = Instant::now();
        let result = host.get_steering();
        let elapsed = start.elapsed();

        assert!(result.is_empty(), "failure posture: empty on timeout");
        assert!(elapsed < Duration::from_millis(600), "timeout too slow: {:?}", elapsed);
    }

    // ── R-PLUG-100: project-local plugin ignored ──────────────────────────────

    #[test]
    fn project_plugin_ignored() {
        let configs = vec![
            PluginConfig {
                name: "user-plugin".to_string(),
                command: "user-plugin-bin".to_string(),
                args: vec![],
                project_local: false,
            },
            PluginConfig {
                name: "project-plugin".to_string(),
                command: "project-plugin-bin".to_string(),
                args: vec![],
                project_local: true,
            },
        ];
        let filtered = filter_configs(configs);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "user-plugin");
    }

    // ── R-PLUG-110: spawn_session ─────────────────────────────────────────────

    #[test]
    fn spawn_session() {
        use kn9t_core::{ToolCtx, Cancel};
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::path::PathBuf;

        // Mock executor: records calls and returns a fixed result.
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();

        let executor = Box::new(move |task: &str, _budget: f64, _tools: Option<Vec<String>>, _model: Option<&str>, _session: Option<&str>| {
            *called_clone.lock().unwrap() = true;
            Ok(format!("done: {task}"))
        });

        let tool = SpawnTool::new(None, None, executor);
        let args = json!({"task": "summarize the repo"});

        // Build a minimal ToolCtx
        let bus = Arc::new(kn9t_core::Bus::new());
        let ctx = ToolCtx {
            cwd: PathBuf::from("/tmp"),
            read: Arc::new(Mutex::new(HashMap::new())),
            bus: bus.clone(),
            call_id: kn9t_core::CallId("test-call".to_string()),
            session: None,
        };
        let cancel = Cancel::new();

        let output = tool.execute(&args, &ctx, &cancel).expect("spawn_session execute");
        assert!(!output.is_error);
        assert!(*called.lock().unwrap());
        match &output.content[0] {
            Content::Text { text } => assert!(text.contains("done:")),
            _ => panic!("expected text"),
        }
    }

    // ── R-PLUG-120: spawn_toolset ─────────────────────────────────────────────

    #[test]
    fn spawn_toolset() {
        use kn9t_core::{ToolCtx, Cancel};
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::path::PathBuf;

        let received_tools: Arc<Mutex<Option<Option<Vec<String>>>>> = Arc::new(Mutex::new(None));
        let received_clone = received_tools.clone();

        let executor = Box::new(move |_task: &str, _budget: f64, tools: Option<Vec<String>>, _model: Option<&str>, _session: Option<&str>| {
            *received_clone.lock().unwrap() = Some(tools);
            Ok("ok".to_string())
        });

        let child_tools = Some(vec!["read".to_string(), "bash".to_string()]);
        let tool = SpawnTool::new(child_tools.clone(), None, executor);
        let args = json!({"task": "do something"});

        let bus = Arc::new(kn9t_core::Bus::new());
        let ctx = ToolCtx {
            cwd: PathBuf::from("/tmp"),
            read: Arc::new(Mutex::new(HashMap::new())),
            bus: bus.clone(),
            call_id: kn9t_core::CallId("test-call".to_string()),
            session: None,
        };
        let cancel = Cancel::new();

        tool.execute(&args, &ctx, &cancel).expect("spawn_toolset execute");

        let got = received_tools.lock().unwrap().clone().unwrap();
        assert_eq!(got, child_tools);
    }

    // ── R-PLUG-130: spawn_budget ──────────────────────────────────────────────

    #[test]
    fn spawn_budget() {
        use kn9t_core::{ToolCtx, Cancel};
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::path::PathBuf;

        let bus = Arc::new(kn9t_core::Bus::new());
        let make_ctx = || ToolCtx {
            cwd: PathBuf::from("/tmp"),
            read: Arc::new(Mutex::new(HashMap::new())),
            bus: bus.clone(),
            call_id: kn9t_core::CallId("test-call".to_string()),
            session: None,
        };
        let cancel = Cancel::new();

        // Case 1: budget_usd_arg > parent_remaining → capped to parent_remaining
        {
            let received_budget: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
            let rb = received_budget.clone();
            let executor = Box::new(move |_task: &str, budget: f64, _tools: Option<Vec<String>>, _model: Option<&str>, _session: Option<&str>| {
                *rb.lock().unwrap() = Some(budget);
                Ok("ok".to_string())
            });
            let tool = SpawnTool::new(None, Some(1.0), executor); // parent has $1 remaining
            let args = json!({"task": "t", "budget_usd": 5.0}); // requests $5
            tool.execute(&args, &make_ctx(), &cancel).unwrap();
            let got = received_budget.lock().unwrap().unwrap();
            assert!((got - 1.0).abs() < 1e-9, "budget should be capped to parent remaining");
        }

        // Case 2: parent_remaining = 0 → error ToolResult
        {
            let executor = Box::new(|_task: &str, _budget: f64, _tools: Option<Vec<String>>, _model: Option<&str>, _session: Option<&str>| {
                Ok("should not run".to_string())
            });
            let tool = SpawnTool::new(None, Some(0.0), executor);
            let args = json!({"task": "t", "budget_usd": 1.0});
            let output = tool.execute(&args, &make_ctx(), &cancel).unwrap();
            assert!(output.is_error, "should be error when budget exhausted");
        }

        // Case 3: no parent budget → use requested budget directly
        {
            let received_budget: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
            let rb = received_budget.clone();
            let executor = Box::new(move |_task: &str, budget: f64, _tools: Option<Vec<String>>, _model: Option<&str>, _session: Option<&str>| {
                *rb.lock().unwrap() = Some(budget);
                Ok("ok".to_string())
            });
            let tool = SpawnTool::new(None, None, executor); // no parent budget
            let args = json!({"task": "t", "budget_usd": 2.5});
            tool.execute(&args, &make_ctx(), &cancel).unwrap();
            let got = received_budget.lock().unwrap().unwrap();
            assert!((got - 2.5).abs() < 1e-9);
        }
    }

    // ── concurrent call tests ─────────────────────────────────────────────────

    /// Test that two concurrent calls don't block each other.
    /// Before the fix: call 2 would block waiting for call 1's lock.
    /// After the fix: both run in parallel.
    #[test]
    fn plug_concurrent_calls_no_block() {
        use kn9t_plugin::PluginHost;

        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));

        // Plugin: respond to calls with delays, but in order received.
        // Call 1 takes 200ms, Call 2 takes 50ms.
        // If blocking: total ~250ms. If concurrent: total ~200ms.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            
            // Read first call
            let mut line1 = String::new();
            reader.read_line(&mut line1).unwrap();
            let msg1: HostMsg = serde_json::from_str(line1.trim_end()).unwrap();
            let id1 = match msg1 { HostMsg::Hook { id, .. } => id, _ => panic!() };
            
            // Read second call (should arrive quickly if not blocked)
            let mut line2 = String::new();
            reader.read_line(&mut line2).unwrap();
            let msg2: HostMsg = serde_json::from_str(line2.trim_end()).unwrap();
            let id2 = match msg2 { HostMsg::Hook { id, .. } => id, _ => panic!() };

            // Respond to call 2 first (faster)
            std::thread::sleep(Duration::from_millis(50));
            let reply2 = PluginMsg::Result { id: id2, body: json!({"messages": []}) };
            write_plugin_msg(&mut writer, &reply2).unwrap();

            // Respond to call 1 later (slower)
            std::thread::sleep(Duration::from_millis(150));
            let reply1 = PluginMsg::Result { id: id1, body: json!({"messages": []}) };
            write_plugin_msg(&mut writer, &reply1).unwrap();
        });

        let host = Arc::new(host);
        let h1 = Arc::clone(&host);
        let h2 = Arc::clone(&host);

        let start = Instant::now();

        // Launch two concurrent calls
        let t1 = std::thread::spawn(move || {
            h1.get_steering()
        });
        let t2 = std::thread::spawn(move || {
            h2.get_steering()
        });

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        let elapsed = start.elapsed();

        assert!(r1.is_empty(), "call 1 should return empty messages");
        assert!(r2.is_empty(), "call 2 should return empty messages");
        
        // If concurrent: ~200ms. If blocking: ~250ms+.
        // Use 230ms threshold to detect blocking.
        assert!(
            elapsed < Duration::from_millis(230),
            "concurrent calls took {:?}, expected <230ms (blocking detected)",
            elapsed
        );
    }

    /// P1 96E-5: PluginHost session context must be isolated per concurrent session.
    /// Before fix: `current_session` is a single Mutex, so session B overwrites A.
    /// After fix: per-call/per-thread context, each hook sees its own session.
    #[test]
    fn p1_96e5_session_context_isolation() {
        use std::sync::Barrier;

        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::BeforeToolCall]), Arc::new(NoOpPluginKv));

        let seen_sessions = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen_sessions.clone();

        // Mock plugin: record session_id from each Hook payload
        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            for _ in 0..2 {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
                let msg: HostMsg = serde_json::from_str(line.trim_end()).unwrap();
                if let HostMsg::Hook { id, payload, .. } = msg {
                    let sess = payload.get("session_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "none".to_string());
                    // Also handle case where session_id is nested as Option<String> -> null
                    // The host sends `session_id: Option<String>` which serializes to null when None
                    // and to "session-A" when Some. So check for null vs string.
                    seen_clone.lock().unwrap().push(sess);
                    let reply = PluginMsg::Result { id, body: json!({"action": "allow"}) };
                    write_plugin_msg(&mut writer, &reply).unwrap();
                }
            }
        });

        let host = Arc::new(host);
        let barrier = Arc::new(Barrier::new(2));

        let h1 = Arc::clone(&host);
        let b1 = Arc::clone(&barrier);
        let t1 = std::thread::spawn(move || {
            h1.set_session("session-A");
            b1.wait(); // wait for both to have set session
            // Small sleep to ensure the other thread's set_session has definitely run
            std::thread::sleep(Duration::from_millis(10));
            h1.before_tool_call("bash", &json!({}), Path::new("/"))
        });

        let h2 = Arc::clone(&host);
        let b2 = Arc::clone(&barrier);
        let t2 = std::thread::spawn(move || {
            h2.set_session("session-B");
            b2.wait();
            std::thread::sleep(Duration::from_millis(10));
            h2.before_tool_call("bash", &json!({}), Path::new("/"))
        });

        let _ = t1.join().unwrap();
        let _ = t2.join().unwrap();

        // Give plugin time to process
        std::thread::sleep(Duration::from_millis(50));
        let seen = seen_sessions.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "plugin should have seen 2 hook calls, got {:?}", seen);
        // Before fix: both will be "session-B" (last writer wins) -> fails
        // After fix: one "session-A", one "session-B" (in any order) -> passes
        assert!(seen.contains(&"session-A".to_string()), "should have seen session-A, got {:?}", seen);
        assert!(seen.contains(&"session-B".to_string()), "should have seen session-B, got {:?}", seen);
        // Also ensure they are distinct
        assert_ne!(seen[0], seen[1], "concurrent sessions must not share context, both saw {:?}", seen[0]);
    }

    /// P1 96E-5: bus isolation — plugin HookFailed and events must go to correct session's bus
    #[test]
    fn p1_96e5_bus_isolation() {
        use kn9t_core::{Bus, Event};
        use std::sync::Barrier;

        let (h_read, h_write, p_read, p_write) = make_pipes();
        // Use GetSteering which has 500ms timeout (vs 30s for BeforeToolCall) so test is fast
        let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));

        // Plugin that never replies -> will timeout and trigger HookFailed (500ms)
        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let _writer = p_write;
            // Read one hook and never reply (let it timeout). Need to handle 2 calls.
            for _ in 0..2 {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
            }
            std::thread::sleep(Duration::from_millis(600));
        });

        let bus_a = Arc::new(Bus::new());
        let bus_b = Arc::new(Bus::new());
        let sub_a = bus_a.subscribe(16);
        let sub_b = bus_b.subscribe(16);

        let host = Arc::new(host);
        let barrier = Arc::new(Barrier::new(2));

        let h1 = Arc::clone(&host);
        let b1 = Arc::clone(&barrier);
        let bus_a_clone = bus_a.clone();
        let t1 = std::thread::spawn(move || {
            h1.set_bus(bus_a_clone);
            h1.set_session("session-A");
            b1.wait();
            std::thread::sleep(Duration::from_millis(10));
            // This will timeout (500ms) and emit HookFailed to bus_a
            let _ = h1.get_steering();
        });

        let h2 = Arc::clone(&host);
        let b2 = Arc::clone(&barrier);
        let bus_b_clone = bus_b.clone();
        let t2 = std::thread::spawn(move || {
            h2.set_bus(bus_b_clone);
            h2.set_session("session-B");
            b2.wait();
            std::thread::sleep(Duration::from_millis(10));
            let _ = h2.get_steering();
        });

        let _ = t1.join().unwrap();
        let _ = t2.join().unwrap();

        // Give time for HookFailed to be emitted
        std::thread::sleep(Duration::from_millis(100));

        // Before fix: both HookFailed would go to the same bus (last set, B), so A would have 0, B would have 2
        // After fix: each bus gets exactly 1 HookFailed for its session
        let a_events: Vec<_> = std::iter::from_fn(|| sub_a.try_recv()).collect();
        let b_events: Vec<_> = std::iter::from_fn(|| sub_b.try_recv()).collect();

        let a_failed = a_events.iter().filter(|e| matches!(e, Event::HookFailed { .. })).count();
        let b_failed = b_events.iter().filter(|e| matches!(e, Event::HookFailed { .. })).count();

        // We expect exactly one HookFailed per bus (the timeout). Before fix, one bus will have 0.
        assert_eq!(a_failed, 1, "bus A should have exactly 1 HookFailed, got {}", a_failed);
        assert_eq!(b_failed, 1, "bus B should have exactly 1 HookFailed, got {}", b_failed);
    }

    /// P1 96E-9: plugin event backlog must not block RPC response processing.
    /// Reader thread must use non-blocking try_send for transient events, so a
    /// noisy plugin cannot stall unrelated hook calls. Before fix, burst of events
    /// fills the bounded channel (64) and reader blocks on `send`, delaying the
    /// subsequent hook Result beyond the hook timeout.
    #[test]
    fn p1_96e9_event_backlog_does_not_block_rpc() {
        use kn9t_core::{Event, LiveEvent, EventSink};

        struct SlowSink {
            events: Mutex<Vec<LiveEvent>>,
            delay: Duration,
        }
        impl EventSink for SlowSink {
            fn emit(&self, e: LiveEvent) {
                // Only slow down transient plugin notifications so HookFailed path stays fast
                if matches!(e, LiveEvent::PluginNotification { .. }) {
                    std::thread::sleep(self.delay);
                }
                self.events.lock().unwrap().push(e);
            }
        }

        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(
            h_read,
            h_write,
            decl("test", vec![HookName::GetSteering]),
            Arc::new(NoOpPluginKv),
        );
        let slow = Arc::new(SlowSink {
            events: Mutex::new(Vec::new()),
            delay: Duration::from_millis(30),
        });
        host.set_session("sess-9");
        host.set_bus(slow.clone());

        // Plugin: waits for hook, then floods 200 transient events before replying.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let msg: HostMsg = serde_json::from_str(line.trim_end()).expect("hook parse");
            let id = match msg {
                HostMsg::Hook { id, .. } => id,
                _ => panic!("expected Hook"),
            };
            for i in 0..200 {
                let ev = PluginMsg::Event {
                    event: json!({"plugin":"test","message":format!("ev {i}"),"session_id":"sess-9"}),
                };
                let _ = write_plugin_msg(&mut writer, &ev);
            }
            // Give reader a moment to start filling channel, then answer the hook.
            std::thread::sleep(Duration::from_millis(20));
            let reply = PluginMsg::Result {
                id,
                body: json!({"messages":[]}),
            };
            let _ = write_plugin_msg(&mut writer, &reply);
            // Keep pipes alive a bit so host can drain
            std::thread::sleep(Duration::from_millis(300));
        });

        let host = Arc::new(host);
        let start = Instant::now();
        let result = host.get_steering();
        let elapsed = start.elapsed();

        // With try_send fix: hook completes quickly (<500 ms timeout) even though event
        // consumer is slow. Before fix: reader blocks on bounded channel, hook times out
        // at 500 ms and returns via failure posture; elapsed >=500 ms.
        assert!(
            elapsed < Duration::from_millis(450),
            "RPC should complete while event consumer slow; took {:?} (expected <450 ms, timeout is 500 ms)",
            elapsed
        );
        assert!(result.is_empty(), "expected empty steering on success");
        // HookFailed would be emitted on timeout; check immediately (HookFailed path does not go through event channel)
        {
            let evs = slow.events.lock().unwrap().clone();
            let hook_failed = evs.iter().filter(|e| matches!(e, LiveEvent::HookFailed { .. })).count();
            assert_eq!(
                hook_failed, 0,
                "hook should not have timed out; HookFailed count={}, elapsed={:?}, events={} (dropped is OK)",
                hook_failed,
                elapsed,
                evs.len()
            );
        }
        // Transient events may be dropped under pressure — give event thread time to drain
        // some of the buffered 64 before asserting.
        std::thread::sleep(Duration::from_millis(400));
        let evs2 = slow.events.lock().unwrap().clone();
        let notif = evs2.iter().filter(|e| matches!(e, LiveEvent::PluginNotification { .. })).count();
        assert!(notif > 0, "at least some notifications should arrive, got {notif}");
        assert!(notif <= 200, "cannot exceed sent");
        // And ensure RPC still completed (no late HookFailed appeared)
        let hook_failed2 = evs2.iter().filter(|e| matches!(e, LiveEvent::HookFailed { .. })).count();
        assert_eq!(hook_failed2, 0, "no HookFailed should appear after drain");
    }

    /// 96E-9: saturation — transient events may be dropped, RPC still completes.
    #[test]
    fn p1_96e9_transient_events_may_be_dropped_under_pressure() {
        use kn9t_core::{LiveEvent, EventSink};

        struct CountingSink {
            count: Mutex<usize>,
        }
        impl EventSink for CountingSink {
            fn emit(&self, e: LiveEvent) {
                if matches!(e, LiveEvent::PluginNotification { .. }) {
                    // Simulate slow consumer without sleeping too long: just count, but we
                    // will fill channel by not draining fast enough via sleep in event thread?
                    // Instead we use a bus that never drains fast? Simpler: use a channel bus
                    // that blocks? But current fix should drop, so count <=200.
                    *self.count.lock().unwrap() += 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(
            h_read,
            h_write,
            decl("test", vec![HookName::GetSteering]),
            Arc::new(NoOpPluginKv),
        );
        let sink = Arc::new(CountingSink { count: Mutex::new(0) });
        host.set_session("sess-9b");
        host.set_bus(sink.clone());

        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let msg: HostMsg = serde_json::from_str(line.trim_end()).unwrap();
            let id = match msg { HostMsg::Hook { id, .. } => id, _ => panic!() };
            for i in 0..150 {
                let ev = PluginMsg::Event {
                    event: json!({"plugin":"t","message":format!("x{i}"),"session_id":"sess-9b"}),
                };
                let _ = write_plugin_msg(&mut writer, &ev);
            }
            let reply = PluginMsg::Result { id, body: json!({"messages":[]}) };
            let _ = write_plugin_msg(&mut writer, &reply);
            std::thread::sleep(Duration::from_millis(200));
        });

        let host = Arc::new(host);
        let start = Instant::now();
        let _ = host.get_steering();
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(450), "RPC must complete even under event flood, took {:?}", elapsed);
        let got = *sink.count.lock().unwrap();
        // Dropped is allowed, so just check not panicked and RPC completed; got may be <150
        assert!(got <= 150, "count {got} should not exceed sent");
    }

    /// 96E-10: malformed protocol message must poison the host.
    /// Before fix: reader `continue`s after parse error, host stays healthy and
    /// next call can still succeed (reader still alive). After fix: host becomes
    /// unhealthy, pending calls fail, new calls fail fast.
    #[test]
    fn p1_96e10_protocol_corruption_marks_unhealthy() {
        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));

        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            // First hook
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 { return; }
            let msg: HostMsg = serde_json::from_str(line.trim_end()).expect("hook parse");
            let _id = match msg { HostMsg::Hook { id, .. } => id, _ => panic!() };
            // Send malformed line instead of a valid Result
            writer.write_all(b"not json at all\n").unwrap();
            writer.flush().unwrap();
            // Keep pipe alive a bit so reader can see the corruption
            std::thread::sleep(Duration::from_millis(300));
            // Try to handle a second hook if host still alive (should not happen after fix)
            let mut line2 = String::new();
            // Non-blocking attempt: host may send second hook; we read with short timeout via try?
            // Use read_line with the same blocking reader - if host sends, we'll reply.
            if reader.read_line(&mut line2).unwrap_or(0) > 0 {
                if let Ok(HostMsg::Hook { id: id2, .. }) = serde_json::from_str(line2.trim_end()) {
                    let reply = PluginMsg::Result { id: id2, body: json!({"messages":[]}) };
                    let _ = write_plugin_msg(&mut writer, &reply);
                }
            }
        });

        let host = Arc::new(host);
        // First call: will see parse error broadcast, returns failure posture (empty)
        let start = Instant::now();
        let r1 = host.get_steering();
        let elapsed1 = start.elapsed();
        // Before fix: this times out at 500ms (parse error -> Err on pending but continue, so pending gets Err quickly? Actually before fix pending does get Err, so elapsed <450ms even before fix. So we can't assert timeout. But health flag is the discriminator.)
        // We just ensure it returned (empty) and did not hang
        assert!(r1.is_empty(), "first call should return failure posture");
        assert!(elapsed1 < Duration::from_millis(600), "first call should not hang, took {:?}", elapsed1);

        // After corruption, host must be unhealthy
        assert!(!host.is_healthy(), "host should be unhealthy after protocol corruption, but is_healthy==true");
        let reason = host.poison_reason().unwrap_or_default();
        assert!(
            reason.contains("protocol") || reason.contains("malformed") || reason.contains("parse") || reason.contains("violation"),
            "poison reason should mention protocol violation/parse, got {:?}", reason
        );

        // New call must fail deterministically / fast, not via 500ms timeout
        let start2 = Instant::now();
        let r2 = host.get_steering();
        let elapsed2 = start2.elapsed();
        assert!(r2.is_empty(), "poisoned host should return failure posture");
        assert!(
            elapsed2 < Duration::from_millis(100),
            "new call after poison should fail fast (<100ms), took {:?}", elapsed2
        );
    }

    /// 96E-10: pending call fails correctly on corruption and new calls remain poisoned
    #[test]
    fn p1_96e10_new_calls_fail_deterministically_after_corruption() {
        let (h_read, h_write, p_read, p_write) = make_pipes();
        let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));

        std::thread::spawn(move || {
            let mut reader = BufReader::new(p_read);
            let mut writer = p_write;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            // Immediately corrupt
            writer.write_all(b"{bad json}\n").unwrap();
            writer.flush().unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });

        let host = Arc::new(host);
        let _ = host.get_steering(); // triggers corruption
        assert!(!host.is_healthy(), "should be poisoned");

        // Multiple subsequent calls all fail fast with same poison reason
        for _ in 0..3 {
            let s = Instant::now();
            let r = host.get_steering();
            let e = s.elapsed();
            assert!(r.is_empty());
            assert!(e < Duration::from_millis(100), "subsequent poisoned call should fail fast, took {:?}", e);
            assert!(!host.is_healthy());
        }
    }

    /// 96E-10: a fresh host after poisoning has a clean stream (restart semantics)
    #[test]
    fn p1_96e10_restarted_host_has_clean_stream() {
        // First host gets poisoned
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));
            std::thread::spawn(move || {
                let mut reader = BufReader::new(p_read);
                let mut writer = p_write;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                writer.write_all(b"!!! not json !!!\n").unwrap();
                writer.flush().unwrap();
                std::thread::sleep(Duration::from_millis(200));
            });
            let _ = host.get_steering();
            assert!(!host.is_healthy(), "first host should be poisoned");
        }
        // Second host with fresh pipes must be healthy and able to serve
        {
            let (h_read, h_write, p_read, p_write) = make_pipes();
            let host = PluginHost::from_io(h_read, h_write, decl("test", vec![HookName::GetSteering]), Arc::new(NoOpPluginKv));
            spawn_plugin_responder(p_read, p_write, json!({"messages":[]}));
            assert!(host.is_healthy(), "fresh host should be healthy");
            let r = host.get_steering();
            assert!(r.is_empty(), "fresh host should serve normally");
            assert!(host.is_healthy(), "fresh host should stay healthy after clean call");
        }
    }

    // ── 96E-17: plugin → host API (host_api capability) ─────────────────────

    #[test]
    fn p1_96e17_host_api_request_roundtrip() {
        use kn9t_plugin::host_api::HostApi;
        let (h_read, h_write, p_read, p_write) = make_pipes();

        // "Plugin" side: send a request, expect an ApiResult reply.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let script = std::thread::spawn(move || {
            // Wait until the host has installed its API handler (no read race).
            ready_rx.recv().unwrap();
            let mut writer = p_write;
            write_plugin_msg(
                &mut writer,
                &PluginMsg::Request {
                    id: 7,
                    op: "session_read".to_string(),
                    payload: json!({"session": "s1", "start": 0, "end": 5}),
                },
            )
            .unwrap();
            let mut reader = BufReader::new(p_read);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let reply: HostMsg = serde_json::from_str(line.trim_end()).unwrap();
            let HostMsg::ApiResult { id, ok, result, error } = reply else {
                panic!("expected ApiResult, got {line}");
            };
            assert_eq!(id, 7, "reply must echo the request id");
            assert!(ok, "handler succeeded, error={error:?}");
            assert!(error.is_none());
            let result = result.expect("ok reply carries a result");
            assert_eq!(result["messages"][0]["seq"], 5, "handler output passes through");
        });

        struct DummyApi;
        impl HostApi for DummyApi {
            fn handle(
                &self,
                plugin: &str,
                session: Option<&str>,
                op: &str,
                payload: &serde_json::Value,
            ) -> Result<serde_json::Value, String> {
                assert_eq!(plugin, "t", "plugin name is passed");
                assert_eq!(session, Some("s1"), "session rides inside the payload");
                assert_eq!(op, "session_read");
                Ok(json!({"messages": [{"seq": payload["end"]}]}))
            }
        }

        let host = PluginHost::from_io(h_read, h_write, decl("t", vec![]), Arc::new(NoOpPluginKv));
        host.set_api_handler(Arc::new(DummyApi));
        ready_tx.send(()).unwrap();
        script.join().unwrap();
        assert!(host.is_healthy());
    }

    #[test]
    fn p1_96e17_host_api_request_error_reply() {
        use kn9t_plugin::host_api::HostApi;
        let (h_read, h_write, p_read, p_write) = make_pipes();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let script = std::thread::spawn(move || {
            ready_rx.recv().unwrap();
            let mut writer = p_write;
            write_plugin_msg(
                &mut writer,
                &PluginMsg::Request {
                    id: 3,
                    op: "nope".to_string(),
                    payload: json!({}),
                },
            )
            .unwrap();
            let mut reader = BufReader::new(p_read);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let reply: HostMsg = serde_json::from_str(line.trim_end()).unwrap();
            let HostMsg::ApiResult { id, ok, result, error } = reply else {
                panic!("expected ApiResult, got {line}");
            };
            assert_eq!(id, 3);
            assert!(!ok, "unknown op must fail");
            assert!(result.is_none());
            assert!(error.unwrap().contains("unknown host API op"));
        });

        struct FailingApi;
        impl HostApi for FailingApi {
            fn handle(
                &self,
                _plugin: &str,
                _session: Option<&str>,
                _op: &str,
                _payload: &serde_json::Value,
            ) -> Result<serde_json::Value, String> {
                Err("unknown host API op \"nope\"".to_string())
            }
        }

        let host = PluginHost::from_io(h_read, h_write, decl("t", vec![]), Arc::new(NoOpPluginKv));
        host.set_api_handler(Arc::new(FailingApi));
        ready_tx.send(()).unwrap();
        script.join().unwrap();
        assert!(host.is_healthy(), "an op error must not poison the connection");
    }

    // ── 96E-16/17: RemoteCompactor delegation over the hook wire ────────────

    #[test]
    fn p1_96e17_remote_compactor_roundtrip() {
        use kn9t_core::{CompactSpan, Compactor as _, ModelRef, SeqRange};
        use kn9t_plugin::RemoteCompactor;

        let (h_read, h_write, p_read, p_write) = make_pipes();
        // The compactor plugin answers compactor_compact with a plan.
        let reply_body = json!({
            "summary": {
                "id": "m1",
                "role": "assistant",
                "content": [{"type": "text", "text": "resume"}],
                "silent": false
            },
            "handoff": {
                "keep": ["t1"],
                "summarize": [],
                "drop": [],
                "resume_actions": ["go on"]
            }
        });
        spawn_plugin_responder(p_read, p_write, reply_body);

        let mut d = decl("compactor", vec![]);
        d.capabilities = vec!["compactor".to_string()];
        let host = PluginHost::from_io(h_read, h_write, d, Arc::new(NoOpPluginKv));
        // The server calls set_session on the turn thread; session_id() then
        // travels in the hook payload.
        host.set_session("sess-1");
        let rc = RemoteCompactor::new(Arc::new(host));

        let span = CompactSpan {
            replaced: SeqRange { start: 1, end: 5 },
            messages: vec![],
        };
        let model = ModelRef { provider: "p".to_string(), id: "m".to_string() };
        let plan = rc.compact(span, &model).expect("compact must succeed");
        let txt = plan.summary.content.iter().find_map(|c| match c {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        });
        assert_eq!(txt.as_deref(), Some("resume"), "summary message parsed from wire");
        let h = plan.handoff.expect("handoff attached");
        assert_eq!(h.keep[0].0, "t1");
        assert_eq!(h.resume_actions, vec!["go on".to_string()]);
    }

    #[test]
    fn p1_96e17_remote_compactor_error_reply() {
        use kn9t_core::{CompactSpan, Compactor as _, ModelRef, SeqRange};
        use kn9t_plugin::RemoteCompactor;

        let (h_read, h_write, p_read, p_write) = make_pipes();
        spawn_plugin_responder(p_read, p_write, json!({"error": "compactor blew up"}));

        let mut d = decl("compactor", vec![]);
        d.capabilities = vec!["compactor".to_string()];
        let host = PluginHost::from_io(h_read, h_write, d, Arc::new(NoOpPluginKv));
        let rc = RemoteCompactor::new(Arc::new(host));
        let span = CompactSpan { replaced: SeqRange { start: 1, end: 5 }, messages: vec![] };
        let model = ModelRef { provider: "p".to_string(), id: "m".to_string() };
        let err = rc.compact(span, &model).map_err(|e| e.clone()).err().expect("error reply must surface as Err");
        assert!(err.contains("blew up"), "plugin error message propagates, got {err}");
    }

} // mod plug
