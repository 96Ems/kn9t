//! Turn execution + abort/approval registries + auto-titling (R-SRV-100).
//!
//! `prompt` runs a turn on a background OS thread (GI-5: no async), wiring `ReactLoop` to the
//! concrete store, the `kn9t-tools` plugin (R-PLUG2-110), the policy, and the provider; events
//! flow to the session bus via `SessionSink`. After the first assistant turn of a nameless
//! session, a cheap best-effort call generates a title (R-SRV-100).

use kn9t_macros::safe_expect;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kn9t_core::{
    Cancel, Content, Decision, Event, EventSink, HookHost, Message, ModelSpec, MsgId, Price,
    Request, Role, SessionId, Store, Thinking, Tokens, Usage, UsageKind,
};
use kn9t_plugin::ComposedHookHost;
use kn9t_react::{ReactLoop, RunParams};

use crate::bus::SessionSink;
use crate::state::ServerState;
use crate::system_prompt;

/// HookHost wrapper that drains steering messages from `ServerState`, so `POST /steer`
/// messages land after tool_results rather than interleaving them.
struct ServerHookHost {
    inner: Arc<dyn HookHost>,
    state: Arc<ServerState>,
    session: String,
}

impl HookHost for ServerHookHost {
    fn before_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &std::path::Path,
    ) -> kn9t_core::HookVeto {
        self.inner.before_tool_call(tool, args, cwd)
    }

    fn after_tool_call(
        &self,
        tool: &str,
        args: &serde_json::Value,
        cwd: &std::path::Path,
        result: Vec<Content>,
    ) -> Vec<Content> {
        self.inner.after_tool_call(tool, args, cwd, result)
    }

    fn before_request(
        &self,
        msgs: Vec<Message>,
        model: &kn9t_core::ModelRef,
        system: Option<&str>,
    ) -> Vec<Message> {
        self.inner.before_request(msgs, model, system)
    }

    fn should_stop_after_turn(
        &self,
        stop: kn9t_core::StopReason,
        usage: &Usage,
        turn: u32,
    ) -> bool {
        self.inner.should_stop_after_turn(stop, usage, turn)
    }

    fn prepare_next_turn(
        &self,
        stop: kn9t_core::StopReason,
        usage: &Usage,
    ) -> kn9t_core::NextTurnPatch {
        self.inner.prepare_next_turn(stop, usage)
    }

    /// Drain the server's steering queue, then any plugin steering.
    fn get_steering(&self) -> Vec<Message> {
        let mut out = self.state.drain_steering(&self.session);
        out.extend(self.inner.get_steering());
        out
    }

    fn get_followup(&self) -> Vec<Message> {
        self.inner.get_followup()
    }

    fn get_api_key(&self, provider: &str) -> Option<String> {
        self.inner.get_api_key(provider)
    }
}

/// `ServerState` as a live tool source (GI-1: the loop sees only `Arc<dyn ToolSource>`).
/// `snapshot()` clones the registry once per model call — cheap next to the round-trip.
struct LiveTools {
    state: Arc<ServerState>,
}

impl kn9t_react::ToolSource for LiveTools {
    fn snapshot(&self) -> kn9t_core::ToolRegistry {
        // scanning here (not on a timer) makes a crash observable within the running
        // turn; the scan is a cheap per-host flag read that announces each failure once.
        self.state.scan_plugin_health();
        self.state.tools_snapshot()
    }

    fn blocked(&self) -> std::collections::HashSet<String> {
        self.state.blocked_tools()
    }
}

/// the first host declaring `compactor` becomes the delegate; `None` leaves the loop
/// fail-closed. The plugin drives its own turn via the host_api ops.
fn compactor_from_hosts(
    hosts: &[Arc<kn9t_plugin::PluginHost>],
) -> Option<Arc<dyn kn9t_core::Compactor>> {
    hosts
        .iter()
        .find(|h| h.has_capability("compactor"))
        .map(|h| {
            Arc::new(kn9t_plugin::RemoteCompactor::new(h.clone())) as Arc<dyn kn9t_core::Compactor>
        })
}

/// B5/B6 — one turn's registration in `ServerState::aborts`, released on `Drop`.
///
/// The id makes every write a compare-and-swap, so a stale teardown or ESC cannot touch a
/// successor turn. `Drop` runs on the normal path, on `?`, and on panic unwind — the slot
/// and the idle count cannot outlive their turn and wedge the session at 409.
pub struct TurnSlot {
    state: Arc<ServerState>,
    session: String,
    id: u64,
    cancel: Cancel,
}

impl TurnSlot {
    /// Claim `session`'s turn slot with a fresh `Cancel`, displacing any previous holder.
    pub fn register(state: &Arc<ServerState>, session: &str) -> Self {
        Self::register_with(state, session, Cancel::new())
    }

    /// As [`TurnSlot::register`], but adopting an existing `Cancel` — used where the caller
    /// must hold the handle before the turn starts (a sub-agent inheriting its parent's).
    pub fn register_with(state: &Arc<ServerState>, session: &str, cancel: Cancel) -> Self {
        let id = state.next_turn_id.fetch_add(1, Ordering::SeqCst);
        safe_expect!(state.aborts.lock(), "aborts poisoned")
            .insert(session.to_owned(), (id, cancel.clone()));
        // Paired with `turn_ended()` in `Drop` so the idle counter cannot drift from the
        // registration; a leaked count pins the process alive (`IdleTracker::should_exit`).
        state.idle.turn_started();
        crate::log!("[turn] registered session={} turn_id={}", session, id);
        TurnSlot {
            state: state.clone(),
            session: session.to_owned(),
            id,
            cancel,
        }
    }

    /// This turn's id, so an `abort` can name the turn it meant.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The `Cancel` this turn runs under.
    pub fn cancel(&self) -> &Cancel {
        &self.cancel
    }
}

/// B5/B6 — release `session`'s slot only if `id` still owns it. Idempotent: shared by
/// `TurnSlot::drop` and the `TurnFinishing` interception, whichever runs first wins.
pub(crate) fn release_turn(state: &Arc<ServerState>, session: &str, id: u64) {
    let mut map = safe_expect!(state.aborts.lock(), "aborts poisoned");
    match map.get(session) {
        Some((cur, _)) if *cur == id => {
            map.remove(session);
            crate::log!("[turn] released session={} turn_id={}", session, id);
        }
        _ => crate::log!(
            "[turn] stale release ignored session={} turn_id={}",
            session,
            id
        ),
    }
}

impl Drop for TurnSlot {
    fn drop(&mut self) {
        // Backstop for the sink's early release; also covers compose failure and panic unwind.
        release_turn(&self.state, &self.session, self.id);
        // Falls exactly once per turn whatever the exit path (`release_turn` may have run already).
        self.state.idle.turn_ended();
    }
}

/// Check if a turn is currently running for `session`.
pub fn is_turn_running(state: &Arc<ServerState>, session: &str) -> bool {
    safe_expect!(state.aborts.lock(), "aborts poisoned").contains_key(session)
}

/// Fire the cancel for `session`'s running turn. `Some(id)` cancels only if that turn still
/// owns the slot (a late ESC is a no-op, not a hit on the successor); `None` hits whatever runs
/// now, which is what `POST /abort` wants. Fired with the lock held so no successor can register
/// in between.
pub fn abort_turn(state: &Arc<ServerState>, session: &str, turn_id: Option<u64>) {
    let map = safe_expect!(state.aborts.lock(), "aborts poisoned");
    match map.get(session) {
        Some((id, cancel)) if turn_id.is_none_or(|want| want == *id) => {
            crate::log!("[abort] firing cancel session={} turn_id={}", session, id);
            cancel.cancel();
        }
        Some((id, _)) => crate::log!(
            "[abort] stale abort ignored session={} running_turn={} requested={:?}",
            session,
            id,
            turn_id
        ),
        None => crate::log!("[abort] no turn registered session={}", session),
    }
}

/// Fire the cancel for `session`'s running turn, whichever it is (`POST /abort`).
pub fn abort(state: &Arc<ServerState>, session: &str) {
    abort_turn(state, session, None)
}

/// Test seam for [`release_turn`], which is crate-private because only the sink and the guard
/// may call it. Exposed so a test can reproduce the sink's early release without a bus.
#[doc(hidden)]
pub fn release_turn_for_test(state: &Arc<ServerState>, session: &str, id: u64) {
    release_turn(state, session, id)
}

/// The id of the turn currently registered for `session`, if any.
pub(crate) fn running_turn_id(state: &Arc<ServerState>, session: &str) -> Option<u64> {
    safe_expect!(state.aborts.lock(), "aborts poisoned")
        .get(session)
        .map(|(id, _)| *id)
}

/// Get the Cancel for `session`'s running turn, if any.
/// used by host_api to pass Cancel to blocking waits.
pub fn get_cancel(state: &Arc<ServerState>, session: &str) -> Option<Cancel> {
    safe_expect!(state.aborts.lock(), "aborts poisoned")
        .get(session)
        .map(|(_, c)| c.clone())
}

/// Record an approval decision (R-SRV-010 `/approve`): allow/always -> Allow, else Deny,
/// resolved via the command path, never the bus (DESIGN §10). Legacy; prefer `resolve_approval`.
pub fn record_approval(state: &Arc<ServerState>, id: u64, allow: bool) {
    let decision = if allow {
        Decision::Allow
    } else {
        Decision::Deny {
            reason: "denied by user".into(),
        }
    };
    // Resolving an unknown id is a no-op 200 (`approve_no_pending_is_ok`).
    let _ = state.approval_registry.resolve(id, decision);
}

/// New scope-aware approval: validates `decision` and `scope`, resolves the
/// registry, and updates session/persistent caches. Returns error string for 400.
pub fn resolve_approval(
    state: &Arc<ServerState>,
    id: u64,
    decision_str: &str,
    scope_str: Option<&str>,
) -> Result<Decision, String> {
    // Validate decision — return 400 on unknown instead of default-deny (F4 fix).
    let is_allow = match decision_str {
        "allow" | "always" => true,
        "deny" => false,
        other => {
            return Err(format!(
                "unknown decision {other:?}; expected allow|deny|always"
            ))
        }
    };
    let scope = match scope_str {
        Some(s) => match s {
            "once" | "session" | "always" => s,
            other => {
                return Err(format!(
                    "unknown scope {other:?}; expected once|session|always"
                ))
            }
        },
        None => {
            // Legacy: decision "always" implies scope always, else once
            if decision_str == "always" {
                "always"
            } else {
                "once"
            }
        }
    };
    // Handle legacy "always" decision as scope always (DESIGN §10)
    let effective_scope = if decision_str == "always" {
        "always"
    } else {
        scope
    };

    let decision = if is_allow {
        Decision::Allow
    } else {
        Decision::Deny {
            reason: "denied by user".into(),
        }
    };

    // Resolve with meta for caching
    if let Some(meta) = state
        .approval_registry
        .resolve_with_meta(id, decision.clone())
    {
        // Only cache Allow decisions, never Deny, and never HardDeny (which has no meta)
        if is_allow {
            match effective_scope {
                "session" => {
                    // Use the session id from the registry meta (captured at request time)
                    let sid = if meta.session_id.is_empty() {
                        // fallback: no session id captured, nothing to cache
                        String::new()
                    } else {
                        meta.session_id.clone()
                    };
                    if !sid.is_empty() {
                        state.approval_cache.approve_session(sid, meta.fingerprint);
                    }
                }
                "always" => {
                    // Persistent: write back to config.toml (HardDeny emits no meta, so this is safe).
                    if let Err(e) = state.approval_cache.approve_persistent(meta.fingerprint) {
                        // Log but don't fail the approval — the turn is unblocked regardless.
                        crate::log!("approve_persistent failed: {e}");
                    }
                }
                _ => {} // once: nothing
            }
        }
    } else {
        // No pending id: fall back to plain resolve for idempotency.
        let _ = state.approval_registry.resolve(id, decision.clone());
    }
    Ok(decision)
}

/// Per-run tool gating: tools blocked for this session, plus a one-shot re-enable notice
/// (last `ToolsToggled` wins; drains `ServerState::pending_reactivation`).
fn tool_gating_for_session(
    state: &Arc<ServerState>,
    session: &SessionId,
) -> (std::collections::HashSet<String>, Option<Message>) {
    let disabled: std::collections::HashSet<String> = state
        .store
        .snapshot(session)
        .map(|s| s.disabled_tools.into_iter().collect())
        .unwrap_or_default();

    let reactivated: Vec<String> = {
        let mut map = state
            .pending_reactivation
            .lock()
            .expect("pending_reactivation poisoned");
        map.remove(&session.0)
            .map(|set| {
                let mut v: Vec<String> = set.into_iter().collect();
                v.sort();
                v
            })
            .unwrap_or_default()
    };

    let reminder = if reactivated.is_empty() {
        None
    } else {
        let list = reactivated.join(", ");
        Some(Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![Content::Text {
                text: format!(
                    "<system-reminder>The following tool(s) have been re-enabled for this \
                     session and are available again: {list}. You may use them now.</system-reminder>"
                ),
            }],
            silent: true,
        })
    };

    (disabled, reminder)
}

/// `session`'s cwd from the database, or the server's cwd if unknown.
pub(crate) fn get_session_cwd(state: &Arc<ServerState>, session: &SessionId) -> std::path::PathBuf {
    state
        .store
        .query_one(
            "SELECT cwd FROM sessions WHERE id=?1",
            &[&session.0.as_str()],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.cwd.clone())
}

/// Compose the full `ReactLoop`: bus + sink, hooks, provider, tools, compactor. Single
/// composition point shared by `spawn_turn` and the host_api `session_prompt` op.
/// `session_cwd` is the session's directory, not the server process cwd.
pub(crate) fn compose_loop(
    state: &Arc<ServerState>,
    session: &SessionId,
    model: &ModelSpec,
    tool_names: Option<Vec<String>>,
    session_cwd: &std::path::Path,
) -> Result<(ReactLoop, Arc<dyn EventSink>), String> {
    let bus = state.buses.bus_for(&session.0);
    // R-STOR-116: the store salvages in-flight tool progress; `state` lets the sink release
    // the turn slot on TurnFinishing.
    let sink: Arc<dyn EventSink> = Arc::new(SessionSink::with_store(
        bus.clone(),
        state.store.clone(),
        session.clone(),
        state.clone(),
    ));

    // Compose hooks from all plugins (R-PLUG-060); each host gets the bus and session for events.
    let hosts = state.hosts_snapshot();
    // ADR-0008: a test may install hooks in-process rather than spawning a policy plugin.
    let inner_hooks: Arc<dyn HookHost> = if let Some(h) = state.hooks_override_snapshot() {
        h
    } else if hosts.is_empty() {
        Arc::new(kn9t_core::NoopHookHost)
    } else {
        for host in &hosts {
            host.set_bus(sink.clone());
            host.set_session(&session.0);
            host.set_cwd(session_cwd);
        }
        Arc::new(ComposedHookHost::new(hosts.clone()))
    };
    let hooks: Arc<dyn HookHost> = Arc::new(ServerHookHost {
        inner: inner_hooks,
        state: state.clone(),
        session: session.0.clone(),
    });

    let tools: Arc<dyn kn9t_react::ToolSource> = {
        // a live handle, re-read on every model call, so a plugin stop/start/reload
        // mid-turn is observed now rather than at the next prompt.
        let live: Arc<dyn kn9t_react::ToolSource> = Arc::new(LiveTools {
            state: state.clone(),
        });
        match tool_names {
            // Filtered per snapshot, so a child keeps observing lifecycle changes for its tools.
            Some(names) => Arc::new(kn9t_react::FilteredTools { inner: live, names }),
            None => live,
        }
    };

    // Clone the provider out of the lock: the turn owns this `Arc`, so a config reload cannot
    // swap it mid-stream (R-SRV-CFG-100).
    let provider = state
        .get_provider(&model.r#ref.provider)
        .or_else(|| state.provider_snapshot())
        .ok_or_else(|| format!("no provider for {}", model.r#ref.provider))?;

    Ok((
        ReactLoop {
            provider: provider.clone(),
            store: state.store.clone(),
            approver: state.approver_snapshot(),
            tools,
            hooks,
            bus: sink.clone(),
            compactor: compactor_from_hosts(&hosts),
        },
        sink,
    ))
}

/// Run one synchronous turn on `session`, returning the final assistant text. A spawned
/// session running a turn *is* a sub-agent. `parent_cancel` propagates the parent's
/// ESC; without it a fresh `Cancel` with only a timeout watchdog is created.
pub(crate) fn run_session_turn(
    state: &Arc<ServerState>,
    session: &SessionId,
    text: &str,
    tool_names: Option<Vec<String>>,
    timeout_s: u64,
    parent_cancel: Option<Cancel>,
) -> Result<String, String> {
    // The model spec comes from the session's fork (model_at_fork) or default.
    let model = state
        .store
        .get_model_spec_for_session(&session.0)
        .or_else(|| state.default_model_snapshot())
        .ok_or_else(|| "no model available for session".to_string())?;
    state.store.register_model_spec(model.clone());

    // Append the user message durably.
    state
        .store
        .append(
            session,
            Event::MessageAppended {
                seq: 0,
                msg: Message {
                    id: MsgId::new(),
                    role: Role::User,
                    content: vec![Content::Text {
                        text: text.to_string(),
                    }],
                    silent: false,
                },
            },
        )
        .map_err(|e| format!("append message: {}", e.0))?;

    // This turn runs on the caller's thread, so `compose_loop` overwrites its TL_SESSION/TL_BUS;
    // restore on exit or later hooks mis-attribute to the child (AGENTS.md reminder leak).
    let _scope = kn9t_plugin::SessionScope::capture();

    // Use the session's cwd from the database, not the server's process cwd.
    let session_cwd = get_session_cwd(state, session);
    let (loop_, _sink) = compose_loop(state, session, &model, tool_names, &session_cwd)?;

    // B11: the child gets its OWN `Cancel`; the parent's is only observed. Propagation still
    // matters (ESC on the parent aborts the sub-agent), so a watcher forwards
    // cancellation down. Both threads exit when the child is done, scoping the timeout here.
    let cancel = Cancel::new();
    let child_done = Arc::new(AtomicBool::new(false));

    // Watchdog fires on the child only; the loop aborts at its next cancel checkpoint.
    {
        let cancel_watch = cancel.clone();
        let done = child_done.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(timeout_s);
            while std::time::Instant::now() < deadline {
                if done.load(Ordering::SeqCst) {
                    return; // child finished; never touch its cancel
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !done.load(Ordering::SeqCst) {
                cancel_watch.cancel();
            }
        });
    }

    // ESC propagation: parent cancelled -> child cancelled. One-directional, so the child
    // can never cancel its parent.
    if let Some(parent) = parent_cancel {
        let child = cancel.clone();
        let done = child_done.clone();
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if parent.cancelled() {
                    child.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }

    // Releases both helper threads on every exit path below, including the `?`s.
    struct DoneGuard(Arc<AtomicBool>);
    impl Drop for DoneGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _done_guard = DoneGuard(child_done.clone());

    // B6: the child session gets its own registration, so `/abort` on the child id works
    // and `is_turn_running` is honest about it. Previously only the parent was registered,
    // which made a sub-agent turn invisible to both.
    let _child_slot = TurnSlot::register_with(state, &session.0, cancel.clone());

    let (disabled_tools, reactivation_reminder) = tool_gating_for_session(state, session);
    let params = RunParams {
        session: session.clone(),
        model: model.clone(),
        thinking: model.thinking,
        max_tokens: Some(model.max_out),
        cwd: session_cwd.clone(),
        config: state.react_config(),
        read_map: Arc::new(Mutex::new(std::collections::HashMap::new())),
        system: Some(system_prompt::default_system_prompt()),
        cancel: Some(cancel),
        disabled_tools,
        reactivation_reminder,
    };
    loop_
        .run(params)
        .map_err(|e| format!("session turn failed: {e:?}"))?;

    // Budget enforcement against the fork snapshot (R-PLUG-130).
    let budget: Option<f64> = state
        .store
        .query_one(
            "SELECT budget_remaining_usd FROM sessions WHERE id=?1",
            &[&session.0.as_str()],
            |r| r.get(0),
        )
        .ok();
    if let Some(budget) = budget {
        let spent: f64 = state
            .store
            .query_one(
                "SELECT COALESCE(SUM(cost_usd),0) FROM usage WHERE session_id=?1",
                &[&session.0.as_str()],
                |r| r.get::<_, f64>(0),
            )
            .unwrap_or(0.0);
        if spent > budget {
            return Err(format!(
                "session budget exceeded: spent ${spent:.4} > ${budget:.4}"
            ));
        }
    }

    // Final assistant text.
    let content: Option<String> = state
        .store
        .query_one(
            "SELECT content FROM messages WHERE session_id=?1 AND role='assistant'
             ORDER BY seq DESC LIMIT 1",
            &[&session.0.as_str()],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let text = content
        .and_then(|json| serde_json::from_str::<Vec<Content>>(&json).ok())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("session produced no assistant text".into());
    }
    Ok(text)
}

/// Spawn a background turn for `session`. If no provider is wired, this is a no-op
/// (the user message is already durably appended).
pub fn spawn_turn(state: Arc<ServerState>, session: SessionId) {
    // Use session's current model (from ModelChanged events), fallback to default.
    let session_model = state.store.get_model_spec_for_session(&session.0);
    crate::log!(
        "[spawn_turn] session={} session_model={:?}",
        session.0,
        session_model
            .as_ref()
            .map(|m| format!("{}:{}", m.r#ref.provider, m.r#ref.id))
    );
    let model = session_model.or_else(|| {
        crate::log!("[spawn_turn] using default model");
        state.default_model_snapshot()
    });
    let Some(model) = model else {
        crate::log!("[spawn_turn] no model available");
        return;
    };
    crate::log!(
        "[spawn_turn] using model {}:{}",
        model.r#ref.provider,
        model.r#ref.id
    );

    std::thread::spawn(move || {
        // B6: one guard owns the running-turn fact, its `Cancel`, and the idle count, and
        // releases all three on every exit path including a panic.
        let slot = TurnSlot::register(&state, &session.0);
        let cancel = slot.cancel().clone();

        // The model spec must be registered with the store so `plan_request` can
        // compute cache breakpoints and the compaction threshold.
        state.store.register_model_spec(model.clone());

        // The session's cwd, not the server's: one server may host sessions in different directories.
        let session_cwd = get_session_cwd(&state, &session);

        // Single composition point. The loop passes its sink through `ApprovalCtx`,
        // so nothing is threaded by hand here.
        let (loop_, _sink) = match compose_loop(&state, &session, &model, None, &session_cwd) {
            Ok(v) => v,
            Err(e) => {
                crate::log!("[spawn_turn] compose failed: {e}");
                // `slot` drops here, releasing the registration and the idle count.
                return;
            }
        };

        let (disabled_tools, reactivation_reminder) = tool_gating_for_session(&state, &session);
        let params = RunParams {
            session: session.clone(),
            model: model.clone(),
            thinking: model.thinking,
            max_tokens: Some(model.max_out),
            cwd: session_cwd,
            config: state.react_config(),
            read_map: Arc::new(Mutex::new(HashMap::new())),
            system: Some(system_prompt::default_system_prompt()),
            cancel: Some(cancel.clone()),
            disabled_tools,
            reactivation_reminder,
        };

        crate::log!("turn started: session={}", session.0);
        let run_result = loop_.run(params);
        match run_result {
            Ok(_) => crate::log!("turn finished: session={}", session.0),
            Err(e) => crate::log!("turn error: session={} error={e:?}", session.0),
        }

        // R-SRV-100: title after the first assistant turn. `slot` stays alive so `should_exit()`
        // cannot reap mid-titling; `aborts` was already released, so a new `/prompt` is accepted.
        maybe_autotitle(&state, &session);
        drop(slot);
    });
}

/// R-SRV-100 — best-effort auto-title: one cheap provider call, then set the name and record
/// a `UsageKind::Title` usage row. Skipped if already named or no assistant message; failures
/// are swallowed.
pub fn maybe_autotitle(state: &Arc<ServerState>, session: &SessionId) {
    crate::log!("[autotitle] checking session={}", session.0);

    // Already named? A name (at creation or via API) suppresses auto-titling.
    let name: Option<String> = state
        .store
        .query_one(
            "SELECT name FROM sessions WHERE id=?1",
            &[&session.0.as_str()],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    if name.is_some() {
        crate::log!("[autotitle] already named, skipping");
        return;
    }

    // Require at least one assistant message (the "first assistant turn").
    let assistant_count: i64 = state
        .store
        .query_one(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND role='assistant'",
            &[&session.0.as_str()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if assistant_count == 0 {
        crate::log!("[autotitle] no assistant messages yet, skipping");
        return;
    }

    // Configured title model, else the session's own — never a cheapest-on-provider pick
    // (that landed on a model which returns no text).
    let session_model = state.store.get_model_spec_for_session(&session.0);
    let default_model = state.default_model_snapshot();
    let model = state
        .title_model_snapshot()
        .or_else(|| session_model.clone())
        .or_else(|| default_model.clone());

    let Some(model) = model else {
        crate::log!("[autotitle] no model available");
        return;
    };

    // Use the provider matching the model, not the default provider
    let provider = state
        .get_provider(&model.r#ref.provider)
        .or_else(|| state.provider_snapshot());

    let Some(provider) = provider else {
        crate::log!("[autotitle] no provider for {}", model.r#ref.provider);
        return;
    };
    crate::log!(
        "[autotitle] using model {}:{}",
        model.r#ref.provider,
        model.r#ref.id
    );

    // Gather a short transcript excerpt to title from (first user message text).
    let excerpt: String = state
        .store
        .query_one(
            "SELECT content FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq LIMIT 1",
            &[&session.0.as_str()],
            |r| r.get(0),
        )
        .ok()
        .map(|c: String| first_text(&c))
        .unwrap_or_default();

    let title_prompt = Message {
        id: MsgId::new(),
        role: Role::User,
        content: vec![Content::Text {
            text: format!(
                "Give a terse 3-6 word title (no quotes) for a session that began with: {excerpt}"
            ),
        }],
        silent: false,
    };
    let messages = vec![title_prompt];
    let no_tools = Vec::new();
    let no_cache = Vec::new();
    let req = Request {
        model: &model,
        system: Some("You produce only a short title, nothing else."),
        messages: &messages,
        tools: &no_tools,
        thinking: Thinking::Off,
        // 512 leaves room for a reasoning model's scratchpad plus the few title tokens;
        // at 32 the stream ended empty and read as "skip".
        max_tokens: Some(512),
        cache: &no_cache,
        session: Some(session.0.as_str()),
    };

    let cancel = Cancel::new();
    crate::log!("[autotitle] calling provider.stream()");
    let stream = match provider.stream(&req, &cancel) {
        Ok(s) => s,
        Err(e) => {
            crate::log!("[autotitle] provider.stream() failed: {e:?}");
            return;
        }
    };

    // Fold the (small) title stream into text + usage.
    let mut title = String::new();
    let mut tokens = Tokens::default();
    let mut usage_reported = false;
    for item in stream {
        match item {
            Ok(chunk) => match chunk {
                kn9t_core::Chunk::Text { delta, .. } => title.push_str(&delta),
                kn9t_core::Chunk::Usage(u) => {
                    tokens = u.tokens;
                    usage_reported = true;
                }
                _ => {}
            },
            Err(e) => {
                crate::log!("[autotitle] stream error: {e:?}");
                return;
            }
        }
    }
    crate::log!("[autotitle] raw title: {:?}", title);
    let title = sanitize_title(&title);
    if title.is_empty() {
        crate::log!("[autotitle] title empty after sanitize, skipping");
        return;
    }
    crate::log!("[autotitle] setting title: {:?}", title);

    // Persist the name.
    let _ = state.store.execute_raw(
        "UPDATE sessions SET name=?1 WHERE id=?2 AND name IS NULL",
        &[&title.as_str(), &session.0.as_str()],
    );

    // Emit TitleChanged so TUI can update the sidebar.
    state.buses.publish(
        &session.0,
        Event::TitleChanged {
            title: title.clone(),
        },
    );

    // R-CORE-150 / R-SRV-100: titling is a server-owned side call, so the server records
    // the usage row directly rather than through the loop.
    let usage = Usage {
        tokens,
        model: model.r#ref.clone(),
    };
    let cost_micros = compute_cost(&usage.tokens, &model.price);
    let cost_usd = cost_micros as f64 / 1_000_000.0;
    let _ = state.store.append(
        session,
        kn9t_core::Event::UsageRecorded {
            seq: 0,
            provider: model.r#ref.provider.clone(),
            model: model.r#ref.id.clone(),
            kind: UsageKind::Title,
            tokens: usage.tokens,
            price_snapshot: model.price,
            cost_micros,
            cost_usd,
            estimated: !usage_reported,
        },
    );
}

/// The first `Text` block's content from a serialized `Vec<Content>` JSON.
fn first_text(content_json: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(content_json).unwrap_or(serde_json::Value::Null);
    if let Some(arr) = v.as_array() {
        for c in arr {
            if c.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                    let t = t.trim();
                    return t.chars().take(200).collect();
                }
            }
        }
    }
    String::new()
}

fn sanitize_title(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect()
}

fn compute_cost(tokens: &Tokens, price: &Price) -> i64 {
    kn9t_core::cost_micros(tokens, price)
}

/// Spawn a background compaction for `session` (fire-and-forget; emits `Compacted` or `Error`).
/// fail-closed, same as the automatic path (`exec.rs:223`): the compactor plugin is the
/// only engine. With none, the session runs to the context ceiling — a host-side summary
/// fallback would destroy the span and look like success.
pub fn spawn_compact(
    state: Arc<ServerState>,
    session: SessionId,
    compact_span: kn9t_core::CompactSpan,
) {
    std::thread::spawn(move || {
        crate::log!("[spawn_compact] starting: session={}", session.0);

        // Registration makes compaction abortable and rejects a concurrent /prompt (which
        // would race the Compacted event); the B6 guard releases on every exit path.
        let _slot = TurnSlot::register(&state, &session.0);

        let model_ref = state
            .store
            .get_model_spec_for_session(&session.0)
            .or_else(|| state.default_model_snapshot())
            .map(|m| m.r#ref.clone())
            .unwrap_or_else(|| kn9t_core::ModelRef {
                provider: "unknown".into(),
                id: "unknown".into(),
            });

        // A fresh thread: `compose_loop` sets the plugin host's thread-local session/bus/cwd,
        // so this one must too, or the hook payload carries `session: null` and the plugin bails.
        let bus = state.buses.bus_for(&session.0);
        let sink: Arc<dyn EventSink> = Arc::new(SessionSink::with_store(
            bus,
            state.store.clone(),
            session.clone(),
            state.clone(),
        ));

        // the compactor plugin is the ONLY compaction engine.
        let compaction_result = {
            let hosts = state.hosts_snapshot();
            let compactor_host = hosts
                .iter()
                .find(|h| h.has_capability("compactor"))
                .cloned();

            match compactor_host {
                Some(host) => {
                    // Use the session's cwd, not the server's process cwd.
                    let session_cwd = get_session_cwd(&state, &session);
                    host.set_bus(sink.clone());
                    host.set_session(&session.0);
                    host.set_cwd(&session_cwd);

                    use kn9t_core::Compactor;
                    kn9t_plugin::RemoteCompactor::new(host)
                        .compact(compact_span.clone(), &model_ref)
                }
                None => Err(
                    "no compactor plugin installed; compaction unavailable (session continues \
                     uncompacted until the context ceiling)"
                        .to_string(),
                ),
            }
        };

        let compaction_plan = match compaction_result {
            Ok(plan) => plan,
            Err(e) => {
                // Surface the failure: a fallback that looks like success hides a broken compactor.
                crate::log!("[spawn_compact] compactor failed: {e}");
                state.buses.publish(
                    &session.0,
                    Event::Error {
                        message: format!("Compaction failed: {e}"),
                    },
                );
                // `_slot` drops here, releasing the registration and the idle count.
                return;
            }
        };

        let event = Event::Compacted {
            seq: 0,
            replaced: compact_span.replaced,
            summary: compaction_plan.summary,
        };

        match state.store.append(&session, event) {
            Ok(seq) => {
                crate::log!(
                    "[spawn_compact] completed: session={} seq={}",
                    session.0,
                    seq
                );
            }
            Err(e) => {
                crate::log!("[spawn_compact] store error: {}", e.0);
                state.buses.publish(
                    &session.0,
                    Event::Error {
                        message: format!("Compaction failed: {}", e.0),
                    },
                );
            }
        }
        // `_slot` drops here: registration and idle count released together.
    });
}




