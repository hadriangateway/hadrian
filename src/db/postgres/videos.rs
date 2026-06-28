//! Postgres implementation of [`VideosRepo`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::{
    error::{DbError, DbResult},
    repos::{
        NewVideo, NewVideoCharacter, ResponseOwnerType, VideoCharacterRecord, VideoListOrder,
        VideoPatch, VideoRecord, VideosRepo,
    },
};

/// Org-scope filter for `videos`; org id is `$1` (referenced five times).
const ORG_SCOPE_FILTER: &str = r#"
    AND (
        (videos.owner_type = 'organization' AND videos.owner_id = $1)
        OR (videos.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = videos.owner_id AND t.org_id = $1
        ))
        OR (videos.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = videos.owner_id AND pr.org_id = $1
        ))
        OR (videos.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = videos.owner_id AND om.org_id = $1
        ))
        OR (videos.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = videos.owner_id AND sa.org_id = $1
        ))
    )
"#;

const CHARACTER_ORG_SCOPE_FILTER: &str = r#"
    AND (
        (video_characters.owner_type = 'organization' AND video_characters.owner_id = $1)
        OR (video_characters.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = video_characters.owner_id AND t.org_id = $1
        ))
        OR (video_characters.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = video_characters.owner_id AND pr.org_id = $1
        ))
        OR (video_characters.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = video_characters.owner_id AND om.org_id = $1
        ))
        OR (video_characters.owner_type = 'service_account' AND EXISTS (
            SELECT 1 FROM service_accounts sa WHERE sa.id = video_characters.owner_id AND sa.org_id = $1
        ))
    )
"#;

/// `owner_type` is cast to TEXT for direct string parsing.
const VIDEO_COLUMNS: &str = "id, org_id, owner_type::TEXT, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    status, model, provider, prompt, size, seconds, progress, remixed_from_video_id, \
    created_at, completed_at, expires_at, error, snapshot, updated_at, retention_expires_at";

const CHARACTER_COLUMNS: &str = "id, org_id, owner_type::TEXT, owner_id, \
    project_id, user_id, api_key_id, service_account_id, \
    provider, model, name, snapshot, created_at";

pub struct PostgresVideosRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresVideosRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
    }
}

fn parse_owner_type(s: &str) -> DbResult<ResponseOwnerType> {
    ResponseOwnerType::parse(s)
        .ok_or_else(|| DbError::Internal(format!("unknown video owner_type: {s}")))
}

fn row_to_video(row: &sqlx::postgres::PgRow) -> DbResult<VideoRecord> {
    Ok(VideoRecord {
        id: row.get("id"),
        org_id: row.get("org_id"),
        owner_type: parse_owner_type(&row.get::<String, _>("owner_type"))?,
        owner_id: row.get("owner_id"),
        project_id: row.get("project_id"),
        user_id: row.get("user_id"),
        api_key_id: row.get("api_key_id"),
        service_account_id: row.get("service_account_id"),
        status: row.get("status"),
        model: row.get("model"),
        provider: row.get("provider"),
        prompt: row.get("prompt"),
        size: row.get("size"),
        seconds: row.get("seconds"),
        progress: row.get("progress"),
        remixed_from_video_id: row.get("remixed_from_video_id"),
        created_at: row.get("created_at"),
        completed_at: row.get("completed_at"),
        expires_at: row.get("expires_at"),
        error: row.get("error"),
        snapshot: row.get("snapshot"),
        updated_at: row.get("updated_at"),
        retention_expires_at: row.get("retention_expires_at"),
    })
}

fn row_to_character(row: &sqlx::postgres::PgRow) -> DbResult<VideoCharacterRecord> {
    Ok(VideoCharacterRecord {
        id: row.get("id"),
        org_id: row.get("org_id"),
        owner_type: parse_owner_type(&row.get::<String, _>("owner_type"))?,
        owner_id: row.get("owner_id"),
        project_id: row.get("project_id"),
        user_id: row.get("user_id"),
        api_key_id: row.get("api_key_id"),
        service_account_id: row.get("service_account_id"),
        provider: row.get("provider"),
        model: row.get("model"),
        name: row.get("name"),
        snapshot: row.get("snapshot"),
        created_at: row.get("created_at"),
    })
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl VideosRepo for PostgresVideosRepo {
    async fn insert(&self, input: NewVideo) -> DbResult<VideoRecord> {
        sqlx::query(
            r#"
            INSERT INTO videos (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                status, model, provider, prompt, size, seconds, progress, remixed_from_video_id,
                created_at, completed_at, expires_at, error, snapshot, updated_at, retention_expires_at
            )
            VALUES (
                $1, $2, $3::response_owner_type, $4,
                $5, $6, $7, $8,
                $9, $10, $11, $12, $13, $14, $15, $16,
                $17, $18, $19, $20, $21, $22, $23
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
        .bind(&input.status)
        .bind(&input.model)
        .bind(&input.provider)
        .bind(&input.prompt)
        .bind(&input.size)
        .bind(&input.seconds)
        .bind(input.progress)
        .bind(&input.remixed_from_video_id)
        .bind(input.created_at)
        .bind(input.completed_at)
        .bind(input.expires_at)
        .bind(&input.error)
        .bind(&input.snapshot)
        .bind(input.created_at)
        .bind(input.retention_expires_at)
        .execute(&self.write_pool)
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
            created_at: input.created_at,
            completed_at: input.completed_at,
            expires_at: input.expires_at,
            error: input.error,
            snapshot: input.snapshot,
            updated_at: input.created_at,
            retention_expires_at: input.retention_expires_at,
        })
    }

    async fn get_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<Option<VideoRecord>> {
        let sql = format!(
            "SELECT {cols} FROM videos WHERE id = $2{scope}",
            cols = VIDEO_COLUMNS,
            scope = ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
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
        let sql = format!(
            "UPDATE videos SET status = $2, progress = $3, completed_at = $4, expires_at = $5, \
             error = $6, snapshot = $7, updated_at = $8 WHERE id = $9{scope} RETURNING {cols}",
            scope = ORG_SCOPE_FILTER,
            cols = VIDEO_COLUMNS,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(&patch.status)
            .bind(patch.progress)
            .bind(patch.completed_at)
            .bind(patch.expires_at)
            .bind(&patch.error)
            .bind(&patch.snapshot)
            .bind(Utc::now())
            .bind(id)
            .fetch_optional(&self.write_pool)
            .await?;
        match result {
            Some(row) => Ok(Some(row_to_video(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_by_id_and_org(&self, id: &str, org_id: Uuid) -> DbResult<bool> {
        let sql = format!(
            "DELETE FROM videos WHERE id = $2{scope}",
            scope = ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .execute(&self.write_pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_for_owner(
        &self,
        owner_type: ResponseOwnerType,
        owner_id: Uuid,
        org_id: Uuid,
        after: Option<String>,
        limit: i64,
        order: VideoListOrder,
    ) -> DbResult<(Vec<VideoRecord>, bool)> {
        let fetch_limit = limit + 1;
        let (comparison, direction) = match order {
            VideoListOrder::Desc => ("<", "DESC"),
            VideoListOrder::Asc => (">", "ASC"),
        };

        // Resolve `after` (a video id) into its (created_at, id) position.
        // Scoped to the caller's org so a cursor can't leak across orgs.
        let boundary: Option<DateTime<Utc>> = match &after {
            Some(after_id) => sqlx::query(
                "SELECT created_at FROM videos \
                 WHERE id = $1 AND owner_type = $2::response_owner_type AND owner_id = $3 \
                 AND org_id = $4",
            )
            .bind(after_id)
            .bind(owner_type.as_str())
            .bind(owner_id)
            .bind(org_id)
            .fetch_optional(&self.read_pool)
            .await?
            .map(|row| row.get::<DateTime<Utc>, _>("created_at")),
            None => None,
        };

        let rows = match (&after, boundary) {
            (Some(after_id), Some(after_ts)) => {
                let sql = format!(
                    "SELECT {cols} FROM videos \
                     WHERE owner_type = $1::response_owner_type AND owner_id = $2 AND org_id = $6 \
                     AND (created_at, id) {cmp} ($3, $4) \
                     ORDER BY created_at {dir}, id {dir} LIMIT $5",
                    cols = VIDEO_COLUMNS,
                    cmp = comparison,
                    dir = direction,
                );
                sqlx::query(&sql)
                    .bind(owner_type.as_str())
                    .bind(owner_id)
                    .bind(after_ts)
                    .bind(after_id)
                    .bind(fetch_limit)
                    .bind(org_id)
                    .fetch_all(&self.read_pool)
                    .await?
            }
            _ => {
                let sql = format!(
                    "SELECT {cols} FROM videos \
                     WHERE owner_type = $1::response_owner_type AND owner_id = $2 AND org_id = $3 \
                     ORDER BY created_at {dir}, id {dir} LIMIT $4",
                    cols = VIDEO_COLUMNS,
                    dir = direction,
                );
                sqlx::query(&sql)
                    .bind(owner_type.as_str())
                    .bind(owner_id)
                    .bind(org_id)
                    .bind(fetch_limit)
                    .fetch_all(&self.read_pool)
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
        let result = sqlx::query("DELETE FROM videos WHERE retention_expires_at < $1")
            .bind(before)
            .execute(&self.write_pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn insert_character(&self, input: NewVideoCharacter) -> DbResult<VideoCharacterRecord> {
        sqlx::query(
            r#"
            INSERT INTO video_characters (
                id, org_id, owner_type, owner_id,
                project_id, user_id, api_key_id, service_account_id,
                provider, model, name, snapshot, created_at
            )
            VALUES (
                $1, $2, $3::response_owner_type, $4,
                $5, $6, $7, $8,
                $9, $10, $11, $12, $13
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
        .bind(&input.provider)
        .bind(&input.model)
        .bind(&input.name)
        .bind(&input.snapshot)
        .bind(input.created_at)
        .execute(&self.write_pool)
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
            created_at: input.created_at,
        })
    }

    async fn get_character_by_id_and_org(
        &self,
        id: &str,
        org_id: Uuid,
    ) -> DbResult<Option<VideoCharacterRecord>> {
        let sql = format!(
            "SELECT {cols} FROM video_characters WHERE id = $2{scope}",
            cols = CHARACTER_COLUMNS,
            scope = CHARACTER_ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
            Some(row) => Ok(Some(row_to_character(&row)?)),
            None => Ok(None),
        }
    }
}
