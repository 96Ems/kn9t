//! [`ServerState`] — the shared, thread-safe wiring of the server (DESIGN §12).
//!
//! This is the one place that names concrete `Store` (`SqliteStore`), tool, and
//! policy types (GI-1 exception). Every `tiny_http` connection thread holds an
//! `Arc<ServerState>`. Interior state (leases, buses, idle counters) is guarded by
//! fine-grained locks so a long SSE backlog read never blocks a write (§12.4).
//!
//! The provider used for turns and auto-titling is injected as `Arc<dyn Provider>`
//! so tests drive the server fully offline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kn9t_core::{Approver, Decision, ModelSpec, Provider, ToolCall, ToolRegistry};
use kn9t_plugin::PluginHost;
use kn9t_store::SqliteStore;

use crate::bus::SessionBuses;
use crate::lease::{LeaseMap, DEFAULT_LEASE_IDLE};
use crate::policy::{ApprovalCache, ApprovalRegistry, InteractiveApprover, NonInteractiveApprover};

/// Grace period after last client disconnects before the server exits.
/// Short enough to feel immediate, long enough to survive a TUI restart.
/// Overridable via `[server] idle_exit_secs` in config.toml (0 = disable).
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(5);

/// ADR-0008 — the approver used when nothing can answer a prompt. Reached only if a policy
/// plugin returned `Ask`, so denying is the honest answer: there is no one to ask.
///
/// Note this is *not* the "no policy installed" path. With no policy plugin the hook layer
/// answers `Allow` and no approver is consulted at all — kn9t runs unguarded by design
/// (ADR-0008 decision 5).
pub struct DenyAllApprover;
impl Approver for DenyAllApprover {
    fn request(&self, _call: &ToolCall, _cwd: &std::path::Path, reason: &str) -> Decision {
        Decision::Deny { reason: format!("approval required ({reason}), no approver configured") }
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
    /// ADR-0008 -- turns a policy plugin's `Ask` into a `Decision`. Not a decider: the
    /// judgement already happened in the plugin. `RwLock` so a non-interactive run can swap
    /// in the deny-on-ask adapter at startup.
    pub approver: std::sync::RwLock<Arc<dyn Approver>>,
    /// Registry for blocking approval requests (DESIGN §10).
    pub approval_registry: Arc<ApprovalRegistry>,
    /// Session + persistent approval cache (scope=session|always).
    pub approval_cache: Arc<ApprovalCache>,
    /// Working directory root (server process cwd), used for the tool context when
    /// a session does not pin its own.
    pub cwd: PathBuf,
    /// Provider-reported budget figure, injectable (gateway `/user/usage`,
    /// R-NBED-040 / R-SRV-120). `None` where unavailable.
    pub provider_reported_budget: Mutex<Option<f64>>,
    /// All model specs loaded from config (GET /models registry, DESIGN §8.2).
    pub model_registry: Vec<ModelSpec>,
    /// Tools registry — populated from external auto-discovered plugins in
    /// `~/.kn9t/plugins/` plus pinned `[[plugin]]` entries (R-PLUG2-110, ADR-0004).
    /// Wrapped in a Mutex for hot-reload (R-PLUG2-100): `POST /plugin/{name}/reload`
    /// swaps the host and rebuilds the registry without restarting the server.
    pub tools: Mutex<ToolRegistry>,
    /// Plugin hosts — for composing hooks from all plugins (discovered + pinned).
    /// Mutex for hot-reload.
    pub plugin_hosts: Mutex<Vec<Arc<PluginHost>>>,
    /// Spawn recipe per plugin declared name — used to respawn on reload (R-PLUG2-100).
    /// `cmd` is the exact argv (binary + args) and `env` the injected vars.
    pub plugin_spawn: Mutex<HashMap<String, (Vec<String>, Vec<(String, String)>)>>,
    /// ADR-0008 -- an in-process `HookHost` that replaces the composed plugin hooks.
    ///
    /// Since ADR-0008 an `Ask` can only originate from a policy plugin, so exercising the
    /// approval flow end-to-end would otherwise require spawning a real subprocess. This seam
    /// lets a test supply the verdict directly. `None` in production, where hooks are always
    /// composed from `plugin_hosts`.
    pub hooks_override: Mutex<Option<Arc<dyn kn9t_core::HookHost>>>,
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
        let approval_cache = Arc::new(ApprovalCache::new(crate::config::global_config_path()));
        let approver: Arc<dyn Approver> = Arc::new(InteractiveApprover::with_cache(
            approval_registry.clone(),
            approval_cache.clone(),
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
            approver: std::sync::RwLock::new(approver),
            approval_registry,
            approval_cache,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            provider_reported_budget: Mutex::new(None),
            model_registry: Vec::new(),
            tools: Mutex::new(tools),
            plugin_hosts: Mutex::new(plugin_hosts),
            plugin_spawn: Mutex::new(HashMap::new()),
            hooks_override: Mutex::new(None),
        }
    }

    /// Snapshot the current tool registry (clone under lock) — used by turns.
    pub fn tools_snapshot(&self) -> ToolRegistry {
        self.tools.lock().expect("tools poisoned").clone()
    }

    /// Snapshot the current plugin hosts (clone under lock).
    pub fn hosts_snapshot(&self) -> Vec<Arc<PluginHost>> {
        self.plugin_hosts.lock().expect("hosts poisoned").clone()
    }

    /// Record the spawn recipe for a plugin (called once at startup after discovery).
    pub fn set_plugin_spawn(&self, name: String, cmd: Vec<String>, env: Vec<(String, String)>) {
        self.plugin_spawn.lock().expect("spawn poisoned").insert(name, (cmd, env));
    }

    /// Hot-reload a plugin by declared name (R-PLUG2-100).
    ///
    /// Steps, per spec:
    /// 1. `cancel` for every in-flight call on that plugin.
    /// 2. wait up to `before_tool_call` timeout for `done` replies.
    /// 3. `shutdown`, close write pipe.
    /// 4. respawn from the same `cmd`.
    /// 5. re-handshake; re-register tools, provider, hooks, event subscriptions.
    ///
    /// In-flight calls that miss step 3 get a synthetic error result at the call site
    /// (the pending channel is dropped / returns `disconnected`).
    pub fn reload_plugin(&self, name: &str) -> Result<(String, usize), String> {
        // 0. Lookup host and spawn recipe (hold lock briefly).
        let (old_host, cmd, env) = {
            let hosts = self.plugin_hosts.lock().expect("hosts poisoned");
            let idx = hosts.iter().position(|h| h.declaration.name == name)
                .ok_or_else(|| format!("plugin {name:?} not found"))?;
            let host = hosts[idx].clone();
            let spawn = self.plugin_spawn.lock().expect("spawn poisoned");
            let (cmd, env) = spawn.get(name)
                .cloned()
                .ok_or_else(|| format!("plugin {name:?} has no spawn recipe (was it a provider plugin? not reloadable via this route)"))?;
            (host, cmd, env)
        };

        crate::log!("hot-reload: plugin '{}' cancel/shutdown ({} in-flight)", name, old_host.pending_count());

        // 1. cancel every in-flight call.
        for id in old_host.pending_ids() {
            old_host.cancel_call(id);
        }

        // 2. wait up to before_tool_call timeout (30s) for done replies.
        let deadline = std::time::Instant::now() + kn9t_plugin::host::default_timeout(kn9t_core::HookName::BeforeToolCall);
        while std::time::Instant::now() < deadline {
            if old_host.pending_count() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if old_host.pending_count() != 0 {
            crate::log!("hot-reload: plugin '{}' still has {} in-flight after timeout — proceeding to shutdown", name, old_host.pending_count());
        }

        // 3. shutdown and close write pipe.
        old_host.shutdown();
        // Give the child a moment to observe shutdown and exit; the reader thread will close.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // 4. respawn from the same cmd.
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        crate::log!("hot-reload: respawning plugin '{}' from {:?}", name, cmd);
        let new_host = crate::tools::spawn_with_cmd_public(&cmd, &env_refs, self.store.clone() as Arc<dyn kn9t_core::PluginKv>)
            .map_err(|e| format!("respawn failed: {e}"))?;
        let new_decl_name = new_host.declaration.name.clone();
        if new_decl_name != name {
            crate::log!("hot-reload: warning: plugin declared name '{}' differs from requested '{}' — using declared name for registry", new_decl_name, name);
        }
        let new_host = Arc::new(new_host);
        let new_tools = crate::tools::extract_tools_public(&new_host);

        // 5. swap host and rebuild registry (dedup, first wins, same as startup).
        {
            let mut hosts = self.plugin_hosts.lock().expect("hosts poisoned");
            if let Some(pos) = hosts.iter().position(|h| h.declaration.name == name) {
                hosts[pos] = new_host.clone();
            } else {
                // Should not happen (we found it earlier), but push for safety.
                hosts.push(new_host.clone());
            }

            // Rebuild tool registry from all current hosts (dedup first wins).
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut all_tools: Vec<Arc<dyn kn9t_core::Tool>> = Vec::new();
            // Pinned order is already in hosts vec (pinned first, then discovered sorted).
            // Respect that order for dedup.
            for h in hosts.iter() {
                let tools_for_host = if h.declaration.name == new_decl_name {
                    new_tools.clone()
                } else {
                    crate::tools::extract_tools_public(h)
                };
                for t in tools_for_host {
                    let n = t.spec().name.clone();
                    if seen.contains(&n) {
                        continue;
                    }
                    seen.insert(n);
                    all_tools.push(t);
                }
            }
            let registry = ToolRegistry::from_tools(all_tools);
            let n = registry.len();
            *self.tools.lock().expect("tools poisoned") = registry;
            crate::log!("hot-reload: plugin '{}' re-registered, total tools now {}", name, n);
            return Ok((new_decl_name, n));
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
    pub fn with_approver(self, a: Arc<dyn Approver>) -> Self {
        *self.approver.write().expect("approver poisoned") = a;
        self
    }
    /// ADR-0008 -- pick the approval adapter. This is *not* a risk decision: it only says who
    /// can answer an `Ask` that a policy plugin already raised. Interactive runs prompt; `-p`
    /// and CI have nobody to prompt, so an unanswerable ask is denied.
    pub fn approver_for(
        interactive: bool,
        registry: &Arc<ApprovalRegistry>,
        cache: &Arc<ApprovalCache>,
    ) -> Arc<dyn Approver> {
        if interactive {
            Arc::new(InteractiveApprover::with_cache(registry.clone(), cache.clone()))
        } else {
            Arc::new(NonInteractiveApprover::new(cache.clone()))
        }
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

    /// ADR-0008 -- install an in-process hook host, replacing plugin-composed hooks.
    /// Test-only seam: production hooks come from `plugin_hosts`.
    pub fn with_hooks_override(self, h: Arc<dyn kn9t_core::HookHost>) -> Self {
        *self.hooks_override.lock().expect("hooks_override poisoned") = Some(h);
        self
    }

    /// The hook host for a turn: the override if one was installed, else `None` so the
    /// caller composes from `plugin_hosts`.
    pub fn hooks_override_snapshot(&self) -> Option<Arc<dyn kn9t_core::HookHost>> {
        self.hooks_override.lock().expect("hooks_override poisoned").clone()
    }

    /// ADR-0008 -- snapshot the current approver for a turn.
    pub fn approver_snapshot(&self) -> Arc<dyn Approver> {
        self.approver.read().expect("approver poisoned").clone()
    }
}
