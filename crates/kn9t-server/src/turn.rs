//! Turn execution + abort/approval registries + auto-titling (R-SRV-100).
//!
//! `prompt` spawns a turn on a background OS thread (GI-5: no async). The turn
//! drives `kn9t_react::ReactLoop`, wired here with the concrete `SqliteStore`, the
//! tools from the `kn9t-tools` plugin subprocess (R-PLUG2-110), the server policy,
//! and the injected provider. Events flow to the session bus via `SessionSink`.
//!
//! After the first assistant turn of a nameless session, one cheap provider call
//! generates a title, recorded as `UsageKind::Title` (R-SRV-100). It is
//! best-effort: any failure leaves `name` null and surfaces no client error.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kn9t_core::{
    Cancel, Content, Decision, Event, EventSink, HookHost, Message, MsgId, Price, Request, Role,
    SessionId, Store, Thinking, Tokens, Usage, UsageKind,
};
use kn9t_plugin::ComposedHookHost;
use kn9t_react::{ReactConfig, ReactLoop, RunParams};

use crate::bus::SessionSink;
use crate::state::ServerState;
use crate::system_prompt;

/// Per-session cancellation handles for `abort` (R-SRV-060 command). A running turn
/// registers its `Cancel`; `abort` fires it.
static ABORTS: Mutex<Option<HashMap<String, Cancel>>> = Mutex::new(None);

fn aborts() -> std::sync::MutexGuard<'static, Option<HashMap<String, Cancel>>> {
    let mut g = ABORTS.lock().expect("aborts poisoned");
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    g
}

fn register_cancel(session: &str, cancel: Cancel) {
    aborts().as_mut().unwrap().insert(session.to_owned(), cancel);
}
fn clear_cancel(session: &str) {
    aborts().as_mut().unwrap().remove(session);
}

/// Check if a turn is currently running for `session`.
pub fn is_turn_running(session: &str) -> bool {
    aborts().as_ref().unwrap().contains_key(session)
}

/// Fire the cancel for `session`'s running turn, if any.
pub fn abort(_state: &Arc<ServerState>, session: &str) {
    crate::log!("[DEBUG abort] session={}", session);
    if let Some(c) = aborts().as_ref().unwrap().get(session) {
        crate::log!("[DEBUG abort] firing cancel for session={}", session);
        c.cancel();
    } else {
        crate::log!("[DEBUG abort] no cancel registered for session={}", session);
    }
}

/// Record an approval decision (R-SRV-010 `/approve`).
/// `decision` is `"allow"`/`"always"` -> Allow, else Deny. Resolved via the
/// command path (never the bus) — DESIGN §10.
/// Kept for backward compat; new code should use `resolve_approval`.
pub fn record_approval(state: &Arc<ServerState>, id: u64, allow: bool) {
    let decision = if allow {
        Decision::Allow
    } else {
        Decision::Deny {
            reason: "denied by user".into(),
        }
    };
    // Resolve the blocking InteractivePolicy waiter if any. If no pending
    // request with this id, this is a no-op 200 (idempotent, test `approve_no_pending_is_ok`).
    let _ = state.approval_registry.resolve(id, decision);
}

/// New scope-aware approval: validates `decision` and `scope`, resolves the
/// registry, and updates session/persistent caches. Returns error string for 400.
pub fn resolve_approval(state: &Arc<ServerState>, id: u64, decision_str: &str, scope_str: Option<&str>) -> Result<Decision, String> {
    // Validate decision — return 400 on unknown instead of default-deny (F4 fix).
    let is_allow = match decision_str {
        "allow" | "always" => true,
        "deny" => false,
        other => return Err(format!("unknown decision {other:?}; expected allow|deny|always")),
    };
    let scope = match scope_str {
        Some(s) => match s {
            "once" | "session" | "always" => s,
            other => return Err(format!("unknown scope {other:?}; expected once|session|always")),
        },
        None => {
            // Legacy: decision "always" implies scope always, else once
            if decision_str == "always" { "always" } else { "once" }
        }
    };
    // Handle legacy "always" decision as scope always (DESIGN §10)
    let effective_scope = if decision_str == "always" { "always" } else { scope };

    let decision = if is_allow {
        Decision::Allow
    } else {
        Decision::Deny { reason: "denied by user".into() }
    };

    // Resolve with meta for caching
    if let Some(meta) = state.approval_registry.resolve_with_meta(id, decision.clone()) {
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
                    // Persistent — write back to config.toml
                    // HardDeny commands are never emitted, so no meta for them; this is safe.
                    if let Err(e) = state.approval_cache.approve_persistent(meta.fingerprint) {
                        // Log but don't fail the approval — the turn is unblocked regardless.
                        crate::log!("approve_persistent failed: {e}");
                    }
                }
                _ => {} // once: nothing
            }
        }
    } else {
        // No pending approval with this id — still need to resolve for waiting turn?
        // `resolve_with_meta` already tried; fallback to plain resolve for idempotency.
        let _ = state.approval_registry.resolve(id, decision.clone());
    }
    Ok(decision)
}

/// Spawn a background turn for `session`. If no provider is wired, this is a no-op
/// (the user message is already durably appended).
pub fn spawn_turn(state: Arc<ServerState>, session: SessionId) {
    // Use session's current model (from ModelChanged events), fallback to default.
    let session_model = state.store.get_model_spec_for_session(&session.0);
    crate::log!("[spawn_turn] session={} session_model={:?}", session.0, session_model.as_ref().map(|m| format!("{}:{}", m.r#ref.provider, m.r#ref.id)));
    let model = session_model.or_else(|| {
        crate::log!("[spawn_turn] using default model");
        state.default_model.clone()
    });
    let Some(model) = model else {
        crate::log!("[spawn_turn] no model available");
        return;
    };
    crate::log!("[spawn_turn] using model {}:{}", model.r#ref.provider, model.r#ref.id);
    
    // Get the provider for this model.
    let provider = state.get_provider(&model.r#ref.provider)
        .or_else(|| state.provider.clone());
    let Some(provider) = provider else {
        crate::log!("[spawn_turn] no provider for {}", model.r#ref.provider);
        return;
    };
    crate::log!("[spawn_turn] using provider {}", model.r#ref.provider);

    std::thread::spawn(move || {
        state.idle.turn_started();
        let cancel = Cancel::new();
        register_cancel(&session.0, cancel.clone());

        // The model spec must be registered with the store so `plan_request` can
        // compute cache breakpoints and the compaction threshold.
        state.store.register_model_spec(model.clone());

        let bus = state.buses.bus_for(&session.0);
        // R-STOR-116: salvage in-flight tool progress so a crash mid-batch still leaves
        // usable content for R-STOR-115's synthesized result.
        let sink: Arc<dyn EventSink> = Arc::new(SessionSink::with_store(
            bus.clone(),
            state.store.clone(),
            session.clone(),
        ));

        // Compose hooks from all plugins (R-PLUG-060).
        // Each plugin host gets a reference to the bus and session for emitting events.
        let hosts = state.hosts_snapshot();
        // ADR-0008: a test may install hooks in-process rather than spawning a policy plugin.
        let hooks: Arc<dyn HookHost> = if let Some(h) = state.hooks_override_snapshot() {
            h
        } else if hosts.is_empty() {
            Arc::new(kn9t_core::NoopHookHost)
        } else {
            // Set the bus and session on each plugin host
            for host in &hosts {
                host.set_bus(sink.clone());
                host.set_session(&session.0);
            }
            Arc::new(ComposedHookHost::new(hosts))
        };

        let loop_ = ReactLoop {
            provider: provider.clone(),
            store: state.store.clone(),
            approver: state.approver_snapshot(),
            tools: state.tools_snapshot(),  // R-PLUG2-110: tools from plugin subprocess
            hooks,
            bus: sink.clone(),
        };

        let params = RunParams {
            session: session.clone(),
            model: model.clone(),
            thinking: Thinking::Off,
            max_tokens: Some(model.max_out),
            cwd: state.cwd.clone(),
            config: ReactConfig::default(),
            read_map: Arc::new(Mutex::new(HashMap::new())),
            system: Some(system_prompt::default_system_prompt()),
            cancel: Some(cancel.clone()),
        };

        // Publish ApprovalRequest via policy check needs the session sink.
        // Install the sink + session id in TLS for InteractivePolicy (keeps Policy::check
        // signature unchanged — thread-local per turn, no global race as each
        // turn runs on its own thread).
        crate::policy::set_policy_sink(Some(sink.clone()));
        crate::policy::set_policy_session(Some(session.0.clone()));
        crate::log!("turn started: session={}", session.0);
        let run_result = loop_.run(params);
        crate::policy::set_policy_sink(None);
        crate::policy::set_policy_session(None);
        match run_result {
            Ok(_)  => crate::log!("turn finished: session={}", session.0),
            Err(e) => crate::log!("turn error: session={} error={e:?}", session.0),
        }

        clear_cancel(&session.0);
        state.idle.turn_ended();

        // R-SRV-100: after the first assistant turn of a nameless session, title it.
        maybe_autotitle(&state, &session);
    });
}

/// R-SRV-100 — best-effort auto-title. If the session has a name, or has no
/// assistant message yet, do nothing. Otherwise issue one cheap provider call, set
/// the name, and record a `UsageKind::Title` usage row. Any failure is swallowed.
pub fn maybe_autotitle(state: &Arc<ServerState>, session: &SessionId) {
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
        return;
    }

    let (Some(provider), Some(model)) = (state.provider.clone(), state.default_model.clone())
    else {
        return;
    };

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
        max_tokens: Some(16),
        cache: &no_cache,
    };

    let cancel = Cancel::new();
    let stream = match provider.stream(&req, &cancel) {
        Ok(s) => s,
        Err(_) => return, // best-effort: silently ignore
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
            Err(_) => return, // mid-stream failure: leave name null, no title
        }
    }
    let title = sanitize_title(&title);
    if title.is_empty() {
        return;
    }

    // Persist the name.
    let _ = state.store.execute_raw(
        "UPDATE sessions SET name=?1 WHERE id=?2 AND name IS NULL",
        &[&title.as_str(), &session.0.as_str()],
    );

    // Emit TitleChanged so TUI can update the sidebar.
    state.buses.publish(&session.0, Event::TitleChanged {
        title: title.clone(),
    });

    // Record UsageKind::Title (R-CORE-150 / R-SRV-100). The loop is normally the
    // only usage emitter, but titling is a server-owned side call, so the server
    // records it here directly.
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
