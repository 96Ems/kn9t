//! R-STOR-120, R-STOR-130 — session forking.

use kn9t_core::{
    Event, ForkReason, ForkSnapshot, ModelRef, SessionId, StoreErr, Thinking,
};
use rusqlite::params;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::SqliteStore;
use crate::project;

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// Fork `origin` at `origin_seq` into `new_id`.
/// Copies MessageAppended, ModelChanged, Compacted (NOT UsageRecorded).
/// seq 0 = SessionForked; copied events renumbered from 1.
pub fn fork_session(
    store: &SqliteStore,
    origin: &SessionId,
    new_id: &SessionId,
    origin_seq: u64,
    reason: ForkReason,
    budget_remaining_usd: Option<f64>,
    cwd: &str,
) -> Result<(), StoreErr> {
    let origin_sid = origin.0.clone();
    let new_sid    = new_id.0.clone();
    let ts = now_ts();

    let conn = store.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| StoreErr(format!("begin fork: {e}")))?;

    // Inherited totals up to origin_seq
    let (inh_cost, inh_tok_in, inh_tok_out, inh_cache_read): (f64, i64, i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(cost_usd),0), COALESCE(SUM(tokens_in),0),
                    COALESCE(SUM(tokens_out),0), COALESCE(SUM(cache_read),0)
             FROM usage WHERE session_id=?1 AND seq<=?2",
            params![origin_sid, origin_seq as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| StoreErr(format!("fork usage query: {e}")))?;

    let inh_ctx: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(est_tokens),0) FROM messages WHERE session_id=?1 AND seq<=?2",
            params![origin_sid, origin_seq as i64],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("fork ctx: {e}")))?;

    let inh_messages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND seq<=?2",
            params![origin_sid, origin_seq as i64],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("fork msg count: {e}")))?;

    let model_at_fork_json: Option<String> = conn
        .query_row(
            "SELECT model_at_fork FROM sessions WHERE id=?1",
            params![origin_sid],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("fork model: {e}")))?;

    let model_at_fork: ModelRef = model_at_fork_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .ok_or_else(|| StoreErr("no model_at_fork on origin session".into()))?;

    let fork_reason_str = match reason {
        ForkReason::Fork     => "fork",
        ForkReason::Rewind   => "rewind",
        ForkReason::Subagent => "subagent",
        ForkReason::Tree     => "tree",
    };

    let fork_snap = ForkSnapshot {
        origin_session: origin.clone(),
        origin_seq,
        reason,
        inherited_cost_usd:   inh_cost,
        inherited_tokens_in:  inh_tok_in  as u64,
        inherited_tokens_out: inh_tok_out as u64,
        inherited_cache_read: inh_cache_read as u64,
        inherited_messages:   inh_messages as u32,
        inherited_ctx_tokens: inh_ctx      as u32,
        budget_remaining_usd,
        model_at_fork: model_at_fork.clone(),
        thinking_at_fork: Thinking::Off,
        cwd_at_fork: cwd.into(),
    };

    let model_json = serde_json::to_string(&model_at_fork).unwrap_or_default();

    conn.execute(
        "INSERT INTO sessions(id,created_at,cwd,origin_session,origin_seq,fork_reason,
          inherited_cost_usd,inherited_tokens_in,inherited_tokens_out,inherited_ctx_tokens,
          budget_remaining_usd,model_at_fork,head_seq)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0)",
        params![new_sid, ts, cwd, origin_sid, origin_seq as i64, fork_reason_str,
                inh_cost, inh_tok_in, inh_tok_out, inh_ctx,
                budget_remaining_usd, model_json],
    ).map_err(|e| StoreErr(format!("create fork session: {e}")))?;

    // seq=0 SessionForked event
    let fork_event   = Event::SessionForked { seq: 0, fork: fork_snap };
    let fork_payload = serde_json::to_string(&fork_event)
        .map_err(|e| StoreErr(format!("serialize SessionForked: {e}")))?;
    conn.execute(
        "INSERT INTO events(session_id,seq,ts,kind,payload) VALUES(?1,0,?2,'SessionForked',?3)",
        params![new_sid, ts, fork_payload],
    ).map_err(|e| StoreErr(format!("insert SessionForked: {e}")))?;

    // Collect copyable events from origin
    struct CopyRow { old_seq: u64, kind: String, payload: String, ts: i64 }
    let copy_rows: Vec<CopyRow> = {
        let mut stmt = conn.prepare(
            "SELECT seq, kind, payload, ts FROM events WHERE session_id=?1 AND seq<=?2
             AND kind IN ('MessageAppended','ModelChanged','Compacted') ORDER BY seq",
        ).map_err(|e| StoreErr(format!("fork copy prepare: {e}")))?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![origin_sid, origin_seq as i64])
            .map_err(|e| StoreErr(format!("fork copy query: {e}")))?;
        while let Some(r) = rows.next().map_err(|e| StoreErr(format!("fork copy row: {e}")))? {
            out.push(CopyRow {
                old_seq: r.get::<_, i64>(0).unwrap_or(0) as u64,
                kind:    r.get(1).unwrap_or_default(),
                payload: r.get(2).unwrap_or_default(),
                ts:      r.get(3).unwrap_or(ts),
            });
        }
        out
    };

    // Build old_seq → new_seq map for Compacted.replaced remapping
    let mut seq_map = std::collections::HashMap::new();
    for (i, row) in copy_rows.iter().enumerate() {
        seq_map.insert(row.old_seq, (i + 1) as u64);
    }

    for (i, row) in copy_rows.iter().enumerate() {
        let new_seq = (i + 1) as u64;
        let new_payload = remap_event_seq(&row.payload, new_seq, &seq_map)?;

        conn.execute(
            "INSERT INTO events(session_id,seq,ts,kind,payload) VALUES(?1,?2,?3,?4,?5)",
            params![new_sid, new_seq as i64, row.ts, row.kind, new_payload],
        ).map_err(|e| StoreErr(format!("insert copied event: {e}")))?;

        let event: Event = serde_json::from_str(&new_payload)
            .map_err(|e| StoreErr(format!("decode copied event: {e}")))?;
        let proj_rows = project::project(&new_sid, row.ts, &event);
        project::write_rows(&conn, proj_rows)?;
    }

    let head = copy_rows.len() as i64;
    conn.execute(
        "UPDATE sessions SET head_seq=?1 WHERE id=?2",
        params![head, new_sid],
    ).map_err(|e| StoreErr(format!("update fork head_seq: {e}")))?;

    conn.execute_batch("COMMIT")
        .map_err(|e| StoreErr(format!("commit fork: {e}")))?;
    Ok(())
}

fn remap_event_seq(
    payload: &str,
    new_seq: u64,
    seq_map: &std::collections::HashMap<u64, u64>,
) -> Result<String, StoreErr> {
    let mut v: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| StoreErr(format!("remap parse: {e}")))?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("seq".to_owned(), serde_json::json!(new_seq));
        // Remap Compacted.replaced ranges
        if let Some(replaced) = obj.get("replaced").cloned() {
            let start = replaced.get("start").and_then(|s| s.as_u64());
            let end   = replaced.get("end").and_then(|s| s.as_u64());
            if let (Some(s), Some(e)) = (start, end) {
                let ns = seq_map.get(&s).copied().unwrap_or(s);
                let ne = seq_map.get(&e).copied().unwrap_or(e);
                obj.insert("replaced".to_owned(), serde_json::json!({"start": ns, "end": ne}));
            }
        }
    }
    serde_json::to_string(&v).map_err(|e| StoreErr(format!("remap serialize: {e}")))
}
