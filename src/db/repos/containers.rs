//! Repo for `containers` and `container_files` — the shell-tool
//! `/mnt/data` artifact store.
//!
//! A *container* tracks one persistent shell-tool session (Phase 1
//! scopes it to a response; Phase 4 will reuse it across responses).
//! A *container_file* tracks one file under `/mnt/data` in that
//! container, with content stored via the same `FileStorage` backend
//! abstraction the OpenAI Files API uses.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db::{
    error::DbResult,
    repos::{ResponseOwner, ResponseOwnerType},
};

/// Lifecycle states for a container row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerStatus {
    /// Session is live, can still accept new files / commands.
    Active,
    /// Idle TTL elapsed; the VM has been torn down. Existing files
    /// remain downloadable until the container is hard-deleted.
    Expired,
    /// Operator or owner deleted it. Files cascade-delete with the
    /// row (CASCADE on `container_files.container_id`).
    Deleted,
}

impl ContainerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "expired" => Some(Self::Expired),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// One persisted container row.
#[derive(Debug, Clone)]
pub struct ContainerRecord {
    /// `cntr_<32hex>` — same string the API emits.
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub status: ContainerStatus,
    /// `microsandbox`, `opensandbox`, etc. Free-form so adding a new
    /// runtime doesn't require a migration.
    pub runtime_label: String,
    /// Response this container was originally provisioned for.
    /// `None` for Phase 4 manually-created containers.
    pub source_response_id: Option<String>,
    pub idle_ttl_secs: i64,
    pub last_active_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Set when status transitions to `Expired` (so we know when the
    /// VM stopped backing the row).
    pub expires_at: Option<DateTime<Utc>>,
}

/// Fields needed to create a new container row.
#[derive(Debug, Clone)]
pub struct NewContainer {
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub status: ContainerStatus,
    pub runtime_label: String,
    pub source_response_id: Option<String>,
    pub idle_ttl_secs: i64,
    pub created_at: DateTime<Utc>,
}

impl NewContainer {
    /// Convenience for the foreground/background pipeline: bundle the
    /// principal-derived owner together with the rest of the row.
    pub fn from_owner(
        id: String,
        org_id: Uuid,
        owner: ResponseOwner,
        runtime_label: impl Into<String>,
        source_response_id: Option<String>,
        idle_ttl_secs: i64,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            org_id,
            owner_type: owner.owner_type(),
            owner_id: owner.owner_id(),
            status: ContainerStatus::Active,
            runtime_label: runtime_label.into(),
            source_response_id,
            idle_ttl_secs,
            created_at,
        }
    }
}

/// Origin of a captured container file. Mirrors
/// [`crate::api_types::responses::ContainerFileSource`] but lives in
/// the repo layer so persistence code doesn't depend on the API
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFileSourceKind {
    User,
    Assistant,
}

impl ContainerFileSourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// One persisted container_file row.
#[derive(Debug, Clone)]
pub struct ContainerFileRecord {
    /// `cfile_<32hex>` — same string the API emits.
    pub id: String,
    pub container_id: String,
    pub org_id: Uuid,
    /// Absolute path inside the container, always under `/mnt/data/`.
    pub path: String,
    pub filename: String,
    pub size_bytes: i64,
    pub content_type: Option<String>,
    pub content_hash: String,
    pub source: ContainerFileSourceKind,
    pub storage_backend: crate::models::StorageBackend,
    /// Path inside the external storage backend, when applicable.
    pub storage_path: Option<String>,
    pub source_response_id: Option<String>,
    pub source_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Fields needed to insert or overwrite a container_file row.
///
/// Repos resolve same-`(container_id, path)` collisions by replacing
/// the existing row in place (UPSERT). That keeps a re-run of the
/// same shell command from creating new rows for an idempotent
/// artifact — but also means the row's `id` may change between Phase
/// 1's in-memory view and the persisted one. Callers that emitted a
/// `cfile_…` id into an annotation should pass that same id here so
/// downloads stay stable across overwrites.
#[derive(Debug, Clone)]
pub struct NewContainerFile {
    pub id: String,
    pub container_id: String,
    pub org_id: Uuid,
    pub path: String,
    pub filename: String,
    pub size_bytes: i64,
    pub content_type: Option<String>,
    pub content_hash: String,
    pub source: ContainerFileSourceKind,
    pub storage_backend: crate::models::StorageBackend,
    /// Bytes when `storage_backend == Database`. Repos ignore when the
    /// backend is filesystem/S3 (those write through a `FileStorage`
    /// adapter before the row is inserted).
    pub file_data: Option<Vec<u8>>,
    pub storage_path: Option<String>,
    pub source_response_id: Option<String>,
    pub source_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Patch applied to a container row in [`ContainersRepo::update_within_org`].
/// Only `Some` fields are written. `expires_at` is set when status
/// transitions to `Expired`; clear it explicitly by passing `Some(None)`
/// using the `Option<Option<…>>` shape would be redundant since
/// expiry is monotonic.
#[derive(Debug, Clone, Default)]
pub struct ContainerPatch {
    pub status: Option<ContainerStatus>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ContainersRepo: Send + Sync {
    /// Insert a new container row. The caller picks the `cntr_…` id
    /// up front so it can be emitted into SSE/annotations before
    /// persistence completes.
    async fn insert(&self, input: NewContainer) -> DbResult<ContainerRecord>;

    /// Org-scoped fetch by ID. Same enumeration-resistance pattern as
    /// `ResponsesRepo`: returns `None` for missing-or-wrong-org without
    /// distinguishing the two cases.
    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<ContainerRecord>>;

    /// Upsert a container_file row. If `(container_id, path)` is
    /// already present, replaces the existing row (path-level
    /// idempotency for overwrites) while keeping the row's `id`
    /// stable so any annotation citing the prior version still
    /// resolves to the latest bytes.
    async fn upsert_file(&self, input: NewContainerFile) -> DbResult<ContainerFileRecord>;

    /// Org-scoped fetch of one container_file by id. Joins through the
    /// container to enforce the org boundary in a single query.
    async fn get_file_by_id_and_org(
        &self,
        file_id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<ContainerFileRecord>>;

    /// Read the raw bytes for a container_file when its
    /// `storage_backend == Database`. For external backends the
    /// caller resolves through the `FileStorage` adapter using
    /// `record.storage_path`.
    async fn read_file_data(&self, file_id: &str, org_id: Uuid) -> DbResult<Option<Vec<u8>>>;

    /// List files inside a container, newest first. The container's
    /// org gate is applied so a cross-tenant `container_id` returns an
    /// empty list. Phase 3 returns a simple slice (caller-supplied
    /// `limit`, default 100); Phase 4 will swap in cursor pagination.
    async fn list_files_by_container(
        &self,
        container_id: &str,
        org_id: Uuid,
        limit: i64,
    ) -> DbResult<Vec<ContainerFileRecord>>;

    /// All files for a container, regardless of `org_id`. Used by the
    /// reattach path which has already validated org access via the
    /// `containers` row lookup. Files come back newest-first.
    async fn list_files_for_replay(&self, container_id: &str)
    -> DbResult<Vec<ContainerFileRecord>>;

    /// Read raw bytes by `(container_id, file_id)` without an org
    /// check. Used by reattach to replay files into a fresh VM.
    async fn read_file_data_for_replay(
        &self,
        container_id: &str,
        file_id: &str,
    ) -> DbResult<Option<Vec<u8>>>;

    /// Patch lifecycle fields on a container row, org-scoped.
    /// Returns the updated record, or `None` when nothing matched.
    async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: ContainerPatch,
    ) -> DbResult<Option<ContainerRecord>>;

    /// Atomically mark all `active` containers whose
    /// `last_active_at + idle_ttl_secs` is older than `now` as
    /// `expired`. Returns the list of ids that just transitioned so
    /// the caller can evict matching entries from the in-memory
    /// session registry.
    async fn mark_expired_idle(&self, now: DateTime<Utc>) -> DbResult<Vec<String>>;
}
