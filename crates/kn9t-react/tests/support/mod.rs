//! Shared test scaffolding for the `rct::*` acceptance tests: a recording bus, a stub
//! store, stub policies, and helpers to build in-memory replay fixtures. No network, no
//! keys, no spend -- everything is driven by `ReplayProvider` over synthetic fixtures.
//!
//! Tools are loaded from the real `kn9t-tools` plugin subprocess (R-PLUG2-110), not stubs.
#![allow(dead_code)] // a shared toolbox; not every helper is used by every test

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kn9t_core::{
    Cache, CacheMode, CompactSpan, Decision, Event, EventSink, Message, ModelRef, ModelSpec, Policy,
    Price, Quirks, RequestPlan, SessionId, SessionSnapshot, StoreErr, Tool, ToolCall, ToolSpec,
    ToolRegistry,
};
use kn9t_plugin::{PluginHost, RemoteTool};
use kn9t_provider_replay::fixture::Fixture;
use kn9t_provider_replay::ReplayProvider;

/// A bus that records every published event for assertions.
#[derive(Clone, Default)]
pub struct RecordingBus {
    pub events: Arc<Mutex<Vec<Event>>>,
}

impl RecordingBus {
    pub fn new() -> Self {
        RecordingBus {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn kinds(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(event_tag)
            .collect()
    }
    pub fn snapshot(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink for RecordingBus {
    fn emit(&self, e: Event) {
        self.events.lock().unwrap().push(e);
    }
}

pub fn event_tag(e: &Event) -> String {
    match e {
        Event::SessionForked { .. } => "SessionForked",
        Event::MessageAppended { .. } => "MessageAppended",
        Event::ModelChanged { .. } => "ModelChanged",
        Event::Compacted { .. } => "Compacted",
        Event::UsageRecorded { .. } => "UsageRecorded",
        Event::TurnStarted { .. } => "TurnStarted",
        Event::TextDelta { .. } => "TextDelta",
        Event::ThinkingDelta { .. } => "ThinkingDelta",
        Event::ToolArgsDelta { .. } => "ToolArgsDelta",
        Event::ToolStarted { .. } => "ToolStarted",
        Event::ToolProgress { .. } => "ToolProgress",
        Event::ToolFinished { .. } => "ToolFinished",
        Event::ApprovalRequest { .. } => "ApprovalRequest",
        Event::TurnEnded { .. } => "TurnEnded",
        Event::HookFailed { .. } => "HookFailed",
        Event::Error { .. } => "Error",
        Event::RetryAttempt { .. } => "RetryAttempt",
        Event::TurnStatus { .. } => "TurnStatus",
        Event::TitleChanged { .. } => "TitleChanged",
        Event::PluginNotification { .. } => "PluginNotification",
    }
    .to_string()
}

/// A store stub that:
/// - records every appended event (assigning a monotonic seq);
/// - serves a scripted sequence of `plan_request` results (so compaction can be forced).
pub struct StubStore {
    pub appended: Arc<Mutex<Vec<Event>>>,
    seq: Arc<Mutex<u64>>,
    plans: Arc<Mutex<std::collections::VecDeque<PlanScript>>>,
    default_plan: PlanScript,
    pub plan_calls: Arc<Mutex<u32>>,
}

/// What one `plan_request` returns.
#[derive(Clone)]
pub struct PlanScript {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub compact: bool,
}

impl PlanScript {
    pub fn plain(messages: Vec<Message>) -> Self {
        PlanScript {
            messages,
            tools: Vec::new(),
            compact: false,
        }
    }
    pub fn compacting() -> Self {
        PlanScript {
            messages: Vec::new(),
            tools: Vec::new(),
            compact: true,
        }
    }
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
}

impl StubStore {
    pub fn new(default_plan: PlanScript) -> Self {
        StubStore {
            appended: Arc::new(Mutex::new(Vec::new())),
            seq: Arc::new(Mutex::new(0)),
            plans: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            default_plan,
            plan_calls: Arc::new(Mutex::new(0)),
        }
    }
    /// Queue scripted plan responses; when exhausted, `default_plan` is used.
    pub fn script(mut self, plans: Vec<PlanScript>) -> Self {
        self.plans = Arc::new(Mutex::new(plans.into()));
        self
    }
    pub fn appended_tags(&self) -> Vec<String> {
        self.appended.lock().unwrap().iter().map(event_tag).collect()
    }
}

impl kn9t_core::Store for StubStore {
    fn plan_request(&self, _session: &SessionId) -> Result<RequestPlan, StoreErr> {
        *self.plan_calls.lock().unwrap() += 1;
        let script = self
            .plans
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.default_plan.clone());
        let cache: Vec<Cache> = Vec::new();
        let compact = if script.compact {
            Some(CompactSpan {
                replaced: kn9t_core::SeqRange { start: 0, end: 1 },
                messages: script.messages.clone(),
            })
        } else {
            None
        };
        Ok(RequestPlan {
            system: None,
            messages: script.messages,
            tools: script.tools,
            cache,
            compact,
        })
    }

    fn append(&self, _session: &SessionId, event: Event) -> Result<u64, StoreErr> {
        let mut seq = self.seq.lock().unwrap();
        *seq += 1;
        self.appended.lock().unwrap().push(event);
        Ok(*seq)
    }

    fn snapshot(&self, _session: &SessionId) -> Result<SessionSnapshot, StoreErr> {
        Ok(SessionSnapshot {
            head_seq: *self.seq.lock().unwrap(),
            ctx_tokens: 0,
            cost_usd: 0.0,
            model: test_model_ref(),
        })
    }
}

/// Policy that allows everything.
pub struct AllowAll;
impl Policy for AllowAll {
    fn check(&self, _call: &ToolCall, _cwd: &std::path::Path) -> Decision {
        Decision::Allow
    }
}

/// Policy that denies everything with a fixed reason.
pub struct DenyAll(pub String);
impl Policy for DenyAll {
    fn check(&self, _call: &ToolCall, _cwd: &std::path::Path) -> Decision {
        Decision::Deny {
            reason: self.0.clone(),
        }
    }
}

pub fn test_model_ref() -> ModelRef {
    ModelRef {
        provider: "replay".to_string(),
        id: "test".to_string(),
    }
}

pub fn test_model_spec() -> ModelSpec {
    ModelSpec {
        r#ref: test_model_ref(),
        api_id: "test".to_string(),
        ctx_window: 100_000,
        max_out: 8_000,
        price: Price {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 1.25,
        },
        cache: CacheMode::None,
        streaming: true,
        quirks: Quirks::default(),
    }
}

/// Build a native `kind: replay` fixture from raw SSE body text.
pub fn fixture_from_body(body: &str) -> Fixture {
    Fixture {
        kind: "replay".to_string(),
        status: 200,
        content_type: "text/event-stream".to_string(),
        chunks: Vec::new(),
        extra: Vec::new(),
        body: body.as_bytes().to_vec(),
    }
}

/// Build a native fixture that ends with a `terminal-error:` header value.
pub fn fixture_with_terminal(body: &str, terminal: &str) -> Fixture {
    let mut f = fixture_from_body(body);
    f.extra.push(("terminal-error".to_string(), terminal.to_string()));
    f
}

pub fn replay(fixture: Fixture) -> Arc<ReplayProvider> {
    Arc::new(ReplayProvider::from_fixture_struct(fixture))
}

pub fn empty_read_map() -> kn9t_react::ReadMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// A provider that serves a scripted sequence of outcomes, one per `stream()` call, so the
/// truncation ladder (a sequence of `Truncated` then success) and abort paths can be driven
/// deterministically. Each entry is either a fixture or a pre-stream error.
pub enum StreamScript {
    Fixture(Fixture),
    PreStreamErr(kn9t_core::ProvErr),
}

pub struct ScriptedProvider {
    scripts: Mutex<std::collections::VecDeque<StreamScript>>,
    pub calls: Arc<Mutex<u32>>,
}

impl ScriptedProvider {
    pub fn new(scripts: Vec<StreamScript>) -> Self {
        ScriptedProvider {
            scripts: Mutex::new(scripts.into()),
            calls: Arc::new(Mutex::new(0)),
        }
    }
}

impl kn9t_core::Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }
    fn stream(
        &self,
        req: &kn9t_core::Request,
        cancel: &kn9t_core::Cancel,
    ) -> Result<
        Box<dyn Iterator<Item = Result<kn9t_core::Chunk, kn9t_core::ProvErr>> + Send>,
        kn9t_core::ProvErr,
    > {
        *self.calls.lock().unwrap() += 1;
        let next = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("ScriptedProvider ran out of scripts");
        match next {
            StreamScript::Fixture(f) => {
                let p = ReplayProvider::from_fixture_struct(f);
                p.stream(req, cancel)
            }
            StreamScript::PreStreamErr(e) => Err(e),
        }
    }
}

// â”€â”€ Tools plugin helper (R-PLUG2-110) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Locate the built `kn9t-tools` plugin binary.
///
/// NOTE â€” this is **build-time artifact location, NOT runtime plugin discovery**.
/// The server discovers plugins only in `~/.kn9t/plugins/` (ADR-0004); it NEVER
/// scans the repo's `plugins/` directory. These tests handshake the plugin
/// *directly* (no server) to validate the ReAct loop, so they must locate the
/// cargo build artifact. Searching `plugins/kn9t-tools/target/` here is fine â€”
/// that is a build artifact path, not a runtime scan.
///
/// Search order:
///   1. `plugins/kn9t-tools/target/{debug,release}/kn9t-tools[.exe]` â€” the
///      standalone crate build. `kn9t-tools` is no longer a workspace member;
///      build it with `cd plugins/kn9t-tools && cargo build`.
///   2. `target/{debug,release}/kn9t-tools[.exe]` â€” legacy: in case the binary
///      was built into the workspace target dir instead (pre-move layout).
///   3. `<KN9T_HOME|~/.kn9t>/plugins/kn9t-tools[.exe]` â€” installed location
///      where bootstrap copies it on first run.
fn locate_tools_binary() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent().unwrap()  // crates/
        .parent().unwrap()  // <repo root>
        .to_path_buf();
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let name = format!("kn9t-tools{ext}");

    // (a) the external standalone build â€” the plugin now lives at
    //     plugins/kn9t-tools and is built on its own.
    let external = workspace_root
        .join("plugins").join("kn9t-tools").join("target").join(profile).join(&name);
    // (b) legacy workspace-target build.
    let legacy = workspace_root.join("target").join(profile).join(&name);

    // (c) installed location â€” where bootstrap puts it.
    let installed = {
        let home = std::env::var("KN9T_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .ok()
                    .map(|h| PathBuf::from(h).join(".kn9t"))
            });
        match home {
            Some(h) => h.join("plugins").join(&name),
            None => external.clone(), // placeholder; no install location known
        }
    };

    [&external, &legacy, &installed]
        .iter()
        .find(|p| p.is_file())
        .map(|p| (*p).clone())
        .unwrap_or(external)
}

/// Spawn the `kn9t-tools` plugin and return a `ToolRegistry` with all declared tools.
/// Panics if the binary is not found. Build it first:
/// `cd plugins/kn9t-tools && cargo build` (it is a standalone crate, not a workspace
/// member â€” `cargo build -p kn9t-tools` from the root does not work).
pub fn spawn_tools_registry() -> (Arc<PluginHost>, ToolRegistry) {
    let binary = locate_tools_binary();
    if !binary.exists() {
        panic!(
            "kn9t-tools binary not found at {}. Build it with \
             `cd plugins/kn9t-tools && cargo build`, or install it into \
             ~/.kn9t/plugins/ (first `kn9t` run does this automatically).",
            binary.display()
        );
    }

    let host = PluginHost::spawn(&binary, &[], Arc::new(kn9t_plugin::NoOpPluginKv))
        .expect("failed to spawn kn9t-tools plugin");

    let host = Arc::new(host);
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    for spec in &host.declaration.tools {
        let tool_spec = kn9t_core::ToolSpec {
            name: spec.name.clone(),
            description: spec.description.clone(),
            schema: spec.schema.clone(), hidden: false, effects: spec.effects.clone(), policy: Default::default()
        };
        let remote = RemoteTool::new(tool_spec, host.clone());
        tools.push(Arc::new(remote));
    }

    (host, ToolRegistry::from_tools(tools))
}

/// Get a specific tool from the registry by name.
pub fn get_tool(registry: &ToolRegistry, name: &str) -> Arc<dyn Tool> {
    registry.get(name)
        .cloned()
        .unwrap_or_else(|| panic!("tool '{}' not found in registry", name))
}
