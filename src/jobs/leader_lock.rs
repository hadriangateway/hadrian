//! Cross-replica leader election for periodic background jobs.
//!
//! Without coordination every gateway replica runs every cleanup tick — that
//! duplicates upstream calls (vector store deletes, provider health probes),
//! emits redundant events, and wastes egress. We use Postgres'
//! `pg_try_advisory_lock(bigint)` (session-level) for the duration of a
//! single tick.
//!
//! Postgres only releases session-level advisory locks when the holding
//! session ends, so we explicitly call `pg_advisory_unlock` on Drop and only
//! return the connection to the pool after the unlock has been observed.
//! The fallback path on a runtime-tear-down race detaches the connection so
//! Postgres reclaims the lock when the underlying socket closes — that way
//! the lock can never persist past tick.
//!
//! SQLite is single-process by construction, so the helper is a no-op there;
//! every tick proceeds.

use crate::db::DbPool;

/// Stable lock keys (random 64-bit constants). Don't reuse across jobs.
///
/// Only cleanup-style workers — those whose work is shared global state
/// (DB rows, external storage) — get a key here. `model_catalog_sync` and
/// `provider_health_check` deliberately don't, because they fan out per-
/// replica state (in-memory registries, circuit breakers) that every
/// replica must compute independently.
pub mod keys {
    pub const VECTOR_STORE_CLEANUP: i64 = 0x6861_6472_5f76_7363_u64 as i64;
    pub const OAUTH_CODE_CLEANUP: i64 = 0x6861_6472_5f6f_6163_u64 as i64;
    pub const RESPONSES_RETENTION: i64 = 0x6861_6472_5f72_6573_u64 as i64;
    pub const CONTAINERS_REAPER: i64 = 0x6861_6472_5f63_7472_u64 as i64;
}

/// Outcome of a leader-election attempt.
#[allow(dead_code)] // `Leader` / `NotLeader` are unused on SQLite-only builds
pub enum LeadershipOutcome {
    /// We acquired the lock; caller should run the work and let the guard
    /// drop after to release the Postgres session.
    Leader(LeaderGuard),
    /// Another replica already holds the lock; skip this tick.
    NotLeader,
    /// SQLite (or no DB-side advisory lock available); proceed without
    /// coordination.
    NoCoordination,
}

/// Holds an open dedicated connection that owns a Postgres advisory lock.
///
/// Sync `Drop` cannot `await`, so it spawns a task that calls
/// `pg_advisory_unlock` and only then drops the pooled connection. If no
/// Tokio runtime is available (e.g. drop firing during shutdown), the
/// connection is detached from the pool so dropping it terminates the
/// Postgres session and releases the lock that way.
pub struct LeaderGuard {
    #[cfg(feature = "database-postgres")]
    conn: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
    #[cfg(feature = "database-postgres")]
    key: i64,
}

#[cfg(feature = "database-postgres")]
impl Drop for LeaderGuard {
    fn drop(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let key = self.key;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(err) = sqlx::query("SELECT pg_advisory_unlock($1)")
                        .bind(key)
                        .execute(&mut *conn)
                        .await
                    {
                        tracing::warn!(
                            error = %err,
                            key,
                            "advisory lock: pg_advisory_unlock failed; detaching connection so the session ends and the lock is released",
                        );
                        // Detaching drops the inner connection rather than
                        // returning it to the pool, so the Postgres session
                        // ends and the lock is released regardless.
                        drop(conn.detach());
                    }
                });
            }
            Err(_) => {
                // No async runtime to issue an explicit unlock. Detach so
                // dropping the connection closes the socket — Postgres
                // releases session-level locks when the session ends.
                drop(conn.detach());
            }
        }
    }
}

/// Try to acquire the named advisory lock for the duration of the returned
/// guard. Returns `LeadershipOutcome::NoCoordination` for SQLite so existing
/// single-replica deployments keep behaving as before.
pub async fn try_acquire(db: &DbPool, key: i64) -> LeadershipOutcome {
    #[cfg(feature = "database-postgres")]
    {
        let Some(pool) = db.pg_write_pool() else {
            return LeadershipOutcome::NoCoordination;
        };
        let mut conn = match pool.acquire().await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(error = %err, key, "advisory lock: could not acquire connection");
                return LeadershipOutcome::NotLeader;
            }
        };
        let acquired: bool = match sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut *conn)
            .await
        {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, key, "advisory lock: pg_try_advisory_lock failed");
                return LeadershipOutcome::NotLeader;
            }
        };
        if acquired {
            LeadershipOutcome::Leader(LeaderGuard {
                conn: Some(conn),
                key,
            })
        } else {
            LeadershipOutcome::NotLeader
        }
    }
    #[cfg(not(feature = "database-postgres"))]
    {
        let _ = (db, key);
        LeadershipOutcome::NoCoordination
    }
}
