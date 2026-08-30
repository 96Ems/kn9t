//! R-STOR-170 — live_messages upsert/read/delete.

use kn9t_core::{Content, MsgId, SessionId, StoreErr};
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
