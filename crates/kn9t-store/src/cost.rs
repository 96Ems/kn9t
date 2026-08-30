//! R-STOR-180 — cost analytics queries.

use kn9t_core::{SessionId, StoreErr};
use rusqlite::{OptionalExtension, params};

use crate::db::SqliteStore;

pub struct CostRollup {
    pub marginal:  f64,
    pub effective: f64,
    pub family:    f64,
}

impl SqliteStore {
    pub fn cost_rollup(&self, session: &SessionId) -> Result<CostRollup, StoreErr> {
        let sid  = session.0.clone();
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;

        let marginal: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd),0.0) FROM usage WHERE session_id=?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(|e| StoreErr(format!("marginal: {e}")))?;

        let inherited: f64 = conn
            .query_row(
                "SELECT inherited_cost_usd FROM sessions WHERE id=?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(|e| StoreErr(format!("inherited: {e}")))?;

        let effective = marginal + inherited;
        let family    = family_cost(&conn, &sid)?;

        Ok(CostRollup { marginal, effective, family })
    }
}

/// Recursive ancestor marginal-cost rollup.
fn family_cost(conn: &rusqlite::Connection, sid: &str) -> Result<f64, StoreErr> {
    let own: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(cost_usd),0.0) FROM usage WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .map_err(|e| StoreErr(format!("family own: {e}")))?;

    let origin: Option<String> = conn
        .query_row(
            "SELECT origin_session FROM sessions WHERE id=?1",
            params![sid],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StoreErr(format!("family origin: {e}")))?
        .flatten();

    match origin {
        Some(parent) => Ok(own + family_cost(conn, &parent)?),
        None         => Ok(own),
    }
}
