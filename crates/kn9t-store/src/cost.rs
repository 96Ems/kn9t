//! R-STOR-180 — cost analytics queries.

use kn9t_core::{SessionId, StoreErr};
use rusqlite::{OptionalExtension, params};

use crate::db::SqliteStore;

pub struct CostRollup {
    pub marginal_micros:  i64,
    pub effective_micros: i64,
    pub family_micros:    i64,
    // Deprecated float views for wire compat
    pub marginal:  f64,
    pub effective: f64,
    pub family:    f64,
}

impl SqliteStore {
    pub fn cost_rollup(&self, session: &SessionId) -> Result<CostRollup, StoreErr> {
        let sid  = session.0.clone();
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;

        let marginal_micros: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_micros),0) FROM usage WHERE session_id=?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(|e| StoreErr(format!("marginal: {e}")))?;

        let inherited_micros: i64 = conn
            .query_row(
                "SELECT COALESCE(inherited_cost_micros,0) FROM sessions WHERE id=?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(|e| StoreErr(format!("inherited: {e}")))?;

        let effective_micros = marginal_micros + inherited_micros;
        let family_micros    = family_cost(&conn, &sid)?;

        Ok(CostRollup {
            marginal_micros,
            effective_micros,
            family_micros,
            marginal: marginal_micros as f64 / 1_000_000.0,
            effective: effective_micros as f64 / 1_000_000.0,
            family: family_micros as f64 / 1_000_000.0,
        })
    }
}

/// Recursive ancestor marginal-cost rollup (micros, deterministic).
fn family_cost(conn: &rusqlite::Connection, sid: &str) -> Result<i64, StoreErr> {
    let own: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(cost_micros),0) FROM usage WHERE session_id=?1",
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
