//! Postgres implementation of [`ContainersRepo`].

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::{
    error::{DbError, DbResult},
    repos::{
        ContainerFileRecord, ContainerFileSourceKind, ContainerPatch, ContainerRecord,
        ContainerStatus, ContainersRepo, NewContainer, NewContainerFile, ResponseOwnerType,
    },
};

const CONTAINER_COLUMNS: &str = "id, org_id, owner_type::TEXT, owner_id, status, runtime_label, \
    source_response_id, idle_ttl_secs, last_active_at, created_at, expires_at, \
    name, memory_limit_mb, network_policy_json, skill_ids_json";

const CONTAINER_FILE_COLUMNS: &str = "id, container_id, org_id, path, filename, size_bytes, \
    content_type, content_hash, source, storage_backend::TEXT, storage_path, \
    source_response_id, source_call_id, created_at";

pub struct PostgresContainersRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresContainersRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
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

fn row_to_container(row: &sqlx::postgres::PgRow) -> DbResult<ContainerRecord> {
    Ok(ContainerRecord {
        id: row.get("id"),
        org_id: row.get("org_id"),
        owner_type: parse_owner_type(&row.get::<String, _>("owner_type"))?,
        owner_id: row.get("owner_id"),
        status: parse_status(&row.get::<String, _>("status"))?,
        runtime_label: row.get("runtime_label"),
        source_response_id: row.get("source_response_id"),
        idle_ttl_secs: row.get("idle_ttl_secs"),
        last_active_at: row.get("last_active_at"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        name: row.get("name"),
        memory_limit_mb: row.get::<Option<i32>, _>("memory_limit_mb").map(i64::from),
        network_policy_json: row.get("network_policy_json"),
        skill_ids_json: row.get("skill_ids_json"),
    })
}

fn row_to_file(row: &sqlx::postgres::PgRow) -> DbResult<ContainerFileRecord> {
    Ok(ContainerFileRecord {
        id: row.get("id"),
        container_id: row.get("container_id"),
        org_id: row.get("org_id"),
        path: row.get("path"),
        filename: row.get("filename"),
        size_bytes: row.get("size_bytes"),
        content_type: row.get("content_type"),
        content_hash: row.get("content_hash"),
        source: parse_source(&row.get::<String, _>("source"))?,
        storage_backend: parse_storage_backend(&row.get::<String, _>("storage_backend"))?,
        storage_path: row.get("storage_path"),
        source_response_id: row.get("source_response_id"),
        source_call_id: row.get("source_call_id"),
        created_at: row.get("created_at"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ContainersRepo for PostgresContainersRepo {
    async fn insert(&self, input: NewContainer) -> DbResult<ContainerRecord> {
        // Both timestamps land at insert time. last_active_at == created_at.
        let sql = format!(
            r#"
            INSERT INTO containers (
                id, org_id, owner_type, owner_id, status, runtime_label,
                source_response_id, idle_ttl_secs, last_active_at, created_at,
                name, memory_limit_mb, network_policy_json, skill_ids_json
            )
            VALUES ($1, $2, $3::response_owner_type, $4, $5, $6, $7, $8, $9, $9, $10, $11, $12, $13)
            RETURNING {CONTAINER_COLUMNS}
            "#
        );
        let row = sqlx::query(&sql)
            .bind(&input.id)
            .bind(input.org_id)
            .bind(input.owner_type.as_str())
            .bind(input.owner_id)
            .bind(input.status.as_str())
            .bind(&input.runtime_label)
            .bind(&input.source_response_id)
            .bind(input.idle_ttl_secs)
            .bind(input.created_at)
            .bind(&input.name)
            .bind(input.memory_limit_mb.map(|m| m as i32))
            .bind(&input.network_policy_json)
            .bind(&input.skill_ids_json)
            .fetch_one(&self.write_pool)
            .await?;
        row_to_container(&row)
    }

    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<ContainerRecord>> {
        let sql =
            format!("SELECT {CONTAINER_COLUMNS} FROM containers WHERE id = $1 AND org_id = $2");
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(org_id)
            .fetch_optional(&self.read_pool)
            .await?;
        row.map(|r| row_to_container(&r)).transpose()
    }

    async fn upsert_file(&self, input: NewContainerFile) -> DbResult<ContainerFileRecord> {
        // ON CONFLICT keeps the original row's id stable so any
        // annotation that already cited the prior version still
        // resolves to the latest bytes.
        let sql = format!(
            r#"
            INSERT INTO container_files (
                id, container_id, org_id, path, filename, size_bytes,
                content_type, content_hash, source, storage_backend,
                file_data, storage_path, source_response_id, source_call_id, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::file_storage_backend, $11, $12, $13, $14, $15)
            ON CONFLICT (container_id, path) DO UPDATE SET
                filename = EXCLUDED.filename,
                size_bytes = EXCLUDED.size_bytes,
                content_type = EXCLUDED.content_type,
                content_hash = EXCLUDED.content_hash,
                source = EXCLUDED.source,
                storage_backend = EXCLUDED.storage_backend,
                file_data = EXCLUDED.file_data,
                storage_path = EXCLUDED.storage_path,
                source_response_id = EXCLUDED.source_response_id,
                source_call_id = EXCLUDED.source_call_id,
                created_at = EXCLUDED.created_at
            RETURNING {CONTAINER_FILE_COLUMNS}
            "#
        );
        let row = sqlx::query(&sql)
            .bind(&input.id)
            .bind(&input.container_id)
            .bind(input.org_id)
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
            .bind(input.created_at)
            .fetch_one(&self.write_pool)
            .await?;
        row_to_file(&row)
    }

    async fn get_file_by_id_and_org(
        &self,
        file_id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<ContainerFileRecord>> {
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE id = $1 AND org_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(file_id)
            .bind(org_id)
            .fetch_optional(&self.read_pool)
            .await?;
        row.map(|r| row_to_file(&r)).transpose()
    }

    async fn read_file_data(&self, file_id: &str, org_id: Uuid) -> DbResult<Option<Vec<u8>>> {
        let row = sqlx::query(
            r#"
            SELECT file_data FROM container_files
            WHERE id = $1 AND org_id = $2
            "#,
        )
        .bind(file_id)
        .bind(org_id)
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(row.and_then(|r| r.get::<Option<Vec<u8>>, _>("file_data")))
    }

    async fn list_files_by_container(
        &self,
        container_id: &str,
        org_id: Uuid,
        limit: i64,
    ) -> DbResult<Vec<ContainerFileRecord>> {
        let clamped = limit.clamp(1, 1000);
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE container_id = $1 AND org_id = $2 \
             ORDER BY created_at DESC, id DESC \
             LIMIT $3"
        );
        let rows = sqlx::query(&sql)
            .bind(container_id)
            .bind(org_id)
            .bind(clamped)
            .fetch_all(&self.read_pool)
            .await?;
        rows.iter().map(row_to_file).collect()
    }

    async fn list_files_for_replay(
        &self,
        container_id: &str,
    ) -> DbResult<Vec<ContainerFileRecord>> {
        let sql = format!(
            "SELECT {CONTAINER_FILE_COLUMNS} FROM container_files \
             WHERE container_id = $1 \
             ORDER BY created_at DESC, id DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(container_id)
            .fetch_all(&self.read_pool)
            .await?;
        rows.iter().map(row_to_file).collect()
    }

    async fn read_file_data_for_replay(
        &self,
        container_id: &str,
        file_id: &str,
    ) -> DbResult<Option<Vec<u8>>> {
        let row = sqlx::query(
            r#"
            SELECT file_data FROM container_files
            WHERE container_id = $1 AND id = $2
            "#,
        )
        .bind(container_id)
        .bind(file_id)
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(row.and_then(|r| r.get::<Option<Vec<u8>>, _>("file_data")))
    }

    async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: ContainerPatch,
    ) -> DbResult<Option<ContainerRecord>> {
        let mut setters: Vec<String> = Vec::new();
        let mut idx = 3usize; // $1 = id, $2 = org_id
        if patch.status.is_some() {
            setters.push(format!("status = ${idx}"));
            idx += 1;
        }
        if patch.last_active_at.is_some() {
            setters.push(format!("last_active_at = ${idx}"));
            idx += 1;
        }
        if patch.expires_at.is_some() {
            setters.push(format!("expires_at = ${idx}"));
        }
        if setters.is_empty() {
            return self.get_by_id_and_org(id, org_id).await;
        }
        let sql = format!(
            "UPDATE containers SET {set} WHERE id = $1 AND org_id = $2 \
             RETURNING {CONTAINER_COLUMNS}",
            set = setters.join(", "),
        );
        let mut q = sqlx::query(&sql).bind(id).bind(org_id);
        if let Some(s) = patch.status {
            q = q.bind(s.as_str());
        }
        if let Some(ts) = patch.last_active_at {
            q = q.bind(ts);
        }
        if let Some(ts) = patch.expires_at {
            q = q.bind(ts);
        }
        let row = q.fetch_optional(&self.write_pool).await?;
        row.as_ref().map(row_to_container).transpose()
    }

    async fn delete_file_by_id_and_org(
        &self,
        file_id: &str,
        container_id: &str,
        org_id: Uuid,
    ) -> DbResult<bool> {
        let res = sqlx::query(
            r#"
            DELETE FROM container_files
            WHERE id = $1 AND container_id = $2 AND org_id = $3
            "#,
        )
        .bind(file_id)
        .bind(container_id)
        .bind(org_id)
        .execute(&self.write_pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn mark_expired_idle(&self, now: chrono::DateTime<chrono::Utc>) -> DbResult<Vec<String>> {
        let rows = sqlx::query(
            r#"
            UPDATE containers
            SET status = 'expired', expires_at = $1
            WHERE status = 'active'
              AND last_active_at + (idle_ttl_secs || ' seconds')::interval < $1
            RETURNING id
            "#,
        )
        .bind(now)
        .fetch_all(&self.write_pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("id")).collect())
    }
}
