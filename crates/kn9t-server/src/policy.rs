#![deny(clippy::unwrap_used)]
//! DESIGN §10 → ADR-0008 — the approval **mechanism**.
//!
//! This module used to decide risk (`ConfigPolicy`, `InteractivePolicy`, `dispatch_policy`,
//! and a shell classifier). ADR-0008 moved that judgement to a user-installed policy plugin,
//! which answers `before_tool_call` with `HookVeto::Allow|Ask|Deny|Replace`. Nothing here
//! judges a tool call any more.
//!
//! What remains is the part a plugin subprocess cannot own, because it needs the session bus,
//! the write lease and the user's config file:
//! - `ApprovalRegistry` — id → slot, resolved by `POST /approve` (command path, never the
//!   bus — DESIGN §10, Principle 3).
//! - `ApprovalCache` — `once|session|always` scopes, `always` persisted to
//!   `[policy.approvals]` in `~/.kn9t/config.toml`.
//! - `InteractiveApprover` / `NonInteractiveApprover` — the two `Approver` adapters that
//!   turn a plugin's `Ask` into a `Decision`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use kn9t_core::{ApprovalCtx, ApprovalId, Approver, Decision, LiveEvent, ToolCall};

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
                    if let Some(arr) = val
                        .get("policy")
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
        if self
            .persistent
            .lock()
            .expect("policy.rs: ApprovalCache::is_approved persistent lock poisoned")
            .contains(fp)
        {
            return true;
        }
        if let Some(sid) = session_id {
            if let Some(set) = self
                .session
                .lock()
                .expect("policy.rs: ApprovalCache::is_approved session lock poisoned")
                .get(sid)
            {
                if set.contains(fp) {
                    return true;
                }
            }
        }
        false
    }

    pub fn approve_session(&self, session_id: String, fp: String) {
        self.session
            .lock()
            .expect("policy.rs: ApprovalCache::approve_session session lock poisoned")
            .entry(session_id)
            .or_default()
            .insert(fp);
    }

    /// Persist `fp` as `always`. Writes back to `config_path` under
    /// `[policy.approvals] always = [...]`. Returns error string on failure.
    pub fn approve_persistent(&self, fp: String) -> Result<(), String> {
        {
            let mut guard = self
                .persistent
                .lock()
                .expect("policy.rs: ApprovalCache::approve_persistent persistent lock poisoned");
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
        // Ensure structure policy.approvals.always is an array — malformed shapes return Err instead of panicking
        let root = val
            .as_table_mut()
            .expect("policy.rs: config root must be a table — constructed as Table above");
        let policy_val = root
            .entry("policy".to_string())
            .or_insert(toml::Value::Table(toml::map::Map::new()));
        let policy_table = match policy_val.as_table_mut() {
            Some(t) => t,
            None => return Err(format!("policy entry in {} is not a table", path.display())),
        };
        let approvals_val = policy_table
            .entry("approvals".to_string())
            .or_insert(toml::Value::Table(toml::map::Map::new()));
        let approvals_table = match approvals_val.as_table_mut() {
            Some(t) => t,
            None => {
                return Err(format!(
                    "policy.approvals in {} is not a table",
                    path.display()
                ))
            }
        };
        let always_arr = approvals_table
            .entry("always".to_string())
            .or_insert(toml::Value::Array(Vec::new()));
        if !always_arr.is_array() {
            return Err(format!(
                "policy.approvals.always in {} is not an array",
                path.display()
            ));
        }
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
        self.persistent
            .lock()
            .expect("policy.rs: ApprovalCache::has_persistent lock poisoned")
            .contains(fp)
    }
    pub fn has_session(&self, sid: &str, fp: &str) -> bool {
        self.session
            .lock()
            .expect("policy.rs: ApprovalCache::has_session lock poisoned")
            .get(sid)
            .map_or(false, |s| s.contains(fp))
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
        self.inner
            .lock()
            .expect("policy.rs: ApprovalRegistry::create inner lock poisoned")
            .insert(id, slot.clone());
        self.meta
            .lock()
            .expect("policy.rs: ApprovalRegistry::create meta lock poisoned")
            .insert(id, meta);
        slot
    }

    fn remove(&self, id: u64) {
        self.inner
            .lock()
            .expect("policy.rs: ApprovalRegistry::remove inner lock poisoned")
            .remove(&id);
        self.meta
            .lock()
            .expect("policy.rs: ApprovalRegistry::remove meta lock poisoned")
            .remove(&id);
    }

    /// Block until `id` is resolved. Caller must have created the slot.
    fn wait(&self, slot: Arc<ApprovalSlot>) -> Decision {
        let mut guard = slot
            .decision
            .lock()
            .expect("policy.rs: ApprovalRegistry::wait decision lock poisoned");
        while guard.is_none() {
            guard = slot
                .cvar
                .wait(guard)
                .expect("policy.rs: ApprovalRegistry::wait condvar wait poisoned");
        }
        guard
            .clone()
            .expect("policy.rs: ApprovalRegistry::wait decision must be Some after wait loop")
    }

    /// Resolve `id` with `decision`, waking any waiter. Returns true if found.
    pub fn resolve(&self, id: u64, decision: Decision) -> bool {
        let slot = {
            let map = self
                .inner
                .lock()
                .expect("policy.rs: ApprovalRegistry::resolve inner lock poisoned");
            map.get(&id).cloned()
        };
        if let Some(slot) = slot {
            let mut guard = slot
                .decision
                .lock()
                .expect("policy.rs: ApprovalRegistry::resolve decision lock poisoned");
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
            let map = self
                .inner
                .lock()
                .expect("policy.rs: ApprovalRegistry::resolve_with_meta inner lock poisoned");
            map.get(&id).cloned()
        };
        let meta = self
            .meta
            .lock()
            .expect("policy.rs: ApprovalRegistry::resolve_with_meta meta lock poisoned")
            .get(&id)
            .cloned();
        if let Some(slot) = slot {
            let mut guard = slot
                .decision
                .lock()
                .expect("policy.rs: ApprovalRegistry::resolve_with_meta decision lock poisoned");
            *guard = Some(decision);
            slot.cvar.notify_all();
            meta
        } else {
            None
        }
    }

    /// For scope handling: get meta without resolving.
    pub fn get_meta(&self, id: u64) -> Option<ApprovalMeta> {
        self.meta
            .lock()
            .expect("policy.rs: ApprovalRegistry::get_meta meta lock poisoned")
            .get(&id)
            .cloned()
    }

    /// For tests: is there a pending slot for `id`?
    #[cfg(test)]
    pub fn has_pending(&self, id: u64) -> bool {
        self.inner
            .lock()
            .expect("policy.rs: ApprovalRegistry::has_pending lock poisoned")
            .contains_key(&id)
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

// ── Approver (ADR-0008) ──────────────────────────────────────────────────────

/// ADR-0008 — the approval **mechanism**. It does not decide anything.
///
/// Before ADR-0008 this type was `InteractivePolicy` and it did two jobs: judge the call
/// (via `dispatch_policy`/`classify`) and, if the verdict was `Ask`, run the prompt. The
/// judgement moved to a policy plugin (`HookVeto::Ask` on `before_tool_call`), so only the
/// prompt remains — the part a subprocess cannot own, because it needs the session bus, the
/// write lease and `~/.kn9t/config.toml`.
///
/// `request` is called only when a plugin already said "ask". It short-circuits on a cached
/// approval (`once|session|always`), otherwise emits `Event::ApprovalRequest` and blocks the
/// calling turn thread on a `Condvar` until `POST /approve` resolves it (command path, never
/// the bus — DESIGN §10, Principle 3).
pub struct InteractiveApprover {
    pub registry: Arc<ApprovalRegistry>,
    pub cache: Arc<ApprovalCache>,
}

impl InteractiveApprover {
    pub fn new(registry: Arc<ApprovalRegistry>) -> Self {
        let cache = Arc::new(ApprovalCache::new(crate::config::global_config_path()));
        InteractiveApprover { registry, cache }
    }

    pub fn with_cache(registry: Arc<ApprovalRegistry>, cache: Arc<ApprovalCache>) -> Self {
        InteractiveApprover { registry, cache }
    }
}

impl Approver for InteractiveApprover {
    fn request(&self, call: &ToolCall, cwd: &Path, reason: &str, ctx: &ApprovalCtx) -> Decision {
        // A previous `always`/`session` approval for the same fingerprint answers without
        // troubling the user again.
        let fp = fingerprint(call);
        if self.cache.is_approved(Some(ctx.session), &fp) {
            return Decision::Allow;
        }

        let id = NEXT_APPROVAL_ID.fetch_add(1, Ordering::SeqCst);
        let meta = ApprovalMeta {
            fingerprint: fp,
            session_id: ctx.session.to_string(),
            tool: call.name.clone(),
        };
        let slot = self.registry.create(id, meta);

        let args_val: serde_json::Value =
            serde_json::from_str(&call.args_json).unwrap_or(serde_json::Value::Null);

        ctx.sink.emit(LiveEvent::ApprovalRequest {
            id: ApprovalId(id),
            tool: call.name.clone(),
            args: args_val,
            cwd: cwd.to_path_buf(),
            reason: reason.to_string(),
        });

        // Blocks until `POST /approve` arrives. The human wait happens here, server-side,
        // *after* the hook returned — so a user taking their time cannot trip the plugin's
        // 30 s hook timeout (ADR-0008).
        let decision = self.registry.wait(slot);
        self.registry.remove(id);
        decision
    }
}

/// ADR-0008 — the non-interactive approver: `-p` / CI, where no one can answer a prompt.
/// A plugin's `Ask` becomes `Deny`, since an unanswerable question is not permission.
/// Cached `always` approvals still apply, so a scripted run honours what the user already
/// approved persistently.
pub struct NonInteractiveApprover {
    pub cache: Arc<ApprovalCache>,
}

impl NonInteractiveApprover {
    pub fn new(cache: Arc<ApprovalCache>) -> Self {
        NonInteractiveApprover { cache }
    }
}

impl Approver for NonInteractiveApprover {
    fn request(&self, call: &ToolCall, _cwd: &Path, reason: &str, ctx: &ApprovalCtx) -> Decision {
        if self
            .cache
            .is_approved(Some(ctx.session), &fingerprint(call))
        {
            return Decision::Allow;
        }
        Decision::Deny {
            reason: format!("approval required ({reason}) but session is non-interactive"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use kn9t_core::{EventSink, LiveEvent};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<LiveEvent>>,
    }
    impl EventSink for RecordingSink {
        fn emit(&self, e: LiveEvent) {
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

    /// Poll until at least one event is recorded, or panic.
    fn wait_for_event(sink: &RecordingSink, what: &str) -> u64 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let evs = sink.events.lock().unwrap();
                if let Some(LiveEvent::ApprovalRequest { id, .. }) = evs.first() {
                    return id.0;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("{what}: ApprovalRequest never emitted");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // ── The mechanism: emit, block, resolve ──────────────────────────────────

    /// ADR-0008 — an `Ask` from a policy plugin emits `ApprovalRequest`, blocks the calling
    /// thread, and returns whatever `POST /approve` resolved (here: allow).
    #[test]
    fn approver_emits_and_blocks_until_resolved() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let s = sink_c;
            let ctx = ApprovalCtx {
                session: "test-session",
                sink: s.as_ref(),
            };
            a_c.request(&bash_call("rm -rf /"), Path::new("/"), "dangerous", &ctx)
        });

        let id = wait_for_event(&sink, "allow path");
        assert!(reg.resolve(id, Decision::Allow));
        assert_eq!(handle.join().unwrap(), Decision::Allow);
    }

    /// The user's refusal is propagated verbatim, not softened.
    #[test]
    fn approver_propagates_deny() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let s = sink_c;
            let ctx = ApprovalCtx {
                session: "test-session",
                sink: s.as_ref(),
            };
            a_c.request(&bash_call("rm -rf /"), Path::new("/"), "dangerous", &ctx)
        });

        let id = wait_for_event(&sink, "deny path");
        reg.resolve(
            id,
            Decision::Deny {
                reason: "nope".into(),
            },
        );
        assert_eq!(
            handle.join().unwrap(),
            Decision::Deny {
                reason: "nope".into()
            }
        );
    }

    /// ADR-0008 — the plugin's `reason` reaches the prompt, so the user is told *why*.
    #[test]
    fn approver_forwards_plugin_reason() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let s = sink_c;
            let ctx = ApprovalCtx {
                session: "test-session",
                sink: s.as_ref(),
            };
            a_c.request(
                &bash_call("git push"),
                Path::new("/"),
                "not in ALLOW list",
                &ctx,
            )
        });

        let id = wait_for_event(&sink, "reason path");
        let reason = match &sink.events.lock().unwrap()[0] {
            LiveEvent::ApprovalRequest { reason, .. } => reason.clone(),
            _ => panic!("wrong event"),
        };
        assert_eq!(reason, "not in ALLOW list");
        reg.resolve(id, Decision::Allow);
        let _ = handle.join();
    }

    /// With nobody listening there is no one to ask, so the call is denied rather than
    /// hanging forever on a prompt no client will ever see.
    /// 96E-33 — the sink is now a parameter, so "no sink" is unrepresentable: the compiler
    /// rejects a call that omits it. What this test guards instead is the property that
    /// replaced it — an approval driven from a thread that never started a turn still emits
    /// to the session it was told about. Under the previous thread-local design this
    /// returned `Deny { "approval required (no sink)" }`, which is exactly how
    /// `host_api::tool_execute` was silently broken.
    #[test]
    fn approver_emits_from_a_foreign_thread() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache));
        let sink = Arc::new(RecordingSink::default());

        // A thread with no turn on it and no ambient state whatsoever.
        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let ctx = ApprovalCtx {
                session: "sess-foreign",
                sink: sink_c.as_ref(),
            };
            a_c.request(&bash_call("ls"), Path::new("/"), "because", &ctx)
        });

        // The prompt arrives on the session we named, and resolving it unblocks the caller.
        let id = wait_for_event(&sink, "foreign thread");
        assert!(reg.resolve(id, Decision::Allow));
        assert_eq!(handle.join().unwrap(), Decision::Allow);
    }

    /// ADR-0008 — `-p`/CI cannot prompt, so an ask is denied outright. The reason is carried
    /// through so the transcript explains the refusal.
    #[test]
    fn non_interactive_approver_denies_ask() {
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = NonInteractiveApprover::new(cache);
        let sink = Arc::new(RecordingSink::default());
        let ctx = ApprovalCtx {
            session: "test-session",
            sink: sink.as_ref(),
        };
        match a.request(&bash_call("rm x"), Path::new("/"), "mutation", &ctx) {
            Decision::Deny { reason } => assert!(reason.contains("mutation")),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    // ── Scope caching ────────────────────────────────────────────────────────

    /// `scope=session`: the second identical call in the same session is not re-prompted,
    /// but a different session still is.
    #[test]
    fn cache_session_allows_second_call_without_prompt() {
        let reg = Arc::new(ApprovalRegistry::new());
        let cache = Arc::new(ApprovalCache::new_empty());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache.clone()));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let s = sink_c;
            let ctx = ApprovalCtx {
                session: "sess1",
                sink: s.as_ref(),
            };
            a_c.request(
                &bash_call("rm -rf /tmp/x"),
                Path::new("/"),
                "mutation",
                &ctx,
            )
        });
        let id = wait_for_event(&sink, "first ask");
        let meta = reg.get_meta(id).expect("meta must exist");
        cache.approve_session(meta.session_id, meta.fingerprint);
        reg.resolve(id, Decision::Allow);
        assert_eq!(handle.join().unwrap(), Decision::Allow);

        // Same session, same call → answered from cache, no new event.
        sink.events.lock().unwrap().clear();
        let d2 = {
            let s = sink.clone();
            let ctx = ApprovalCtx {
                session: "sess1",
                sink: s.as_ref(),
            };
            a.request(
                &bash_call("rm -rf /tmp/x"),
                Path::new("/"),
                "mutation",
                &ctx,
            )
        };
        assert_eq!(d2, Decision::Allow);
        assert!(
            sink.events.lock().unwrap().is_empty(),
            "session cache must not prompt"
        );

        // A different session is not covered by that approval.
        sink.events.lock().unwrap().clear();
        let a2 = a.clone();
        let sink2 = sink.clone();
        let handle2 = std::thread::spawn(move || {
            let s = sink2;
            let ctx = ApprovalCtx {
                session: "sess2",
                sink: s.as_ref(),
            };
            a2.request(
                &bash_call("rm -rf /tmp/x"),
                Path::new("/"),
                "mutation",
                &ctx,
            )
        });
        let id2 = wait_for_event(&sink, "second session must prompt");
        reg.resolve(
            id2,
            Decision::Deny {
                reason: "test".into(),
            },
        );
        let _ = handle2.join();
    }

    /// `scope=always` survives the session *and* the process: it is written to
    /// `[policy.approvals]` and re-read from disk.
    #[test]
    fn cache_persistent_allows_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let cache = Arc::new(ApprovalCache::new(path.clone()));
        let reg = Arc::new(ApprovalRegistry::new());
        let a = Arc::new(InteractiveApprover::with_cache(reg.clone(), cache.clone()));
        let sink = Arc::new(RecordingSink::default());

        let sink_c = sink.clone();
        let a_c = a.clone();
        let handle = std::thread::spawn(move || {
            let s = sink_c;
            let ctx = ApprovalCtx {
                session: "s1",
                sink: s.as_ref(),
            };
            a_c.request(
                &bash_call("rm -rf /tmp/persist"),
                Path::new("/"),
                "mutation",
                &ctx,
            )
        });
        let id = wait_for_event(&sink, "persist ask");
        let meta = reg.get_meta(id).unwrap();
        cache.approve_persistent(meta.fingerprint.clone()).unwrap();
        reg.resolve(id, Decision::Allow);
        assert_eq!(handle.join().unwrap(), Decision::Allow);

        // A brand-new session is covered, because `always` is not session-scoped.
        sink.events.lock().unwrap().clear();
        let d2 = {
            let s = sink.clone();
            let ctx = ApprovalCtx {
                session: "different",
                sink: s.as_ref(),
            };
            a.request(
                &bash_call("rm -rf /tmp/persist"),
                Path::new("/"),
                "mutation",
                &ctx,
            )
        };
        assert_eq!(d2, Decision::Allow);
        assert!(sink.events.lock().unwrap().is_empty());

        // Durable on disk, and reloadable.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("rm -rf /tmp/persist"),
            "config should contain fingerprint, got {text}"
        );
        let cache2 = ApprovalCache::new(path);
        assert!(cache2.has_persistent("bash:rm -rf /tmp/persist"));
    }

    /// The fingerprint is what makes caching meaningful: `bash` keys on the command text, so
    /// approving `ls` does not silently approve `rm`.
    #[test]
    fn fingerprint_distinguishes_commands() {
        assert_eq!(fingerprint(&bash_call("ls")), "bash:ls");
        assert_ne!(
            fingerprint(&bash_call("ls")),
            fingerprint(&bash_call("rm -rf /"))
        );
    }
}
