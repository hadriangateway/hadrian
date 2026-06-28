//! Service wrapper around the `videos` repository.
//!
//! Hadrian persists only a routing map + last-known snapshot for video jobs
//! (proxy-on-read); this thin store handles row creation, snapshot refresh,
//! org-scoped lookup/delete, owner-scoped listing, and retention pruning.

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::db::{
    DbPool, DbResult,
    repos::{
        NewVideo, NewVideoCharacter, ResponseOwnerType, VideoCharacterRecord, VideoListOrder,
        VideoPatch, VideoRecord,
    },
};

/// Thin service over [`crate::db::repos::VideosRepo`].
#[derive(Clone)]
pub struct VideoStore {
    db: Arc<DbPool>,
    retention: StdDuration,
}

impl VideoStore {
    pub fn new(db: Arc<DbPool>, retention: StdDuration) -> Self {
        Self { db, retention }
    }

    /// Retention deadline for a row created at `from`.
    pub fn retention_expires_at(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        from + Duration::from_std(self.retention).unwrap_or_else(|_| Duration::seconds(86_400))
    }

    /// Persist a new video job.
    pub async fn create(&self, input: NewVideo) -> DbResult<VideoRecord> {
        self.db.videos().insert(input).await
    }

    /// Fetch a video job by id, scoped to the caller's org.
    pub async fn get(&self, id: &str, org_id: Uuid) -> DbResult<Option<VideoRecord>> {
        self.db.videos().get_by_id_and_org(id, org_id).await
    }

    /// Refresh the stored snapshot/status after a live upstream lookup.
    pub async fn refresh(
        &self,
        id: &str,
        org_id: Uuid,
        patch: VideoPatch,
    ) -> DbResult<Option<VideoRecord>> {
        self.db.videos().update_within_org(id, org_id, patch).await
    }

    /// Delete a video job within the org. Idempotent.
    pub async fn delete(&self, id: &str, org_id: Uuid) -> DbResult<bool> {
        self.db.videos().delete_by_id_and_org(id, org_id).await
    }

    /// List video jobs owned by a principal scope, keyset-paginated.
    pub async fn list(
        &self,
        owner_type: ResponseOwnerType,
        owner_id: Uuid,
        org_id: Uuid,
        after: Option<String>,
        limit: i64,
        order: VideoListOrder,
    ) -> DbResult<(Vec<VideoRecord>, bool)> {
        self.db
            .videos()
            .list_for_owner(owner_type, owner_id, org_id, after, limit, order)
            .await
    }

    /// Delete jobs past their retention window.
    pub async fn prune_expired(&self, before: DateTime<Utc>) -> DbResult<u64> {
        self.db.videos().delete_expired(before).await
    }

    /// Persist a new character.
    pub async fn create_character(
        &self,
        input: NewVideoCharacter,
    ) -> DbResult<VideoCharacterRecord> {
        self.db.videos().insert_character(input).await
    }

    /// Fetch a character by id, scoped to the caller's org.
    pub async fn get_character(
        &self,
        id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<VideoCharacterRecord>> {
        self.db
            .videos()
            .get_character_by_id_and_org(id, org_id)
            .await
    }
}
