//! SQLite implementation of [`ContainersRepo`].

use async_trait::async_trait;

use super::{
    backend::{Pool, RowExt, query},
    common::parse_uuid,
};
use crate::db::{
    error::{DbError, DbResult},
    repos::{
        ContainerFileRecord, ContainerFileSourceKind, ContainerPatch, ContainerRecord,
        ContainerStatus, ContainersRepo, NewContainer, NewContainerFile, ResponseOwnerType,
        truncate_to_millis,
    },
};

const CONTAINER_COLUMNS: &str = "id, org_id, owner_type, owner_id, status, runtime_label, \
    source_response_id, idle_ttl_secs, last_active_at, created_at, expires_at, \
    name, memory_limit_mb, network_policy_json, skill_ids_json";

const CONTAINER_FILE_COLUMNS: &str = "id, container_id, org_id, path, filename, size_bytes, \
    content_type, content_hash, source, storage_backend, storage_path, \
    source_response_id, source_call_id, created_at";

pub struct SqliteContainersRepo {
    pool: Pool,
}

impl SqliteContainersRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn parse_status(s: &str) -> DbResult<ContainerStatus> {
    ContainerStatus::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown container status: {s}")))
}

fn parse_owner_type(s: &str) -> DbResult<ResponseOwnerType> {
    ResponseOwnerType::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown container owner_type: {s}")))
}

fn parse_source(s: &str) -> DbResult<ContainerFileSourceKind> {
    ContainerFileSourceKind::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown container_file source: {s}")))
}

fn parse_storage_backend(s: &str) -> DbResult<crate::models::StorageBackend> {
    s.parse::<crate::models::StorageBackend>()
        .map_err(DbError::Internal)
}

fn row_to_container(row: &super::backend::Row) -> DbResult<ContainerRecord> {
    Ok(ContainerRecord {
        id: row.col("id"),
        org_id: parse_uuid(&row.col::<String>("org_id"))?,
        owner_type: parse_owner_type(&row.col::<String>("owner_type"))?,
        owner_id: parse_uuid(&row.col::<String>("owner_id"))?,
        status: parse_status(&row.col::<String>("status"))?,
        runtime_label: row.col("runtime_label"),
        source_response_id: row.col("source_response_id"),
        idle_ttl_secs: row.col("idle_ttl_secs"),
        last_active_at: row.col("last_active_at"),
        created_at: row.col("created_at"),
        expires_at: row.col("expires_at"),
        name: row.col("name"),
        memory_limit_mb: row.col("memory_limit_mb"),
        network_policy_json: row.col("network_policy_json"),
        skill_ids_json: row.col("skill_ids_json"),
    })
}

fn row_to_file(row: &super::backend::Row) -> DbResult<ContainerFileRecord> {
    Ok(ContainerFileRecord {
        id: row.col("id"),
        container_id: row.col("container_id"),
        org_id: parse_uuid(&row.col::<String>("org_id"))?,
        path: row.col("path"),
        filename: row.col("filename"),
        size_bytes: row.col("size_bytes"),
        content_type: row.col("content_type"),
        content_hash: row.col("content_hash"),
        source: parse_source(&row.col::<String>("source"))?,
        storage_backend: parse_storage_backend(&row.col::<String>("storage_backend"))?,
        storage_path: row.col("storage_path"),
        source_response_id: row.col("source_response_id"),
        source_call_id: row.col("source_call_id"),
        created_at: row.col("created_at"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ContainersRepo for SqliteContainersRepo {
    async fn insert(&self, input: NewContainer) -> DbResult<ContainerRecord> {
        let created_at = truncate_to_millis(input.created_at);
        let last_active_at = created_at;

        query(
            r#"
            INSERT INTO containers (
                id, org_id, owner_type, owner_id, status, runtime_label,
                source_response_id, idle_ttl_secs, last_active_at, created_at,
                name, memory_limit_mb, network_policy_json, skill_ids_json
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&input.id)
        .bind(input.org_id.to_string())
        .bind(input.owner_type.as_str())
        .bind(input.owner_id.to_string())
        .bind(input.status.as_str())
        .bind(&input.runtime_label)
        .bind(&input.source_response_id)
        .bind(input.idle_ttl_secs)
        .bind(last_active_at)
        .bind(created_at)
        .bind(&input.name)
        .bind(input.memory_limit_mb)
        .bind(&input.network_policy_json)
        .bind(&input.skill_ids_json)
        .execute(&self.pool)
        .await?;

        Ok(ContainerRecord {
            id: input.id,
            org_id: input.org_id,
            owner_type: input.owner_type,
            owner_id: input.owner_id,
            status: input.status,
            runtime_label: input.runtime_label,
            source_response_id: input.source_response_id,
            idle_ttl_secs: input.idle_ttl_secs,
            last_active_at,
            created_at,
            expires_at: None,
            name: input.name,
            memory_limit_mb: input.memory_limit_mb,
            network_policy_json: input.network_policy_json,
            skill_ids_json: input.skill_ids_json,
        })
    }

    async fn get_by_id_and_org(
        &self,
        id: &str,
        org_id: uuid::Uuid,
    ) -> DbResult<Option<ContainerRecord>> {
        let sql = format!("SELECT {CONTAINER_COLUMNS} FROM containers WHERE id = ? AND org_id = ?");
        let row = query(&sql)
            .bind(id)
            .bind(org_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| row_to_container(&r)).transpose()
    }

    async fn upsert_file(&self, input: NewContainerFile) -> DbResult<ContainerFileRecord> {
        let created_at = truncate_to_millis(input.created_at);

        // ON CONFLICT (container_id, path) DO UPDATE — keeps the
        // existing row's id so any `container_file_citation` annotation
        // emitted before the overwrite still resolves to the latest
        // bytes. The replacement row uses the OLD id even if the
        // caller passed a new one.
        query(
            r#"
            INSERT INTO container_files (
                id, container_id, org_id, path, filename, size_bytes,
                content_type, content_hash, source, storage_backend,
                file_data, storage_path, source_response_id, source_call_id, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(container_id, path) DO UPDATE SET
                filename = excluded.filename,
                size_bytes = excluded.size_bytes,
                content_type = excluded.content_type,
                content_hash = excluded.content_hash,
                source = excluded.source,
                storage_backend = excluded.storage_backend,
                file_data = excluded.file_data,
                storage_path = excluded.storage_path,
                source_response_id = excluded.source_response_id,
                source_call_id = excluded.source_call_id,
                created_at = excluded.created_at
            "#,
        )
        .bind(&input.id)
        .bind(&input.container_id)
        .bind(input.org_id.to_string())
        .bind(&input.path)
        .bind(&input.filename)
        .bind(input.size_bytes)
        .bind(&input.content_type)
        .bind(&input.content_hash)
        .bind(input.source.as_str())
        .bind(input.storage_backend.as_str())
        .bind(&input.file_data)
        .bind(&input.storage_path)
        .bind(&input.source_response_id)
        .bind(&input.source_call_id)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        // Re-read so we return the canonical row (in particular the
        // pre-existing `id` when an upsert hit a conflict).
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE container_id = ? AND path = ?"
        );
        let row = query(&sql)
            .bind(&input.container_id)
            .bind(&input.path)
            .fetch_one(&self.pool)
            .await?;
        row_to_file(&row)
    }

    async fn get_file_by_id_and_org(
        &self,
        file_id: &str,
        org_id: uuid::Uuid,
    ) -> DbResult<Option<ContainerFileRecord>> {
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE id = ? AND org_id = ?"
        );
        let row = query(&sql)
            .bind(file_id)
            .bind(org_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| row_to_file(&r)).transpose()
    }

    async fn read_file_data(&self, file_id: &str, org_id: uuid::Uuid) -> DbResult<Option<Vec<u8>>> {
        let row = query(
            r#"
            SELECT file_data
            FROM container_files
            WHERE id = ? AND org_id = ?
            "#,
        )
        .bind(file_id)
        .bind(org_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.col::<Option<Vec<u8>>>("file_data")))
    }

    async fn list_files_by_container(
        &self,
        container_id: &str,
        org_id: uuid::Uuid,
        limit: i64,
    ) -> DbResult<Vec<ContainerFileRecord>> {
        let clamped = limit.clamp(1, 1000);
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE container_id = ? AND org_id = ? \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?"
        );
        let rows = query(&sql)
            .bind(container_id)
            .bind(org_id.to_string())
            .bind(clamped)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_file).collect()
    }

    async fn list_files_for_replay(
        &self,
        container_id: &str,
    ) -> DbResult<Vec<ContainerFileRecord>> {
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE container_id = ? \
             ORDER BY created_at DESC, id DESC"
        );
        let rows = query(&sql).bind(container_id).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_file).collect()
    }

    async fn read_file_data_for_replay(
        &self,
        container_id: &str,
        file_id: &str,
    ) -> DbResult<Option<Vec<u8>>> {
        let row = query(
            r#"
            SELECT file_data
            FROM container_files
            WHERE container_id = ? AND id = ?
            "#,
        )
        .bind(container_id)
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.col::<Option<Vec<u8>>>("file_data")))
    }

    async fn update_within_org(
        &self,
        id: &str,
        org_id: uuid::Uuid,
        patch: ContainerPatch,
    ) -> DbResult<Option<ContainerRecord>> {
        let mut setters: Vec<&str> = Vec::new();
        if patch.status.is_some() {
            setters.push("status = ?");
        }
        if patch.last_active_at.is_some() {
            setters.push("last_active_at = ?");
        }
        if patch.expires_at.is_some() {
            setters.push("expires_at = ?");
        }
        if setters.is_empty() {
            return self.get_by_id_and_org(id, org_id).await;
        }
        let sql = format!(
            "UPDATE containers SET {set} WHERE id = ? AND org_id = ? \
             RETURNING {CONTAINER_COLUMNS}",
            set = setters.join(", "),
        );
        let mut q = query(&sql);
        if let Some(s) = patch.status {
            q = q.bind(s.as_str().to_string());
        }
        if let Some(ts) = patch.last_active_at {
            q = q.bind(truncate_to_millis(ts));
        }
        if let Some(ts) = patch.expires_at {
            q = q.bind(truncate_to_millis(ts));
        }
        q = q.bind(id).bind(org_id.to_string());
        let row = q.fetch_optional(&self.pool).await?;
        row.as_ref().map(row_to_container).transpose()
    }

    async fn delete_file_by_id_and_org(
        &self,
        file_id: &str,
        container_id: &str,
        org_id: uuid::Uuid,
    ) -> DbResult<bool> {
        let res = query(
            r#"
            DELETE FROM container_files
            WHERE id = ? AND container_id = ? AND org_id = ?
            "#,
        )
        .bind(file_id)
        .bind(container_id)
        .bind(org_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn mark_expired_idle(&self, now: chrono::DateTime<chrono::Utc>) -> DbResult<Vec<String>> {
        let now_ts = truncate_to_millis(now);
        // SQLite's `datetime(x, '+N seconds')` strips fractional
        // seconds and the timezone suffix, returning `YYYY-MM-DD HH:MM:SS`.
        // sqlx-sqlite encodes a bound `DateTime<Utc>` as `YYYY-MM-DD
        // HH:MM:SS.fff+00:00`. The two strings share the same date+time
        // prefix but differ in suffix length, so a raw lex-comparison
        // can trip ~1 ms early at the boundary. Wrap the bound value
        // in `datetime()` too so both sides go through the same
        // normalization and the comparison is apples-to-apples.
        let rows = query(
            r#"
            UPDATE containers
            SET status = 'expired', expires_at = ?
            WHERE status = 'active'
              AND datetime(last_active_at, '+' || idle_ttl_secs || ' seconds')
                  < datetime(?)
            RETURNING id
            "#,
        )
        .bind(now_ts)
        .bind(now_ts)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.col("id")).collect())
    }
}
