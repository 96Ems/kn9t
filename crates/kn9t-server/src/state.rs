//! [`ServerState`] — the shared, thread-safe wiring of the server (DESIGN §12).
//!
//! This is the one place that names concrete `Store` (`SqliteStore`), tool, and
//! policy types (GI-1 exception). Every `tiny_http` connection thread holds an
//! `Arc<ServerState>`. Interior state (leases, buses, idle counters) is guarded by
//! fine-grained locks so a long SSE backlog read never blocks a write (§12.4).
//!
//! The provider used for turns and auto-titling is injected as `Arc<dyn Provider>`
//! so tests drive the server fully offline.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kn9t_core::{Decision, ModelSpec, Policy, Provider, ToolCall, ToolRegistry};
use kn9t_plugin::PluginHost;
use kn9t_store::SqliteStore;

use crate::bus::SessionBuses;
use crate::classify::BashPolicy;
use crate::lease::{LeaseMap, DEFAULT_LEASE_IDLE};
use crate::policy::{ApprovalRegistry, InteractivePolicy};

/// Grace period after last client disconnects before the server exits.
/// Short enough to feel immediate, long enough to survive a TUI restart.
/// Overridable via `[server] idle_exit_secs` in config.toml (0 = disable).
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(5);

/// A permissive policy — the server delegates approval to hooks/clients via the
/// `/approve` route (DESIGN §12.1); the base policy allows, and denials arrive as
/// explicit approval decisions. (The real TUI/plugin policy layers land in later
/// stages; this is the wiring default.)
pub struct AllowPolicy;
impl Policy for AllowPolicy {
    fn check(&self, _call: &ToolCall, _cwd: &std::path::Path) -> Decision {
        Decision::Allow
    }
}

/// Idle / activity accounting for R-SRV-080. The server exits when there is **no
/// attached client and no running turn** for `idle_exit`. Any activity (attach,
/// turn start, write) stamps `last_activity`.
pub struct IdleTracker {
    attached_clients: AtomicU64,
    running_turns: AtomicU64,
    last_activity: Mutex<Instant>,
    idle_exit: Duration,
}

impl IdleTracker {
    pub fn new(idle_exit: Duration) -> Self {
        IdleTracker {
            attached_clients: AtomicU64::new(0),
            running_turns: AtomicU64::new(0),
            last_activity: Mutex::new(Instant::now()),
            idle_exit,
        }
    }

    pub fn touch(&self) {
        *self.last_activity.lock().expect("idle poisoned") = Instant::now();
    }

    pub fn client_attached(&self) {
        self.attached_clients.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }
    pub fn client_detached(&self) {
        self.attached_clients.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }
    pub fn attached_count(&self) -> u64 {
        self.attached_clients.load(Ordering::SeqCst)
    }

    pub fn turn_started(&self) {
        self.running_turns.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }
    pub fn turn_ended(&self) {
        self.running_turns.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }
    pub fn running_turns(&self) -> u64 {
        self.running_turns.load(Ordering::SeqCst)
    }

    /// R-SRV-080 — exit when no client is attached and no turn is running,
    /// after a short grace period since the last detach.
    ///
    /// - If `idle_exit` is zero: never exit (disabled).
    /// - If any client is still attached: stay up regardless of idle time.
    /// - If a turn is running: stay up (client may reconnect to see the result).
    /// - Otherwise: exit once `idle_exit` has elapsed since last activity.
    pub fn should_exit(&self) -> bool {
        if self.idle_exit.is_zero() { return false; }
        if self.attached_count() > 0 { return false; }
        if self.running_turns() > 0  { return false; }
        let last = *self.last_activity.lock().expect("idle poisoned");
        last.elapsed() >= self.idle_exit
    }

    pub fn idle_exit_period(&self) -> Duration {
        self.idle_exit
    }

    pub fn last_activity_elapsed(&self) -> Duration {
        self.last_activity.lock().expect("idle poisoned").elapsed()
    }
}

/// The shared server state. All fields are `Send + Sync`; connection threads share
/// it through an `Arc`.
pub struct ServerState {
    pub store: Arc<SqliteStore>,
    pub buses: SessionBuses,
    pub leases: LeaseMap,
    pub idle: IdleTracker,
    pub token: String,
    /// Set by `POST /stop` — the watchdog detects this and exits cleanly.
    pub stop_requested: AtomicBool,
    /// Provider used for auto-titling and running turns. `None` disables both
    /// (routes still function; a `prompt` without a provider is a no-op turn).
    pub provider: Option<Arc<dyn Provider>>,
    /// All providers by name, for model switching.
    pub providers: std::collections::HashMap<String, Arc<dyn Provider>>,
    /// Default model spec for new sessions and titling.
    pub default_model: Option<ModelSpec>,
    /// Policy for tool dispatch inside turns.
    pub policy: Arc<dyn Policy>,
    /// Registry for blocking approval requests (DESIGN §10).
    pub approval_registry: Arc<ApprovalRegistry>,
    /// Working directory root (server process cwd), used for the tool context when
    /// a session does not pin its own.
    pub cwd: PathBuf,
    /// Provider-reported budget figure, injectable (gateway `/user/usage`,
    /// R-NBED-040 / R-SRV-120). `None` where unavailable.
    pub provider_reported_budget: Mutex<Option<f64>>,
    /// All model specs loaded from config (GET /models registry, DESIGN §8.2).
    pub model_registry: Vec<ModelSpec>,
    /// Tools registry — populated from the kn9t-tools plugin subprocess (R-PLUG2-110).
    pub tools: ToolRegistry,
    /// Plugin hosts — for composing hooks from all plugins.
    pub plugin_hosts: Vec<Arc<PluginHost>>,
}

impl ServerState {
    /// Build state with the mandatory store and token; everything else optional.
    pub fn new(
        store: Arc<SqliteStore>,
        token: String,
        tools: ToolRegistry,
        plugin_hosts: Vec<Arc<PluginHost>>,
    ) -> Self {
        let approval_registry = Arc::new(ApprovalRegistry::new());
        let policy: Arc<dyn Policy> = Arc::new(InteractivePolicy::new(
            BashPolicy::default(),
            approval_registry.clone(),
        ));
        ServerState {
            store,
            buses: SessionBuses::new(),
            leases: LeaseMap::new(DEFAULT_LEASE_IDLE),
            idle: IdleTracker::new(DEFAULT_IDLE_EXIT),
            token,
            stop_requested: AtomicBool::new(false),
            provider: None,
            providers: std::collections::HashMap::new(),
            default_model: None,
            policy,
            approval_registry,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            provider_reported_budget: Mutex::new(None),
            model_registry: Vec::new(),
            tools,
            plugin_hosts,
        }
    }

    pub fn with_lease_idle(mut self, d: Duration) -> Self {
        self.leases = LeaseMap::new(d);
        self
    }
    pub fn with_idle_exit(mut self, d: Duration) -> Self {
        self.idle = IdleTracker::new(d);
        self
    }
    pub fn with_provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.provider = Some(p);
        self
    }
    pub fn with_policy(mut self, p: Arc<dyn Policy>) -> Self {
        self.policy = p;
        self
    }
    pub fn with_providers(mut self, providers: Vec<(String, Arc<dyn Provider>)>) -> Self {
        self.providers = providers.into_iter().collect();
        self
    }
    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }
    pub fn with_default_model(mut self, m: ModelSpec) -> Self {
        self.default_model = Some(m);
        self
    }
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = cwd;
        self
    }
    pub fn with_provider_budget(self, b: f64) -> Self {
        *self.provider_reported_budget.lock().unwrap() = Some(b);
        self
    }
}
