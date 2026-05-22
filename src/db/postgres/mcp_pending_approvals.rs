//! Postgres implementation of [`McpPendingApprovalsRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::{
    error::DbResult,
    repos::{McpPendingApproval, McpPendingApprovalsRepo, NewMcpPendingApproval},
};

pub struct PostgresMcpPendingApprovalsRepo {
    write_pool: PgPool,
}

impl PostgresMcpPendingApprovalsRepo {
    /// This repo has no read-only queries (the lookup is a
    /// `DELETE … RETURNING` claim that must hit the primary), so it only
    /// needs the write pool — unlike sibling repos that take a replica
    /// read pool.
    pub fn new(write_pool: PgPool) -> Self {
        Self { write_pool }
    }
}

#[async_trait]
impl McpPendingApprovalsRepo for PostgresMcpPendingApprovalsRepo {
    async fn insert(&self, row: NewMcpPendingApproval) -> DbResult<()> {
        sqlx::query(
            r#"
            INSERT INTO mcp_pending_approvals (
                id, response_id, org_id, call_id,
                server_label, server_url, tool_name,
                arguments_json, created_at, expires_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(&row.id)
        .bind(&row.response_id)
        .bind(row.org_id)
        .bind(&row.call_id)
        .bind(&row.server_label)
        .bind(&row.server_url)
        .bind(&row.tool_name)
        .bind(&row.arguments_json)
        .bind(row.created_at)
        .bind(row.expires_at)
        .execute(&self.write_pool)
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
        // one caller executes the gated tool. Runs on the write pool. The
        // `expires_at > $3` guard makes an expired approval unclaimable
        // even before the retention worker sweeps it.
        let row = sqlx::query(
            r#"
            DELETE FROM mcp_pending_approvals
            WHERE id = $1 AND org_id = $2 AND expires_at > $3
            RETURNING id, response_id, org_id, call_id,
                      server_label, server_url, tool_name,
                      arguments_json, created_at, expires_at
            "#,
        )
        .bind(id)
        .bind(org_id)
        .bind(now)
        .fetch_optional(&self.write_pool)
        .await?;

        Ok(row.map(|r| McpPendingApproval {
            id: r.get("id"),
            response_id: r.get("response_id"),
            org_id: r.get("org_id"),
            call_id: r.get("call_id"),
            server_label: r.get("server_label"),
            server_url: r.get("server_url"),
            tool_name: r.get("tool_name"),
            arguments_json: r.get("arguments_json"),
            created_at: r.get("created_at"),
            expires_at: r.get("expires_at"),
        }))
    }

    async fn delete_expired(&self, cutoff: DateTime<Utc>) -> DbResult<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM mcp_pending_approvals
            WHERE expires_at < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.write_pool)
        .await?;
        Ok(result.rows_affected())
    }
}
