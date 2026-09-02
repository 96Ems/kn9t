//! R-STOR-040, R-STOR-050 — append + snapshot.

use kn9t_core::{Event, ModelRef, SessionId, SessionSnapshot, StoreErr};
use rusqlite::{OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::SqliteStore;
use crate::project;

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// R-STOR-040 — single-transaction append.
pub fn append(store: &SqliteStore, session: &SessionId, event: Event) -> Result<u64, StoreErr> {
    // R-STOR-050 — reject transient events
    if event.seq().is_none() {
        return Err(StoreErr(format!("append: transient event rejected")));
    }

    let sid = session.0.clone();
    let ts = now_ts();
    let kind = event_kind_name(&event);

    let conn = store.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;

    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| StoreErr(format!("begin: {e}")))?;

    let head_seq: i64 = conn
        .query_row(
            "SELECT head_seq FROM sessions WHERE id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("read head_seq (session '{}' not found?): {e}", sid)))?;
    let seq = (head_seq + 1) as u64;

    // R-STOR-040/060 — the store owns seq assignment: callers construct durable
    // events with a placeholder `seq` and rely on append to stamp the true value.
    // Stamp it *before* serializing the payload and projecting, so `events.payload`
    // (the reproject source of truth, G2) and every projection row carry the
    // authoritative, gapless seq rather than the caller's placeholder.
    let event = event.with_seq(seq);
    let payload = serde_json::to_string(&event)
        .map_err(|e| StoreErr(format!("serialize event: {e}")))?;

    conn.execute(
        "INSERT INTO events(session_id,seq,ts,kind,payload) VALUES(?1,?2,?3,?4,?5)",
        params![sid, seq as i64, ts, kind, payload],
    ).map_err(|e| { let _ = conn.execute_batch("ROLLBACK"); StoreErr(format!("insert event: {e}")) })?;

    let rows = project::project(&sid, ts, &event);
    if let Err(e) = project::write_rows(&conn, rows) {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }

    // Blob refcount for MessageAppended
    if let Event::MessageAppended { msg, .. } = &event {
        let cj = serde_json::to_string(&msg.content).unwrap_or_default();
        if let Err(e) = project::incr_blob_refs(&conn, &cj) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    conn.execute(
        "UPDATE sessions SET head_seq = ?1 WHERE id = ?2",
        params![seq as i64, sid],
    ).map_err(|e| { let _ = conn.execute_batch("ROLLBACK"); StoreErr(format!("update head_seq: {e}")) })?;

    conn.execute_batch("COMMIT").map_err(|e| StoreErr(format!("commit: {e}")))?;
    Ok(seq)
}

/// R-CORE-250 snapshot.
pub fn snapshot(store: &SqliteStore, session: &SessionId) -> Result<SessionSnapshot, StoreErr> {
    let sid = session.0.clone();
    let conn = store.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;

    let (head_seq, model_at_fork): (i64, Option<String>) = conn
        .query_row(
            "SELECT head_seq, model_at_fork FROM sessions WHERE id = ?1",
            params![sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| StoreErr(format!("snapshot query: {e}")))?;

    let ctx_tokens: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(est_tokens),0) FROM messages WHERE session_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("ctx_tokens: {e}")))?;

    let cost_micros: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(cost_micros),0) FROM usage WHERE session_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("cost query: {e}")))?;
    let cost_usd = cost_micros as f64 / 1_000_000.0;

    let model = reconstruct_model(&conn, &sid, &model_at_fork)?;

    Ok(SessionSnapshot {
        head_seq: head_seq as u64,
        ctx_tokens: ctx_tokens as u32,
        cost_usd,
        cost_micros,
        model,
    })
}

fn reconstruct_model(
    conn: &rusqlite::Connection,
    session_id: &str,
    model_at_fork: &Option<String>,
) -> Result<ModelRef, StoreErr> {
    // Last ModelChanged wins
    let last: Option<String> = conn
        .query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='ModelChanged' ORDER BY seq DESC LIMIT 1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StoreErr(format!("model query: {e}")))?;

    if let Some(p) = last {
        if let Ok(Event::ModelChanged { model, .. }) = serde_json::from_str(&p) {
            return Ok(model);
        }
    }
    if let Some(j) = model_at_fork {
        if let Ok(m) = serde_json::from_str::<ModelRef>(j) {
            return Ok(m);
        }
    }
    // SessionForked fallback
    let fork_p: Option<String> = conn
        .query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='SessionForked' LIMIT 1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StoreErr(format!("fork model query: {e}")))?;

    if let Some(p) = fork_p {
        if let Ok(Event::SessionForked { fork, .. }) = serde_json::from_str(&p) {
            return Ok(fork.model_at_fork);
        }
    }
    Err(StoreErr("no model found for session".into()))
}

/// Create a new root session row. Call before first `append`.
pub fn create_session(
    store: &SqliteStore,
    id: &SessionId,
    cwd: &str,
    model: &ModelRef,
) -> Result<(), StoreErr> {
    let ts = now_ts();
    let model_json = serde_json::to_string(model)
        .map_err(|e| StoreErr(format!("serialize model: {e}")))?;
    let conn = store.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
    conn.execute(
        "INSERT INTO sessions(id,created_at,cwd,model_at_fork,head_seq) VALUES(?1,?2,?3,?4,0)",
        params![id.0.clone(), ts, cwd, model_json],
    ).map_err(|e| StoreErr(format!("create session: {e}")))?;
    Ok(())
}

fn event_kind_name(event: &Event) -> &'static str {
    match event {
        Event::SessionForked { .. }   => "SessionForked",
        Event::MessageAppended { .. } => "MessageAppended",
        Event::ModelChanged { .. }    => "ModelChanged",
        Event::Compacted { .. }       => "Compacted",
        Event::Handoff { .. }         => "Handoff",
        Event::UsageRecorded { .. }   => "UsageRecorded",
        Event::TurnStarted { .. }     => "TurnStarted",
        Event::TextDelta { .. }       => "TextDelta",
        Event::ThinkingDelta { .. }   => "ThinkingDelta",
        Event::ToolArgsDelta { .. }   => "ToolArgsDelta",
        Event::ToolStarted { .. }     => "ToolStarted",
        Event::ToolProgress { .. }    => "ToolProgress",
        Event::ToolFinished { .. }    => "ToolFinished",
        Event::ApprovalRequest { .. } => "ApprovalRequest",
        Event::TurnEnded { .. }       => "TurnEnded",
        Event::HookFailed { .. }      => "HookFailed",
        Event::TitleChanged { .. }        => "TitleChanged",
        Event::Error { .. }               => "Error",
        Event::RetryAttempt { .. }        => "RetryAttempt",
        Event::TurnStatus { .. }          => "TurnStatus",
        Event::PluginNotification { .. }  => "PluginNotification",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kn9t_core::{CallId, Content, Message, MsgId, Role, StopReason};

    #[test]
    fn test_now_ts_returns_positive() {
        let ts = now_ts();
        assert!(ts > 0, "timestamp should be positive");
    }

    #[test]
    fn test_now_ts_returns_reasonable_value() {
        let ts = now_ts();
        // Should be after 2020 (1577836800000 ms) and before 2100
        assert!(ts > 1577836800000, "timestamp should be after 2020");
        assert!(ts < 4102444800000, "timestamp should be before 2100");
    }

    #[test]
    fn test_event_kind_name_message_appended() {
        let event = Event::MessageAppended {
            seq: 1,
            msg: Message {
                id: MsgId::new(),
                role: Role::User,
                content: vec![Content::Text { text: "hello".into() }],
                silent: false,
            },
        };
        assert_eq!(event_kind_name(&event), "MessageAppended");
    }

    #[test]
    fn test_event_kind_name_model_changed() {
        let event = Event::ModelChanged {
            seq: 1,
            model: ModelRef {
                provider: "openai".into(),
                id: "gpt-4".into(),
            },
        };
        assert_eq!(event_kind_name(&event), "ModelChanged");
    }

    #[test]
    fn test_event_kind_name_turn_started() {
        let event = Event::TurnStarted { turn: 1 };
        assert_eq!(event_kind_name(&event), "TurnStarted");
    }

    #[test]
    fn test_event_kind_name_turn_ended() {
        let event = Event::TurnEnded { turn: 1, stop: StopReason::Stop };
        assert_eq!(event_kind_name(&event), "TurnEnded");
    }

    #[test]
    fn test_event_kind_name_text_delta() {
        let event = Event::TextDelta { 
            msg_id: MsgId::new(),
            idx: 0,
            delta: "hello".into(),
        };
        assert_eq!(event_kind_name(&event), "TextDelta");
    }

    #[test]
    fn test_event_kind_name_tool_started() {
        let event = Event::ToolStarted {
            call_id: CallId("call-1".into()),
            name: "bash".into(),
        };
        assert_eq!(event_kind_name(&event), "ToolStarted");
    }

    #[test]
    fn test_event_kind_name_tool_finished() {
        let event = Event::ToolFinished {
            call_id: CallId("call-1".into()),
            is_error: false,
        };
        assert_eq!(event_kind_name(&event), "ToolFinished");
    }

    #[test]
    fn test_event_kind_name_error() {
        let event = Event::Error { message: "something went wrong".into() };
        assert_eq!(event_kind_name(&event), "Error");
    }
}
