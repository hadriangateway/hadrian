use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::ListResult;
use crate::{
    db::error::DbResult,
    models::{AuditLog, AuditLogQuery, CreateAuditLog},
};

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait AuditLogRepo: Send + Sync {
    /// Create a new audit log entry
    async fn create(&self, input: CreateAuditLog) -> DbResult<AuditLog>;

    /// Get an audit log entry by ID
    async fn get_by_id(&self, id: Uuid) -> DbResult<Option<AuditLog>>;

    /// List audit logs with optional filtering and cursor-based pagination
    /// (`cursor` and `direction` fields in the query)
    async fn list(&self, query: AuditLogQuery) -> DbResult<ListResult<AuditLog>>;

    /// Count audit logs matching the query (ignores pagination parameters)
    async fn count(&self, query: AuditLogQuery) -> DbResult<i64>;

    // ==================== Retention Operations ====================

    /// Delete audit log entries older than the given cutoff date.
    ///
    /// Deletes in batches to avoid locking the database.
    /// Returns the total number of records deleted.
    async fn delete_before(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: u32,
        max_deletes: u64,
    ) -> DbResult<u64>;
}
