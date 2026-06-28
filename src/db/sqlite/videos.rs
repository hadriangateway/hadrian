//! SQLite implementation of [`VideosRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::{
    backend::{Pool, RowExt, query},
    common::parse_uuid,
};
use crate::db::{
    error::{DbError, DbResult},
    repos::{
        NewVideo, NewVideoCharacter, ResponseOwnerType, VideoCharacterRecord, VideoListOrder,
        VideoPatch, VideoRecord, VideosRepo, truncate_to_millis,
    },
};

/// Org-scope filter for reads/updates/deletes against `videos`. Mirrors
/// the `responses` filter; each `?` binds the caller's org id (five times).
const ORG_SCOPE_FILTER: &str = r#"
    AND (
        (videos.owner_type = 'organization' AND videos.owner_id = ?)
        OR (videos.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = videos.owner_id AND t.org_id = ?
        ))
        OR (videos.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = videos.owner_id AND pr.org_id = ?
        ))
        OR (videos.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = videos.owner_id AND om.org_id = ?
        ))
        OR (videos.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = videos.owner_id AND sa.org_id = ?
        ))
    )
"#;

/// Same cascade for `video_characters`.
const CHARACTER_ORG_SCOPE_FILTER: &str = r#"
    AND (
        (video_characters.owner_type = 'organization' AND video_characters.owner_id = ?)
        OR (video_characters.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = video_characters.owner_id AND t.org_id = ?
        ))
        OR (video_characters.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = video_characters.owner_id AND pr.org_id = ?
        ))
        OR (video_characters.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = video_characters.owner_id AND om.org_id = ?
        ))
        OR (video_characters.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = video_characters.owner_id AND sa.org_id = ?
        ))
    )
"#;

const ORG_SCOPE_BINDS: usize = 5;

const VIDEO_COLUMNS: &str = "id, org_id, owner_type, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    status, model, provider, prompt, size, seconds, progress, remixed_from_video_id, \
    created_at, completed_at, expires_at, error, snapshot, updated_at, retention_expires_at";

const CHARACTER_COLUMNS: &str = "id, org_id, owner_type, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    provider, model, name, snapshot, created_at";

pub struct SqliteVideosRepo {
    pool: Pool,
}

impl SqliteVideosRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn parse_owner_type(s: &str) -> DbResult<ResponseOwnerType> {
    ResponseOwnerType::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown video owner_type: {s}")))
}

fn parse_optional_uuid(s: Option<String>) -> DbResult<Option<Uuid>> {
    s.map(|s| parse_uuid(&s)).transpose()
}

fn parse_json(s: Option<String>) -> DbResult<Option<Value>> {
    match s {
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        None => Ok(None),
    }
}

fn row_to_video(row: &super::backend::Row) -> DbResult<VideoRecord> {
    let snapshot: String = row.col("snapshot");
    Ok(VideoRecord {
        id: row.col("id"),
        org_id: parse_uuid(&row.col::<String>("org_id"))?,
        owner_type: parse_owner_type(&row.col::<String>("owner_type"))?,
        owner_id: parse_uuid(&row.col::<String>("owner_id"))?,
        project_id: parse_optional_uuid(row.col("project_id"))?,
        user_id: parse_optional_uuid(row.col("user_id"))?,
        api_key_id: parse_optional_uuid(row.col("api_key_id"))?,
        service_account_id: parse_optional_uuid(row.col("service_account_id"))?,
        status: row.col("status"),
        model: row.col("model"),
        provider: row.col("provider"),
        prompt: row.col("prompt"),
        size: row.col("size"),
        seconds: row.col("seconds"),
        progress: row.col::<Option<i64>>("progress").map(|v| v as i32),
        remixed_from_video_id: row.col("remixed_from_video_id"),
        created_at: row.col("created_at"),
        completed_at: row.col("completed_at"),
        expires_at: row.col("expires_at"),
        error: parse_json(row.col("error"))?,
        snapshot: serde_json::from_str(&snapshot)?,
        updated_at: row.col("updated_at"),
        retention_expires_at: row.col("retention_expires_at"),
    })
}

fn row_to_character(row: &super::backend::Row) -> DbResult<VideoCharacterRecord> {
    let snapshot: String = row.col("snapshot");
    Ok(VideoCharacterRecord {
        id: row.col("id"),
        org_id: parse_uuid(&row.col::<String>("org_id"))?,
        owner_type: parse_owner_type(&row.col::<String>("owner_type"))?,
        owner_id: parse_uuid(&row.col::<String>("owner_id"))?,
        project_id: parse_optional_uuid(row.col("project_id"))?,
        user_id: parse_optional_uuid(row.col("user_id"))?,
        api_key_id: parse_optional_uuid(row.col("api_key_id"))?,
        service_account_id: parse_optional_uuid(row.col("service_account_id"))?,
        provider: row.col("provider"),
        model: row.col("model"),
        name: row.col("name"),
        snapshot: serde_json::from_str(&snapshot)?,
        created_at: row.col("created_at"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl VideosRepo for SqliteVideosRepo {
    async fn insert(&self, input: NewVideo) -> DbResult<VideoRecord> {
        let created_at = truncate_to_millis(input.created_at);
        let retention_expires_at = truncate_to_millis(input.retention_expires_at);
        let snapshot_json = serde_json::to_string(&input.snapshot)?;
        let error_json = input
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        query(
            r#"
            INSERT INTO videos (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                status, model, provider, prompt, size, seconds, progress, remixed_from_video_id,
                created_at, completed_at, expires_at, error, snapshot, updated_at, retention_expires_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&input.status)
        .bind(&input.model)
        .bind(&input.provider)
        .bind(&input.prompt)
        .bind(&input.size)
        .bind(&input.seconds)
        .bind(input.progress.map(|v| v as i64))
        .bind(&input.remixed_from_video_id)
        .bind(created_at)
        .bind(input.completed_at.map(truncate_to_millis))
        .bind(input.expires_at.map(truncate_to_millis))
        .bind(error_json)
        .bind(&snapshot_json)
        .bind(created_at)
        .bind(retention_expires_at)
        .execute(&self.pool)
        .await?;

        Ok(VideoRecord {
            id: input.id,
            org_id: input.org_id,
            owner_type: input.owner_type,
            owner_id: input.owner_id,
            project_id: input.project_id,
            user_id: input.user_id,
            api_key_id: input.api_key_id,
            service_account_id: input.service_account_id,
            status: input.status,
            model: input.model,
            provider: input.provider,
            prompt: input.prompt,
            size: input.size,
            seconds: input.seconds,
            progress: input.progress,
            remixed_from_video_id: input.remixed_from_video_id,
            created_at,
            completed_at: input.completed_at.map(truncate_to_millis),
            expires_at: input.expires_at.map(truncate_to_millis),
            error: input.error,
            snapshot: input.snapshot,
            updated_at: created_at,
            retention_expires_at,
        })
    }

    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<VideoRecord>> {
        let sql = format!(
            "SELECT {cols} FROM videos WHERE id = ?{scope}",
            cols = VIDEO_COLUMNS,
            scope = ORG_SCOPE_FILTER,
        );
        let mut q = query(&sql).bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        match q.fetch_optional(&self.pool).await? {
            Some(row) => Ok(Some(row_to_video(&row)?)),
            None => Ok(None),
        }
    }

    async fn update_within_org(
        &self,
        id: &str,
        org_id: Uuid,
        patch: VideoPatch,
    ) -> DbResult<Option<VideoRecord>> {
        let snapshot_json = serde_json::to_string(&patch.snapshot)?;
        let error_json = patch
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let now = truncate_to_millis(Utc::now());

        let sql = format!(
            "UPDATE videos SET status = ?, progress = ?, completed_at = ?, expires_at = ?, \
             error = ?, snapshot = ?, updated_at = ? WHERE id = ?{scope} RETURNING {cols}",
            scope = ORG_SCOPE_FILTER,
            cols = VIDEO_COLUMNS,
        );
        let mut q = query(&sql)
            .bind(&patch.status)
            .bind(patch.progress.map(|v| v as i64))
            .bind(patch.completed_at.map(truncate_to_millis))
            .bind(patch.expires_at.map(truncate_to_millis))
            .bind(error_json)
            .bind(&snapshot_json)
            .bind(now)
            .bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        match q.fetch_optional(&self.pool).await? {
            Some(row) => Ok(Some(row_to_video(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<bool> {
        let sql = format!(
            "DELETE FROM videos WHERE id = ?{scope}",
            scope = ORG_SCOPE_FILTER,
        );
        let mut q = query(&sql).bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        Ok(q.execute(&self.pool).await?.rows_affected() > 0)
    }

    async fn list_for_owner(
        &self,
        owner_type: ResponseOwnerType,
        owner_id: Uuid,
        _org_id: Uuid,
        after: Option<String>,
        limit: i64,
        order: VideoListOrder,
    ) -> DbResult<(Vec<VideoRecord>, bool)> {
        let fetch_limit = limit + 1;
        let (comparison, direction) = match order {
            VideoListOrder::Desc => ("<", "DESC"),
            VideoListOrder::Asc => (">", "ASC"),
        };

        // Resolve the `after` cursor (a video id) into its (created_at, id)
        // position within this owner scope. An unknown id starts from the top.
        let boundary: Option<DateTime<Utc>> = match &after {
            Some(after_id) => query(
                "SELECT created_at FROM videos WHERE id = ? AND owner_type = ? AND owner_id = ?",
            )
            .bind(after_id)
            .bind(owner_type.as_str())
            .bind(owner_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .map(|row| row.col::<DateTime<Utc>>("created_at")),
            None => None,
        };

        let rows = match (&after, boundary) {
            (Some(after_id), Some(after_ts)) => {
                let sql = format!(
                    "SELECT {cols} FROM videos \
                     WHERE owner_type = ? AND owner_id = ? \
                     AND (created_at, id) {cmp} (?, ?) \
                     ORDER BY created_at {dir}, id {dir} LIMIT ?",
                    cols = VIDEO_COLUMNS,
                    cmp = comparison,
                    dir = direction,
                );
                query(&sql)
                    .bind(owner_type.as_str())
                    .bind(owner_id.to_string())
                    .bind(after_ts)
                    .bind(after_id)
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            _ => {
                let sql = format!(
                    "SELECT {cols} FROM videos \
                     WHERE owner_type = ? AND owner_id = ? \
                     ORDER BY created_at {dir}, id {dir} LIMIT ?",
                    cols = VIDEO_COLUMNS,
                    dir = direction,
                );
                query(&sql)
                    .bind(owner_type.as_str())
                    .bind(owner_id.to_string())
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };

        let has_more = rows.len() as i64 > limit;
        let items = rows
            .iter()
            .take(limit as usize)
            .map(row_to_video)
            .collect::<DbResult<Vec<_>>>()?;
        Ok((items, has_more))
    }

    async fn delete_expired(&self, before: DateTime<Utc>) -> DbResult<u64> {
        let result = query("DELETE FROM videos WHERE retention_expires_at < ?")
            .bind(truncate_to_millis(before))
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn insert_character(&self, input: NewVideoCharacter) -> DbResult<VideoCharacterRecord> {
        let created_at = truncate_to_millis(input.created_at);
        let snapshot_json = serde_json::to_string(&input.snapshot)?;

        query(
            r#"
            INSERT INTO video_characters (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                provider, model, name, snapshot, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&input.provider)
        .bind(&input.model)
        .bind(&input.name)
        .bind(&snapshot_json)
        .bind(created_at)
        .execute(&self.pool)
        .await?;

        Ok(VideoCharacterRecord {
            id: input.id,
            org_id: input.org_id,
            owner_type: input.owner_type,
            owner_id: input.owner_id,
            project_id: input.project_id,
            user_id: input.user_id,
            api_key_id: input.api_key_id,
            service_account_id: input.service_account_id,
            provider: input.provider,
            model: input.model,
            name: input.name,
            snapshot: input.snapshot,
            created_at,
        })
    }

    async fn get_character_by_id_and_org(
        &self,
        id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<VideoCharacterRecord>> {
        let sql = format!(
            "SELECT {cols} FROM video_characters WHERE id = ?{scope}",
            cols = CHARACTER_COLUMNS,
            scope = CHARACTER_ORG_SCOPE_FILTER,
        );
        let mut q = query(&sql).bind(id);
        let org_str = org_id.to_string();
        for _ in 0..ORG_SCOPE_BINDS {
            q = q.bind(org_str.clone());
        }
        match q.fetch_optional(&self.pool).await? {
            Some(row) => Ok(Some(row_to_character(&row)?)),
            None => Ok(None),
        }
    }
}
