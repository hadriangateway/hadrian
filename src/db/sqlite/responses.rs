//! SQLite implementation of [`ResponsesRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::{
    backend::{Pool, RowExt, query},
    common::parse_uuid,
};
use crate::db::{
    error::DbResult,
    repos::{
        NewResponse, ResponseCompletion, ResponseOwnerType, ResponseRecord, ResponseStatus,
        ResponsesRepo, truncate_to_millis,
    },
};

/// Org-scope filter for reads/updates/deletes against `responses`.
/// Mirrors `skills::ORG_SCOPE_FILTER` and the postgres counterpart.
/// Each `?` is bound to the caller's org id — five times, once per
/// owner type. Prefixed with `AND` so it appends to an existing
/// `WHERE id = ?` predicate.
const ORG_SCOPE_FILTER: &str = r#"
    AND (
        (responses.owner_type = 'organization' AND responses.owner_id = ?)
        OR (responses.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = responses.owner_id AND t.org_id = ?
        ))
        OR (responses.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = responses.owner_id AND pr.org_id = ?
        ))
        OR (responses.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = responses.owner_id AND om.org_id = ?
        ))
        OR (responses.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = responses.owner_id AND sa.org_id = ?
        ))
    )
"#;

/// Number of `?` placeholders in [`ORG_SCOPE_FILTER`] that resolve
/// to the caller's org id. Used by callers when chaining binds.
const ORG_SCOPE_BINDS: usize = 5;

/// Canonical column list for SELECT / RETURNING.
const RESPONSE_COLUMNS: &str = "id, org_id, owner_type, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    status, background, model, provider, \
    created_at, started_at, completed_at, \
    request_payload, output, usage, error, \
    retention_expires_at, last_sequence_number, container_id";

pub struct SqliteResponsesRepo {
    pool: Pool,
}

impl SqliteResponsesRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn parse_status(s: &str) -> DbResult<ResponseStatus> {
    ResponseStatus::parse(s)
        .ok_or_else(|| crate::db::error::DbError::Internal(format!("unknown response status: {s}")))
}

fn parse_owner_type(s: &str) -> DbResult<ResponseOwnerType> {
    ResponseOwnerType::parse(s).ok_or_else(|| {
        crate::db::error::DbError::Internal(format!("unknown response owner_type: {s}"))
    })
}

fn parse_json(s: Option<String>) -> DbResult<Option<Value>> {
    match s {
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        None => Ok(None),
    }
}

fn parse_optional_uuid(s: Option<String>) -> DbResult<Option<Uuid>> {
    s.map(|s| parse_uuid(&s)).transpose()
}

fn row_to_record(row: &super::backend::Row) -> DbResult<ResponseRecord> {
    let request_payload: String = row.col("request_payload");
    Ok(ResponseRecord {
        id: row.col("id"),
        org_id: parse_uuid(&row.col::<String>("org_id"))?,
        owner_type: parse_owner_type(&row.col::<String>("owner_type"))?,
        owner_id: parse_uuid(&row.col::<String>("owner_id"))?,
        project_id: parse_optional_uuid(row.col("project_id"))?,
        user_id: parse_optional_uuid(row.col("user_id"))?,
        api_key_id: parse_optional_uuid(row.col("api_key_id"))?,
        service_account_id: parse_optional_uuid(row.col("service_account_id"))?,
        status: parse_status(&row.col::<String>("status"))?,
        background: row.col::<i64>("background") != 0,
        model: row.col("model"),
        provider: row.col("provider"),
        created_at: row.col("created_at"),
        started_at: row.col("started_at"),
        completed_at: row.col("completed_at"),
        request_payload: serde_json::from_str(&request_payload)?,
        output: parse_json(row.col("output"))?,
        usage: parse_json(row.col("usage"))?,
        error: parse_json(row.col("error"))?,
        retention_expires_at: row.col("retention_expires_at"),
        last_sequence_number: row.col("last_sequence_number"),
        container_id: row.col("container_id"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ResponsesRepo for SqliteResponsesRepo {
    async fn insert(&self, input: NewResponse) -> DbResult<ResponseRecord> {
        let created_at = truncate_to_millis(input.created_at);
        let retention_expires_at = truncate_to_millis(input.retention_expires_at);
        let request_payload_json = serde_json::to_string(&input.request_payload)?;

        query(
            r#"
            INSERT INTO responses (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                status, background, model, provider,
                created_at, request_payload, retention_expires_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&input.id)
        .bind(input.org_id.to_string())
        .bind(input.owner_type.as_str())
        .bind(input.owner_id.to_string())
        .bind(input.project_id.map(|id| id.to_string()))
        .bind(input.user_id.map(|id| id.to_string()))
        .bind(input.api_key_id.map(|id| id.to_string()))
        .bind(input.service_account_id.map(|id| id.to_string()))
        .bind(input.status.as_str())
        .bind(input.background as i64)
        .bind(&input.model)
        .bind(&input.provider)
        .bind(created_at)
        .bind(&request_payload_json)
        .bind(retention_expires_at)
        .execute(&self.pool)
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
            created_at,
            started_at: None,
            completed_at: None,
            request_payload: input.request_payload,
            output: None,
            usage: None,
            error: None,
            retention_expires_at,
            last_sequence_number: 0,
            container_id: None,
        })
    }

    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<ResponseRecord>> {
        let sql = format!(
            "SELECT {cols} FROM responses WHERE id = ?{scope}",
            cols = RESPONSE_COLUMNS,
            scope = ORG_SCOPE_FILTER,
        );
        let mut q = query(&sql).bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        let result = q.fetch_optional(&self.pool).await?;
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
        // Build the SET clause dynamically. SQLite handles this fine
        // with one bind per Some field.
        let mut setters: Vec<&str> = Vec::new();
        if patch.status.is_some() {
            setters.push("status = ?");
        }
        if patch.started_at.is_some() {
            setters.push("started_at = ?");
        }
        if patch.completed_at.is_some() {
            setters.push("completed_at = ?");
        }
        if patch.output.is_some() {
            setters.push("output = ?");
        }
        if patch.usage.is_some() {
            setters.push("usage = ?");
        }
        if patch.error.is_some() {
            setters.push("error = ?");
        }
        if patch.retention_expires_at.is_some() {
            setters.push("retention_expires_at = ?");
        }
        if patch.container_id.is_some() {
            setters.push("container_id = ?");
        }
        if setters.is_empty() {
            return self.get_by_id_and_org(id, org_id).await;
        }

        let sql = format!(
            "UPDATE responses SET {set} WHERE id = ?{scope} RETURNING {cols}",
            set = setters.join(", "),
            scope = ORG_SCOPE_FILTER,
            cols = RESPONSE_COLUMNS,
        );
        let mut q = query(&sql);
        if let Some(status) = patch.status {
            q = q.bind(status.as_str().to_string());
        }
        if let Some(ts) = patch.started_at {
            q = q.bind(truncate_to_millis(ts));
        }
        if let Some(ts) = patch.completed_at {
            q = q.bind(truncate_to_millis(ts));
        }
        if let Some(output) = patch.output {
            q = q.bind(serde_json::to_string(&output)?);
        }
        if let Some(usage) = patch.usage {
            q = q.bind(serde_json::to_string(&usage)?);
        }
        if let Some(error) = patch.error {
            q = q.bind(serde_json::to_string(&error)?);
        }
        if let Some(ts) = patch.retention_expires_at {
            q = q.bind(truncate_to_millis(ts));
        }
        if let Some(cid) = patch.container_id {
            q = q.bind(cid);
        }
        q = q.bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }

        let result = q.fetch_optional(&self.pool).await?;
        match result {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<bool> {
        let sql = format!(
            "DELETE FROM responses WHERE id = ?{scope}",
            scope = ORG_SCOPE_FILTER,
        );
        let mut q = query(&sql).bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        let result = q.execute(&self.pool).await?;
        Ok(result.rows_affected() > 0)
    }

    async fn claim_queued(&self, now: DateTime<Utc>) -> DbResult<Option<ResponseRecord>> {
        let now = truncate_to_millis(now);
        // SQLite serialises writes, so a plain UPDATE...RETURNING with
        // a subselect of one row gives atomic claim semantics: the
        // first transaction wins, the rest see status != 'queued' and
        // get no rows back. Worker runs gateway-wide, so no scope
        // filter — every queued row is claimable.
        let sql = format!(
            r#"
            UPDATE responses
            SET status = 'in_progress', started_at = ?
            WHERE id = (
                SELECT id FROM responses
                WHERE status = 'queued'
                ORDER BY created_at ASC
                LIMIT 1
            )
            RETURNING {cols}
            "#,
            cols = RESPONSE_COLUMNS,
        );
        let result = query(&sql).bind(now).fetch_optional(&self.pool).await?;
        match result {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    async fn list_cancelled_among(&self, ids: &[String]) -> DbResult<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite doesn't have an ANY(array) operator, so we build a
        // placeholder list. Capped indirectly by the caller (the
        // in-flight set has a worker_concurrency bound).
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT id FROM responses WHERE status = 'cancelled' AND id IN ({placeholders})"
        );
        let mut q = query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| r.col::<String>("id")).collect())
    }

    async fn delete_expired(&self, before: DateTime<Utc>) -> DbResult<u64> {
        let before = truncate_to_millis(before);
        let result = query(
            r#"
            DELETE FROM responses
            WHERE retention_expires_at < ?
              AND status IN ('completed', 'failed', 'cancelled', 'incomplete')
            "#,
        )
        .bind(before)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn reap_stuck_in_progress(
        &self,
        started_before: DateTime<Utc>,
        completed_at: DateTime<Utc>,
        retention_expires_at: DateTime<Utc>,
    ) -> DbResult<u64> {
        let started_before = truncate_to_millis(started_before);
        let completed_at = truncate_to_millis(completed_at);
        let retention_expires_at = truncate_to_millis(retention_expires_at);
        let error_json = serde_json::to_string(&serde_json::json!({
            "code": "worker_lost",
            "message": "Worker died mid-execution; reaped by retention worker",
        }))?;
        let result = query(
            r#"
            UPDATE responses
            SET status = 'failed',
                completed_at = ?,
                error = ?,
                retention_expires_at = ?
            WHERE status = 'in_progress'
              AND started_at IS NOT NULL
              AND started_at < ?
            "#,
        )
        .bind(completed_at)
        .bind(error_json)
        .bind(retention_expires_at)
        .bind(started_before)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
