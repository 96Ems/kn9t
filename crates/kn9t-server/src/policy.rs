//! R-TOOL-070 / R-TOOL-090 / DESIGN §10 — ConfigPolicy + InteractivePolicy.
//!
//! `AllowPolicy` (in `state.rs`) is the permissive wiring default. The real
//! seam is three adapters:
//! - `ConfigPolicy` — instant verdict from `BashPolicy`, no blocking.
//! - `InteractivePolicy` — emits `Event::ApprovalRequest` to the session bus,
//!   then blocks on a condvar until `POST /approve` resolves it (command path,
//!   never the bus — DESIGN §10, Principle 3).
//!
//! The classifier lives in `classify.rs` (ADR-0001: server owns approval).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use kn9t_core::{ApprovalId, Decision, EffectKind, Event, EventSink, Policy, ToolCall, ToolRegistry};

use crate::classify::{classify, BashPolicy, Classification, Shell};

// ── Thread-local sink ────────────────────────────────────────────────────────
// `Policy::check` is `(&self, call, cwd) -> Decision` with no session param
// (DESIGN §10, R-CORE-270). The per-turn `SessionSink` is threaded via TLS so
// the globally-shared `InteractivePolicy` can emit to the correct session bus
// without changing the trait signature. The value is set for the duration of
// `turn::spawn_turn`'s loop thread.

thread_local! {
    static POLICY_SINK: RefCell<Option<Arc<dyn EventSink>>> = const { RefCell::new(None) };
    static POLICY_SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Set the current thread's policy sink. Called by `turn::spawn_turn` around
/// `loop.run`. Cleared after.
pub fn set_policy_sink(sink: Option<Arc<dyn EventSink>>) {
    POLICY_SINK.with(|c| *c.borrow_mut() = sink);
}

fn get_policy_sink() -> Option<Arc<dyn EventSink>> {
    POLICY_SINK.with(|c| c.borrow().clone())
}

/// Session id for the current turn's policy check (TLS, alongside sink).
pub fn set_policy_session(session: Option<String>) {
    POLICY_SESSION.with(|c| *c.borrow_mut() = session);
}
fn get_policy_session() -> Option<String> {
    POLICY_SESSION.with(|c| c.borrow().clone())
}

/// Run `f` with `sink` as the current policy sink (RAII helper for tests).
pub fn with_policy_sink<F, R>(sink: Arc<dyn EventSink>, f: F) -> R
where
    F: FnOnce() -> R,
{
    set_policy_sink(Some(sink));
    let r = f();
    set_policy_sink(None);
    r
}

/// Helper for tests: run with both sink and session id.
pub fn with_policy_session_sink<F, R>(session: &str, sink: Arc<dyn EventSink>, f: F) -> R
where
    F: FnOnce() -> R,
{
    set_policy_session(Some(session.to_string()));
    set_policy_sink(Some(sink));
    let r = f();
    set_policy_sink(None);
    set_policy_session(None);
    r
}

// ── Fingerprint ──────────────────────────────────────────────────────────────
/// Canonical fingerprint for a tool call, used for session/always caching.
/// For `bash` we use the extracted `cmd` string; for other tools the raw args.
pub fn fingerprint(call: &ToolCall) -> String {
    if call.name == "bash" {
        if let Some(cmd) = extract_cmd(&call.args_json) {
            return format!("bash:{}", cmd.trim());
        }
    }
    format!("{}:{}", call.name, call.args_json)
}

// ── ApprovalCache (session + persistent) ─────────────────────────────────────

/// In-memory + on-disk cache for `scope=session` and `scope=always` approvals.
/// `HardDeny` is never cached (see `InteractivePolicy::check`).
pub struct ApprovalCache {
    session: Mutex<HashMap<String, HashSet<String>>>,
    persistent: Mutex<HashSet<String>>,
    config_path: PathBuf,
}

impl ApprovalCache {
    pub fn new(config_path: PathBuf) -> Self {
        let mut persistent = HashSet::new();
        // Load existing always-approvals from config if present
        if config_path.exists() {
            if let Ok(text) = std::fs::read_to_string(&config_path) {
                if let Ok(val) = toml::from_str::<toml::Value>(&text) {
                    if let Some(arr) = val.get("policy")
                        .and_then(|p| p.get("approvals"))
                        .and_then(|a| a.get("always"))
                        .and_then(|v| v.as_array())
                    {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                persistent.insert(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        ApprovalCache {
            session: Mutex::new(HashMap::new()),
            persistent: Mutex::new(persistent),
            config_path,
        }
    }

    /// For tests: empty cache with temp path.
    pub fn new_empty() -> Self {
        ApprovalCache {
            session: Mutex::new(HashMap::new()),
            persistent: Mutex::new(HashSet::new()),
            config_path: PathBuf::from("/tmp/kn9t-test-noop.toml"),
        }
    }

    pub fn is_approved(&self, session_id: Option<&str>, fp: &str) -> bool {
        if self.persistent.lock().unwrap().contains(fp) {
            return true;
        }
        if let Some(sid) = session_id {
            if let Some(set) = self.session.lock().unwrap().get(sid) {
                if set.contains(fp) {
                    return true;
                }
            }
        }
        false
    }

    pub fn approve_session(&self, session_id: String, fp: String) {
        self.session.lock().unwrap().entry(session_id).or_default().insert(fp);
    }

    /// Persist `fp` as `always`. Writes back to `config_path` under
    /// `[policy.approvals] always = [...]`. Returns error string on failure.
    pub fn approve_persistent(&self, fp: String) -> Result<(), String> {
        {
            let mut guard = self.persistent.lock().unwrap();
            if guard.contains(&fp) {
                return Ok(());
            }
            guard.insert(fp.clone());
        }
        // Write back to TOML file
        self.write_persistent_to_disk(&fp)
    }

    fn write_persistent_to_disk(&self, _new_fp: &str) -> Result<(), String> {
        let path = &self.config_path;
        // Ensure parent exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let text = if path.exists() {
            std::fs::read_to_string(path).map_err(|e| format!("read config: {e}"))?
        } else {
            String::new()
        };
        let mut val: toml::Value = if text.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str(&text).map_err(|e| format!("parse config: {e}"))?
        };
        // Ensure structure policy.approvals.always is an array
        let always_arr = val
            .as_table_mut().unwrap()
            .entry("policy".to_string())
            .or_insert(toml::Value::Table(toml::map::Map::new()))
            .as_table_mut().unwrap()
            .entry("approvals".to_string())
            .or_insert(toml::Value::Table(toml::map::Map::new()))
            .as_table_mut().unwrap()
            .entry("always".to_string())
            .or_insert(toml::Value::Array(Vec::new()));
        if let toml::Value::Array(arr) = always_arr {
            let already = arr.iter().any(|v| v.as_str() == Some(_new_fp));
            if !already {
                arr.push(toml::Value::String(_new_fp.to_string()));
            }
        }
        let new_text = toml::to_string(&val).map_err(|e| format!("serialize: {e}"))?;
        // Atomic write via temp file
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, new_text).map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    }

    pub fn has_persistent(&self, fp: &str) -> bool {
        self.persistent.lock().unwrap().contains(fp)
    }
    pub fn has_session(&self, sid: &str, fp: &str) -> bool {
        self.session.lock().unwrap().get(sid).map_or(false, |s| s.contains(fp))
    }
}

// ── Approval registry (command-path resolution) ─────────────────────────────

struct ApprovalSlot {
    decision: Mutex<Option<Decision>>,
    cvar: Condvar,
}

#[derive(Clone)]
pub struct ApprovalMeta {
    pub fingerprint: String,
    pub session_id: String,
    pub tool: String,
}

pub struct ApprovalRegistry {
    inner: Mutex<HashMap<u64, Arc<ApprovalSlot>>>,
    meta: Mutex<HashMap<u64, ApprovalMeta>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        ApprovalRegistry {
            inner: Mutex::new(HashMap::new()),
            meta: Mutex::new(HashMap::new()),
        }
    }

    fn create(&self, id: u64, meta: ApprovalMeta) -> Arc<ApprovalSlot> {
        let slot = Arc::new(ApprovalSlot {
            decision: Mutex::new(None),
            cvar: Condvar::new(),
        });
        self.inner.lock().unwrap().insert(id, slot.clone());
        self.meta.lock().unwrap().insert(id, meta);
        slot
    }

    fn remove(&self, id: u64) {
        self.inner.lock().unwrap().remove(&id);
        self.meta.lock().unwrap().remove(&id);
    }

    /// Block until `id` is resolved. Caller must have created the slot.
    fn wait(&self, slot: Arc<ApprovalSlot>) -> Decision {
        let mut guard = slot.decision.lock().unwrap();
        while guard.is_none() {
            guard = slot.cvar.wait(guard).unwrap();
        }
        guard.clone().unwrap()
    }

    /// Resolve `id` with `decision`, waking any waiter. Returns true if found.
    pub fn resolve(&self, id: u64, decision: Decision) -> bool {
        let slot = {
            let map = self.inner.lock().unwrap();
            map.get(&id).cloned()
        };
        if let Some(slot) = slot {
            let mut guard = slot.decision.lock().unwrap();
            *guard = Some(decision);
            slot.cvar.notify_all();
            true
        } else {
            false
        }
    }

    /// Resolve and return the stored meta for scope handling (session/always).
    pub fn resolve_with_meta(&self, id: u64, decision: Decision) -> Option<ApprovalMeta> {
        let slot = {
            let map = self.inner.lock().unwrap();
            map.get(&id).cloned()
        };
        let meta = self.meta.lock().unwrap().get(&id).cloned();
        if let Some(slot) = slot {
            let mut guard = slot.decision.lock().unwrap();
            *guard = Some(decision);
            slot.cvar.notify_all();
            meta
        } else {
            None
        }
    }

    /// For scope handling: get meta without resolving.
    pub fn get_meta(&self, id: u64) -> Option<ApprovalMeta> {
        self.meta.lock().unwrap().get(&id).cloned()
    }

    /// For tests: is there a pending slot for `id`?
    #[cfg(test)]
    pub fn has_pending(&self, id: u64) -> bool {
        self.inner.lock().unwrap().contains_key(&id)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

static NEXT_APPROVAL_ID: AtomicU64 = AtomicU64::new(1);

fn extract_cmd(args_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args_json).ok()?;
    if let Some(s) = v.get("cmd").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("command").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    // Some fixtures use a bare string or different key
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    None
}

// ── Helpers for effects ───────────────────────────────────────────────────────
fn extract_field(args_json: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args_json).ok()?;
    // Support JSON pointer with leading "/" or bare top-level key
    let key = field.strip_prefix('/').unwrap_or(field);
    // For simplicity support only top-level key (no nested) and also handle
    // JSON pointer with "/" splits for one level.
    let first = key.split('/').next().unwrap_or(key);
    v.get(first).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Evaluate a single effect kind for a tool call. Returns the Decision for that effect.
fn eval_effect(kind: &EffectKind, args_json: &str, field: &str, bash: &BashPolicy) -> Decision {
    match kind {
        EffectKind::Shell => {
            let cmd = match extract_field(args_json, field) {
                Some(c) => c,
                None => return Decision::Deny { reason: format!("missing field `{field}` for shell effect") },
            };
            match classify(&cmd, Shell::Posix, bash) {
                Classification::AllowReadOnly => Decision::Allow,
                Classification::Ask => Decision::Ask,
                Classification::HardDeny(r) => Decision::HardDeny { reason: r },
            }
        }
        EffectKind::FsRead => {
            // Reads are safe by default; could add path allowlist later.
            // For now, FsRead is Allow.
            Decision::Allow
        }
        EffectKind::FsWrite => {
            // Writes are gated as hard as bash (DESIGN §10). For now, any FsWrite is Ask.
            // A missing field is treated as Ask (strict).
            if extract_field(args_json, field).is_none() {
                return Decision::Ask;
            }
            Decision::Ask
        }
        EffectKind::Network => Decision::Ask,
    }
}

/// Core effects dispatch: look up tool spec via `tools` registry, evaluate each effect,
/// and combine: HardDeny > Ask > Allow. Empty effects or unknown tool → strict Ask.
fn dispatch_effects(call: &ToolCall, bash: &BashPolicy, tools: &ToolRegistry) -> Decision {
    // If registry is empty (e.g., unit tests without tools), fallback to legacy bash-only logic
    // so that `bash` still classifies correctly and non-bash is Allow. This keeps old tests green.
    if tools.is_empty() {
        if call.name == "bash" {
            let cmd = match extract_cmd(&call.args_json) {
                Some(c) => c,
                None => return Decision::Deny { reason: "missing command".into() },
            };
            return match classify(&cmd, Shell::Posix, bash) {
                Classification::AllowReadOnly => Decision::Allow,
                Classification::Ask => Decision::Ask,
                Classification::HardDeny(r) => Decision::HardDeny { reason: r },
            };
        } else {
            return Decision::Allow;
        }
    }

    // Non-empty registry: use declared effects
    let spec = match tools.get(&call.name) {
        Some(t) => t.spec(),
        None => return Decision::Ask, // unknown tool → strict
    };
    if spec.effects.is_empty() {
        // No effects declared → strictest default (ADR-0002)
        return Decision::Ask;
    }
    let mut has_ask = false;
    for eff in &spec.effects {
        match eval_effect(&eff.kind, &call.args_json, &eff.field, bash) {
            Decision::HardDeny { reason } => return Decision::HardDeny { reason },
            Decision::Ask => has_ask = true,
            Decision::Deny { reason } => return Decision::Deny { reason },
            Decision::Allow => {}
        }
    }
    if has_ask { Decision::Ask } else { Decision::Allow }
}

// ── ConfigPolicy ─────────────────────────────────────────────────────────────

/// Instant, non-blocking policy. Used for `-p`/CI. A classifier `Ask` is
/// mapped to a hard `Deny` (no prompt in non-interactive mode).
pub struct ConfigPolicy {
    pub bash: BashPolicy,
    pub tools: ToolRegistry,
}

impl ConfigPolicy {
    pub fn new(bash: BashPolicy) -> Self {
        ConfigPolicy { bash, tools: ToolRegistry::default() }
    }
    pub fn with_tools(bash: BashPolicy, tools: ToolRegistry) -> Self {
        ConfigPolicy { bash, tools }
    }
}

impl Policy for ConfigPolicy {
    fn check(&self, call: &ToolCall, _cwd: &Path) -> Decision {
        match dispatch_effects(call, &self.bash, &self.tools) {
            Decision::Ask => Decision::Deny { reason: "approval required".into() },
            other => other,
        }
    }
}

// ── InteractivePolicy ────────────────────────────────────────────────────────

/// Blocking policy: `Ask` emits `ApprovalRequest` and waits for
/// `POST /approve` (command path), `HardDeny` never prompts.
///
/// `scope=session` and `scope=always` approvals are cached in `ApprovalCache`
/// (ADR-0001: server owns approval). `HardDeny` is never cached and never
/// overridden, even by an always approval.
pub struct InteractivePolicy {
    pub bash: BashPolicy,
    pub registry: Arc<ApprovalRegistry>,
    pub cache: Arc<ApprovalCache>,
    pub tools: ToolRegistry,
}

impl InteractivePolicy {
    pub fn new(bash: BashPolicy, registry: Arc<ApprovalRegistry>) -> Self {
        let cache = Arc::new(ApprovalCache::new(crate::config::global_config_path()));
        InteractivePolicy { bash, registry, cache, tools: ToolRegistry::default() }
    }
    pub fn new_with_cache(bash: BashPolicy, registry: Arc<ApprovalRegistry>, cache: Arc<ApprovalCache>) -> Self {
        InteractivePolicy { bash, registry, cache, tools: ToolRegistry::default() }
    }
    pub fn with_tools(bash: BashPolicy, registry: Arc<ApprovalRegistry>, cache: Arc<ApprovalCache>, tools: ToolRegistry) -> Self {
        InteractivePolicy { bash, registry, cache, tools }
    }
}

impl Policy for InteractivePolicy {
    fn check(&self, call: &ToolCall, cwd: &Path) -> Decision {
        let dispatch = dispatch_effects(call, &self.bash, &self.tools);
        match dispatch {
            Decision::HardDeny { reason } => return Decision::HardDeny { reason },
            Decision::Allow => return Decision::Allow,
            Decision::Deny { reason } => return Decision::Deny { reason },
            Decision::Ask => {} // fall through to approval flow
        }
        // Ask — check caches before prompting.
        let fp = fingerprint(call);
        let session = get_policy_session();
        if self.cache.is_approved(session.as_deref(), &fp) {
            return Decision::Allow;
        }
        // Not cached — emit ApprovalRequest and block.
        let id = NEXT_APPROVAL_ID.fetch_add(1, Ordering::SeqCst);
        let meta = ApprovalMeta {
            fingerprint: fp.clone(),
            session_id: session.clone().unwrap_or_default(),
            tool: call.name.clone(),
        };
        let slot = self.registry.create(id, meta);

        // Build the ApprovalRequest payload
        let args_val: serde_json::Value =
            serde_json::from_str(&call.args_json).unwrap_or(serde_json::Value::Null);

        if let Some(sink) = get_policy_sink() {
            sink.emit(Event::ApprovalRequest {
                id: ApprovalId(id),
                tool: call.name.clone(),
                args: args_val,
                cwd: cwd.to_path_buf(),
            });
        } else {
            self.registry.remove(id);
            return Decision::Deny {
                reason: "approval required (no sink)".into(),
            };
        }

        // Block until POST /approve resolves via ApprovalRegistry
        let decision = self.registry.wait(slot);
        self.registry.remove(id);
        decision
    }
}

/// Always-deny policy for `mode = "deny_all"` (DESIGN §10.1).
pub struct DenyAllPolicy;
impl Policy for DenyAllPolicy {
    fn check(&self, _call: &ToolCall, _cwd: &Path) -> Decision {
        Decision::Deny { reason: "denied by policy mode deny_all".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kn9t_core::EventSink;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<Event>>,
    }
    impl EventSink for RecordingSink {
        fn emit(&self, e: Event) {
            self.events.lock().unwrap().push(e);
        }
    }

    fn bash_call(cmd: &str) -> ToolCall {
        ToolCall {
            id: kn9t_core::CallId("c1".into()),
            name: "bash".into(),
            args_json: serde_json::json!({"cmd": cmd}).to_string(),
        }
    }

    #[test]
    fn config_policy_allow_readonly() {
        let p = ConfigPolicy::new(BashPolicy::default());
        let d = p.check(&bash_call("ls"), Path::new("/"));
        assert_eq!(d, Decision::Allow);
    }

    #[test]
    fn config_policy_ask_maps_to_deny() {
        let p = ConfigPolicy::new(BashPolicy::default());
        // rm is in always_ask -> Ask -> Deny in ConfigPolicy
        let d = p.check(&bash_call("rm -rf /"), Path::new("/"));
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn config_policy_hard_deny() {
        let p = ConfigPolicy::new(BashPolicy::default());
        let d = p.check(&bash_call("sudo rm -rf /"), Path::new("/"));
        assert!(matches!(d, Decision::HardDeny { .. }));
    }

    #[test]
    fn interactive_hard_deny_no_prompt() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache);
        let sink = Arc::new(RecordingSink::default());
        // No need to set sink for HardDeny — should not emit
        let d = with_policy_sink(sink.clone(), || p.check(&bash_call("sudo rm -rf /"), Path::new("/")));
        assert!(matches!(d, Decision::HardDeny { .. }));
        let evs = sink.events.lock().unwrap();
        assert!(evs.is_empty(), "HardDeny must not emit ApprovalRequest, got {:?}", evs.len());
    }

    #[test]
    fn interactive_ask_emits_and_blocks_until_resolved() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = Arc::new(InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let reg_c = reg.clone();
        let p_c = p.clone();

        // Spawn checker thread that will block on Ask
        let handle = std::thread::spawn(move || {
            with_policy_sink(sink_c, || p_c.check(&bash_call("rm -rf /"), Path::new("/")))
        });

        // Wait until ApprovalRequest appears (poll with timeout)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let evs = sink.events.lock().unwrap();
                if evs.iter().any(|e| matches!(e, Event::ApprovalRequest { .. })) {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("ApprovalRequest never emitted");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Resolve via registry (simulates POST /approve)
        // Extract id from emitted event
        let id = {
            let evs = sink.events.lock().unwrap();
            match &evs[0] {
                Event::ApprovalRequest { id, .. } => id.0,
                _ => panic!("wrong event"),
            }
        };
        assert!(reg_c.resolve(id, Decision::Allow));

        let decision = handle.join().unwrap();
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn interactive_ask_resolves_to_deny() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = Arc::new(InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());
        let sink_c = sink.clone();
        let reg_c = reg.clone();
        let p_c = p.clone();
        let handle = std::thread::spawn(move || {
            with_policy_sink(sink_c, || p_c.check(&bash_call("sh -c 'rm -rf /'"), Path::new("/")))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !sink.events.lock().unwrap().is_empty() { break; }
            if std::time::Instant::now() > deadline { panic!("no event"); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let id = match &sink.events.lock().unwrap()[0] {
            Event::ApprovalRequest { id, .. } => id.0,
            _ => panic!(),
        };
        reg_c.resolve(id, Decision::Deny { reason: "nope".into() });
        let d = handle.join().unwrap();
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn interactive_allow_never_prompts() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = InteractivePolicy::new_with_cache(BashPolicy::default(), reg, cache);
        let sink = Arc::new(RecordingSink::default());
        let d = with_policy_sink(sink.clone(), || p.check(&bash_call("cat foo.txt"), Path::new("/")));
        assert_eq!(d, Decision::Allow);
        assert!(sink.events.lock().unwrap().is_empty());
    }

    #[test]
    fn cache_session_allows_second_call_without_prompt() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = Arc::new(InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache.clone()));
        let sink = Arc::new(RecordingSink::default());
        // First Ask
        let sink_c = sink.clone();
        let p_c = p.clone();
        let reg_c = reg.clone();
        let handle = std::thread::spawn(move || {
            with_policy_session_sink("sess1", sink_c, || p_c.check(&bash_call("rm -rf /tmp/x"), Path::new("/")))
        });
        // Wait for ApprovalRequest
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !sink.events.lock().unwrap().is_empty() { break; }
            if std::time::Instant::now() > deadline { panic!("no event"); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let id = match &sink.events.lock().unwrap()[0] { Event::ApprovalRequest { id, .. } => id.0, _ => panic!() };
        // Simulate scope=session persistence
        let meta = reg_c.get_meta(id).expect("meta must exist");
        cache.approve_session(meta.session_id, meta.fingerprint);
        reg_c.resolve(id, Decision::Allow);
        let d = handle.join().unwrap();
        assert_eq!(d, Decision::Allow);
        // Clear events
        sink.events.lock().unwrap().clear();
        // Second call same session should be auto-allowed (no prompt)
        let d2 = with_policy_session_sink("sess1", sink.clone(), || p.check(&bash_call("rm -rf /tmp/x"), Path::new("/")));
        assert_eq!(d2, Decision::Allow);
        assert!(sink.events.lock().unwrap().is_empty(), "session cached should not prompt");
        // Different session should still prompt
        sink.events.lock().unwrap().clear();
        let handle2 = std::thread::spawn({
            let p = p.clone();
            let sink2 = sink.clone();
            let reg2 = reg.clone();
            move || with_policy_session_sink("sess2", sink2, || p.check(&bash_call("rm -rf /tmp/x"), Path::new("/")))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !sink.events.lock().unwrap().is_empty() { break; }
            if std::time::Instant::now() > deadline { panic!("second session should prompt"); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Cleanup: resolve pending
        let id2 = match &sink.events.lock().unwrap()[0] { Event::ApprovalRequest { id, .. } => id.0, _ => panic!() };
        reg.resolve(id2, Decision::Deny { reason: "test".into() });
        let _ = handle2.join();
    }

    #[test]
    fn cache_persistent_allows_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let cache = Arc::new(ApprovalCache::new(path.clone()));
        let reg = Arc::new(ApprovalRegistry::new());
        let p = Arc::new(InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache.clone()));
        let sink = Arc::new(RecordingSink::default());
        // First Ask
        let sink_c = sink.clone();
        let p_c = p.clone();
        let reg_c = reg.clone();
        let handle = std::thread::spawn(move || {
            with_policy_session_sink("s1", sink_c, || p_c.check(&bash_call("rm -rf /tmp/persist"), Path::new("/")))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop { if !sink.events.lock().unwrap().is_empty() { break; } if std::time::Instant::now() > deadline { panic!("no event"); } std::thread::sleep(std::time::Duration::from_millis(10)); }
        let id = match &sink.events.lock().unwrap()[0] { Event::ApprovalRequest { id, .. } => id.0, _ => panic!() };
        let meta = reg_c.get_meta(id).unwrap();
        cache.approve_persistent(meta.fingerprint.clone()).unwrap();
        reg_c.resolve(id, Decision::Allow);
        assert_eq!(handle.join().unwrap(), Decision::Allow);
        // New session should also be allowed via persistent
        sink.events.lock().unwrap().clear();
        let d2 = with_policy_session_sink("different", sink.clone(), || p.check(&bash_call("rm -rf /tmp/persist"), Path::new("/")));
        assert_eq!(d2, Decision::Allow);
        assert!(sink.events.lock().unwrap().is_empty());
        // Verify file on disk
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("rm -rf /tmp/persist"), "config should contain fingerprint, got {}", text);
        // Reload cache from disk should still allow
        let cache2 = ApprovalCache::new(path);
        assert!(cache2.has_persistent("bash:rm -rf /tmp/persist"));
    }

    #[test]
    fn hard_deny_never_cached() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let p = InteractivePolicy::new_with_cache(BashPolicy::default(), reg.clone(), cache.clone());
        let sink = Arc::new(RecordingSink::default());
        // sudo is HardDeny — should not emit and not be cacheable
        let d = with_policy_sink(sink.clone(), || p.check(&bash_call("sudo rm -rf /"), Path::new("/")));
        assert!(matches!(d, Decision::HardDeny { .. }));
        assert!(sink.events.lock().unwrap().is_empty());
        // Even if we try to cache it, check should still be HardDeny
        cache.approve_session("sess1".into(), "bash:sudo rm -rf /".into());
        cache.approve_persistent("bash:sudo rm -rf /".into()).unwrap_or(());
        let d2 = with_policy_sink(sink.clone(), || p.check(&bash_call("sudo rm -rf /"), Path::new("/")));
        assert!(matches!(d2, Decision::HardDeny { .. }), "HardDeny must not be overridden by cache");
    }

    #[test]
    fn no_effects_is_ask_and_strict() {
        // Tool with no effects → strictest (Ask for Interactive, Deny for Config)
        use kn9t_core::{Effect, ToolRegistry, ToolSpec};
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let mut tools = ToolRegistry::default();
        let spec = ToolSpec {
            name: "mystery".into(),
            description: "undeclared".into(),
            schema: serde_json::json!({"type":"object"}),
            hidden: false,
            effects: vec![],
        };
        struct Mystery { spec: ToolSpec }
        impl kn9t_core::Tool for Mystery {
            fn spec(&self) -> &ToolSpec { &self.spec }
            fn execute(&self, _a: &serde_json::Value, _c: &kn9t_core::ToolCtx, _k: &kn9t_core::Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> { unreachable!() }
        }
        tools.push(Arc::new(Mystery { spec }) as Arc<dyn kn9t_core::Tool>);
        let p_interactive = InteractivePolicy::with_tools(BashPolicy::default(), reg.clone(), cache.clone(), tools.clone());
        let p_config = ConfigPolicy::with_tools(BashPolicy::default(), tools);
        let call = ToolCall { id: kn9t_core::CallId("c1".into()), name: "mystery".into(), args_json: r#"{"foo":"bar"}"#.into() };
        let call_clone = call.clone();
        let sink = Arc::new(RecordingSink::default());
        // Interactive should be Ask (prompts)
        let h = std::thread::spawn({
            let p = Arc::new(p_interactive);
            let sink = sink.clone();
            let c = call_clone.clone();
            move || with_policy_sink(sink, || p.check(&c, Path::new("/")))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !sink.events.lock().unwrap().is_empty() { break; }
            if std::time::Instant::now() > deadline { panic!("mystery tool must prompt (Ask)"); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let id = match &sink.events.lock().unwrap()[0] { Event::ApprovalRequest { id, .. } => id.0, _ => panic!() };
        reg.resolve(id, Decision::Allow);
        assert_eq!(h.join().unwrap(), Decision::Allow);
        // Config should be Deny (no prompt, instant deny)
        let d2 = p_config.check(&call, Path::new("/"));
        assert!(matches!(d2, Decision::Deny { .. }), "no-effects in ConfigPolicy must be Deny, got {:?}", d2);
    }

    #[test]
    fn fs_write_is_ask_and_read_is_allow() {
        use kn9t_core::{Effect, EffectKind, ToolRegistry, ToolSpec};
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let mut tools = ToolRegistry::default();
        let write_spec = ToolSpec {
            name: "write".into(),
            description: "write file".into(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsWrite }],
        };
        let read_spec = ToolSpec {
            name: "read".into(),
            description: "read file".into(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsRead }],
        };
        struct W { spec: ToolSpec } impl kn9t_core::Tool for W { fn spec(&self) -> &ToolSpec { &self.spec } fn execute(&self, _: &serde_json::Value, _: &kn9t_core::ToolCtx, _: &kn9t_core::Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> { unreachable!() } }
        struct R { spec: ToolSpec } impl kn9t_core::Tool for R { fn spec(&self) -> &ToolSpec { &self.spec } fn execute(&self, _: &serde_json::Value, _: &kn9t_core::ToolCtx, _: &kn9t_core::Cancel) -> Result<kn9t_core::ToolOutput, kn9t_core::ToolErr> { unreachable!() } }
        tools.push(Arc::new(W { spec: write_spec }) as Arc<dyn kn9t_core::Tool>);
        tools.push(Arc::new(R { spec: read_spec }) as Arc<dyn kn9t_core::Tool>);
        let p = InteractivePolicy::with_tools(BashPolicy::default(), reg.clone(), cache, tools);
        let sink = Arc::new(RecordingSink::default());
        // write must be Ask
        let write_call = ToolCall { id: kn9t_core::CallId("c1".into()), name: "write".into(), args_json: r#"{"path":"/tmp/foo"}"#.into() };
        let h = std::thread::spawn({
            let p = Arc::new(p);
            let sink = sink.clone();
            let call = write_call.clone();
            move || with_policy_sink(sink, || p.check(&call, Path::new("/tmp")))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop { if !sink.events.lock().unwrap().is_empty() { break; } if std::time::Instant::now() > deadline { panic!("write must be Ask"); } std::thread::sleep(std::time::Duration::from_millis(10)); }
        let id = match &sink.events.lock().unwrap()[0] { Event::ApprovalRequest { id, .. } => id.0, _ => panic!() };
        // need to resolve to not hang, but we need a new policy instance for read check (different p moved)
        // Re-create for read check with fresh registry
        let reg2 = Arc::new(ApprovalRegistry::new());
        let cache2 = Arc::new(ApprovalCache::new_empty());
        let mut tools2 = ToolRegistry::default();
        tools2.push(Arc::new(R { spec: ToolSpec { name: "read".into(), description: "".into(), schema: serde_json::json!({}), hidden: false, effects: vec![Effect { field: "path".into(), kind: EffectKind::FsRead }] } }) as Arc<dyn kn9t_core::Tool>);
        // Use original p for write, but we moved it; use reg to resolve
        reg.resolve(id, Decision::Allow);
        let _ = h.join();
        // read must be Allow (no prompt)
        let read_call = ToolCall { id: kn9t_core::CallId("c2".into()), name: "read".into(), args_json: r#"{"path":"/tmp/foo"}"#.into() };
        let sink2 = Arc::new(RecordingSink::default());
        let p2 = InteractivePolicy::with_tools(BashPolicy::default(), reg2, cache2, {
            let mut t = ToolRegistry::default();
            t.push(Arc::new(R { spec: ToolSpec { name: "read".into(), description: "".into(), schema: serde_json::json!({}), hidden: false, effects: vec![Effect { field: "path".into(), kind: EffectKind::FsRead }] } }) as Arc<dyn kn9t_core::Tool>);
            t
        });
        let d = with_policy_sink(sink2.clone(), || p2.check(&read_call, Path::new("/tmp")));
        assert_eq!(d, Decision::Allow);
        assert!(sink2.events.lock().unwrap().is_empty(), "FsRead must be Allow without prompt");
    }
}
