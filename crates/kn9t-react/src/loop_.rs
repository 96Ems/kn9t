//! R-RCT-010 .. R-RCT-130 -- the ReAct loop driver.
//!
//! The loop owns only trait objects (R-RCT-010, GI-1) and per-run parameters; it never
//! names a concrete `Provider`, `Tool`, `Store`, or `Approver`. One turn executes the exact
//! sequence of R-RCT-020 / DESIGN sec.9. Everything money-related (provider calls,
//! `UsageRecorded`) happens here and only here (DESIGN sec.3).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use kn9t_provider_core::{
    Approver, Compactor, EventSink, HookHost, ModelSpec, Provider, Sha256, Store, Thinking,
    ToolRegistry,
};

/// SPEC-OPEN (DESIGN sec.18.9) -- truncation give-up count and reminder ladder. Values,
/// not interfaces; tunable freely.
#[derive(Clone)]
pub struct ReactConfig {
    /// R-RCT-070 -- max truncation re-issues before giving up (default 4).
    pub truncation_attempts: u32,
    /// R-RCT-070 -- write-size line ladder (default 150,100,50,25,10).
    pub truncation_ladder: Vec<u32>,
    /// R-RCT-080/090 -- compaction re-plans allowed (exactly one, R-RCT-090).
    pub max_context_replans: u32,
    /// R-RCT-020 -- max turns in one run before the loop refuses to continue (default 100).
    ///
    /// The turn loop's only other exit when the model keeps emitting tool calls is
    /// `should_stop_after_turn`, whose default is `false` (R-RCT-100) and whose panic
    /// fallback is also `false` (R-RCT-110). A model stuck re-issuing the same call would
    /// therefore spend money indefinitely. This is the backstop: high enough that no real
    /// task reaches it, finite so a rut always ends.
    pub max_turns: u32,
    /// R-RCT-130 -- how long a cancelled batch waits for a `parallel_safe` tool to notice
    /// `Cancel` before abandoning it (default 1.5s).
    ///
    /// Tools are expected to poll `Cancel` and return promptly (R-CORE-240). One that does
    /// not -- blocked in a syscall, or a wedged plugin subprocess -- must not hold the turn
    /// open: past this grace the call gets a synthesized result so the transcript stays
    /// 7.5-clean and the turn can end. Configurable because the right value depends on the
    /// installed tools, not on kn9t.
    pub tool_cancel_grace: std::time::Duration,
}

impl Default for ReactConfig {
    fn default() -> Self {
        ReactConfig {
            truncation_attempts: 4,
            truncation_ladder: vec![150, 100, 50, 25, 10],
            max_context_replans: 1,
            max_turns: 100,
            tool_cancel_grace: std::time::Duration::from_millis(1500),
        }
    }
}

/// The shared read-hash map type (`ToolCtx::read`).
pub type ReadMap = Arc<Mutex<HashMap<PathBuf, (Sha256, SystemTime)>>>;

/// Per-run parameters. The loop struct owns only trait objects (R-RCT-010); the model /
/// thinking / cwd / config a run needs arrive here and evolve locally across turns (e.g. a
/// `prepare_next_turn` patch).
pub struct RunParams {
    pub session: kn9t_provider_core::SessionId,
    pub model: ModelSpec,
    pub thinking: Thinking,
    pub max_tokens: Option<u32>,
    pub cwd: PathBuf,
    pub config: ReactConfig,
    /// The read-hash map shared with tools (DESIGN sec.11.2).
    pub read_map: ReadMap,
    /// System prompt (injected by server, cached with tools).
    pub system: Option<String>,
    /// External cancel handle for aborting the run. The server registers this cancel
    /// and fires it when the user presses ESC. If None, a fresh cancel is created per turn.
    pub cancel: Option<kn9t_provider_core::Cancel>,
    /// Tools DISABLED for this session. A call to any name in this set is blocked at
    /// `authorize` time and returned as an `is_error` tool result — the provider still
    /// receives every tool spec, so the level-1 cache prefix is never disturbed (the
    /// whole point of blocking at execution rather than filtering the `tools` array).
    pub disabled_tools: std::collections::HashSet<String>,
    /// A one-shot `<system-reminder>` injected on the FIRST turn of this run (then
    /// dropped), telling the agent that tools were just re-enabled and are available
    /// again. Rides the same ephemeral `reminders` channel as truncation reminders, so
    /// it lands after the cached prefix and costs no cache invalidation.
    pub reactivation_reminder: Option<kn9t_provider_core::Message>,
}

/// Fatal loop error (surfaced as `Event::Error` before returning).
#[derive(Debug)]
pub enum ReactError {
    Store(String),
    Provider(String),
    /// Compaction re-plan still asked to compact a second time (R-RCT-090).
    CompactionLoop,
    /// 96E-17: compaction was demanded (context exhausted) but no compactor plugin is
    /// installed. Fail-closed: the turn ends, nothing is persisted, and the session
    /// cannot continue.
    CompactionUnavailable,
    /// Truncation ladder exhausted (R-RCT-070).
    TruncationGaveUp,
    /// R-RCT-020 -- `ReactConfig::max_turns` reached. The model kept asking for another turn
    /// (typically re-issuing tool calls) and nothing else was going to stop it.
    TurnLimit,
}

/// R-RCT-010 -- the loop driver. Owns only trait objects and the ordered tool registry
/// (`ToolRegistry` is core vocabulary, DB-03). No concrete provider/tool/store/approver type
/// is named.
pub struct ReactLoop {
    pub provider: Arc<dyn Provider>,
    pub store: Arc<dyn Store>,
    pub approver: Arc<dyn Approver>,
    pub tools: Arc<dyn ToolSource>,
    pub hooks: Arc<dyn HookHost>,
    pub bus: Arc<dyn EventSink>,
    /// 96E-16 — optional pluggable compactor. `None` keeps the hardcoded inline prompt
    /// as fallback (same fail-open posture as the rest of the plugin system).
    pub compactor: Option<Arc<dyn Compactor>>,
}

/// 96E-48 -- a live source of tools for a run.
///
/// The loop holds this instead of a `ToolRegistry` value so that hot plugin lifecycle
/// changes (96E-47 stop/start, R-PLUG2-100 reload/load) are visible *within* a turn.
/// `snapshot()` is called once per model call and once per tool batch, so it must be
/// cheap: implementations clone an ordered `Vec<Arc<dyn Tool>>` under a short lock.
pub trait ToolSource: Send + Sync {
    /// The current registry. Order MUST be stable across calls for a given set of
    /// tools (GI-3): the serialized `tools` array is part of the level-1 cache prefix.
    fn snapshot(&self) -> ToolRegistry;

    /// 96E-47 -- tool names refused at *execution* time while still advertised to the
    /// model. Used for stopped plugins: their specs stay in the `tools` array (cache
    /// prefix untouched) but a call gets a clean error instead of reaching a dead
    /// subprocess. Same posture as `RunParams::disabled_tools`.
    fn blocked(&self) -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }
}

/// A fixed registry as a `ToolSource` -- for tests and for callers with nothing to
/// hot-swap. `snapshot()` clones the registry it was built with, every time.
pub struct StaticTools(pub ToolRegistry);

impl ToolSource for StaticTools {
    fn snapshot(&self) -> ToolRegistry {
        self.0.clone()
    }
}

/// Wrap a registry as a live source: `Arc<dyn ToolSource>` in one call.
pub fn static_tools(registry: ToolRegistry) -> Arc<dyn ToolSource> {
    Arc::new(StaticTools(registry))
}

/// 96E-48 -- restrict a live source to a fixed name set (sub-agent toolset, 96E-17).
///
/// The filter is applied *after* each `snapshot()`, not once at composition time, so a
/// sub-agent still observes plugin lifecycle changes for the tools it was granted.
pub struct FilteredTools {
    pub inner: Arc<dyn ToolSource>,
    pub names: Vec<String>,
}

impl ToolSource for FilteredTools {
    fn snapshot(&self) -> ToolRegistry {
        self.inner.snapshot().filter_names(&self.names)
    }

    fn blocked(&self) -> std::collections::HashSet<String> {
        self.inner.blocked()
    }
}
