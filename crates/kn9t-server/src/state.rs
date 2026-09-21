//! [`ServerState`] — the shared, thread-safe wiring of the server (DESIGN §12).
//!
//! The one place naming concrete `Store`/tool/policy types (GI-1 exception); every connection
//! thread holds an `Arc`. Fine-grained locks keep a long SSE backlog read from blocking a write
//! (§12.4). The provider is injected as `Arc<dyn Provider>`, so tests run fully offline.

use kn9t_macros::safe_expect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use kn9t_core::{Approver, Cancel, Decision, ModelSpec, Provider, ToolCall, ToolRegistry};
use kn9t_plugin::PluginHost;
use kn9t_store::SqliteStore;

use crate::bus::SessionBuses;
use crate::interaction::InteractionRegistry;
use crate::lease::{LeaseMap, DEFAULT_LEASE_IDLE};
use crate::policy::{ApprovalCache, ApprovalRegistry, InteractiveApprover, NonInteractiveApprover};
use crate::tools::SpawnRecipe;

/// Grace period after the last client disconnects (long enough to survive a TUI restart);
/// `[server] idle_exit_secs` overrides, `0` disables.
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(5);

/// ADR-0008 — the approver used when nothing can answer a prompt: reached only if a policy
/// plugin returned `Ask`, so deny is the honest answer. Not the "no policy installed" path —
/// then the hook layer allows directly (ADR-0008 §5).
pub struct DenyAllApprover;
impl Approver for DenyAllApprover {
    fn request(
        &self,
        _call: &ToolCall,
        _cwd: &std::path::Path,
        reason: &str,
        _ctx: &kn9t_core::ApprovalCtx,
    ) -> Decision {
        Decision::Deny {
            reason: format!("approval required ({reason}), no approver configured"),
        }
    }
}

/// Idle accounting for R-SRV-080: exit when no client is attached and no turn is running for
/// `idle_exit`. Any activity (attach, turn start, write) stamps `last_activity`.
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
        *safe_expect!(self.last_activity.lock(), "idle poisoned") = Instant::now();
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

    /// R-SRV-080 — exit once `idle_exit` has elapsed with no attached client and no running
    /// turn; a zero period disables exit.
    pub fn should_exit(&self) -> bool {
        if self.idle_exit.is_zero() {
            return false;
        }
        if self.attached_count() > 0 {
            return false;
        }
        if self.running_turns() > 0 {
            return false;
        }
        let last = *safe_expect!(self.last_activity.lock(), "idle poisoned");
        last.elapsed() >= self.idle_exit
    }

    pub fn idle_exit_period(&self) -> Duration {
        self.idle_exit
    }

    pub fn last_activity_elapsed(&self) -> Duration {
        safe_expect!(self.last_activity.lock(), "idle poisoned").elapsed()
    }
}

/// The shared server state; connection threads share it through an `Arc`.
pub struct ServerState {
    pub store: Arc<SqliteStore>,
    pub buses: Arc<SessionBuses>,
    pub leases: LeaseMap,
    pub idle: IdleTracker,
    pub token: String,
    /// Set by `POST /stop` — the watchdog detects this and exits cleanly.
    pub stop_requested: AtomicBool,
    /// Provider for turns and auto-titling; `None` disables both. `RwLock` for hot-reload
    /// (R-SRV-CFG-100); never hold the guard across a provider call — clone the `Arc` out first.
    pub provider: RwLock<Option<Arc<dyn Provider>>>,
    /// All providers by name, for model switching. `RwLock` for hot-reload.
    pub providers: RwLock<std::collections::HashMap<String, Arc<dyn Provider>>>,
    /// Default model spec for new sessions and titling. `RwLock` for hot-reload.
    pub default_model: RwLock<Option<ModelSpec>>,
    /// Model used to title sessions (config `title_model` / explicit `default_model`).
    pub title_model: RwLock<Option<ModelSpec>>,
    /// ADR-0008 — turns a policy plugin's `Ask` into a `Decision`; the judgement already
    /// happened in the plugin. `RwLock` so a non-interactive run can swap in deny-on-ask.
    pub approver: std::sync::RwLock<Arc<dyn Approver>>,
    /// Registry for blocking approval requests (DESIGN §10).
    pub approval_registry: Arc<ApprovalRegistry>,
    /// Session + persistent approval cache (scope=session|always).
    pub approval_cache: Arc<ApprovalCache>,
    /// generic client-to-host interaction registry (opaque JSON payloads).
    pub interaction_registry: Arc<InteractionRegistry>,
    /// Working directory root (server process cwd), used for the tool context when
    /// a session does not pin its own.
    pub cwd: PathBuf,
    /// Provider-reported budget from the gateway `/user/usage` (R-NBED-040 / R-SRV-120);
    /// `None` where unavailable.
    pub provider_reported_budget: Mutex<Option<f64>>,
    /// All model specs loaded from config (GET /models registry, DESIGN §8.2).
    /// `RwLock` for config hot-reload (R-SRV-CFG-100).
    pub model_registry: RwLock<Vec<ModelSpec>>,
    /// Tools from auto-discovered plugins in `~/.kn9t/plugins/` plus pinned `[[plugin]]`
    /// entries (R-PLUG2-110, ADR-0004). Mutex for hot-reload (R-PLUG2-100).
    pub tools: Mutex<ToolRegistry>,
    /// Plugin hosts for composing hooks. Mutex for hot-reload.
    pub plugin_hosts: Mutex<Vec<Arc<PluginHost>>>,
    /// Spawn recipe per plugin declared name — used to respawn on reload (R-PLUG2-100).
    /// `cmd` is the exact argv (binary + args) and `env` the injected vars.
    pub plugin_spawn: Mutex<HashMap<String, SpawnRecipe>>,
    /// Plugin hosts backing `kind = "plugin"` providers. R-SRV-CFG-100: kept so
    /// `reload_config` can shut down the old subprocess instead of leaking one per reload.
    pub provider_hosts: Mutex<Vec<(String, Arc<PluginHost>)>>,
    /// Per-session `Cancel` handles for `abort` (R-SRV-060). Per-server state, not a process-global
    /// `static`, so two servers in one test process do not share it.
    ///
    /// B5/B6: the `(turn_id, Cancel)` pair makes every write a compare-and-swap on the id, so a
    /// stale teardown or ESC is inert.
    pub aborts: Mutex<HashMap<String, (u64, Cancel)>>,
    /// B5/B6 — monotonic turn id source. Server-wide rather than per-session so an id is
    /// never ambiguous, even across sessions.
    pub(crate) next_turn_id: AtomicU64,
    /// Server-side timing knobs from `[server]`; the right values depend on the deployment,
    /// not on kn9t.
    pub timeouts: crate::config::ServerTimeouts,
    /// ADR-0008 — an in-process `HookHost` seam replacing the composed plugin hooks, so a test
    /// can supply an approval verdict without spawning a real policy subprocess. `None` in
    /// production.
    pub hooks_override: Mutex<Option<Arc<dyn kn9t_core::HookHost>>>,
    /// Per-session tools just re-enabled and not yet announced (`POST /session/{id}/tools`
    /// fills it; the next turn drains a one-shot `<system-reminder>`). Transient by design —
    /// a missed notice is harmless, so it is not event-sourced.
    pub pending_reactivation: Mutex<HashMap<String, std::collections::HashSet<String>>>,
    /// Per-session steering queue. `POST /session/{id}/steer` adds here; the turn drains it
    /// via `get_steering()` after tool_results, keeping `[tool_use] -> [tool_result] -> [steer]`.
    pub pending_steering: Mutex<HashMap<String, Vec<kn9t_core::Message>>>,
    /// True while plugins load in the background (the server binds immediately).
    /// Plugin-dependent routes return 503 meanwhile; the TUI polls `GET /health` first.
    plugins_loading: AtomicBool,
    /// plugins currently `Stopped` (subprocess reaped, recipe kept). Their tools stay
    /// in the registry so the level-1 cache prefix (§8.4.2) is not invalidated; they are refused
    /// at execution via `ToolSource::blocked()`, so a stop takes effect mid-turn.
    stopped_plugins: Mutex<std::collections::HashSet<String>>,
    /// per-tool `hidden` overrides over the plugin-declared `ToolSpec.hidden`; applied
    /// in `tools_snapshot`, so a flip lands on the next model call.
    hidden_overrides: Mutex<HashMap<String, bool>>,
    /// plugins already reported crashed, announced once per transition. The scan runs
    /// before every model call, so without this a poisoned host would re-broadcast forever.
    crash_announced: Mutex<std::collections::HashSet<String>>,
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
        let interaction_registry = Arc::new(InteractionRegistry::new());
        let approver: Arc<dyn Approver> = Arc::new(InteractiveApprover::with_cache(
            approval_registry.clone(),
            approval_cache.clone(),
        ));
        let buses = Arc::new(SessionBuses::new());
        // durable events reach the bus only through this observer (made the
        // live sink transient-only); called after every `Store::append`, outside the conn lock.
        {
            let buses_for_echo = buses.clone();
            store.set_after_append(Some(Arc::new(move |session, event| {
                buses_for_echo.publish(&session.0, event.clone());
            })));
        }
        ServerState {
            store,
            buses,
            leases: LeaseMap::new(DEFAULT_LEASE_IDLE),
            idle: IdleTracker::new(DEFAULT_IDLE_EXIT),
            token,
            stop_requested: AtomicBool::new(false),
            provider: RwLock::new(None),
            providers: RwLock::new(std::collections::HashMap::new()),
            default_model: RwLock::new(None),
            title_model: RwLock::new(None),
            approver: std::sync::RwLock::new(approver),
            approval_registry,
            approval_cache,
            interaction_registry,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            provider_reported_budget: Mutex::new(None),
            model_registry: RwLock::new(Vec::new()),
            tools: Mutex::new(tools),
            plugin_hosts: Mutex::new(plugin_hosts),
            plugin_spawn: Mutex::new(HashMap::new()),
            provider_hosts: Mutex::new(Vec::new()),
            aborts: Mutex::new(HashMap::new()),
            next_turn_id: AtomicU64::new(1),
            timeouts: crate::config::ServerTimeouts::default(),
            hooks_override: Mutex::new(None),
            pending_reactivation: Mutex::new(HashMap::new()),
            pending_steering: Mutex::new(HashMap::new()),
            plugins_loading: AtomicBool::new(false),
            stopped_plugins: Mutex::new(std::collections::HashSet::new()),
            hidden_overrides: Mutex::new(HashMap::new()),
            crash_announced: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// False while plugins are still loading; plugin-dependent routes return 503.
    pub fn plugins_ready(&self) -> bool {
        !self.plugins_loading.load(Ordering::SeqCst)
    }

    /// Mark plugins loading (true) or ready (false); called by the background loader thread.
    pub fn set_plugins_loading(&self, loading: bool) {
        self.plugins_loading.store(loading, Ordering::SeqCst);
    }

    /// Queue a steering message (`POST /steer`); drained by `get_steering()` after tool_results.
    pub fn queue_steering(&self, session: &str, msg: kn9t_core::Message) {
        let mut map = self
            .pending_steering
            .lock()
            .expect("pending_steering poisoned");
        map.entry(session.to_owned()).or_default().push(msg);
    }

    /// Drain all pending steering messages for the given session. Called by
    /// the HookHost wrapper's `get_steering()` implementation.
    pub fn drain_steering(&self, session: &str) -> Vec<kn9t_core::Message> {
        let mut map = self
            .pending_steering
            .lock()
            .expect("pending_steering poisoned");
        map.remove(session).unwrap_or_default()
    }

    /// Snapshot the tool registry, applying `hidden_overrides` so a flip shows up
    /// next call; the plugin's declared `ToolSpec.hidden` stays its default.
    pub fn tools_snapshot(&self) -> ToolRegistry {
        let registry = safe_expect!(self.tools.lock(), "tools poisoned").clone();
        let overrides = safe_expect!(self.hidden_overrides.lock(), "hidden poisoned").clone();
        if overrides.is_empty() {
            return registry;
        }
        // Order is preserved (GI-3): only the `hidden` flag differs.
        ToolRegistry::from_tools(
            registry
                .iter()
                .map(|t| match overrides.get(&t.spec().name) {
                    Some(&hidden) if hidden != t.spec().hidden => {
                        Arc::new(crate::tools::HiddenOverride::new(t.clone(), hidden))
                            as Arc<dyn kn9t_core::Tool>
                    }
                    _ => t.clone(),
                })
                .collect(),
        )
    }

    /// tool names refused at execution because their plugin is stopped. Derived
    /// from `stopped_plugins`, not stored, so a rename or reload cannot leave a stale entry.
    pub fn blocked_tools(&self) -> std::collections::HashSet<String> {
        let stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned").clone();
        if stopped.is_empty() {
            return std::collections::HashSet::new();
        }
        safe_expect!(self.tools.lock(), "tools poisoned")
            .iter()
            .filter(|t| t.plugin().is_some_and(|p| stopped.contains(p)))
            .map(|t| t.spec().name.clone())
            .collect()
    }

    /// is this plugin currently stopped?
    pub fn is_plugin_stopped(&self, name: &str) -> bool {
        safe_expect!(self.stopped_plugins.lock(), "stopped poisoned").contains(name)
    }

    /// force a tool's `hidden` flag, visible from the next `tools_snapshot`
    /// (including mid-turn). Cache cost is accepted by design.
    pub fn set_tool_hidden(&self, name: &str, hidden: bool) {
        safe_expect!(self.hidden_overrides.lock(), "hidden poisoned")
            .insert(name.to_string(), hidden);
    }

    /// reveal or re-hide every tool owned by `plugin`, returning the affected names.
    /// The server has no opinion about which plugin deserves lazy visibility; a plugin can
    /// only name itself via the `tool_visibility` host_api op.
    pub fn set_plugin_hidden(&self, plugin: &str, hidden: bool) -> Vec<String> {
        let names: Vec<String> = self.plugin_tool_names(plugin);
        {
            let mut overrides = safe_expect!(self.hidden_overrides.lock(), "hidden poisoned");
            for n in &names {
                overrides.insert(n.clone(), hidden);
            }
        }
        if !names.is_empty() {
            crate::log!(
                "visibility: plugin '{}' tools hidden={} ({} tools)",
                plugin,
                hidden,
                names.len()
            );
        }
        names
    }

    /// the tool names owned by one plugin. Used to scope a plugin's own
    /// visibility changes to its own tools.
    pub fn plugin_tool_names(&self, plugin: &str) -> Vec<String> {
        safe_expect!(self.tools.lock(), "tools poisoned")
            .iter()
            .filter(|t| t.plugin() == Some(plugin))
            .map(|t| t.spec().name.clone())
            .collect()
    }

    /// `(name, running, healthy, poison_reason)` per plugin, for the `plugin_health`
    /// op. Healthy means the reader thread saw no protocol violation — the server's own
    /// observation, so a plugin cannot fake another's.
    pub fn plugin_health(&self) -> Vec<(String, bool, bool, Option<String>)> {
        let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").clone();
        let stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned").clone();
        hosts
            .iter()
            .map(|h| {
                let name = h.name();
                let running = !stopped.contains(&name);
                (name, running, h.is_healthy(), h.poison_reason())
            })
            .collect()
    }

    /// stop a plugin without forgetting its spawn recipe: cancel in-flight, wait,
    /// `shutdown`, no respawn. The host and its tools stay in the registry (removing them would
    /// rewrite the `tools` array and bust the level-1 cache); `blocked_tools()` refuses calls.
    pub fn stop_plugin(self: &Arc<Self>, name: &str) -> Result<String, String> {
        let host = {
            let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            hosts
                .iter()
                .find(|h| h.name() == name)
                .cloned()
                .ok_or_else(|| format!("plugin {name:?} not found"))?
        };
        {
            let spawn = safe_expect!(self.plugin_spawn.lock(), "spawn poisoned");
            if !spawn.contains_key(name) {
                return Err(format!(
                    "plugin {name:?} has no spawn recipe (provider plugins are not stoppable via this route)"
                ));
            }
        }
        {
            let mut stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned");
            if stopped.contains(name) {
                return Err(format!("plugin {name:?} already stopped"));
            }
            // Marked before shutdown so a turn cannot dispatch into a dying subprocess.
            stopped.insert(name.to_string());
        }

        crate::log!(
            "stop: plugin '{}' cancel/shutdown ({} in-flight)",
            name,
            host.pending_count()
        );
        for id in host.pending_ids() {
            host.cancel_call(id);
        }
        let deadline = std::time::Instant::now()
            + kn9t_plugin::host::default_timeout(kn9t_core::HookName::BeforeToolCall);
        while std::time::Instant::now() < deadline {
            if host.pending_count() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if host.pending_count() != 0 {
            crate::log!(
                "stop: plugin '{}' still has {} in-flight after timeout — proceeding to shutdown",
                name,
                host.pending_count()
            );
        }
        host.shutdown();
        std::thread::sleep(std::time::Duration::from_millis(50));
        crate::log!("stop: plugin '{}' stopped", name);
        self.announce_plugin_state(name, "stopped", String::new());
        Ok(name.to_string())
    }

    /// start a stopped plugin from its known recipe. Unlike `POST /plugin/load`, an
    /// unknown name is a 404 here, never a silent spawn.
    pub fn start_plugin(self: &Arc<Self>, name: &str) -> Result<(String, usize), String> {
        {
            let spawn = safe_expect!(self.plugin_spawn.lock(), "spawn poisoned");
            if !spawn.contains_key(name) {
                return Err(format!("plugin {name:?} not found"));
            }
        }
        if !self.is_plugin_stopped(name) {
            return Err(format!("plugin {name:?} already running"));
        }
        // `reload_plugin` owns the respawn/re-handshake/registry-rebuild sequence; reuse
        // it rather than growing a second copy that can drift out of step with it.
        let (declared, tools) = self.reload_plugin(name)?;
        {
            let mut stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned");
            stopped.remove(name);
            stopped.remove(&declared);
        }
        crate::log!(
            "start: plugin '{}' started, {} tools total",
            declared,
            tools
        );
        self.announce_plugin_state(&declared, "started", String::new());
        Ok((declared, tools))
    }

    /// notice poisoned hosts and announce each once. Called before every model call,
    /// so a crash is observable inside the running turn; it reports only, the observer decides.
    pub fn scan_plugin_health(&self) {
        let broken: Vec<(String, Option<String>)> =
            safe_expect!(self.plugin_hosts.lock(), "hosts poisoned")
                .iter()
                .filter(|h| !h.is_healthy())
                .map(|h| (h.name(), h.poison_reason()))
                .collect();
        if broken.is_empty() {
            return;
        }
        let fresh: Vec<(String, Option<String>)> = {
            let mut announced = safe_expect!(self.crash_announced.lock(), "crash poisoned");
            broken
                .into_iter()
                .filter(|(name, _)| announced.insert(name.clone()))
                .collect()
        };
        for (name, reason) in fresh {
            // A poisoned host may carry no reason string; "protocol violation" is what the
            // reader thread means by poisoning one, so say that rather than nothing.
            let reason = reason.unwrap_or_else(|| "protocol violation".to_string());
            crate::log!("health: plugin '{}' unhealthy: {}", name, reason);
            self.announce_plugin_state(&name, "crashed", reason);
        }
    }

    /// publish a lifecycle fact to SSE clients and subscribed plugins. The server
    /// only states what happened: a plugin wanting to react subscribes to `plugin_state` and
    /// calls `tool_visibility` on itself.
    fn announce_plugin_state(&self, plugin: &str, state: &str, error: String) {
        let event = kn9t_core::Event::PluginState {
            plugin: plugin.to_string(),
            state: state.to_string(),
            error,
        };
        self.buses.broadcast_all(event.clone());
        // A stopped plugin cannot receive anything, and a plugin does not need to be told
        // about its own transition — it is the one that just went through it.
        self.notify_plugins(&event, Some(plugin));
    }

    /// the plugin inventory the agent (and the TUI) can read: declared name,
    /// running state, and the tools each one contributes.
    pub fn plugin_inventory(&self) -> Vec<(String, bool, Vec<String>)> {
        let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").clone();
        let stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned").clone();
        let registry = safe_expect!(self.tools.lock(), "tools poisoned").clone();
        hosts
            .iter()
            .map(|h| {
                let name = h.name();
                let tools: Vec<String> = registry
                    .iter()
                    .filter(|t| t.plugin() == Some(name.as_str()))
                    .map(|t| t.spec().name.clone())
                    .collect();
                let running = !stopped.contains(&name);
                (name, running, tools)
            })
            .collect()
    }

    /// Snapshot the current plugin hosts (clone under lock).
    pub fn hosts_snapshot(&self) -> Vec<Arc<PluginHost>> {
        safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").clone()
    }

    /// Record the spawn recipe for a plugin (called once at startup after discovery).
    pub fn set_plugin_spawn(&self, name: String, cmd: Vec<String>, env: Vec<(String, String)>) {
        self.plugin_spawn
            .lock()
            .expect("spawn poisoned")
            .insert(name, (cmd, env));
    }

    /// Hot-reload a plugin by declared name (R-PLUG2-100): cancel in-flight calls, wait for
    /// `done`, `shutdown`, respawn from the same `cmd`, then re-handshake and re-register.
    /// Calls that miss the shutdown get a synthetic error at the call site.
    pub fn reload_plugin(self: &Arc<Self>, name: &str) -> Result<(String, usize), String> {
        // 0. Lookup host and spawn recipe (hold lock briefly).
        let (old_host, cmd, env) = {
            let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            let idx = hosts
                .iter()
                .position(|h| h.name() == name)
                .ok_or_else(|| format!("plugin {name:?} not found"))?;
            let host = hosts[idx].clone();
            let spawn = safe_expect!(self.plugin_spawn.lock(), "spawn poisoned");
            let (cmd, env) = spawn.get(name)
                .cloned()
                .ok_or_else(|| format!("plugin {name:?} has no spawn recipe (was it a provider plugin? not reloadable via this route)"))?;
            (host, cmd, env)
        };

        crate::log!(
            "hot-reload: plugin '{}' cancel/shutdown ({} in-flight)",
            name,
            old_host.pending_count()
        );

        // 1. cancel every in-flight call.
        for id in old_host.pending_ids() {
            old_host.cancel_call(id);
        }

        // 2. wait up to before_tool_call timeout (30s) for done replies.
        let deadline = std::time::Instant::now()
            + kn9t_plugin::host::default_timeout(kn9t_core::HookName::BeforeToolCall);
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
        let env_refs: Vec<(&str, &str)> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        crate::log!("hot-reload: respawning plugin '{}' from {:?}", name, cmd);
        let new_host = crate::tools::spawn_with_cmd_public(
            &cmd,
            &env_refs,
            self.store.clone() as Arc<dyn kn9t_core::PluginKv>,
        )
        .map_err(|e| format!("respawn failed: {e}"))?;
        let new_decl_name = new_host.name();
        if new_decl_name != name {
            crate::log!("hot-reload: warning: plugin declared name '{}' differs from requested '{}' — using declared name for registry", new_decl_name, name);
        }
        let new_host = Arc::new(new_host);
        // the respawned host gets the plugin ? host API handler too.
        new_host.set_api_handler(Arc::new(crate::host_api::ServerHostApi {
            state: self.clone(),
        }));
        let new_tools = crate::tools::extract_tools_public(&new_host);

        // 5. swap host and rebuild registry (dedup, first wins, same as startup).
        let (new_decl_name, n) = {
            let mut hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            if let Some(pos) = hosts.iter().position(|h| h.name() == name) {
                hosts[pos] = new_host.clone();
            } else {
                // Should not happen (we found it earlier), but push for safety.
                hosts.push(new_host.clone());
            }

            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut all_tools: Vec<Arc<dyn kn9t_core::Tool>> = Vec::new();
            // Hosts are already ordered (pinned first, then discovered sorted); dedup respects it.
            for h in hosts.iter() {
                let tools_for_host = if h.name() == new_decl_name {
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
            *safe_expect!(self.tools.lock(), "tools poisoned") = registry;
            crate::log!(
                "hot-reload: plugin '{}' re-registered, total tools now {}",
                name,
                n
            );
            // a reload ends with a running subprocess, so clear the stopped flag here
            // too — `POST /plugin/{name}/reload` may target a stopped plugin directly.
            {
                let mut stopped = safe_expect!(self.stopped_plugins.lock(), "stopped poisoned");
                stopped.remove(name);
                stopped.remove(new_decl_name.as_str());
            }
            // A fresh process means a *future* crash is a new fact; without clearing, recovery
            // would silently disarm crash reporting for the rest of the session.
            {
                let mut announced = safe_expect!(self.crash_announced.lock(), "crash poisoned");
                announced.remove(name);
                announced.remove(new_decl_name.as_str());
            }
            (new_decl_name, n)
        };

        // Announce only after the `plugin_hosts` guard is dropped: `announce_plugin_state`
        // fans out through `notify_plugins`, which re-locks `plugin_hosts`. A `std::sync::Mutex`
        // is not reentrant, so announcing while holding the guard deadlocks the caller and
        // hangs the HTTP request.
        self.announce_plugin_state(new_decl_name.as_str(), "reloaded", String::new());
        Ok((new_decl_name, n))
    }

    /// Hot-load a NEW plugin (unlike `reload_plugin`, which replaces one), specified inline
    /// (cmd + env) or from config.toml, and add it to the registry.
    pub fn load_plugin(
        self: &Arc<Self>,
        cmd: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<(String, usize), String> {
        if cmd.is_empty() {
            return Err("empty command".to_string());
        }

        // Check if this plugin is already loaded (by comparing cmd[0]).
        {
            let spawn = safe_expect!(self.plugin_spawn.lock(), "spawn poisoned");
            for (name, (existing_cmd, _)) in spawn.iter() {
                if !existing_cmd.is_empty() && existing_cmd[0] == cmd[0] {
                    return Err(format!(
                        "plugin with cmd {:?} already loaded as '{}'",
                        cmd[0], name
                    ));
                }
            }
        }

        let env_refs: Vec<(&str, &str)> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        crate::log!("hot-load: spawning new plugin from {:?}", cmd);

        let new_host = crate::tools::spawn_with_cmd_public(
            &cmd,
            &env_refs,
            self.store.clone() as Arc<dyn kn9t_core::PluginKv>,
        )
        .map_err(|e| format!("spawn failed: {e}"))?;

        let declared_name = new_host.name();

        // Check if a plugin with this declared name already exists.
        {
            let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            if hosts.iter().any(|h| h.name() == declared_name) {
                // Shutdown the just-spawned host before returning error.
                new_host.shutdown();
                return Err(format!(
                    "plugin '{}' already loaded (declared name collision)",
                    declared_name
                ));
            }
        }

        let new_host = Arc::new(new_host);

        // Install API handler.
        new_host.set_api_handler(Arc::new(crate::host_api::ServerHostApi {
            state: self.clone(),
        }));

        // Install declare callback.
        {
            let state_for_cb = self.clone();
            new_host.set_on_declare(Box::new(move |plugin_name, _decl, added, removed| {
                state_for_cb.on_plugin_declare(plugin_name, added, removed);
            }));
        }

        let new_tools = crate::tools::extract_tools_public(&new_host);
        let tools_count = new_tools.len();

        // Record spawn recipe for future reload.
        self.plugin_spawn
            .lock()
            .expect("spawn poisoned")
            .insert(declared_name.clone(), (cmd, env));

        // Add host and rebuild registry.
        {
            let mut hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            hosts.push(new_host.clone());

            // Rebuild tool registry (dedup first wins).
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut all_tools: Vec<Arc<dyn kn9t_core::Tool>> = Vec::new();
            for h in hosts.iter() {
                let tools_for_host = if h.name() == declared_name {
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
            let total = registry.len();
            *safe_expect!(self.tools.lock(), "tools poisoned") = registry;
            crate::log!(
                "hot-load: plugin '{}' loaded with {} tools, total tools now {}",
                declared_name,
                tools_count,
                total
            );
        }

        // Broadcast event so TUI can refresh.
        let tool_names: Vec<String> = new_tools.iter().map(|t| t.spec().name.clone()).collect();
        let event = kn9t_core::Event::PluginDeclared {
            plugin: declared_name.clone(),
            tools_added: tool_names,
            tools_removed: vec![],
        };
        self.buses.broadcast_all(event.clone());
        // subscribed plugins hear about it too and decide for themselves when to
        // reveal tools via `tool_visibility`; not sent to the newcomer — it has not subscribed.
        self.notify_plugins(&event, Some(&declared_name));

        Ok((declared_name, tools_count))
    }

    /// Re-read config.toml and spawn any `[[plugin]]` entries not already loaded, returning
    /// the newly loaded ones.
    pub fn load_plugins_from_config(self: &Arc<Self>) -> Result<Vec<(String, usize)>, String> {
        let config_path = crate::config::global_config_path();
        let config = crate::config::load(&config_path)?;

        let mut loaded = Vec::new();

        for plugin_cfg in &config.plugins {
            // Skip disabled plugins.
            if plugin_cfg.disabled {
                continue;
            }

            // Skip plugins without a cmd (env-only overrides).
            let cmd = match &plugin_cfg.cmd {
                Some(c) if !c.is_empty() => c.clone(),
                _ => continue,
            };

            // Check if already loaded (by full cmd, not just interpreter).
            {
                let spawn = safe_expect!(self.plugin_spawn.lock(), "spawn poisoned");
                let already_loaded = spawn.values().any(|(existing_cmd, _)| *existing_cmd == cmd);
                if already_loaded {
                    continue;
                }
            }

            match self.load_plugin(cmd, plugin_cfg.env.clone()) {
                Ok((name, tools)) => {
                    loaded.push((name, tools));
                }
                Err(e) => {
                    crate::log!(
                        "hot-load from config: plugin '{}' failed: {}",
                        plugin_cfg.name,
                        e
                    );
                }
            }
        }

        Ok(loaded)
    }

    /// R-SRV-CFG-100 — re-read `config.toml` and swap `providers`, `provider_hosts`,
    /// `model_registry` and `default_model` in place; returns `(providers, models)` counts.
    /// Not reloaded: `[[plugin]]` (R-PLUG2-100 lifecycle), `[policy] mode` (read live) and
    /// `[server] idle_exit_secs` (built at startup). In-flight turns hold their own cloned
    /// provider/model, so it applies next turn. Cost: `config::load` re-fetches `/v1/models`.
    pub fn reload_config(self: &Arc<Self>) -> Result<(usize, usize), String> {
        let path = crate::config::global_config_path();
        let resolved = crate::config::load(&path)?;

        if resolved.providers.is_empty() {
            return Err("config resolved to zero providers; keeping current config".into());
        }

        let old_hosts: Vec<(String, Arc<PluginHost>)> = {
            let hosts = safe_expect!(self.provider_hosts.lock(), "provider_hosts poisoned");
            hosts.clone()
        };

        let n_providers = resolved.providers.len();
        let n_models = resolved.models.len();

        // Re-register every spec with the store so sessions pinning a model pick up
        // edited ctx/max_out/price. `register_model_spec` overwrites by key.
        for spec in &resolved.models {
            self.store.register_model_spec(spec.clone());
        }

        let default_spec = crate::config::pick_default_model(&resolved);

        // Swap. Order matters: providers before default_model, so a turn that reads
        // default_model can always resolve its provider by name.
        *self.providers.write().expect("providers poisoned") =
            resolved.providers.iter().cloned().collect();

        if let Some(spec) = &default_spec {
            if let Some((_, p)) = resolved
                .providers
                .iter()
                .find(|(name, _)| name == &spec.r#ref.provider)
            {
                *self.provider.write().expect("provider poisoned") = Some(p.clone());
            }
            *self.default_model.write().expect("default_model poisoned") = Some(spec.clone());
            crate::log!(
                "config-reload: default model {}:{} (ctx {})",
                spec.r#ref.provider,
                spec.r#ref.id,
                spec.ctx_window
            );
        }

        // `title_model` change applies to the next title; call before `set_models` moves them.
        if let Some(spec) = crate::config::pick_title_model(&resolved) {
            *self.title_model.write().expect("title_model poisoned") = Some(spec.clone());
            crate::log!(
                "config-reload: title model {}:{}",
                spec.r#ref.provider,
                spec.r#ref.id
            );
        }

        self.set_models(resolved.models);

        *safe_expect!(self.provider_hosts.lock(), "provider_hosts poisoned") =
            resolved.provider_hosts;

        // Reap the previous generation of provider-plugin subprocesses. Done last so
        // no window exists where a turn could resolve a provider whose host is dead.
        for (name, host) in old_hosts {
            crate::log!("config-reload: shutting down old provider plugin {name:?}");
            host.shutdown();
        }

        crate::log!("config-reload: {n_providers} provider(s), {n_models} model(s)");
        Ok((n_providers, n_models))
    }

    pub fn with_lease_idle(mut self, d: Duration) -> Self {
        self.leases = LeaseMap::new(d);
        self
    }

    /// install the host_api handler on every plugin host. Must be called once after
    /// `Arc::new(state)` — the handler holds an `Arc<ServerState>`.
    pub fn install_host_api(self: &Arc<Self>) {
        let api = Arc::new(crate::host_api::ServerHostApi {
            state: self.clone(),
        });
        for host in safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").iter() {
            host.set_api_handler(api.clone());
        }
    }

    pub fn with_idle_exit(mut self, d: Duration) -> Self {
        self.idle = IdleTracker::new(d);
        self
    }
    /// Install the `[server]` timing knobs (approval/interaction deadlines, tool cancel
    /// grace). Defaults apply when this is never called.
    pub fn with_timeouts(mut self, t: crate::config::ServerTimeouts) -> Self {
        self.timeouts = t;
        self
    }
    /// The `ReactConfig` a turn runs under, carrying the configured tool-cancellation grace
    /// and the optional turn ceiling (`None` = unbounded).
    pub fn react_config(&self) -> kn9t_react::ReactConfig {
        kn9t_react::ReactConfig {
            tool_cancel_grace: self.timeouts.tool_cancel_grace,
            max_turns: self.timeouts.max_turns,
            ..Default::default()
        }
    }
    pub fn with_provider(self, p: Arc<dyn Provider>) -> Self {
        *self.provider.write().expect("provider poisoned") = Some(p);
        self
    }
    pub fn with_approver(self, a: Arc<dyn Approver>) -> Self {
        *self.approver.write().expect("approver poisoned") = a;
        self
    }
    /// ADR-0008 — pick the approval adapter: not a risk decision, only who can answer an `Ask`
    /// the policy plugin already raised. Interactive runs prompt; `-p`/CI have nobody, so deny.
    pub fn approver_for(
        interactive: bool,
        registry: &Arc<ApprovalRegistry>,
        cache: &Arc<ApprovalCache>,
        timeouts: &crate::config::ServerTimeouts,
    ) -> Arc<dyn Approver> {
        if interactive {
            Arc::new(
                InteractiveApprover::with_cache(registry.clone(), cache.clone())
                    .with_timeout(timeouts.approval),
            )
        } else {
            Arc::new(NonInteractiveApprover::new(cache.clone()))
        }
    }
    pub fn with_providers(self, providers: Vec<(String, Arc<dyn Provider>)>) -> Self {
        *self.providers.write().expect("providers poisoned") = providers.into_iter().collect();
        self
    }
    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .read()
            .expect("providers poisoned")
            .get(name)
            .cloned()
    }
    pub fn with_default_model(self, m: ModelSpec) -> Self {
        *self.default_model.write().expect("default_model poisoned") = Some(m);
        self
    }

    pub fn with_title_model(self, m: ModelSpec) -> Self {
        *self.title_model.write().expect("title_model poisoned") = Some(m);
        self
    }

    /// Snapshot the configured title model, if any.
    pub fn title_model_snapshot(&self) -> Option<ModelSpec> {
        self.title_model
            .read()
            .expect("title_model poisoned")
            .clone()
    }

    /// Snapshot the titling/fallback provider. Clones the `Arc` out so no guard is
    /// held across a provider call (hot-reload writers must not block on a turn).
    pub fn provider_snapshot(&self) -> Option<Arc<dyn Provider>> {
        self.provider.read().expect("provider poisoned").clone()
    }

    /// Snapshot the default model spec.
    pub fn default_model_snapshot(&self) -> Option<ModelSpec> {
        self.default_model
            .read()
            .expect("default_model poisoned")
            .clone()
    }

    /// Look up a model spec by bare id in the registry.
    pub fn find_model(&self, id: &str) -> Option<ModelSpec> {
        self.model_registry
            .read()
            .expect("model_registry poisoned")
            .iter()
            .find(|m| m.r#ref.id == id)
            .cloned()
    }

    /// Snapshot the whole model registry.
    pub fn models_snapshot(&self) -> Vec<ModelSpec> {
        self.model_registry
            .read()
            .expect("model_registry poisoned")
            .clone()
    }

    /// Replace the model registry wholesale (hot-reload).
    pub fn set_models(&self, models: Vec<ModelSpec>) {
        *self
            .model_registry
            .write()
            .expect("model_registry poisoned") = models;
    }
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = cwd;
        self
    }
    pub fn with_provider_budget(self, b: f64) -> Self {
        *safe_expect!(self.provider_reported_budget.lock(), "poisoned") = Some(b);
        self
    }

    /// ADR-0008 -- install an in-process hook host, replacing plugin-composed hooks.
    /// Test-only seam: production hooks come from `plugin_hosts`.
    pub fn with_hooks_override(self, h: Arc<dyn kn9t_core::HookHost>) -> Self {
        *safe_expect!(self.hooks_override.lock(), "hooks_override poisoned") = Some(h);
        self
    }

    /// The hook host for a turn: the override if one was installed, else `None` so the
    /// caller composes from `plugin_hosts`.
    pub fn hooks_override_snapshot(&self) -> Option<Arc<dyn kn9t_core::HookHost>> {
        self.hooks_override
            .lock()
            .expect("hooks_override poisoned")
            .clone()
    }

    /// ADR-0008 -- snapshot the current approver for a turn.
    pub fn approver_snapshot(&self) -> Arc<dyn Approver> {
        self.approver.read().expect("approver poisoned").clone()
    }

    /// R-PLUG2-110: handle a plugin's `declare` message — rebuild the tool registry
    /// and emit `Event::PluginDeclared` to notify SSE clients.
    pub fn on_plugin_declare(
        self: &Arc<Self>,
        plugin_name: &str,
        tools_added: Vec<String>,
        tools_removed: Vec<String>,
    ) {
        crate::log!(
            "plugin '{}' re-declared: +{} tools, -{} tools",
            plugin_name,
            tools_added.len(),
            tools_removed.len()
        );

        // Rebuild tool registry from all current hosts (dedup first wins, same as startup/reload).
        {
            let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut all_tools: Vec<Arc<dyn kn9t_core::Tool>> = Vec::new();
            for h in hosts.iter() {
                let host_tools = crate::tools::extract_tools_public(h);
                for t in host_tools {
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
            *safe_expect!(self.tools.lock(), "tools poisoned") = registry;
            crate::log!(
                "plugin '{}' declare: registry rebuilt, total tools now {}",
                plugin_name,
                n
            );
        }

        // Broadcast event to ALL SSE clients so TUI can refresh.
        let event = kn9t_core::Event::PluginDeclared {
            plugin: plugin_name.to_string(),
            tools_added,
            tools_removed,
        };
        self.buses.broadcast_all(event.clone());
        self.notify_plugins(&event, Some(plugin_name));
    }

    /// Send an event to a specific plugin by name.
    /// Used by POST /plugin/{name}/ui_event to forward UI interactions.
    pub fn send_plugin_event(
        &self,
        plugin_name: &str,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned");
        for host in hosts.iter() {
            if host.name() == plugin_name {
                let event = kn9t_core::Event::PluginNotification { payload };
                if host.send_event(&event) {
                    return Ok(());
                } else {
                    return Err(format!("plugin '{}' unsubscribed from events", plugin_name));
                }
            }
        }
        Err(format!("plugin '{}' not found", plugin_name))
    }

    /// Fan an event out to every plugin subscribed to its kind. Lifecycle events previously
    /// reached SSE clients only, so a plugin could not react to another appearing or dying; the
    /// server holds no view about which plugin should care. `exclude` skips the subject itself.
    pub fn notify_plugins(&self, event: &kn9t_core::Event, exclude: Option<&str>) {
        let kind = crate::sse::event_kind(event);
        let hosts = safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").clone();
        for host in hosts.iter() {
            let name = host.name();
            if exclude == Some(name.as_str()) {
                continue;
            }
            if !host.has_event(kind) {
                continue;
            }
            if !host.send_event(event) {
                crate::log!(
                    "notify_plugins: '{}' unsubscribed, dropping '{}'",
                    name,
                    kind
                );
            }
        }
    }

    /// R-PLUG2-110: install the `on_declare` callback on every plugin host.
    /// Must be called after `Arc::new(state)`.
    pub fn install_declare_callbacks(self: &Arc<Self>) {
        let state = self.clone();
        for host in safe_expect!(self.plugin_hosts.lock(), "hosts poisoned").iter() {
            let state_for_cb = state.clone();
            host.set_on_declare(Box::new(move |plugin_name, _decl, added, removed| {
                state_for_cb.on_plugin_declare(plugin_name, added, removed);
            }));
        }
    }
}
