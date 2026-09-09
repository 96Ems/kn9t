//! Unit tests extracted from src/policy.rs
//!
//! These tests were originally inline in the source file and have been
//! extracted to keep production code free of test code.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use kn9t_core::{Approver, ApprovalCtx, CallId, Decision, EventSink, LiveEvent, ToolCall};
use kn9t_server::policy::{
    fingerprint, ApprovalCache, ApprovalRegistry, InteractiveApprover, NonInteractiveApprover,
};

// ── Test helpers ─────────────────────────────────────────────────────────────

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
        id: CallId("c1".into()),
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

// ── The mechanism: emit, block, resolve ──────────────────────────────────────

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

/// 96E-33 — an approval driven from a thread that never started a turn still emits
/// to the session it was told about.
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

/// ADR-0008 — `-p`/CI cannot prompt, so an ask is denied outright.
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

// ── Scope caching ────────────────────────────────────────────────────────────

/// `scope=session`: the second identical call in the same session is not re-prompted.
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
