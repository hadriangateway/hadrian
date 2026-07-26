//! JavaScript FFI bridge to wa-sqlite running in the browser.
//!
//! `WasmSqlitePool` holds no Rust state — the actual SQLite database lives in
//! JavaScript (wa-sqlite + OPFS). All queries are dispatched via `wasm_bindgen`
//! extern functions and results are deserialized back into Rust types.

use sha2::Digest;
use wasm_bindgen::prelude::*;

use super::types::{WasmDbError, WasmParam, WasmQueryResult, WasmRow, WasmValue};

// ─────────────────────────────────────────────────────────────────────────────
// JS FFI declarations
// ─────────────────────────────────────────────────────────────────────────────

#[wasm_bindgen]
extern "C" {
    /// Execute a SELECT query. Returns a JSON-encoded array of row objects.
    /// Each row is an object mapping column names to values.
    #[wasm_bindgen(js_namespace = ["globalThis", "__hadrian_sqlite"], catch)]
    async fn query(sql: &str, params: JsValue) -> Result<JsValue, JsValue>;

    /// Execute a write statement (INSERT/UPDATE/DELETE).
    /// Returns a JSON-encoded object with `{ changes: number, last_insert_rowid: number }`.
    #[wasm_bindgen(js_namespace = ["globalThis", "__hadrian_sqlite"], catch)]
    async fn execute(sql: &str, params: JsValue) -> Result<JsValue, JsValue>;

    /// Initialize the database (create tables, run migrations).
    #[wasm_bindgen(js_namespace = ["globalThis", "__hadrian_sqlite"], catch)]
    async fn init_database() -> Result<(), JsValue>;

    /// Execute a multi-statement SQL script (e.g. migrations).
    /// Uses sql.js `db.exec()` which handles multiple statements natively.
    #[wasm_bindgen(js_namespace = ["globalThis", "__hadrian_sqlite"], catch)]
    async fn execute_script(sql: &str) -> Result<(), JsValue>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Pool
// ─────────────────────────────────────────────────────────────────────────────

/// Handle to the wa-sqlite database running in JavaScript.
///
/// This is a zero-size type — the actual database is managed by JS. Multiple
/// clones share the same underlying JS database instance.
#[derive(Debug, Clone)]
pub struct WasmSqlitePool;

/// A no-op "connection" handle for WASM SQLite.
///
/// In native SQLite, `pool.acquire()` returns a real connection handle used
/// for transactions (`BEGIN IMMEDIATE` … `COMMIT`). In WASM (single-threaded),
/// all queries go through the same JS bridge, so this is just a thin wrapper
/// that derefs back to `WasmSqlitePool`.
pub struct WasmPoolConnection(WasmSqlitePool);

impl std::ops::Deref for WasmPoolConnection {
    type Target = WasmSqlitePool;
    fn deref(&self) -> &WasmSqlitePool {
        &self.0
    }
}

impl std::ops::DerefMut for WasmPoolConnection {
    fn deref_mut(&mut self) -> &mut WasmSqlitePool {
        &mut self.0
    }
}

impl WasmSqlitePool {
    pub fn new() -> Self {
        Self
    }

    /// Acquire a "connection" (no-op in WASM — mirrors `sqlx::SqlitePool::acquire()`).
    pub async fn acquire(&self) -> Result<WasmPoolConnection, WasmDbError> {
        Ok(WasmPoolConnection(self.clone()))
    }

    /// Initialize the database (called once at startup).
    pub async fn init(&self) -> Result<(), WasmDbError> {
        init_database()
            .await
            .map_err(|e| WasmDbError::Query(js_error_to_string(&e)))
    }

    /// Execute a SELECT query and return all matching rows.
    pub async fn execute_query(
        &self,
        sql: &str,
        params: &[WasmParam],
    ) -> Result<Vec<WasmRow>, WasmDbError> {
        let js_params =
            serde_wasm_bindgen::to_value(params).map_err(|e| WasmDbError::Query(e.to_string()))?;

        let result = query(sql, js_params)
            .await
            .map_err(|e| classify_js_error(&e))?;

        // The JS bridge returns an array of objects: [{ col1: val1, col2: val2, ... }, ...]
        // We deserialize into Vec<Vec<(String, WasmValue)>> via an intermediate representation.
        let rows: Vec<serde_json::Map<String, serde_json::Value>> =
            serde_wasm_bindgen::from_value(result)
                .map_err(|e| WasmDbError::Query(format!("Failed to deserialize rows: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|obj| WasmRow {
                columns: obj
                    .into_iter()
                    .map(|(k, v)| (k, json_to_wasm_value(v)))
                    .collect(),
            })
            .collect())
    }

    /// Execute a write statement (INSERT/UPDATE/DELETE) and return the result.
    pub async fn execute_statement(
        &self,
        sql: &str,
        params: &[WasmParam],
    ) -> Result<WasmQueryResult, WasmDbError> {
        let js_params =
            serde_wasm_bindgen::to_value(params).map_err(|e| WasmDbError::Query(e.to_string()))?;

        let result = execute(sql, js_params)
            .await
            .map_err(|e| classify_js_error(&e))?;

        #[derive(serde::Deserialize)]
        struct ExecResult {
            changes: u64,
            #[serde(default)]
            last_insert_rowid: i64,
        }

        let exec: ExecResult = serde_wasm_bindgen::from_value(result)
            .map_err(|e| WasmDbError::Query(format!("Failed to deserialize exec result: {e}")))?;

        Ok(WasmQueryResult {
            rows_affected: exec.changes,
            last_insert_rowid: exec.last_insert_rowid,
        })
    }

    /// Run the embedded SQLite migration SQL.
    ///
    /// The browser database persists across deploys while the embedded migration
    /// can change under it (pre-1.0, the initial migration is edited in place).
    /// We record a checksum of the applied SQL and rebuild the database from
    /// scratch when it no longer matches: browser data is a local cache, and
    /// losing it beats silently keeping a stale schema that newer queries assume
    /// has moved on.
    pub async fn run_migrations(&self) -> Result<(), WasmDbError> {
        const MIGRATION_NAME: &str = "20250101000000_initial";
        const CREATE_TRACKING_TABLE: &str = "CREATE TABLE IF NOT EXISTS _wasm_migrations (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                checksum TEXT,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )";

        let migration_sql =
            include_str!("../../../migrations_sqlx/sqlite/20250101000000_initial.sql");
        let checksum = hex::encode(sha2::Sha256::digest(migration_sql.as_bytes()));

        self.execute_statement(CREATE_TRACKING_TABLE, &[]).await?;

        // `Some(checksum_of_applied_sql)` if the migration ran before. A legacy
        // tracking table has no `checksum` column, so the query itself erroring
        // is treated like a mismatch below.
        let applied: Option<Option<String>> = match self
            .execute_query(
                "SELECT checksum FROM _wasm_migrations WHERE name = ?",
                &[WasmParam::Text(MIGRATION_NAME.to_string())],
            )
            .await
        {
            Ok(rows) => rows
                .first()
                .map(|row| row.try_get::<Option<String>>("checksum").ok().flatten()),
            Err(_) => Some(None),
        };

        match applied {
            Some(Some(existing)) if existing == checksum => {
                tracing::debug!("WASM SQLite migrations already applied");
                return Ok(());
            }
            Some(_) => {
                tracing::warn!("WASM SQLite schema is stale; rebuilding the browser database");
                self.drop_all_objects().await?;
                self.execute_statement(CREATE_TRACKING_TABLE, &[]).await?;
            }
            None => {}
        }

        // Use execute_script to run the entire migration as one batch.
        // sql.js's db.exec() handles multiple statements natively, avoiding
        // issues with semicolons inside SQL comments.
        execute_script(migration_sql)
            .await
            .map_err(|e| WasmDbError::Query(js_error_to_string(&e)))?;

        // Record migration
        self.execute_statement(
            "INSERT INTO _wasm_migrations (name, checksum) VALUES (?, ?)",
            &[
                WasmParam::Text(MIGRATION_NAME.to_string()),
                WasmParam::Text(checksum),
            ],
        )
        .await?;

        tracing::info!("WASM SQLite migrations applied successfully");
        Ok(())
    }

    /// Drop every user table and view so the migration can run on a clean slate.
    /// Indexes and triggers go down with their tables.
    async fn drop_all_objects(&self) -> Result<(), WasmDbError> {
        let objects = self
            .execute_query(
                "SELECT type, name FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%' AND type IN ('view', 'table')
                 ORDER BY CASE type WHEN 'view' THEN 0 ELSE 1 END",
                &[],
            )
            .await?;

        self.execute_statement("PRAGMA foreign_keys=OFF", &[])
            .await?;
        for row in &objects {
            let ty: String = row.get("type");
            let name: String = row.get("name");
            let kind = if ty == "view" { "VIEW" } else { "TABLE" };
            let quoted = name.replace('"', "\"\"");
            self.execute_statement(&format!("DROP {kind} IF EXISTS \"{quoted}\""), &[])
                .await?;
        }
        self.execute_statement("PRAGMA foreign_keys=ON", &[])
            .await?;
        Ok(())
    }
}

impl Default for WasmSqlitePool {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a serde_json::Value to a WasmValue.
fn json_to_wasm_value(v: serde_json::Value) -> WasmValue {
    match v {
        serde_json::Value::Null => WasmValue::Null,
        serde_json::Value::Bool(b) => WasmValue::Integer(b as i64),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                WasmValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                WasmValue::Real(f)
            } else {
                WasmValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => WasmValue::Text(s),
        other => WasmValue::Text(other.to_string()),
    }
}

/// Extract a human-readable error message from a JsValue.
fn js_error_to_string(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        return err.message().into();
    }
    format!("{e:?}")
}

/// Classify a JS error into the appropriate WasmDbError variant.
fn classify_js_error(e: &JsValue) -> WasmDbError {
    let msg = js_error_to_string(e);

    // SQLite constraint error messages
    if msg.contains("UNIQUE constraint failed") {
        WasmDbError::UniqueViolation(msg)
    } else if msg.contains("FOREIGN KEY constraint failed") {
        WasmDbError::ForeignKeyViolation(msg)
    } else {
        WasmDbError::Query(msg)
    }
}
