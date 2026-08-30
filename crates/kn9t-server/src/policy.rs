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
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use kn9t_core::{ApprovalId, Decision, Event, EventSink, Policy, ToolCall};

use crate::classify::{classify, BashPolicy, Classification, Shell};

// ── Thread-local sink ────────────────────────────────────────────────────────
// `Policy::check` is `(&self, call, cwd) -> Decision` with no session param
// (DESIGN §10, R-CORE-270). The per-turn `SessionSink` is threaded via TLS so
// the globally-shared `InteractivePolicy` can emit to the correct session bus
// without changing the trait signature. The value is set for the duration of
// `turn::spawn_turn`'s loop thread.

thread_local! {
    static POLICY_SINK: RefCell<Option<Arc<dyn EventSink>>> = const { RefCell::new(None) };
}

/// Set the current thread's policy sink. Called by `turn::spawn_turn` around
/// `loop.run`. Cleared after.
pub fn set_policy_sink(sink: Option<Arc<dyn EventSink>>) {
    POLICY_SINK.with(|c| *c.borrow_mut() = sink);
}

fn get_policy_sink() -> Option<Arc<dyn EventSink>> {
    POLICY_SINK.with(|c| c.borrow().clone())
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

// ── Approval registry (command-path resolution) ─────────────────────────────

struct ApprovalSlot {
    decision: Mutex<Option<Decision>>,
    cvar: Condvar,
}

pub struct ApprovalRegistry {
    inner: Mutex<HashMap<u64, Arc<ApprovalSlot>>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        ApprovalRegistry {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn create(&self, id: u64) -> Arc<ApprovalSlot> {
        let slot = Arc::new(ApprovalSlot {
            decision: Mutex::new(None),
            cvar: Condvar::new(),
        });
        self.inner.lock().unwrap().insert(id, slot.clone());
        slot
    }

    fn remove(&self, id: u64) {
        self.inner.lock().unwrap().remove(&id);
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

// ── ConfigPolicy ─────────────────────────────────────────────────────────────

/// Instant, non-blocking policy. Used for `-p`/CI. A classifier `Ask` is
/// mapped to a hard `Deny` (no prompt in non-interactive mode).
pub struct ConfigPolicy {
    pub bash: BashPolicy,
}

impl ConfigPolicy {
    pub fn new(bash: BashPolicy) -> Self {
        ConfigPolicy { bash }
    }
}

impl Policy for ConfigPolicy {
    fn check(&self, call: &ToolCall, _cwd: &Path) -> Decision {
        if call.name != "bash" {
            return Decision::Allow;
        }
        let cmd = match extract_cmd(&call.args_json) {
            Some(c) => c,
            None => return Decision::Deny { reason: "missing command".into() },
        };
        match classify(&cmd, Shell::Posix, &self.bash) {
            Classification::AllowReadOnly => Decision::Allow,
            Classification::Ask => Decision::Deny {
                reason: "approval required".into(),
            },
            Classification::HardDeny(r) => Decision::HardDeny { reason: r },
        }
    }
}

// ── InteractivePolicy ────────────────────────────────────────────────────────

/// Blocking policy: `Ask` emits `ApprovalRequest` and waits for
/// `POST /approve` (command path), `HardDeny` never prompts.
pub struct InteractivePolicy {
    pub bash: BashPolicy,
    pub registry: Arc<ApprovalRegistry>,
}

impl InteractivePolicy {
    pub fn new(bash: BashPolicy, registry: Arc<ApprovalRegistry>) -> Self {
        InteractivePolicy { bash, registry }
    }
}

impl Policy for InteractivePolicy {
    fn check(&self, call: &ToolCall, cwd: &Path) -> Decision {
        eprintln!("[InteractivePolicy] check tool={} args_json={} cwd={}", call.name, call.args_json, cwd.display());
        if call.name != "bash" {
            eprintln!("[InteractivePolicy] -> Allow (non-bash)");
            return Decision::Allow;
        }
        let cmd = match extract_cmd(&call.args_json) {
            Some(c) => {
                eprintln!("[InteractivePolicy] extracted cmd={:?}", c);
                c
            },
            None => {
                eprintln!("[InteractivePolicy] missing cmd, args_json={}", call.args_json);
                return Decision::Deny {
                    reason: "missing command".into(),
                }
            }
        };
        let cls = classify(&cmd, Shell::Posix, &self.bash);
        eprintln!("[InteractivePolicy] classify {:?} => {:?}", cmd, cls);
        match cls {
            Classification::AllowReadOnly => {
                eprintln!("[InteractivePolicy] -> Allow");
                Decision::Allow
            },
            Classification::HardDeny(r) => {
                eprintln!("[InteractivePolicy] -> HardDeny {}", r);
                Decision::HardDeny { reason: r }
            },
            Classification::Ask => {
                let id = NEXT_APPROVAL_ID.fetch_add(1, Ordering::SeqCst);
                let slot = self.registry.create(id);

                // Build the ApprovalRequest payload
                let args_val: serde_json::Value =
                    serde_json::from_str(&call.args_json).unwrap_or(serde_json::Value::Null);

                if let Some(sink) = get_policy_sink() {
                    eprintln!("[InteractivePolicy] emitting ApprovalRequest id={id} tool={} cwd={}", call.name, cwd.display());
                    sink.emit(Event::ApprovalRequest {
                        id: ApprovalId(id),
                        tool: call.name.clone(),
                        args: args_val,
                        cwd: cwd.to_path_buf(),
                    });
                    eprintln!("[InteractivePolicy] emitted, blocking on registry wait id={id}");
                } else {
                    eprintln!("[InteractivePolicy] no sink for ApprovalRequest id={id}, denying");
                    self.registry.remove(id);
                    return Decision::Deny {
                        reason: "approval required (no sink)".into(),
                    };
                }

                // Block until POST /approve resolves via ApprovalRegistry
                let decision = self.registry.wait(slot);
                eprintln!("[InteractivePolicy] unblocked id={id} decision={:?}", decision);
                self.registry.remove(id);
                decision
            }
        }
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
        let p = InteractivePolicy::new(BashPolicy::default(), reg.clone());
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
        let p = Arc::new(InteractivePolicy::new(BashPolicy::default(), reg.clone()));
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
        let p = Arc::new(InteractivePolicy::new(BashPolicy::default(), reg.clone()));
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
        let p = InteractivePolicy::new(BashPolicy::default(), reg);
        let sink = Arc::new(RecordingSink::default());
        let d = with_policy_sink(sink.clone(), || p.check(&bash_call("cat foo.txt"), Path::new("/")));
        assert_eq!(d, Decision::Allow);
        assert!(sink.events.lock().unwrap().is_empty());
    }
}
