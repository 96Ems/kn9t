//! R-STOR-160 — session delete with blob refcount GC.

use kn9t_core::{SessionId, StoreErr};
use rusqlite::params;

use crate::db::SqliteStore;
use crate::project;

impl SqliteStore {
    /// R-STOR-160 — delete session in one transaction; decrement blob refcounts.
    /// Rejects if this session is an `origin_session` of any live fork.
    pub fn delete_session(&self, session: &SessionId) -> Result<(), StoreErr> {
        let sid = session.0.clone();
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreErr("lock poisoned".into()))?;

        let fork_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE origin_session=?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(|e| StoreErr(format!("fork check: {e}")))?;
        if fork_count > 0 {
            return Err(StoreErr(format!(
                "cannot delete session {sid}: origin of {fork_count} fork(s)"
            )));
        }

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| StoreErr(format!("begin delete: {e}")))?;

        // Collect message content JSON for blob decref
        let content_jsons: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT content FROM messages WHERE session_id=?1")
                .map_err(|e| StoreErr(format!("msg content prepare: {e}")))?;
            let mut rows = stmt
                .query(params![sid])
                .map_err(|e| StoreErr(format!("msg content query: {e}")))?;
            let mut out = Vec::new();
            while let Some(r) = rows
                .next()
                .map_err(|e| StoreErr(format!("msg content row: {e}")))?
            {
                out.push(r.get::<_, String>(0).unwrap_or_default());
            }
            out
        };

        for cj in &content_jsons {
            project::decr_blob_refs(&conn, cj).map_err(|e| {
                let _ = conn.execute_batch("ROLLBACK");
                e
            })?;
        }

        for table in &[
            "events",
            "messages",
            "usage",
            "live_messages",
            "live_tool_calls",
        ] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE session_id=?1"),
                params![sid],
            )
            .map_err(|e| {
                let _ = conn.execute_batch("ROLLBACK");
                StoreErr(format!("delete {table}: {e}"))
            })?;
        }
        // plugin_kv: scope = session_id for session-scoped entries.
        conn.execute("DELETE FROM plugin_kv WHERE scope=?1", params![sid])
            .map_err(|e| {
                let _ = conn.execute_batch("ROLLBACK");
                StoreErr(format!("delete plugin_kv scope: {e}"))
            })?;
        conn.execute("DELETE FROM sessions WHERE id=?1", params![sid])
            .map_err(|e| {
                let _ = conn.execute_batch("ROLLBACK");
                StoreErr(format!("delete session: {e}"))
            })?;

        conn.execute_batch("COMMIT")
            .map_err(|e| StoreErr(format!("commit delete: {e}")))?;
        Ok(())
    }
}
