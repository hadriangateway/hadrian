//! Parked MCP tool calls waiting on a `mcp_approval_response`.
//!
//! Written by the `McpExecutor` when a model-initiated tool call is
//! gated by `require_approval`; read by the preprocess in
//! `routes/execution.rs` when a follow-up request includes a
//! matching `mcp_approval_response` input item. See
//! `docs/content/docs/features/mcp-tool.mdx` for the protocol.
//!
//! The table is intentionally simple — one row per parked call, keyed
//! by the `approval_request_id` the gateway emitted on the
//! `mcp_approval_request` SSE item. Cleanup is best-effort via
//! `expires_at`: the responses retention worker
//! (`jobs::responses_retention`) calls [`McpPendingApprovalsRepo::delete_expired`]
//! each pass to sweep rows past their TTL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db::error::DbResult;

/// Persisted shape of one parked approval.
#[derive(Debug, Clone)]
pub struct McpPendingApproval {
    /// `approval_request_id` echoed back on `mcp_approval_response`.
    pub id: String,
    pub response_id: String,
    pub org_id: Uuid,
    /// `call_id` from the original `function_call`.
    pub call_id: String,
    pub server_label: String,
    pub server_url: String,
    pub tool_name: String,
    /// Arguments as a JSON string (matches `function_call.arguments`).
    pub arguments_json: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Fields supplied by the executor at park time.
#[derive(Debug, Clone)]
pub struct NewMcpPendingApproval {
    pub id: String,
    pub response_id: String,
    pub org_id: Uuid,
    pub call_id: String,
    pub server_label: String,
    pub server_url: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait McpPendingApprovalsRepo: Send + Sync {
    /// Park one approval. Conflicts on `id` are rejected — the gateway
    /// generates UUID-based ids so a collision means a programming
    /// error, not retry idempotency.
    async fn insert(&self, row: NewMcpPendingApproval) -> DbResult<()>;

    /// Atomically claim a parked approval by `approval_request_id`,
    /// scoped to `org_id` so cross-tenant lookup is impossible. Deletes
    /// the row and returns it in a single statement (`DELETE … RETURNING`)
    /// so exactly one resume of a given `approval_request_id` can win the
    /// claim — concurrent resumes of the same id see `None` and cannot
    /// double-execute a side-effecting tool. Returns `None` when no row
    /// matches (already consumed, wrong org, or expired).
    ///
    /// `now` gates the claim on `expires_at > now`: the retention worker
    /// only sweeps periodically, so without this gate an expired (but
    /// not-yet-swept) approval would still be claimable and executable.
    /// An expired row is left in place for the sweeper rather than
    /// claimed.
    async fn take_by_id_and_org(
        &self,
        id: &str,
        org_id: Uuid,
        now: DateTime<Utc>,
    ) -> DbResult<Option<McpPendingApproval>>;

    /// Delete every row whose `expires_at` is past `cutoff`. Returns
    /// the number of rows reaped so retention metrics can log it.
    /// Called by the retention worker; safe to invoke ad-hoc.
    async fn delete_expired(&self, cutoff: DateTime<Utc>) -> DbResult<u64>;
}
