//! R-STOR-010, R-STOR-020, R-STOR-030 — open, pragmas, schema.

use kn9t_core::{Event, ModelRef, ModelSpec, PluginKv, RequestPlan, SessionId, SessionSnapshot, Store, StoreErr};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

/// Current projection version — bump when `project()` semantics change.
pub const PROJECTION_VERSION: &str = "2";

pub struct SqliteStore {
    /// Single writer connection; WAL allows concurrent readers on separate connections.
    pub(crate) conn: Mutex<Connection>,
    pub(crate) path: PathBuf,
    /// Runtime model specs by `ModelRef` provider+id key — not stored in DB.
    pub(crate) model_specs: RwLock<HashMap<String, ModelSpec>>,
}

impl SqliteStore {
    pub fn register_model_spec(&self, spec: ModelSpec) {
        let key = format!("{}:{}", spec.r#ref.provider, spec.r#ref.id);
        self.model_specs.write().unwrap().insert(key, spec);
    }

    /// Get the current model for a session.
    /// Priority: last ModelChanged event > model_at_fork > SessionForked.
    pub fn get_model_spec_for_session(&self, session_id: &str) -> Option<ModelSpec> {
        let conn = self.conn.lock().ok()?;
        
        // 1. Check for last ModelChanged event
        let last_changed: Option<String> = conn.query_row(
            "SELECT payload FROM events WHERE session_id=?1 AND kind='ModelChanged' ORDER BY seq DESC LIMIT 1",
            params![session_id],
            |r| r.get(0),
        ).optional().ok().flatten();
        
        if let Some(p) = last_changed {
            if let Ok(kn9t_core::Event::ModelChanged { model, .. }) = serde_json::from_str(&p) {
                let key = format!("{}:{}", model.provider, model.id);
                return self.model_specs.read().ok()?.get(&key).cloned();
            }
        }
        
        // 2. Fallback to model_at_fork
        let model_json: Option<String> = conn.query_row(
            "SELECT model_at_fork FROM sessions WHERE id=?1",
            params![session_id],
            |r| r.get(0),
        ).optional().ok().flatten();
        
        if let Some(j) = model_json {
            if let Ok(model_ref) = serde_json::from_str::<ModelRef>(&j) {
                let key = format!("{}:{}", model_ref.provider, model_ref.id);
                return self.model_specs.read().ok()?.get(&key).cloned();
            }
        }
        
        None
    }
}

impl SqliteStore {
    /// Open (or create) the store at `path`. Applies pragmas and runs schema DDL.
    pub fn open(path: &Path) -> Result<Self, StoreErr> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreErr(format!("create dir: {e}")))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| StoreErr(format!("open db: {e}")))?;
        apply_pragmas(&conn)?;
        create_schema(&conn)?;
        truncate_live_messages(&conn)?;
        check_projection_version(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_owned(),
            model_specs: RwLock::new(HashMap::new()),
        })
    }

    /// Open at the default path `~/.kn9t/kn9t.db`.
    pub fn open_default() -> Result<Self, StoreErr> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| StoreErr("HOME not set".into()))?;
        let path = PathBuf::from(home).join(".kn9t").join("kn9t.db");
        Self::open(&path)
    }

    pub fn path(&self) -> &Path { &self.path }

    /// R-STOR-090 — run reproject --check; returns list of diff descriptions.
    pub fn reproject_check(&self) -> Result<Vec<String>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        crate::reproject::reproject_check(&conn)
    }

    /// Execute a DML statement (UPDATE/DELETE). For tests.
    pub fn execute_raw(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<usize, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(sql, rusqlite::params_from_iter(params.iter().copied()))
            .map_err(|e| StoreErr(format!("execute_raw: {e}")))
    }

    /// Execute a single-value query and return the result. For tests and analytics.
    pub fn query_one<T, F>(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql], f: F) -> Result<T, StoreErr>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.query_row(sql, params, f).map_err(|e| StoreErr(format!("query_one: {e}")))
    }

    /// Execute a query returning multiple rows of a single string column. For tests.
    pub fn query_strings(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<Vec<String>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        let mut stmt = conn.prepare(sql).map_err(|e| StoreErr(format!("prepare: {e}")))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params.iter().copied()))
            .map_err(|e| StoreErr(format!("query: {e}")))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(|e| StoreErr(format!("row: {e}")))? {
            out.push(r.get::<_, String>(0).unwrap_or_default());
        }
        Ok(out)
    }
    
    /// Get a preference value from the meta table.
    pub fn get_pref(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        ).optional().ok().flatten()
    }
    
    /// Set a preference value in the meta table.
    pub fn set_pref(&self, key: &str, value: &str) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        ).map_err(|e| StoreErr(format!("set_pref: {e}")))?;
        Ok(())
    }
}

fn apply_pragmas(conn: &Connection) -> Result<(), StoreErr> {
    let stmts = [
        "PRAGMA journal_mode = WAL",
        "PRAGMA synchronous = NORMAL",
        "PRAGMA foreign_keys = ON",
        "PRAGMA busy_timeout = 5000",
    ];
    for s in &stmts {
        conn.execute_batch(s).map_err(|e| StoreErr(format!("pragma: {e}")))?;
    }
    Ok(())
}

pub(crate) fn create_schema(conn: &Connection) -> Result<(), StoreErr> {
    conn.execute_batch(SCHEMA_DDL).map_err(|e| StoreErr(format!("schema: {e}")))
}

fn truncate_live_messages(conn: &Connection) -> Result<(), StoreErr> {
    conn.execute_batch("DELETE FROM live_messages")
        .map_err(|e| StoreErr(format!("truncate live_messages: {e}")))
}

fn check_projection_version(conn: &Connection) -> Result<(), StoreErr> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'PROJECTION_VERSION'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StoreErr(format!("read meta: {e}")))?;
    match stored {
        None => {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('PROJECTION_VERSION',?1)",
                params![PROJECTION_VERSION],
            ).map_err(|e| StoreErr(format!("set proj ver: {e}")))?;
        }
        Some(v) if v != PROJECTION_VERSION => {
            crate::reproject::reproject(conn)?;
        }
        _ => {}
    }
    Ok(())
}

// ── trait impls ──────────────────────────────────────────────────────────────

impl PluginKv for SqliteStore {
    fn kv_get(&self, plugin: &str, scope: &str, key: &str) -> Result<Option<serde_json::Value>, StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        let raw: Option<String> = conn.query_row(
            "SELECT value FROM plugin_kv WHERE plugin=?1 AND scope=?2 AND key=?3",
            params![plugin, scope, key],
            |r| r.get(0),
        ).optional().map_err(|e| StoreErr(format!("kv_get: {e}")))?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| StoreErr(format!("kv_get parse: {e}"))),
        }
    }

    fn kv_set(&self, plugin: &str, scope: &str, key: &str, value: &serde_json::Value) -> Result<(), StoreErr> {
        let raw = serde_json::to_string(value)
            .map_err(|e| StoreErr(format!("kv_set serialize: {e}")))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO plugin_kv (plugin, scope, key, value, updated_at) VALUES (?1,?2,?3,?4,?5)",
            params![plugin, scope, key, raw, now],
        ).map_err(|e| StoreErr(format!("kv_set: {e}")))?;
        Ok(())
    }

    fn kv_del(&self, plugin: &str, scope: &str, key: &str) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "DELETE FROM plugin_kv WHERE plugin=?1 AND scope=?2 AND key=?3",
            params![plugin, scope, key],
        ).map_err(|e| StoreErr(format!("kv_del: {e}")))?;
        Ok(())
    }

    fn kv_del_scope(&self, plugin: &str, scope: &str) -> Result<(), StoreErr> {
        let conn = self.conn.lock().map_err(|_| StoreErr("lock poisoned".into()))?;
        conn.execute(
            "DELETE FROM plugin_kv WHERE plugin=?1 AND scope=?2",
            params![plugin, scope],
        ).map_err(|e| StoreErr(format!("kv_del_scope: {e}")))?;
        Ok(())
    }
}

impl Store for SqliteStore {
    fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr> {
        crate::session::append(self, session, event)
    }

    fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, StoreErr> {
        crate::session::snapshot(self, session)
    }

    fn plan_request(&self, session: &SessionId) -> Result<RequestPlan, StoreErr> {
        crate::plan::plan_request(self, session)
    }
}

// ── schema DDL ───────────────────────────────────────────────────────────────

pub(crate) const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS sessions (
  id                   TEXT PRIMARY KEY,
  created_at           INTEGER NOT NULL,
  name                 TEXT,
  cwd                  TEXT NOT NULL,
  origin_session       TEXT REFERENCES sessions(id),
  origin_seq           INTEGER,
  fork_reason          TEXT,
  inherited_cost_usd   REAL    NOT NULL DEFAULT 0,
  inherited_tokens_in  INTEGER NOT NULL DEFAULT 0,
  inherited_tokens_out INTEGER NOT NULL DEFAULT 0,
  inherited_ctx_tokens INTEGER NOT NULL DEFAULT 0,
  budget_remaining_usd REAL,
  model_at_fork        TEXT,
  head_seq             INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS events (
  session_id TEXT    NOT NULL REFERENCES sessions(id),
  seq        INTEGER NOT NULL,
  ts         INTEGER NOT NULL,
  kind       TEXT    NOT NULL,
  payload    TEXT    NOT NULL,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS messages (
  session_id TEXT    NOT NULL REFERENCES sessions(id),
  seq        INTEGER NOT NULL,
  role       TEXT    NOT NULL,
  content    TEXT    NOT NULL,
  est_tokens INTEGER NOT NULL,
  silent     INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS usage (
  session_id                 TEXT    NOT NULL REFERENCES sessions(id),
  seq                        INTEGER NOT NULL,
  ts                         INTEGER NOT NULL,
  provider                   TEXT    NOT NULL,
  model                      TEXT    NOT NULL,
  kind                       TEXT    NOT NULL,
  tokens_in                  INTEGER NOT NULL,
  tokens_out                 INTEGER NOT NULL,
  cache_read                 INTEGER NOT NULL,
  cache_write                INTEGER NOT NULL,
  reasoning                  INTEGER NOT NULL DEFAULT 0,
  price_in_snapshot          REAL    NOT NULL,
  price_out_snapshot         REAL    NOT NULL,
  price_cache_read_snapshot  REAL    NOT NULL,
  price_cache_write_snapshot REAL    NOT NULL,
  cost_usd                   REAL    NOT NULL,
  estimated                  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS blobs (
  hash       TEXT PRIMARY KEY,
  mime       TEXT    NOT NULL,
  bytes_len  INTEGER NOT NULL,
  bytes      BLOB    NOT NULL,
  refcount   INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS live_messages (
  session_id      TEXT PRIMARY KEY REFERENCES sessions(id),
  msg_id          TEXT    NOT NULL,
  role            TEXT    NOT NULL,
  partial_content TEXT    NOT NULL,
  updated_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS usage_by_model    ON usage(model, kind);
CREATE INDEX IF NOT EXISTS events_by_session ON events(session_id, seq);

CREATE TABLE IF NOT EXISTS plugin_kv (
  plugin     TEXT    NOT NULL,
  scope      TEXT    NOT NULL,
  key        TEXT    NOT NULL,
  value      TEXT    NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (plugin, scope, key)
);

CREATE INDEX IF NOT EXISTS plugin_kv_by_scope ON plugin_kv(scope);
";
