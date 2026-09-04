//! R-STOR-080, R-STOR-090 — reproject and reproject --check.

use kn9t_core::{Event, StoreErr};
use rusqlite::{params, Connection};

use crate::db::PROJECTION_VERSION;
use crate::project;

/// R-STOR-080 — full reproject in one transaction.
pub fn reproject(conn: &Connection) -> Result<(), StoreErr> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| StoreErr(format!("begin reproject: {e}")))?;

    conn.execute_batch(
        "DROP TABLE IF EXISTS messages;
         DROP TABLE IF EXISTS usage;
         CREATE TABLE messages (
           session_id TEXT NOT NULL REFERENCES sessions(id),
           seq INTEGER NOT NULL,
           role TEXT NOT NULL,
           content TEXT NOT NULL,
           est_tokens INTEGER NOT NULL,
           silent INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (session_id, seq)
         );
         CREATE TABLE usage (
           session_id TEXT NOT NULL REFERENCES sessions(id),
           seq INTEGER NOT NULL,
           ts INTEGER NOT NULL,
           provider TEXT NOT NULL,
           model TEXT NOT NULL,
           kind TEXT NOT NULL,
           tokens_in INTEGER NOT NULL,
           tokens_out INTEGER NOT NULL,
           cache_read INTEGER NOT NULL,
           cache_write INTEGER NOT NULL,
           reasoning INTEGER NOT NULL DEFAULT 0,
           price_in_snapshot REAL NOT NULL,
           price_out_snapshot REAL NOT NULL,
           price_cache_read_snapshot REAL NOT NULL,
           price_cache_write_snapshot REAL NOT NULL,
           price_in_micros INTEGER NOT NULL DEFAULT 0,
           price_out_micros INTEGER NOT NULL DEFAULT 0,
           price_cache_read_micros INTEGER NOT NULL DEFAULT 0,
           price_cache_write_micros INTEGER NOT NULL DEFAULT 0,
           cost_usd REAL NOT NULL,
           cost_micros INTEGER NOT NULL DEFAULT 0,
           estimated INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (session_id, seq)
         );
         CREATE INDEX IF NOT EXISTS usage_by_model ON usage(model, kind);",
    )
    .map_err(|e| StoreErr(format!("recreate tables: {e}")))?;

    replay_events(conn, |sid, ts, event, conn| {
        let rows = project::project(sid, ts, event);
        project::write_rows(conn, rows)
    })?;

    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('PROJECTION_VERSION',?1)",
        params![PROJECTION_VERSION],
    )
    .map_err(|e| StoreErr(format!("update proj ver: {e}")))?;

    conn.execute_batch("COMMIT")
        .map_err(|e| StoreErr(format!("commit reproject: {e}")))?;
    Ok(())
}

/// R-STOR-090 — check: project into temp tables and diff. Returns list of diff descriptions.
pub fn reproject_check(conn: &Connection) -> Result<Vec<String>, StoreErr> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS chk_messages (
           session_id TEXT, seq INTEGER, role TEXT, content TEXT, est_tokens INTEGER,
           silent INTEGER DEFAULT 0,
           PRIMARY KEY (session_id, seq)
         );
         CREATE TEMP TABLE IF NOT EXISTS chk_usage (
           session_id TEXT, seq INTEGER, ts INTEGER, provider TEXT, model TEXT, kind TEXT,
           tokens_in INTEGER, tokens_out INTEGER, cache_read INTEGER, cache_write INTEGER,
           reasoning INTEGER,
           price_in_snapshot REAL, price_out_snapshot REAL,
           price_cache_read_snapshot REAL, price_cache_write_snapshot REAL,
           price_in_micros INTEGER, price_out_micros INTEGER,
           price_cache_read_micros INTEGER, price_cache_write_micros INTEGER,
           cost_usd REAL, cost_micros INTEGER, estimated INTEGER,
           PRIMARY KEY (session_id, seq)
         );
         DELETE FROM temp.chk_messages;
         DELETE FROM temp.chk_usage;",
    )
    .map_err(|e| StoreErr(format!("create temp tables: {e}")))?;

    replay_events(conn, |sid, ts, event, conn| {
        let rows = project::project(sid, ts, event);
        write_rows_temp(conn, rows)
    })?;

    let mut diffs = Vec::new();

    let msg_diff_sql = "
        SELECT m.session_id, m.seq FROM messages m
        LEFT JOIN temp.chk_messages c ON m.session_id=c.session_id AND m.seq=c.seq
        WHERE c.session_id IS NULL
        UNION
        SELECT c.session_id, c.seq FROM temp.chk_messages c
        LEFT JOIN messages m ON m.session_id=c.session_id AND m.seq=c.seq
        WHERE m.session_id IS NULL";
    {
        let mut stmt = conn
            .prepare(msg_diff_sql)
            .map_err(|e| StoreErr(format!("diff msg prepare: {e}")))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| StoreErr(format!("diff msg query: {e}")))?;
        while let Some(r) = rows
            .next()
            .map_err(|e| StoreErr(format!("diff msg row: {e}")))?
        {
            let sid: String = r.get(0).unwrap_or_default();
            let seq: i64 = r.get(1).unwrap_or_default();
            diffs.push(format!("messages diff: session={sid} seq={seq}"));
        }
    }

    let usg_diff_sql = "
        SELECT u.session_id, u.seq FROM usage u
        LEFT JOIN temp.chk_usage c ON u.session_id=c.session_id AND u.seq=c.seq
        WHERE c.session_id IS NULL
        UNION
        SELECT c.session_id, c.seq FROM temp.chk_usage c
        LEFT JOIN usage u ON u.session_id=c.session_id AND u.seq=c.seq
        WHERE u.session_id IS NULL";
    {
        let mut stmt = conn
            .prepare(usg_diff_sql)
            .map_err(|e| StoreErr(format!("diff usage prepare: {e}")))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| StoreErr(format!("diff usage query: {e}")))?;
        while let Some(r) = rows
            .next()
            .map_err(|e| StoreErr(format!("diff usage row: {e}")))?
        {
            let sid: String = r.get(0).unwrap_or_default();
            let seq: i64 = r.get(1).unwrap_or_default();
            diffs.push(format!("usage diff: session={sid} seq={seq}"));
        }
    }

    Ok(diffs)
}

/// Walk all events in (session_id, seq) order, call `f` for each decoded event.
/// Unknown event kinds are skipped with a warning.
fn replay_events<F>(conn: &Connection, mut f: F) -> Result<(), StoreErr>
where
    F: FnMut(&str, i64, &Event, &Connection) -> Result<(), StoreErr>,
{
    // Collect first to avoid borrow conflict on conn inside the closure
    let events: Vec<(String, i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT session_id, ts, payload FROM events ORDER BY session_id, seq")
            .map_err(|e| StoreErr(format!("replay prepare: {e}")))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| StoreErr(format!("replay query: {e}")))?;
        let mut out = Vec::new();
        while let Some(r) = rows
            .next()
            .map_err(|e| StoreErr(format!("replay row: {e}")))?
        {
            out.push((
                r.get(0).unwrap_or_default(),
                r.get(1).unwrap_or_default(),
                r.get(2).unwrap_or_default(),
            ));
        }
        out
    };

    for (sid, ts, payload) in &events {
        let event: Event = match serde_json::from_str(payload) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("[reproject] skip unknown event kind: {err}");
                continue;
            }
        };
        f(sid, *ts, &event, conn)?;
    }
    Ok(())
}

fn write_rows_temp(conn: &Connection, rows: Vec<project::Row>) -> Result<(), StoreErr> {
    for row in rows {
        match row {
            project::Row::Message {
                session_id,
                seq,
                role,
                content_json,
                est_tokens,
                silent,
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO temp.chk_messages(session_id,seq,role,content,est_tokens,silent)\
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![session_id, seq as i64, role, content_json, est_tokens, silent as i64],
                ).map_err(|e| StoreErr(format!("insert temp msg: {e}")))?;
            }
            project::Row::Usage {
                session_id,
                seq,
                ts,
                provider,
                model,
                kind,
                tokens_in,
                tokens_out,
                cache_read,
                cache_write,
                reasoning,
                price_in,
                price_out,
                price_cache_read,
                price_cache_write,
                price_in_micros,
                price_out_micros,
                price_cache_read_micros,
                price_cache_write_micros,
                cost_usd,
                cost_micros,
                estimated,
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO temp.chk_usage(\
                       session_id,seq,ts,provider,model,kind,\
                       tokens_in,tokens_out,cache_read,cache_write,reasoning,\
                       price_in_snapshot,price_out_snapshot,\
                       price_cache_read_snapshot,price_cache_write_snapshot,\
                       price_in_micros,price_out_micros,price_cache_read_micros,price_cache_write_micros,\
                       cost_usd,cost_micros,estimated)\
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                    params![session_id, seq as i64, ts, provider, model, kind,
                        tokens_in, tokens_out, cache_read, cache_write, reasoning,
                        price_in, price_out, price_cache_read, price_cache_write,
                        price_in_micros, price_out_micros, price_cache_read_micros, price_cache_write_micros,
                        cost_usd, cost_micros, estimated],
                ).map_err(|e| StoreErr(format!("insert temp usage: {e}")))?;
            }
            project::Row::Compacted {
                session_id,
                seq,
                replaced_start,
                replaced_end,
                role,
                content_json,
                est_tokens,
            } => {
                conn.execute(
                    "DELETE FROM temp.chk_messages WHERE session_id=?1 AND seq>=?2 AND seq<=?3",
                    params![session_id, replaced_start as i64, replaced_end as i64],
                )
                .map_err(|e| StoreErr(format!("delete temp compacted: {e}")))?;
                // Compacted messages are never silent (they're assistant summaries)
                conn.execute(
                    "INSERT OR REPLACE INTO temp.chk_messages(session_id,seq,role,content,est_tokens,silent)\
                     VALUES(?1,?2,?3,?4,?5,0)",
                    params![session_id, seq as i64, role, content_json, est_tokens],
                ).map_err(|e| StoreErr(format!("insert temp compact: {e}")))?;
            }
        }
    }
    Ok(())
}
