//! Persistence repo for the Videos API.
//!
//! Hadrian is a proxy-on-read gateway for video generation: it stores a
//! `video_id -> provider/owner` mapping plus the last-known job snapshot so the
//! bare-id endpoints (`GET`/`DELETE`/`content`) can route back to the
//! originating provider, then proxies live for fresh status/bytes. Ownership and
//! tenant scoping mirror the `responses` repo via [`ResponseOwnerType`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::ResponseOwnerType;
use crate::db::error::DbResult;

/// Sort order for listing videos (maps to OpenAI's `order` query param).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoListOrder {
    /// Newest first (OpenAI default).
    #[default]
    Desc,
    /// Oldest first.
    Asc,
}

/// A persisted video-generation job.
///
/// `error` and `snapshot` are opaque JSON; `snapshot` holds the full last-known
/// `Video` object so list/retrieve can re-serve it verbatim.
#[derive(Debug, Clone)]
pub struct VideoRecord {
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub status: String,
    pub model: String,
    pub provider: Option<String>,
    pub prompt: Option<String>,
    pub size: Option<String>,
    pub seconds: Option<String>,
    pub progress: Option<i32>,
    pub remixed_from_video_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error: Option<Value>,
    pub snapshot: Value,
    pub updated_at: DateTime<Utc>,
    pub retention_expires_at: DateTime<Utc>,
}

/// Insertion payload for a new video job.
#[derive(Debug, Clone)]
pub struct NewVideo {
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub status: String,
    pub model: String,
    pub provider: Option<String>,
    pub prompt: Option<String>,
    pub size: Option<String>,
    pub seconds: Option<String>,
    pub progress: Option<i32>,
    pub remixed_from_video_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error: Option<Value>,
    pub snapshot: Value,
    pub retention_expires_at: DateTime<Utc>,
}

/// Patch applied when a live `get_video` refreshes the stored snapshot.
#[derive(Debug, Clone)]
pub struct VideoPatch {
    pub status: String,
    pub progress: Option<i32>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error: Option<Value>,
    /// The full refreshed `Video` object.
    pub snapshot: Value,
}

/// A persisted character created from a reference video.
#[derive(Debug, Clone)]
pub struct VideoCharacterRecord {
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub name: String,
    pub snapshot: Value,
    pub created_at: DateTime<Utc>,
}

/// Insertion payload for a new character.
#[derive(Debug, Clone)]
pub struct NewVideoCharacter {
    pub id: String,
    pub org_id: Uuid,
    pub owner_type: ResponseOwnerType,
    pub owner_id: Uuid,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub name: String,
    pub snapshot: Value,
    pub created_at: DateTime<Utc>,
}

/// Repository for persisted video jobs + characters.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait VideosRepo: Send + Sync {
    /// Persist a new video job.
    async fn insert(&self, input: NewVideo) -> DbResult<VideoRecord>;

    /// Fetch a video by id, scoped to the caller's org (cascade through
    /// owner types). Returns `None` for a wrong-org id (no enumeration).
    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<VideoRecord>>;

    /// Refresh the stored snapshot/status. Returns the updated record, or
    /// `None` if the id is not in the org.
    async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: VideoPatch,
    ) -> DbResult<Option<VideoRecord>>;

    /// Delete a video by id within the org. Idempotent.
    async fn delete_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<bool>;

    /// List videos owned by a specific principal scope within the org,
    /// keyset-paginated by `(created_at, id)`. `after` is a video id
    /// (OpenAI's pagination cursor). Returns `(items, has_more)`.
    async fn list_for_owner(
        &self,
        owner_type: ResponseOwnerType,
        owner_id: Uuid,
        org_id: Uuid,
        after: Option<String>,
        limit: i64,
        order: VideoListOrder,
    ) -> DbResult<(Vec<VideoRecord>, bool)>;

    /// Delete jobs whose retention window has elapsed. Returns the count.
    async fn delete_expired(&self, before: DateTime<Utc>) -> DbResult<u64>;

    /// Persist a new character.
    async fn insert_character(&self, input: NewVideoCharacter) -> DbResult<VideoCharacterRecord>;

    /// Fetch a character by id, scoped to the caller's org.
    async fn get_character_by_id_and_org(
        &self,
        id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<VideoCharacterRecord>>;
}
