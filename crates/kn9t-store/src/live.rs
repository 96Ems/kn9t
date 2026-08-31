//! R-STOR-170 — live_messages upsert/read/delete.

use kn9t_core::{CallId, Content, MsgId, SessionId, StoreErr};
use rusqlite::{OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::SqliteStore;

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

impl SqliteStore {
    pub fn upsert_live_message(
        &self,
        session: &SessionId,
        msg_id: &MsgId,
        role: &str,
        partial_content: &[Content],
    ) -> Result<(), StoreErr> {
        let content_json = serde_json::to_string(partial_content)
            .map_err(|e| StoreErr(format!("serialize live msg: {e}")))?;
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO live_messages(session_id,msg_id,role,partial_content,updated_at)\
             VALUES(?1,?2,?3,?4,?5)",
            params![session.0.clone(), msg_id.0.clone(), role, content_json, now_ts()],
        ).map_err(|e| StoreErr(format!("upsert live: {e}")))?;
        Ok(())
    }

    pub fn get_live_message(&self, session: &SessionId) -> Result<Option<String>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.query_row(
            "SELECT partial_content FROM live_messages WHERE session_id=?1",
            params![session.0.clone()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StoreErr(format!("get live: {e}")))
    }

    pub fn delete_live_message(&self, session: &SessionId) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "DELETE FROM live_messages WHERE session_id=?1",
            params![session.0.clone()],
        ).map_err(|e| StoreErr(format!("delete live: {e}")))?;
        Ok(())
    }
}

/// Cap on retained progress per call. A `bash` running a build can emit megabytes; we
/// only need enough to tell the model what happened, and the tail is the informative
/// part (errors surface last). Oldest bytes are dropped, `truncated` is set.
const MAX_PROGRESS_BYTES: usize = 8 * 1024;

impl SqliteStore {
    /// R-STOR-116 — open a progress row for `call_id` at `ToolStarted`. Resets any
    /// stale row from a previous life of the same call id.
    pub fn begin_live_tool_call(
        &self,
        session: &SessionId,
        call_id: &CallId,
        tool: &str,
    ) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO live_tool_calls\
             (session_id,call_id,tool,progress,truncated,updated_at) VALUES(?1,?2,?3,'',0,?4)",
            params![session.0.clone(), call_id.0.clone(), tool, now_ts()],
        ).map_err(|e| StoreErr(format!("begin live tool call: {e}")))?;
        Ok(())
    }

    /// R-STOR-116 — append one `ToolProgress` note. Keeps only the trailing
    /// `MAX_PROGRESS_BYTES`, flagging `truncated` once anything is dropped. A note for an
    /// unopened call is ignored (no row to grow) rather than creating a headless row.
    pub fn append_live_tool_progress(
        &self,
        session: &SessionId,
        call_id: &CallId,
        note: &str,
    ) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        let existing: Option<(String, i64)> = conn.query_row(
            "SELECT progress, truncated FROM live_tool_calls WHERE session_id=?1 AND call_id=?2",
            params![session.0.clone(), call_id.0.clone()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().map_err(|e| StoreErr(format!("read live progress: {e}")))?;
        let Some((mut progress, mut truncated)) = existing else {
            return Ok(());
        };

        progress.push_str(note);
        if progress.len() > MAX_PROGRESS_BYTES {
            // Cut on a char boundary so the stored text stays valid UTF-8.
            let mut cut = progress.len() - MAX_PROGRESS_BYTES;
            while cut < progress.len() && !progress.is_char_boundary(cut) {
                cut += 1;
            }
            progress = progress[cut..].to_owned();
            truncated = 1;
        }

        conn.execute(
            "UPDATE live_tool_calls SET progress=?1, truncated=?2, updated_at=?3\
             WHERE session_id=?4 AND call_id=?5",
            params![progress, truncated, now_ts(), session.0.clone(), call_id.0.clone()],
        ).map_err(|e| StoreErr(format!("append live progress: {e}")))?;
        Ok(())
    }

    /// R-STOR-116 — drop the row once the real `ToolResult` is durable. After this the
    /// progress is redundant: the transcript has the authoritative output.
    pub fn end_live_tool_call(
        &self,
        session: &SessionId,
        call_id: &CallId,
    ) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "DELETE FROM live_tool_calls WHERE session_id=?1 AND call_id=?2",
            params![session.0.clone(), call_id.0.clone()],
        ).map_err(|e| StoreErr(format!("end live tool call: {e}")))?;
        Ok(())
    }

    /// R-STOR-116 — salvaged progress for `call_id`: `(tool, progress, truncated)`.
    /// Used by `plan_request` to give a synthesized result real content.
    pub fn get_live_tool_progress(
        &self,
        session: &SessionId,
        call_id: &CallId,
    ) -> Result<Option<(String, String, bool)>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.query_row(
            "SELECT tool, progress, truncated FROM live_tool_calls\
             WHERE session_id=?1 AND call_id=?2",
            params![session.0.clone(), call_id.0.clone()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()
        .map_err(|e| StoreErr(format!("get live progress: {e}")))
    }
}
