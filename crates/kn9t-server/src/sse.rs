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

/// The wire kind string for an event, as SSE clients and plugin `events` filters
/// both name it. One table, so a plugin subscribing to `"plugin_declared"` and an
/// SSE client listening for `plugin_declared` can never mean different things.
pub fn event_kind(e: &Event) -> &'static str {
    event_name(e)
}

fn event_name(e: &Event) -> &'static str {
    match e {
        Event::SessionForked { .. } => "session_forked",
        Event::MessageAppended { .. } => "message_appended",
        Event::ModelChanged { .. } => "model_changed",
        Event::Compacted { .. } => "compacted",
        Event::Handoff { .. } => "handoff",
        Event::ToolsToggled { .. } => "tools_toggled",
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
        Event::InteractionRequest { .. } => "interaction_request",
        Event::UiDirective { .. } => "ui_directive",
        Event::PluginDeclared { .. } => "plugin_declared",
        Event::PluginState { .. } => "plugin_state",
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
    /// B9 — the highest seq this prelude delivered with **no hole below it**.
    ///
    /// A client must advance its `from` cursor to this, not to the newest seq it saw: the two
    /// differ exactly when a durable echo was evicted from the subscriber ring, and resuming
    /// past the hole would skip that event permanently.
    pub contiguous_through: u64,
    /// B9 — true when a durable event was delivered above a missing one.
    ///
    /// The dedup rule ("`seq <= head_seq` was already emitted in step 2") is sound only if
    /// the ring delivered everything above the watermark. The ring is bounded and drops the
    /// *oldest* on overflow, and since 96E-18 durable echoes ride it too, so a slow attach
    /// can lose one. §5.1 self-healing covers transient loss only — a lost durable event is a
    /// permanent hole in the client's transcript unless it refetches. This flag is how the
    /// client learns it must.
    pub gap_detected: bool,
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
    //
    // B9: track continuity while flushing. Durable seqs are gapless *in the store*, so a
    // jump in what the ring hands us means the ring evicted something — the one case the
    // dedup rule cannot absorb, because the skipped event is neither replayed (the replay
    // already ran) nor re-delivered (it is gone from the ring).
    let mut contiguous_through = head_seq;
    let mut gap_detected = false;
    while let Some(ev) = sub.try_recv() {
        match ev.seq() {
            Some(seq) if seq <= head_seq => continue, // already emitted in step 2
            Some(seq) => {
                if seq == contiguous_through + 1 {
                    contiguous_through = seq;
                } else if seq > contiguous_through + 1 {
                    // Evicted echo(es) between contiguous_through and seq.
                    gap_detected = true;
                }
                frames.push(sse_frame(&ev));
            }
            // Transient: droppable by design (§5.1), never a durable gap.
            None => frames.push(sse_frame(&ev)),
        }
    }

    AttachPrelude {
        frames,
        head_seq,
        contiguous_through,
        gap_detected,
    }
}

/// Read durable events with `seq > from`, ordered, and the session's current
/// `head_seq`. Uses an atomic store snapshot so a concurrent append cannot
/// commit between the two reads (96E-7 fix).
fn read_durable_since(store: &SqliteStore, session: &str, from: u64) -> (Vec<Event>, u64) {
    let (payloads, head_seq) = store
        .read_attach_snapshot(session, from)
        .unwrap_or((Vec::new(), 0));
    let events: Vec<Event> = payloads
        .iter()
        .filter_map(|p| serde_json::from_str::<Event>(p).ok())
        .collect();
    (events, head_seq)
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
///
/// `on_alive` is invoked once per loop iteration (on every forwarded event AND
/// every heartbeat) **after** a successful write, i.e. only while the socket is
/// demonstrably still connected. The SSE handler uses it to keep the owning write
/// lease warm so an attached-but-reading client never idle-loses its lease
/// (DESIGN §12.6 — the lease lives as long as its SSE stream).
pub fn run_live_loop<W: Write>(
    w: &mut W,
    sub: &Subscription,
    on_alive: &mut dyn FnMut(),
) -> std::io::Result<()> {
    loop {
        match sub.recv_timeout(heartbeat_interval()) {
            Some(ev) => {
                let frame = sse_frame(&ev);
                if w.write_all(frame.as_bytes()).is_err() || w.flush().is_err() {
                    return Ok(()); // client disconnected
                }
                on_alive();
            }
            None => {
                // Timeout — send keepalive ping to detect dead clients.
                if w.write_all(heartbeat().as_bytes()).is_err() || w.flush().is_err() {
                    return Ok(()); // client disconnected
                }
                on_alive();
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

