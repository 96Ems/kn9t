//! 96E-39: Tests verifying Cancel propagation into blocking waits.
//!
//! These tests reproduce the three corner cases where Cancel does NOT propagate:
//! 0. ApprovalRegistry::wait — standard tool approval blocks forever on ESC
//! 1. run_session_turn — subagent gets a fresh Cancel, ignores parent's
//! 2. InteractionRegistry::wait — plugin interaction blocks forever on ESC
//!
//! Each test starts a blocking wait, fires cancel from another thread, and
//! asserts the wait returns within a bounded time (currently fails: proves the bug).

use kn9t_core::Cancel;
use kn9t_server::interaction::InteractionRegistry;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 96E-39 FIX VERIFIED: InteractionRegistry::wait now respects Cancel.
///
/// This test verifies the fix is working: wait() returns None quickly
/// when cancel fires, instead of blocking forever.
#[test]
fn interaction_wait_respects_cancel() {
    let reg = Arc::new(InteractionRegistry::new());
    let (_id, handle) = reg.create("sess-1", "test-plugin", &json!({"q": "test"}));

    let cancel = Cancel::new();
    let cancel_c = cancel.clone();

    let reg_c = reg.clone();
    let waiter = std::thread::spawn(move || {
        let start = Instant::now();
        // FIX: wait() now takes Cancel and returns Option<Value>
        let result = reg_c.wait(&handle, &cancel_c);
        (start.elapsed(), result)
    });

    // Give the waiter time to enter the wait
    std::thread::sleep(Duration::from_millis(50));

    // Fire cancel (simulates user pressing ESC)
    cancel.cancel();

    // The waiter should return quickly with None
    let (elapsed, result) = waiter.join().unwrap();

    assert!(
        result.is_none(),
        "Cancelled wait should return None"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "Cancel should unblock quickly, took {:?}",
        elapsed
    );
    eprintln!("FIX VERIFIED: interaction_wait returned in {:?}", elapsed);
}

/// Bug reproduction: ApprovalRegistry::wait ignores Cancel.
///
/// Similar to interaction_wait_ignores_cancel but for tool approvals.
/// The actual ApprovalRegistry is more complex (policy.rs), but the wait
/// pattern is identical: bare cvar.wait() with no cancel check.
#[test]
fn approval_wait_ignores_cancel_documented() {
    // ApprovalRegistry is not public, so we can't test it directly here.
    // See crates/kn9t-server/src/policy.rs lines 273-287.
    //
    // The bug: wait() does:
    //   while guard.is_none() {
    //       guard = slot.cvar.wait(guard).expect(...);
    //   }
    //
    // There's no timeout and no cancel check. The fix is:
    //   while guard.is_none() {
    //       if cancel.cancelled() {
    //           return Decision::Denied; // or Aborted variant
    //       }
    //       let (g, timeout_result) = slot.cvar.wait_timeout(guard, Duration::from_millis(100)).expect(...);
    //       guard = g;
    //   }
    //
    // This test just documents the issue since we can't access the type.
}

/// Bug reproduction: run_session_turn creates a fresh Cancel, ignoring parent's.
///
/// When a plugin calls session_prompt (spawning a subagent), the subagent gets
/// its own Cancel with a 10-minute watchdog. The parent's Cancel is never
/// passed in, so ESC on the parent has no effect until the subagent finishes
/// or times out.
#[test]
fn session_turn_creates_fresh_cancel_documented() {
    // See crates/kn9t-server/src/turn.rs lines 451-456:
    //
    //   let cancel = Cancel::new();  // <-- fresh, disconnected from parent
    //   let cancel_watch = cancel.clone();
    //   std::thread::spawn(move || {
    //       std::thread::sleep(Duration::from_secs(timeout_s)); // default 600s!
    //       cancel_watch.cancel();
    //   });
    //
    // The fix: run_session_turn should accept an optional parent_cancel: Option<Cancel>
    // and either:
    // a) Use it directly (child respects parent's cancel), or
    // b) Monitor it in the watchdog thread (cancel child when parent cancels)
    //
    // Option (a) is simpler. The signature becomes:
    //   pub fn run_session_turn(state, session, text, tools, timeout_s, parent_cancel: Option<Cancel>)
    //
    // And the loop uses parent_cancel.unwrap_or_else(|| Cancel::new()).
}

/// Bug reproduction: tool_execute creates a fresh Cancel, ignoring parent's.
///
/// When a plugin calls tool_execute (running a tool on behalf of the agent),
/// a fresh Cancel is created. If the parent turn is cancelled, the tool
/// execution continues until it finishes on its own.
#[test]
fn tool_execute_creates_fresh_cancel_documented() {
    // See crates/kn9t-server/src/host_api.rs lines 558-566:
    //
    //   let cancel = Cancel::new();  // <-- fresh, disconnected from parent
    //   let ctx = ToolCtx { ... };
    //   let out = tool.execute(&args, &ctx, &cancel)...
    //
    // Same pattern as session_prompt. The tool call runs to completion even
    // if the parent turn is cancelled.
    //
    // The fix: tool_execute should accept the parent's Cancel through HostApi.
    // This requires threading Cancel through the plugin protocol, which is a
    // larger change (the parent turn's Cancel would need to be accessible
    // from the HostApi context).
}

/// Bug reproduction: provider_complete creates a fresh Cancel.
///
/// When a plugin calls provider_complete (direct LLM call), a fresh Cancel
/// is created. Less severe than tool_execute because provider calls are
/// typically shorter, but still incorrect.
#[test]
fn provider_complete_creates_fresh_cancel_documented() {
    // See crates/kn9t-server/src/host_api.rs lines 303-306:
    //
    //   let cancel = Cancel::new();
    //   let chunks = provider.stream_with_sink(&req, &cancel, Some(sink.as_ref()))...
    //
    // Same pattern. The provider call runs to completion even if the parent
    // turn is cancelled.
}
