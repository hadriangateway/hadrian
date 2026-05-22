//! SQLite implementation of [`McpPendingApprovalsRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    backend::{Pool, RowExt, query},
    common::parse_uuid,
};
use crate::db::{
    error::DbResult,
    repos::{
        McpPendingApproval, McpPendingApprovalsRepo, NewMcpPendingApproval, truncate_to_millis,
    },
};

pub struct SqliteMcpPendingApprovalsRepo {
    pool: Pool,
}

impl SqliteMcpPendingApprovalsRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl McpPendingApprovalsRepo for SqliteMcpPendingApprovalsRepo {
    async fn insert(&self, row: NewMcpPendingApproval) -> DbResult<()> {
        query(
            r#"
            INSERT INTO mcp_pending_approvals (
                id, response_id, org_id, call_id,
                server_label, server_url, tool_name,
                arguments_json, created_at, expires_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&row.id)
        .bind(&row.response_id)
        .bind(row.org_id.to_string())
        .bind(&row.call_id)
        .bind(&row.server_label)
        .bind(&row.server_url)
        .bind(&row.tool_name)
        .bind(&row.arguments_json)
        .bind(truncate_to_millis(row.created_at))
        .bind(truncate_to_millis(row.expires_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn take_by_id_and_org(
        &self,
        id: &str,
        org_id: Uuid,
        now: DateTime<Utc>,
    ) -> DbResult<Option<McpPendingApproval>> {
        // DELETE … RETURNING claims the row atomically: a concurrent
        // resume of the same id deletes nothing and gets `None`, so only
        // one caller executes the gated tool. The `expires_at > ?` guard
        // makes an expired approval unclaimable even before the retention
        // worker sweeps it.
        let row = query(
            r#"
            DELETE FROM mcp_pending_approvals
            WHERE id = ? AND org_id = ? AND expires_at > ?
            RETURNING id, response_id, org_id, call_id,
                      server_label, server_url, tool_name,
                      arguments_json, created_at, expires_at
            "#,
        )
        .bind(id)
        .bind(org_id.to_string())
        .bind(truncate_to_millis(now))
        .fetch_optional(&self.pool)
        .await?;

        let Some(r) = row else { return Ok(None) };
        let org_id_str: String = r.col("org_id");
        Ok(Some(McpPendingApproval {
            id: r.col("id"),
            response_id: r.col("response_id"),
            org_id: parse_uuid(&org_id_str)?,
            call_id: r.col("call_id"),
            server_label: r.col("server_label"),
            server_url: r.col("server_url"),
            tool_name: r.col("tool_name"),
            arguments_json: r.col("arguments_json"),
            created_at: r.col::<DateTime<Utc>>("created_at"),
            expires_at: r.col::<DateTime<Utc>>("expires_at"),
        }))
    }

    async fn delete_expired(&self, cutoff: DateTime<Utc>) -> DbResult<u64> {
        let result = query(
            r#"
            DELETE FROM mcp_pending_approvals
            WHERE expires_at < ?
            "#,
        )
        .bind(truncate_to_millis(cutoff))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
