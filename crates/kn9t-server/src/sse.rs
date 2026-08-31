//! R-SRV-040, R-SRV-050 — SSE stream + the attach-race close (DESIGN §12.4, §5.1).
//!
//! `GET /session/{id}/events?from={seq}` must replay durable events past `from` and
//! then go live with **no gap and no duplicate**. The order is exact and is the
//! whole point of this module:
//!
//! 1. **subscribe first** (buffer everything the bus delivers from now on);
//! 2. read durable rows `> from` up to the current `head_seq`; emit them;
//! 3. read `live_messages` for the in-flight partial (R-SRV-050);
//! 4. flush the buffer, **discarding any event with `seq <= head_seq`** (exact
//!    dedup — durable seqs are gapless, §3.1).
//!
//! Read-then-subscribe is forbidden: a durable event committed in the gap between
//! the read and the subscribe would be lost, and §5.1 self-healing does not cover
//! durable events. The backlog read holds no write lock (a 400-event attach must
//! not stall the agent, §12.4) — it reads through the store's own reader path.

use std::io::Write;
use std::time::Duration;

use kn9t_core::{Event, Subscription};
use kn9t_store::SqliteStore;

/// Serialize one event as an SSE frame. The `event:` field carries the durable
/// `kind` (or `transient`), `data:` the JSON payload. Frames are newline-delimited
/// per the SSE spec (`\n\n` terminates an event).
pub fn sse_frame(event: &Event) -> String {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
    let name = event_name(event);
    format!("event: {name}\ndata: {data}\n\n")
}

fn event_name(e: &Event) -> &'static str {
    match e {
        Event::SessionForked { .. } => "session_forked",
        Event::MessageAppended { .. } => "message_appended",
        Event::ModelChanged { .. } => "model_changed",
        Event::Compacted { .. } => "compacted",
        Event::UsageRecorded { .. } => "usage_recorded",
        Event::TurnStarted { .. } => "turn_started",
        Event::TextDelta { .. } => "text_delta",
        Event::ThinkingDelta { .. } => "thinking_delta",
        Event::ToolArgsDelta { .. } => "tool_args_delta",
        Event::ToolStarted { .. } => "tool_started",
        Event::ToolProgress { .. } => "tool_progress",
        Event::ToolFinished { .. } => "tool_finished",
        Event::ApprovalRequest { .. } => "approval_request",
        Event::TurnEnded { .. } => "turn_ended",
        Event::HookFailed { .. } => "hook_failed",
        Event::TitleChanged { .. } => "title_changed",
        Event::Error { .. } => "error",
        Event::RetryAttempt { .. } => "retry_attempt",
        Event::TurnStatus { .. } => "turn_status",
        Event::PluginNotification { .. } => "plugin_notification",
    }
}

/// The frame surfacing the in-flight partial assistant text from `live_messages`
/// (R-SRV-050). Best-effort display convenience; the authoritative
/// `MessageAppended` still follows and supersedes it.
pub fn live_partial_frame(partial_content_json: &str) -> String {
    format!("event: live_partial\ndata: {partial_content_json}\n\n")
}

/// Compute the exact attach-race prelude for a subscriber that has already
/// subscribed. Returns the ordered frames to write **before** switching to the
/// live loop, plus the dedup watermark (`head_seq`).
///
/// This is factored out (pure over store reads + the already-taken subscription's
/// buffer) so the ordering is unit-testable without a socket.
///
/// `sub` is the subscription taken in step 1. `from` is the client's cursor.
pub struct AttachPrelude {
    /// Ordered frames: durable replay, then live_partial, then de-duplicated
    /// buffered events that arrived during the window.
    pub frames: Vec<String>,
    /// Watermark used for dedup; also the point the live loop continues from.
    pub head_seq: u64,
}

/// Step 2–4 of the attach race. The caller has already performed step 1
/// (subscribed). This drains the subscription's buffered events non-blockingly and
/// applies the exact dedup rule.
pub fn build_attach_prelude(
    store: &SqliteStore,
    session: &str,
    from: u64,
    sub: &Subscription,
) -> AttachPrelude {
    // Step 2: durable rows > from up to current head_seq.
    let (durable, head_seq) = read_durable_since(store, session, from);
    let mut frames: Vec<String> = durable.iter().map(sse_frame).collect();

    // Step 3: in-flight partial from live_messages (best-effort).
    if let Ok(Some(partial)) = store.get_live_message(&kn9t_core::SessionId(session.to_owned())) {
        frames.push(live_partial_frame(&partial));
    }

    // Step 4: flush the buffer accumulated since step 1, discarding anything with
    // seq <= head_seq (exact dedup; durable seqs are gapless). Transient events
    // (no seq) are always forwarded.
    while let Some(ev) = sub.try_recv() {
        match ev.seq() {
            Some(seq) if seq <= head_seq => continue, // already emitted in step 2
            _ => frames.push(sse_frame(&ev)),
        }
    }

    AttachPrelude { frames, head_seq }
}

/// Read durable events with `seq > from`, ordered, and the session's current
/// `head_seq`. Uses the store reader path; holds no write lock across the read.
fn read_durable_since(store: &SqliteStore, session: &str, from: u64) -> (Vec<Event>, u64) {
    let payloads = store
        .query_strings(
            "SELECT payload FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq",
            &[&session, &(from as i64)],
        )
        .unwrap_or_default();
    let events: Vec<Event> = payloads
        .iter()
        .filter_map(|p| serde_json::from_str::<Event>(p).ok())
        .collect();
    // head_seq = max durable seq we hold (from the sessions row).
    let head_seq: i64 = store
        .query_one(
            "SELECT head_seq FROM sessions WHERE id=?1",
            &[&session],
            |r| r.get(0),
        )
        .unwrap_or(0);
    (events, head_seq as u64)
}

/// Write the SSE HTTP response head to a raw writer (used with
/// `Request::into_writer`). No `Access-Control-Allow-Origin` — cross-origin is
/// rejected upstream (R-SRV-030).
pub fn write_sse_head<W: Write>(w: &mut W) -> std::io::Result<()> {
    write!(
        w,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         \r\n"
    )?;
    w.flush()
}

/// The live loop: after the prelude, block on the subscription and forward each
/// event, until the client disconnects (a write error) or the bus closes.
/// Sends a keepalive ping every `HEARTBEAT_INTERVAL` so disconnected clients
/// are detected promptly (write failure → `client_detached` → idle-exit).
pub fn run_live_loop<W: Write>(w: &mut W, sub: &Subscription) -> std::io::Result<()> {
    loop {
        match sub.recv_timeout(heartbeat_interval()) {
            Some(ev) => {
                let frame = sse_frame(&ev);
                if w.write_all(frame.as_bytes()).is_err() || w.flush().is_err() {
                    return Ok(()); // client disconnected
                }
            }
            None => {
                // Timeout — send keepalive ping to detect dead clients.
                if w.write_all(heartbeat().as_bytes()).is_err() || w.flush().is_err() {
                    return Ok(()); // client disconnected
                }
            }
        }
    }
}

/// Heartbeat comment frame (keeps intermediaries from closing an idle stream).
pub fn heartbeat() -> &'static str {
    ": keepalive\n\n"
}

/// Convenience: default SSE subscriber ring capacity (bounded; §5.1 permits drops).
pub const SSE_RING_CAPACITY: usize = 1024;

/// How often the live loop sends a keepalive ping.
/// Short enough to detect dead clients promptly; long enough to not spam.
/// Overridable via `KN9T_SSE_HEARTBEAT_MS` env var (for tests).
pub fn heartbeat_interval() -> Duration {
    std::env::var("KN9T_SSE_HEARTBEAT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(15))
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
