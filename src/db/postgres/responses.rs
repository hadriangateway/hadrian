//! Postgres implementation of [`ResponsesRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::{
    error::{DbError, DbResult},
    repos::{
        NewResponse, ResponseCompletion, ResponseOwnerType, ResponseRecord, ResponseStatus,
        ResponsesRepo,
    },
};

/// Org-scope filter for reads/updates/deletes against `responses` —
/// mirrors `skills::ORG_SCOPE_FILTER`. Org id is `$1` (referenced
/// five times — once per owner type). The clause is prefixed with
/// `AND` so it can be appended to a `WHERE` predicate that already
/// matches by `id`.
const ORG_SCOPE_FILTER: &str = r#"
    AND (
        (responses.owner_type = 'organization' AND responses.owner_id = $1)
        OR (responses.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = responses.owner_id AND t.org_id = $1
        ))
        OR (responses.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = responses.owner_id AND pr.org_id = $1
        ))
        OR (responses.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = responses.owner_id AND om.org_id = $1
        ))
        OR (responses.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = responses.owner_id AND sa.org_id = $1
        ))
    )
"#;

/// All columns of `responses` in canonical SELECT order, with
/// `owner_type` cast to TEXT for direct string parsing. Used by every
/// SELECT / RETURNING in this repo so the column list stays in sync.
const RESPONSE_COLUMNS: &str = "id, org_id, owner_type::TEXT, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    status, background, model, provider, \
    created_at, started_at, completed_at, \
    request_payload, output, usage, error, \
    retention_expires_at, last_sequence_number";

pub struct PostgresResponsesRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresResponsesRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
    }
}

fn parse_status(s: &str) -> DbResult<ResponseStatus> {
    ResponseStatus::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown response status: {s}")))
}

fn parse_owner_type(s: &str) -> DbResult<ResponseOwnerType> {
    ResponseOwnerType::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown response owner_type: {s}")))
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> DbResult<ResponseRecord> {
    Ok(ResponseRecord {
        id: row.get("id"),
        org_id: row.get("org_id"),
        owner_type: parse_owner_type(&row.get::<String, _>("owner_type"))?,
        owner_id: row.get("owner_id"),
        project_id: row.get("project_id"),
        user_id: row.get("user_id"),
        api_key_id: row.get("api_key_id"),
        service_account_id: row.get("service_account_id"),
        status: parse_status(&row.get::<String, _>("status"))?,
        background: row.get("background"),
        model: row.get("model"),
        provider: row.get("provider"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        request_payload: row.get("request_payload"),
        output: row.get("output"),
        usage: row.get("usage"),
        error: row.get("error"),
        retention_expires_at: row.get("retention_expires_at"),
        last_sequence_number: row.get("last_sequence_number"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ResponsesRepo for PostgresResponsesRepo {
    async fn insert(&self, input: NewResponse) -> DbResult<ResponseRecord> {
        sqlx::query(
            r#"
            INSERT INTO responses (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                status, background, model, provider,
                created_at, request_payload, retention_expires_at
            )
            VALUES (
                $1, $2, $3::response_owner_type, $4,
                $5, $6, $7, $8,
                $9, $10, $11, $12,
                $13, $14, $15
            )
            "#,
        )
        .bind(&input.id)
        .bind(input.org_id)
        .bind(input.owner_type.as_str())
        .bind(input.owner_id)
        .bind(input.project_id)
        .bind(input.user_id)
        .bind(input.api_key_id)
        .bind(input.service_account_id)
        .bind(input.status.as_str())
        .bind(input.background)
        .bind(&input.model)
        .bind(&input.provider)
        .bind(input.created_at)
        .bind(&input.request_payload)
        .bind(input.retention_expires_at)
        .execute(&self.write_pool)
        .await?;

        Ok(ResponseRecord {
            id: input.id,
            org_id: input.org_id,
            owner_type: input.owner_type,
            owner_id: input.owner_id,
            project_id: input.project_id,
            user_id: input.user_id,
            api_key_id: input.api_key_id,
            service_account_id: input.service_account_id,
            status: input.status,
            background: input.background,
            model: input.model,
            provider: input.provider,
            created_at: input.created_at,
            started_at: None,
            completed_at: None,
            request_payload: input.request_payload,
            output: None,
            usage: None,
            error: None,
            retention_expires_at: input.retention_expires_at,
            last_sequence_number: 0,
        })
    }

    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<ResponseRecord>> {
        let sql = format!(
            "SELECT {cols} FROM responses WHERE id = $2{scope}",
            cols = RESPONSE_COLUMNS,
            scope = ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: ResponseCompletion,
    ) -> DbResult<Option<ResponseRecord>> {
        // Build the SET clause dynamically. Org id is $1 (referenced
        // by the cascade scope filter), so dynamic columns start at
        // $2 and the id placeholder slots in after them.
        let mut setters: Vec<String> = Vec::new();
        let mut idx = 2usize;

        macro_rules! add {
            ($cond:expr, $col:expr) => {
                if $cond {
                    setters.push(format!("{} = ${}", $col, idx));
                    idx += 1;
                }
            };
        }
        add!(patch.status.is_some(), "status");
        add!(patch.started_at.is_some(), "started_at");
        add!(patch.completed_at.is_some(), "completed_at");
        add!(patch.output.is_some(), "output");
        add!(patch.usage.is_some(), "usage");
        add!(patch.error.is_some(), "error");
        add!(patch.retention_expires_at.is_some(), "retention_expires_at");
        if setters.is_empty() {
            return self.get_by_id_and_org(id, org_id).await;
        }

        let id_placeholder = idx;
        let sql = format!(
            "UPDATE responses SET {set} WHERE id = ${id}{scope} RETURNING {cols}",
            set = setters.join(", "),
            id = id_placeholder,
            scope = ORG_SCOPE_FILTER,
            cols = RESPONSE_COLUMNS,
        );
        let mut q = sqlx::query(&sql);
        q = q.bind(org_id);
        if let Some(status) = patch.status {
            q = q.bind(status.as_str().to_string());
        }
        if let Some(ts) = patch.started_at {
            q = q.bind(ts);
        }
        if let Some(ts) = patch.completed_at {
            q = q.bind(ts);
        }
        if let Some(output) = patch.output {
            q = q.bind(output);
        }
        if let Some(usage) = patch.usage {
            q = q.bind(usage);
        }
        if let Some(error) = patch.error {
            q = q.bind(error);
        }
        if let Some(ts) = patch.retention_expires_at {
            q = q.bind(ts);
        }
        q = q.bind(id);

        let result = q.fetch_optional(&self.write_pool).await?;
        match result {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<bool> {
        let sql = format!(
            "DELETE FROM responses WHERE id = $2{scope}",
            scope = ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .execute(&self.write_pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn claim_queued(&self, now: DateTime<Utc>) -> DbResult<Option<ResponseRecord>> {
        // SELECT FOR UPDATE SKIP LOCKED + UPDATE in one CTE gives
        // atomic, contention-free claim semantics across N workers.
        // Worker runs gateway-wide (not per principal), so no scope
        // filter — every queued row is claimable.
        let sql = format!(
            r#"
            WITH claimed AS (
                SELECT id FROM responses
                WHERE status = 'queued'
                ORDER BY created_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE responses
            SET status = 'in_progress', started_at = $1
            FROM claimed
            WHERE responses.id = claimed.id
            RETURNING {cols}
            "#,
            cols = RESPONSE_COLUMNS,
        );
        let result = sqlx::query(&sql)
            .bind(now)
            .fetch_optional(&self.write_pool)
            .await?;
        match result {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn list_cancelled_among(&self, ids: &[String]) -> DbResult<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            r#"
            SELECT id FROM responses
            WHERE status = 'cancelled' AND id = ANY($1)
            "#,
        )
        .bind(ids)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
    }

    async fn delete_expired(&self, before: DateTime<Utc>) -> DbResult<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM responses
            WHERE retention_expires_at < $1
              AND status IN ('completed', 'failed', 'cancelled', 'incomplete')
            "#,
        )
        .bind(before)
        .execute(&self.write_pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn reap_stuck_in_progress(
        &self,
        started_before: DateTime<Utc>,
        completed_at: DateTime<Utc>,
        retention_expires_at: DateTime<Utc>,
    ) -> DbResult<u64> {
        let error_payload = serde_json::json!({
            "code": "worker_lost",
            "message": "Worker died mid-execution; reaped by retention worker",
        });
        let result = sqlx::query(
            r#"
            UPDATE responses
            SET status = 'failed',
                completed_at = $1,
                error = $2,
                retention_expires_at = $3
            WHERE status = 'in_progress'
              AND started_at IS NOT NULL
              AND started_at < $4
            "#,
        )
        .bind(completed_at)
        .bind(error_payload)
        .bind(retention_expires_at)
        .bind(started_before)
        .execute(&self.write_pool)
        .await?;
        Ok(result.rows_affected())
    }
}
