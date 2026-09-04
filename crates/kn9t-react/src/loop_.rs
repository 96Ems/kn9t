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
}

impl Default for ReactConfig {
    fn default() -> Self {
        ReactConfig {
            truncation_attempts: 4,
            truncation_ladder: vec![150, 100, 50, 25, 10],
            max_context_replans: 1,
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
}

/// R-RCT-010 -- the loop driver. Owns only trait objects and the ordered tool registry
/// (`ToolRegistry` is core vocabulary, DB-03). No concrete provider/tool/store/approver type
/// is named.
pub struct ReactLoop {
    pub provider: Arc<dyn Provider>,
    pub store: Arc<dyn Store>,
    pub approver: Arc<dyn Approver>,
    pub tools: ToolRegistry,
    pub hooks: Arc<dyn HookHost>,
    pub bus: Arc<dyn EventSink>,
    /// 96E-16 — optional pluggable compactor. `None` keeps the hardcoded inline prompt
    /// as fallback (same fail-open posture as the rest of the plugin system).
    pub compactor: Option<Arc<dyn Compactor>>,
}
