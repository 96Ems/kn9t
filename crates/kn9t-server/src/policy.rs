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

use kn9t_core::{ApprovalCtx, ApprovalId, Approver, Cancel, Decision, LiveEvent, ToolCall};

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
            .is_some_and(|s| s.contains(fp))
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

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
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

    /// Block until `id` is resolved or `cancel` fires.
    /// 96E-39: Returns Deny if cancelled (ESC during pending approval = abort).
    fn wait(&self, slot: Arc<ApprovalSlot>, id: u64, cancel: &Cancel) -> Decision {
        let mut guard = slot
            .decision
            .lock()
            .expect("policy.rs: ApprovalRegistry::wait decision lock poisoned");

        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

        while guard.is_none() {
            // 96E-39: Check cancel at each iteration
            if cancel.cancelled() {
                eprintln!("[approval] wait id={} cancelled", id);
                // Clean up the pending slot
                self.inner
                    .lock()
                    .expect("policy.rs: ApprovalRegistry::wait cleanup lock poisoned")
                    .remove(&id);
                self.meta
                    .lock()
                    .expect("policy.rs: ApprovalRegistry::wait meta cleanup lock poisoned")
                    .remove(&id);
                return Decision::Deny {
                    reason: "cancelled".to_string(),
                };
            }

            let (new_guard, _timeout) = slot
                .cvar
                .wait_timeout(guard, POLL_INTERVAL)
                .expect("policy.rs: ApprovalRegistry::wait condvar wait poisoned");
            guard = new_guard;
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
        // 96E-39: pass cancel so ESC can abort the approval wait.
        let decision = self.registry.wait(slot, id, ctx.cancel);
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

