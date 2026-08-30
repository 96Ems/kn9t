//! R-STOR-140, R-STOR-150 — blob put/get and refcount GC.

use kn9t_core::StoreErr;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::SqliteStore;

fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

impl SqliteStore {
    /// R-STOR-140 — content-addressed blob store.
    pub fn put_blob(&self, bytes: &[u8], mime: &str) -> Result<String, StoreErr> {
        let hash = sha256_hex(bytes);
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        let exists: Option<i64> = conn
            .query_row("SELECT refcount FROM blobs WHERE hash=?1", params![hash], |r| r.get(0))
            .optional()
            .map_err(|e| StoreErr(format!("blob query: {e}")))?;
        if exists.is_none() {
            let ts = now_ts();
            conn.execute(
                "INSERT INTO blobs(hash,mime,bytes_len,bytes,refcount,created_at)\
                 VALUES(?1,?2,?3,?4,0,?5)",
                params![hash, mime, bytes.len() as i64, bytes, ts],
            ).map_err(|e| StoreErr(format!("insert blob: {e}")))?;
        }
        Ok(hash)
    }

    /// R-STOR-140
    pub fn get_blob(&self, hash: &str) -> Result<Option<(Vec<u8>, String)>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.query_row(
            "SELECT bytes, mime FROM blobs WHERE hash=?1",
            params![hash],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| StoreErr(format!("get blob: {e}")))
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}
