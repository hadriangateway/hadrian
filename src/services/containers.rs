//! Service layer for the container + container_files persistence
//! layer. Sits between the in-memory `ContainerSession` and the
//! database repos.
//!
//! Write-through every captured file and expose read paths for the
//! `/v1/containers/*` GET endpoints. Where the bytes physically live is
//! governed by `[storage.container_files]`: the `database` backend keeps
//! them inline in `container_files.file_data` (simple, scales poorly),
//! while `filesystem` / `s3` offload the bytes to a [`FileStorage`]
//! backend and persist only a `storage_path` reference on the row. The
//! row's `storage_backend` column records which path produced it so reads
//! route correctly even if the operator later switches backends.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, error};
use uuid::Uuid;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

use crate::{
    api_types::responses::{ContainerFileRef, ContainerFileSource},
    db::{
        DbError, DbPool,
        repos::{
            ContainerFileRecord, ContainerFileSourceKind, ContainerRecord, ContainersRepo,
            NewContainer, NewContainerFile, ResponseOwner, truncate_to_millis,
        },
    },
    models::StorageBackend,
    services::{FileStorage, FileStorageError},
};

/// Errors emitted by `ContainersService`. Distinct from `DbError` so
/// route handlers can map cleanly to HTTP status codes.
#[derive(Debug, Error)]
pub enum ContainersServiceError {
    #[error("container not found")]
    NotFound,
    /// Container row exists but is no longer reusable. The caller
    /// should surface this as `410 Gone` (matching OpenAI's
    /// "expired containers cannot be reactivated" semantics) or
    /// silently fall back to a fresh container, depending on whether
    /// the reuse was implicit or explicit.
    #[error("container '{0}' has expired")]
    Expired(String),
    #[error("database error: {0}")]
    Db(String),
    #[error("file content unavailable: {0}")]
    ContentUnavailable(String),
}

impl ContainersServiceError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Expired(_) => "container_expired",
            Self::Db(_) => "internal_error",
            Self::ContentUnavailable(_) => "file_content_unavailable",
        }
    }
}

impl From<DbError> for ContainersServiceError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::NotFound => Self::NotFound,
            other => Self::Db(other.to_string()),
        }
    }
}

pub type ContainersServiceResult<T> = Result<T, ContainersServiceError>;

/// One file ready to be persisted into the `container_files` table.
/// Mirrors `ContainerFileRef` plus the raw content + provenance.
#[derive(Debug, Clone)]
pub struct PersistFileInput {
    /// Stable id the in-memory session already published in
    /// annotations + SSE events. Service uses it as the new row's
    /// PK on first insert; on overwrite the repo's UPSERT keeps the
    /// existing row's id so the published id stays valid.
    pub file_id: String,
    pub path: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub source: ContainerFileSource,
    /// File bytes. Routed through the configured `[storage.container_files]`
    /// [`FileStorage`] backend: stored inline in the row for the `database`
    /// backend, or offloaded to `filesystem` / `s3` with only a
    /// `storage_path` kept on the row.
    pub content: Bytes,
    pub content_hash_hex: String,
    pub source_response_id: Option<String>,
    pub source_call_id: Option<String>,
}

#[derive(Clone)]
pub struct ContainersService {
    db: Arc<DbPool>,
    /// Where container-file bytes live, from `[storage.container_files]`.
    /// The `database` backend is a thin shim that stores nothing here and
    /// signals the row to keep bytes inline; `filesystem` / `s3` persist the
    /// bytes and return a `storage_path`.
    file_storage: Arc<dyn FileStorage>,
    /// The configured backend, recorded on every row this service writes so
    /// reads can route without re-deriving it. Derived from
    /// `file_storage.backend_name()` at construction.
    storage_backend: StorageBackend,
}

impl ContainersService {
    pub fn new(db: Arc<DbPool>, file_storage: Arc<dyn FileStorage>) -> Self {
        let storage_backend = file_storage
            .backend_name()
            .parse()
            .unwrap_or(StorageBackend::Database);
        Self {
            db,
            file_storage,
            storage_backend,
        }
    }

    fn repo(&self) -> Arc<dyn ContainersRepo> {
        self.db.containers()
    }

    /// Persist `content` to the configured backend and return the column
    /// values for the new row: the recorded `storage_backend`, the inline
    /// `file_data` (only for the `database` backend), and the external
    /// `storage_path` (only for `filesystem` / `s3`).
    ///
    /// The two are mutually exclusive — the `database` shim returns `None`
    /// from `store()`, every external backend returns `Some(path)`.
    async fn stage_content(
        &self,
        file_id: &str,
        content: &[u8],
    ) -> ContainersServiceResult<(StorageBackend, Option<Vec<u8>>, Option<String>)> {
        let storage_path = self
            .file_storage
            .store(file_id, content)
            .await
            .map_err(|e| {
                ContainersServiceError::ContentUnavailable(format!(
                    "failed to store container file via {} backend: {e}",
                    self.storage_backend.as_str()
                ))
            })?;
        match storage_path {
            Some(path) => Ok((self.storage_backend, None, Some(path))),
            None => Ok((StorageBackend::Database, Some(content.to_vec()), None)),
        }
    }

    /// Fetch the bytes for an external-backend (`filesystem` / `s3`) record.
    /// Returns `Ok(None)` when the underlying object is gone so callers can
    /// mirror the "metadata-only row" handling used for dropped DB content.
    async fn read_external(
        &self,
        record: &ContainerFileRecord,
    ) -> ContainersServiceResult<Option<Vec<u8>>> {
        // Prefer the stored path; fall back to the file id so a backend that
        // derives keys from ids (S3 without an explicit path) still resolves.
        let key = record.storage_path.as_deref().unwrap_or(&record.id);
        match self.file_storage.retrieve(key).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(FileStorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(ContainersServiceError::ContentUnavailable(e.to_string())),
        }
    }

    /// Insert a container row for a freshly-started session. Caller
    /// supplies the `cntr_…` id (already emitted in `output_files`
    /// references / `container_file_citation` annotations) and the
    /// owner derived from the request principal.
    pub async fn provision(
        &self,
        container_id: String,
        org_id: Uuid,
        owner: ResponseOwner,
        runtime_label: impl Into<String>,
        source_response_id: Option<String>,
        idle_ttl_secs: i64,
    ) -> ContainersServiceResult<ContainerRecord> {
        let created_at = truncate_to_millis(Utc::now());
        let new = NewContainer::from_owner(
            container_id,
            org_id,
            owner,
            runtime_label,
            source_response_id,
            idle_ttl_secs,
            created_at,
        );
        let record = self.repo().insert(new).await?;
        debug!(
            stage = "container_provisioned",
            container_id = %record.id,
            org_id = %record.org_id,
            "Inserted containers row"
        );
        Ok(record)
    }

    /// `POST /v1/containers` — create an unattached container row.
    /// The VM is not booted yet; the row's `network_policy_json` /
    /// `memory_limit_mb` / `skill_ids_json` are picked up when the
    /// first response references this container.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_explicit(
        &self,
        container_id: String,
        org_id: Uuid,
        owner: ResponseOwner,
        runtime_label: impl Into<String>,
        idle_ttl_secs: i64,
        name: Option<String>,
        memory_limit_mb: Option<i64>,
        network_policy_json: Option<String>,
        skill_ids_json: Option<String>,
    ) -> ContainersServiceResult<ContainerRecord> {
        let created_at = truncate_to_millis(Utc::now());
        let mut new = NewContainer::from_owner(
            container_id,
            org_id,
            owner,
            runtime_label,
            None,
            idle_ttl_secs,
            created_at,
        );
        new.name = name;
        new.memory_limit_mb = memory_limit_mb;
        new.network_policy_json = network_policy_json;
        new.skill_ids_json = skill_ids_json;
        let record = self.repo().insert(new).await?;
        debug!(
            stage = "container_created",
            container_id = %record.id,
            org_id = %record.org_id,
            "Created container via POST /v1/containers"
        );
        Ok(record)
    }

    /// Persist a batch of captured files. Errors on individual rows
    /// are logged + skipped so a single bad row doesn't lose the
    /// rest. Returns the canonical records (with the repo's view of
    /// the id, after any UPSERT collision).
    pub async fn persist_files(
        &self,
        container_id: &str,
        org_id: Uuid,
        files: Vec<PersistFileInput>,
    ) -> ContainersServiceResult<Vec<ContainerFileRecord>> {
        let repo = self.repo();
        let mut out = Vec::with_capacity(files.len());
        for input in files {
            let (storage_backend, file_data, storage_path) =
                match self.stage_content(&input.file_id, &input.content).await {
                    Ok(staged) => staged,
                    Err(e) => {
                        error!(
                            stage = "container_file_store_failed",
                            container_id,
                            path = %input.path,
                            file_id = %input.file_id,
                            error = %e,
                            "Failed to store container file bytes; skipping"
                        );
                        continue;
                    }
                };
            let new = NewContainerFile {
                id: input.file_id.clone(),
                container_id: container_id.to_string(),
                org_id,
                path: input.path.clone(),
                filename: input.filename,
                size_bytes: input.content.len() as i64,
                content_type: input.content_type,
                content_hash: input.content_hash_hex,
                source: match input.source {
                    ContainerFileSource::User => ContainerFileSourceKind::User,
                    ContainerFileSource::Assistant => ContainerFileSourceKind::Assistant,
                },
                storage_backend,
                file_data,
                storage_path,
                source_response_id: input.source_response_id,
                source_call_id: input.source_call_id,
                created_at: truncate_to_millis(Utc::now()),
            };
            match repo.upsert_file(new).await {
                Ok(rec) => out.push(rec),
                Err(e) => {
                    error!(
                        stage = "container_file_upsert_failed",
                        container_id,
                        path = %input.path,
                        file_id = %input.file_id,
                        error = %e,
                        "Failed to persist container file; skipping"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Org-scoped container lookup.
    pub async fn get_container(
        &self,
        id: &str,
        org_id: Uuid,
    ) -> ContainersServiceResult<ContainerRecord> {
        self.repo()
            .get_by_id_and_org(id, org_id)
            .await?
            .ok_or(ContainersServiceError::NotFound)
    }

    /// List containers in an org, newest first. Fetches `limit + 1`
    /// rows so the handler can derive `has_more` without a second
    /// query.
    pub async fn list_containers(
        &self,
        org_id: Uuid,
        limit: i64,
        after: Option<&str>,
    ) -> ContainersServiceResult<Vec<ContainerRecord>> {
        Ok(self.repo().list_by_org(org_id, limit, after).await?)
    }

    /// Get-or-create a container row by id. Used at attach time: the
    /// pipeline pre-picks a `cntr_…` (either from the
    /// `previous_response_id` chain or freshly generated) and asks
    /// the service to materialise the row.
    ///
    /// Returns `Err(Expired)` when the row exists but isn't reusable
    /// (`expired` or `deleted`). Returns the resolved record
    /// otherwise.
    pub async fn ensure_container(
        &self,
        container_id: String,
        org_id: Uuid,
        owner: ResponseOwner,
        runtime_label: impl Into<String>,
        source_response_id: Option<String>,
        idle_ttl_secs: i64,
    ) -> ContainersServiceResult<ContainerRecord> {
        if let Some(existing) = self.repo().get_by_id_and_org(&container_id, org_id).await? {
            return match existing.status {
                crate::db::repos::ContainerStatus::Active => Ok(existing),
                crate::db::repos::ContainerStatus::Expired => {
                    Err(ContainersServiceError::Expired(container_id))
                }
                crate::db::repos::ContainerStatus::Deleted => Err(ContainersServiceError::NotFound),
            };
        }
        self.provision(
            container_id,
            org_id,
            owner,
            runtime_label,
            source_response_id,
            idle_ttl_secs,
        )
        .await
    }

    /// Org-scoped file metadata lookup.
    pub async fn get_file(
        &self,
        file_id: &str,
        org_id: Uuid,
    ) -> ContainersServiceResult<ContainerFileRecord> {
        self.repo()
            .get_file_by_id_and_org(file_id, org_id)
            .await?
            .ok_or(ContainersServiceError::NotFound)
    }

    /// Org-scoped file content read. Routes by the row's recorded
    /// `storage_backend`: `database` reads the inline `file_data` column,
    /// `filesystem` / `s3` fetch the bytes from the configured
    /// [`FileStorage`] backend by `storage_path`.
    pub async fn read_content(
        &self,
        file_id: &str,
        org_id: Uuid,
    ) -> ContainersServiceResult<Vec<u8>> {
        let record = self.get_file(file_id, org_id).await?;
        match record.storage_backend {
            StorageBackend::Database => self
                .repo()
                .read_file_data(file_id, org_id)
                .await?
                .ok_or_else(|| {
                    ContainersServiceError::ContentUnavailable(
                        "row exists but file_data is NULL".into(),
                    )
                }),
            StorageBackend::Filesystem | StorageBackend::S3 => {
                self.read_external(&record).await?.ok_or_else(|| {
                    ContainersServiceError::ContentUnavailable(format!(
                        "{} object for container file is missing",
                        record.storage_backend.as_str()
                    ))
                })
            }
        }
    }

    /// Org-scoped listing inside one container.
    pub async fn list_files(
        &self,
        container_id: &str,
        org_id: Uuid,
        limit: i64,
    ) -> ContainersServiceResult<Vec<ContainerFileRecord>> {
        Ok(self
            .repo()
            .list_files_by_container(container_id, org_id, limit)
            .await?)
    }

    /// Used by `ContainerSession::replay_from_db` to enumerate every
    /// file under a container at reattach time. Skips the org gate
    /// because the caller already validated org access through the
    /// `containers` row.
    pub async fn list_files_for_replay(
        &self,
        container_id: &str,
    ) -> ContainersServiceResult<Vec<ContainerFileRecord>> {
        Ok(self.repo().list_files_for_replay(container_id).await?)
    }

    /// Used by `ContainerSession::replay_from_db` to fetch raw bytes
    /// at reattach time. Same rationale as `list_files_for_replay` —
    /// skips the org gate because the caller has already authorized
    /// via the parent container row. Takes the already-fetched record so
    /// it can route by `storage_backend` without a second metadata query.
    pub async fn read_content_for_replay(
        &self,
        record: &ContainerFileRecord,
    ) -> ContainersServiceResult<Option<Vec<u8>>> {
        match record.storage_backend {
            StorageBackend::Database => Ok(self
                .repo()
                .read_file_data_for_replay(&record.container_id, &record.id)
                .await?),
            StorageBackend::Filesystem | StorageBackend::S3 => self.read_external(record).await,
        }
    }

    /// Patch lifecycle fields on a container row, org-scoped.
    pub async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: crate::db::repos::ContainerPatch,
    ) -> ContainersServiceResult<Option<ContainerRecord>> {
        Ok(self.repo().update_within_org(id, org_id, patch).await?)
    }

    /// Touch `last_active_at` so the reaper doesn't expire a busy
    /// container and the containers API surfaces a moving `expires_at`.
    /// Called by `ContainerSession::exec` after every successful shell
    /// command. Truncates to millis to match every other container
    /// timestamp write (cursor-comparison safety).
    pub async fn touch_last_active(&self, id: &str, org_id: Uuid) -> ContainersServiceResult<()> {
        let patch = crate::db::repos::ContainerPatch {
            last_active_at: Some(truncate_to_millis(Utc::now())),
            ..Default::default()
        };
        self.update_within_org(id, org_id, patch).await?;
        Ok(())
    }

    /// Atomically mark idle containers as `expired`. Called by the
    /// reaper job. Returns the ids that just transitioned so the
    /// caller can drop their entries from the in-memory registry.
    pub async fn mark_expired_idle(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> ContainersServiceResult<Vec<String>> {
        Ok(self.repo().mark_expired_idle(now).await?)
    }

    /// Org-scoped delete of one container_file row. Returns
    /// `NotFound` when the row doesn't exist (or belongs to a
    /// different container / org).
    pub async fn delete_file(
        &self,
        container_id: &str,
        file_id: &str,
        org_id: Uuid,
    ) -> ContainersServiceResult<()> {
        // Capture the storage location before dropping the row so we can
        // clean up an external object. Tolerate a missing record here — the
        // delete below is the authoritative not-found check.
        let record = self.get_file(file_id, org_id).await.ok();
        let removed = self
            .repo()
            .delete_file_by_id_and_org(file_id, container_id, org_id)
            .await?;
        if !removed {
            return Err(ContainersServiceError::NotFound);
        }
        // Best-effort external cleanup. The row is already gone, so a storage
        // hiccup orphans bytes rather than failing the API call — log and move
        // on instead of resurrecting a deleted file.
        if let Some(record) = record
            && matches!(
                record.storage_backend,
                StorageBackend::Filesystem | StorageBackend::S3
            )
        {
            let key = record.storage_path.as_deref().unwrap_or(&record.id);
            if let Err(e) = self.file_storage.delete(key).await {
                error!(
                    stage = "container_file_storage_delete_failed",
                    container_id,
                    file_id,
                    backend = record.storage_backend.as_str(),
                    error = %e,
                    "Deleted container_files row but failed to remove backing object"
                );
            }
        }
        Ok(())
    }

    /// Upsert one file under an existing container. Used by
    /// `POST /v1/containers/{id}/files`. Returns the canonical record
    /// (the repo's view of the row, post-conflict resolution).
    #[allow(clippy::too_many_arguments)] // each arg is load-bearing for the persisted row
    pub async fn upload_file(
        &self,
        container_id: &str,
        org_id: Uuid,
        path: String,
        filename: String,
        content_type: Option<String>,
        content: Vec<u8>,
        source: ContainerFileSource,
        source_response_id: Option<String>,
        source_call_id: Option<String>,
    ) -> ContainersServiceResult<ContainerFileRecord> {
        // Compute a sha256 of the bytes; this is what the
        // capture/exec paths persist so an uploaded file behaves the
        // same as a captured one downstream.
        let content_hash_hex = sha256_hex(&content);
        let file_id = format!("cfile_{}", uuid::Uuid::new_v4().simple());
        let now = truncate_to_millis(Utc::now());
        let (storage_backend, file_data, storage_path) =
            self.stage_content(&file_id, &content).await?;
        let new = NewContainerFile {
            id: file_id,
            container_id: container_id.to_string(),
            org_id,
            path,
            filename,
            size_bytes: content.len() as i64,
            content_type,
            content_hash: content_hash_hex,
            source: match source {
                ContainerFileSource::User => ContainerFileSourceKind::User,
                ContainerFileSource::Assistant => ContainerFileSourceKind::Assistant,
            },
            storage_backend,
            file_data,
            storage_path,
            source_response_id,
            source_call_id,
            created_at: now,
        };
        Ok(self.repo().upsert_file(new).await?)
    }

    /// Stamp the `container_id` column on a `responses` row so the
    /// next request that chains via `previous_response_id` can find
    /// the container to reattach to. No-ops for `response_id == None`
    /// (in-memory / no-persistence runs) — the row doesn't exist.
    pub async fn link_response_to_container(
        &self,
        response_id: &str,
        container_id: &str,
        org_id: Uuid,
    ) -> ContainersServiceResult<()> {
        let patch = crate::db::repos::ResponseCompletion {
            container_id: Some(container_id.to_string()),
            ..Default::default()
        };
        self.db
            .responses()
            .update_within_org(response_id, org_id, patch)
            .await
            .map_err(|e| ContainersServiceError::Db(e.to_string()))?;
        Ok(())
    }
}

/// Render a `ContainerFileRecord` into the API-facing
/// `ContainerFileRef` shape so handlers can return one type from both
/// in-memory + persisted code paths.
pub fn record_to_api_ref(record: &ContainerFileRecord) -> ContainerFileRef {
    let source = match record.source {
        ContainerFileSourceKind::User => ContainerFileSource::User,
        ContainerFileSourceKind::Assistant => ContainerFileSource::Assistant,
    };
    ContainerFileRef {
        container_id: record.container_id.clone(),
        file_id: record.id.clone(),
        filename: record.filename.clone(),
        path: record.path.clone(),
        bytes: record.size_bytes.max(0) as u64,
        content_type: record.content_type.clone(),
        source,
    }
}

#[cfg(all(test, feature = "database-sqlite", feature = "server"))]
mod tests {
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::{
        config::FilesystemStorageConfig,
        db::{DbPool, repos::ResponseOwner, tests::harness},
        models::CreateOrganization,
        services::{DatabaseFileStorage, FilesystemFileStorage},
    };

    /// Stand up an in-memory SQLite DbPool with a seeded org + an active
    /// container, ready for file persistence assertions.
    async fn seed() -> (Arc<DbPool>, Uuid, String) {
        let pool = harness::create_sqlite_pool().await;
        harness::run_sqlite_migrations(&pool).await;
        let db = Arc::new(DbPool::from_sqlite(pool));

        let org = db
            .organizations()
            .create(CreateOrganization {
                slug: "acme".to_string(),
                name: "Acme".to_string(),
            })
            .await
            .expect("create org");

        let container_id = format!("cntr_{}", Uuid::new_v4().simple());
        db.containers()
            .insert(NewContainer::from_owner(
                container_id.clone(),
                org.id,
                ResponseOwner::Organization(org.id),
                "test",
                None,
                1200,
                truncate_to_millis(Utc::now()),
            ))
            .await
            .expect("insert container");

        (db, org.id, container_id)
    }

    async fn upload(
        svc: &ContainersService,
        container_id: &str,
        org_id: Uuid,
        content: &[u8],
    ) -> ContainerFileRecord {
        svc.upload_file(
            container_id,
            org_id,
            "/mnt/data/out.bin".to_string(),
            "out.bin".to_string(),
            Some("application/octet-stream".to_string()),
            content.to_vec(),
            ContainerFileSource::Assistant,
            None,
            None,
        )
        .await
        .expect("upload_file")
    }

    #[tokio::test]
    async fn database_backend_stores_inline() {
        let (db, org_id, container_id) = seed().await;
        let svc =
            ContainersService::new(db.clone(), Arc::new(DatabaseFileStorage::new(db.clone())));

        let rec = upload(&svc, &container_id, org_id, b"hello db").await;
        assert_eq!(rec.storage_backend, StorageBackend::Database);
        assert!(
            rec.storage_path.is_none(),
            "DB backend keeps no storage_path"
        );

        // Bytes live inline in the row and read back through the service.
        let inline = db
            .containers()
            .read_file_data(&rec.id, org_id)
            .await
            .unwrap();
        assert_eq!(inline.as_deref(), Some(b"hello db".as_ref()));
        assert_eq!(
            svc.read_content(&rec.id, org_id).await.unwrap(),
            b"hello db"
        );
    }

    #[tokio::test]
    async fn filesystem_backend_offloads_bytes() {
        let (db, org_id, container_id) = seed().await;
        let dir = TempDir::new().unwrap();
        let storage = FilesystemFileStorage::new(FilesystemStorageConfig {
            path: dir.path().to_string_lossy().to_string(),
            create_dir: true,
            file_mode: 0o600,
            dir_mode: 0o700,
        })
        .unwrap();
        let svc = ContainersService::new(db.clone(), Arc::new(storage));

        let rec = upload(&svc, &container_id, org_id, b"hello fs").await;

        // Bytes are offloaded: backend recorded, path set, nothing inline.
        assert_eq!(rec.storage_backend, StorageBackend::Filesystem);
        let path = rec
            .storage_path
            .as_deref()
            .expect("filesystem storage_path");
        assert!(
            std::path::Path::new(path).exists(),
            "object written to disk"
        );
        let inline = db
            .containers()
            .read_file_data(&rec.id, org_id)
            .await
            .unwrap();
        assert!(inline.is_none(), "filesystem backend leaves file_data NULL");

        // Round-trips through the read path.
        assert_eq!(
            svc.read_content(&rec.id, org_id).await.unwrap(),
            b"hello fs"
        );

        // Replay path resolves the same bytes from the record.
        let replayed = svc.read_content_for_replay(&rec).await.unwrap();
        assert_eq!(replayed.as_deref(), Some(b"hello fs".as_ref()));

        // Delete removes both the row and the backing object.
        svc.delete_file(&container_id, &rec.id, org_id)
            .await
            .unwrap();
        assert!(
            !std::path::Path::new(path).exists(),
            "object removed on delete"
        );
        assert!(matches!(
            svc.read_content(&rec.id, org_id).await,
            Err(ContainersServiceError::NotFound)
        ));
    }

    #[tokio::test]
    async fn missing_filesystem_object_surfaces_unavailable() {
        let (db, org_id, container_id) = seed().await;
        let dir = TempDir::new().unwrap();
        let storage = FilesystemFileStorage::new(FilesystemStorageConfig {
            path: dir.path().to_string_lossy().to_string(),
            create_dir: true,
            file_mode: 0o600,
            dir_mode: 0o700,
        })
        .unwrap();
        let svc = ContainersService::new(db.clone(), Arc::new(storage));

        let rec = upload(&svc, &container_id, org_id, b"transient").await;
        // Remove the backing object out from under the row.
        std::fs::remove_file(rec.storage_path.as_deref().unwrap()).unwrap();

        assert!(matches!(
            svc.read_content(&rec.id, org_id).await,
            Err(ContainersServiceError::ContentUnavailable(_))
        ));
        // Replay tolerates the gap (metadata-only) rather than erroring.
        assert!(svc.read_content_for_replay(&rec).await.unwrap().is_none());
    }
}
