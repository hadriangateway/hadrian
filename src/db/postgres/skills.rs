use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    db::{
        error::{DbError, DbResult},
        repos::{
            CursorDirection, ListParams, ListResult, PageCursors, SkillRepo, cursor_from_row,
            sniff_skill_content_type,
        },
    },
    models::{
        CreateSkill, CreateSkillVersion, Skill, SkillFile, SkillFileInput, SkillFileManifest,
        SkillOwnerType, SkillRef, SkillVersion, VersionSelector,
    },
};

/// Skill projection columns: identity + pointers joined with the default
/// version's metadata. `version_id` is the default version's row id.
const SKILL_SELECT: &str = "s.id AS id, s.owner_type::TEXT AS owner_type, s.owner_id AS owner_id, \
     s.name AS name, s.default_version_seq AS default_version_seq, \
     s.latest_version_seq AS latest_version_seq, s.created_at AS created_at, \
     s.updated_at AS updated_at, v.id AS version_id, v.description AS description, \
     v.user_invocable AS user_invocable, v.disable_model_invocation AS disable_model_invocation, \
     v.allowed_tools AS allowed_tools, v.argument_hint AS argument_hint, \
     v.source_url AS source_url, v.source_ref AS source_ref, \
     v.frontmatter_extra AS frontmatter_extra, v.total_bytes AS total_bytes";

const SKILL_FROM: &str = "FROM skills s JOIN skill_versions v \
     ON v.skill_id = s.id AND v.version_seq = s.default_version_seq AND v.deleted_at IS NULL";

const VERSION_COLUMNS: &str = "id, skill_id, version_seq, name, description, user_invocable, \
     disable_model_invocation, allowed_tools, argument_hint, source_url, source_ref, \
     frontmatter_extra, total_bytes, created_at";

/// Org-scope filter (references $1 four times — bind org_id first).
const ORG_SCOPE_FILTER: &str = r#"
    AND (
        (s.owner_type = 'organization' AND s.owner_id = $1)
        OR (s.owner_type = 'team' AND EXISTS (
            SELECT 1 FROM teams t WHERE t.id = s.owner_id AND t.org_id = $1
        ))
        OR (s.owner_type = 'project' AND EXISTS (
            SELECT 1 FROM projects pr WHERE pr.id = s.owner_id AND pr.org_id = $1
        ))
        OR (s.owner_type = 'user' AND EXISTS (
            SELECT 1 FROM org_memberships om WHERE om.user_id = s.owner_id AND om.org_id = $1
        ))
    )
"#;

pub struct PostgresSkillRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresSkillRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
    }

    fn parse_json_array(v: Option<serde_json::Value>) -> DbResult<Option<Vec<String>>> {
        v.map(serde_json::from_value)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to parse allowed_tools: {}", e)))
    }

    fn parse_json_obj(
        v: Option<serde_json::Value>,
    ) -> DbResult<Option<HashMap<String, serde_json::Value>>> {
        v.map(serde_json::from_value)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to parse frontmatter_extra: {}", e)))
    }

    fn parse_skill_row(row: &sqlx::postgres::PgRow) -> DbResult<(Skill, Uuid)> {
        let owner_type: SkillOwnerType = row
            .get::<String, _>("owner_type")
            .parse()
            .map_err(DbError::Internal)?;
        let allowed_tools = Self::parse_json_array(row.get("allowed_tools"))?;
        let frontmatter_extra = Self::parse_json_obj(row.get("frontmatter_extra"))?;

        let skill = Skill {
            id: row.get("id"),
            owner_type,
            owner_id: row.get("owner_id"),
            name: row.get("name"),
            default_version_seq: row.get("default_version_seq"),
            latest_version_seq: row.get("latest_version_seq"),
            description: row.get("description"),
            user_invocable: row.get("user_invocable"),
            disable_model_invocation: row.get("disable_model_invocation"),
            allowed_tools,
            argument_hint: row.get("argument_hint"),
            source_url: row.get("source_url"),
            source_ref: row.get("source_ref"),
            frontmatter_extra,
            total_bytes: row.get("total_bytes"),
            files: Vec::new(),
            files_manifest: Vec::new(),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        };
        Ok((skill, row.get("version_id")))
    }

    fn parse_version_row(row: &sqlx::postgres::PgRow) -> DbResult<SkillVersion> {
        let allowed_tools = Self::parse_json_array(row.get("allowed_tools"))?;
        let frontmatter_extra = Self::parse_json_obj(row.get("frontmatter_extra"))?;
        Ok(SkillVersion {
            id: row.get("id"),
            skill_id: row.get("skill_id"),
            version_seq: row.get("version_seq"),
            name: row.get("name"),
            description: row.get("description"),
            user_invocable: row.get("user_invocable"),
            disable_model_invocation: row.get("disable_model_invocation"),
            allowed_tools,
            argument_hint: row.get("argument_hint"),
            source_url: row.get("source_url"),
            source_ref: row.get("source_ref"),
            frontmatter_extra,
            total_bytes: row.get("total_bytes"),
            files: Vec::new(),
            files_manifest: Vec::new(),
            created_at: row.get("created_at"),
        })
    }

    fn parse_file(row: &sqlx::postgres::PgRow) -> SkillFile {
        SkillFile {
            path: row.get("path"),
            content: row.get("content"),
            byte_size: row.get("byte_size"),
            content_type: row.get("content_type"),
            created_at: row.get("created_at"),
        }
    }

    fn parse_manifest(row: &sqlx::postgres::PgRow) -> SkillFileManifest {
        SkillFileManifest {
            path: row.get("path"),
            byte_size: row.get("byte_size"),
            content_type: row.get("content_type"),
        }
    }

    async fn load_files(&self, version_id: Uuid) -> DbResult<Vec<SkillFile>> {
        let rows = sqlx::query(
            r#"
            SELECT path, content, byte_size, content_type, created_at
            FROM skill_version_files
            WHERE skill_version_id = $1
            ORDER BY path ASC
            "#,
        )
        .bind(version_id)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(rows.iter().map(Self::parse_file).collect())
    }

    async fn attach_skill_manifests(
        &self,
        skills: &mut [Skill],
        version_ids: &[Uuid],
    ) -> DbResult<()> {
        if skills.is_empty() {
            return Ok(());
        }
        let rows = sqlx::query(
            r#"
            SELECT skill_version_id, path, byte_size, content_type
            FROM skill_version_files
            WHERE skill_version_id = ANY($1)
            ORDER BY path ASC
            "#,
        )
        .bind(version_ids)
        .fetch_all(&self.read_pool)
        .await?;

        let mut by_version: HashMap<Uuid, Vec<SkillFileManifest>> = HashMap::new();
        for row in rows.iter() {
            let vid: Uuid = row.get("skill_version_id");
            by_version
                .entry(vid)
                .or_default()
                .push(Self::parse_manifest(row));
        }
        for (skill, vid) in skills.iter_mut().zip(version_ids.iter()) {
            if let Some(manifest) = by_version.remove(vid) {
                skill.files_manifest = manifest;
            }
        }
        Ok(())
    }

    async fn attach_version_manifests(&self, versions: &mut [SkillVersion]) -> DbResult<()> {
        if versions.is_empty() {
            return Ok(());
        }
        let ids: Vec<Uuid> = versions.iter().map(|v| v.id).collect();
        let rows = sqlx::query(
            r#"
            SELECT skill_version_id, path, byte_size, content_type
            FROM skill_version_files
            WHERE skill_version_id = ANY($1)
            ORDER BY path ASC
            "#,
        )
        .bind(&ids)
        .fetch_all(&self.read_pool)
        .await?;

        let mut by_version: HashMap<Uuid, Vec<SkillFileManifest>> = HashMap::new();
        for row in rows.iter() {
            let vid: Uuid = row.get("skill_version_id");
            by_version
                .entry(vid)
                .or_default()
                .push(Self::parse_manifest(row));
        }
        for version in versions.iter_mut() {
            if let Some(manifest) = by_version.remove(&version.id) {
                version.files_manifest = manifest;
            }
        }
        Ok(())
    }

    fn split_rows(rows: &[sqlx::postgres::PgRow], limit: i64) -> DbResult<(Vec<Skill>, Vec<Uuid>)> {
        let mut items = Vec::new();
        let mut version_ids = Vec::new();
        for row in rows.iter().take(limit as usize) {
            let (skill, vid) = Self::parse_skill_row(row)?;
            items.push(skill);
            version_ids.push(vid);
        }
        Ok((items, version_ids))
    }

    fn files_with_size(files: &[SkillFileInput]) -> Vec<(SkillFileInput, i64, String)> {
        files
            .iter()
            .map(|f| {
                let size = f.content.len() as i64;
                let ct = f
                    .content_type
                    .clone()
                    .unwrap_or_else(|| sniff_skill_content_type(&f.path).to_string());
                (f.clone(), size, ct)
            })
            .collect()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SkillRepo for PostgresSkillRepo {
    async fn create_skill(&self, input: CreateSkill) -> DbResult<Skill> {
        let id = Uuid::new_v4();
        let version_id = Uuid::new_v4();
        let owner_type = input.owner.owner_type();
        let owner_id = input.owner.owner_id();

        let allowed_tools_json: Option<serde_json::Value> = input
            .allowed_tools
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to serialize allowed_tools: {}", e)))?;
        let frontmatter_extra_json: Option<serde_json::Value> = input
            .frontmatter_extra
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| {
                DbError::Internal(format!("Failed to serialize frontmatter_extra: {}", e))
            })?;

        let files = Self::files_with_size(&input.files);
        let total_bytes: i64 = files.iter().map(|(_, s, _)| *s).sum();

        let mut tx = self.write_pool.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO skills (
                id, owner_type, owner_id, name,
                default_version_seq, latest_version_seq, next_version_seq
            )
            VALUES ($1, $2::skill_owner_type, $3, $4, 1, 1, 2)
            "#,
        )
        .bind(id)
        .bind(owner_type.as_str())
        .bind(owner_id)
        .bind(&input.name)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                DbError::Conflict(format!(
                    "Skill with name '{}' already exists for this owner",
                    input.name
                ))
            }
            _ => DbError::from(e),
        })?;

        sqlx::query(
            r#"
            INSERT INTO skill_versions (
                id, skill_id, version_seq, name, description,
                user_invocable, disable_model_invocation, allowed_tools,
                argument_hint, source_url, source_ref, frontmatter_extra, total_bytes
            )
            VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
        )
        .bind(version_id)
        .bind(id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(input.user_invocable)
        .bind(input.disable_model_invocation)
        .bind(&allowed_tools_json)
        .bind(&input.argument_hint)
        .bind(&input.source_url)
        .bind(&input.source_ref)
        .bind(&frontmatter_extra_json)
        .bind(total_bytes)
        .execute(&mut *tx)
        .await?;

        for (file, size, content_type) in files.iter() {
            sqlx::query(
                r#"
                INSERT INTO skill_version_files (
                    skill_version_id, path, content, byte_size, content_type
                )
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(version_id)
            .bind(&file.path)
            .bind(&file.content)
            .bind(*size)
            .bind(content_type)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        self.get_skill(id)
            .await?
            .ok_or_else(|| DbError::Internal("Skill vanished after create".into()))
    }

    async fn get_skill(&self, id: Uuid) -> DbResult<Option<Skill>> {
        let sql = format!(
            "SELECT {cols} {from} WHERE s.id = $1 AND s.deleted_at IS NULL",
            cols = SKILL_SELECT,
            from = SKILL_FROM
        );
        let result = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
            Some(row) => {
                let (mut skill, version_id) = Self::parse_skill_row(&row)?;
                skill.files = self.load_files(version_id).await?;
                Ok(Some(skill))
            }
            None => Ok(None),
        }
    }

    async fn get_skill_and_org(&self, id: Uuid, org_id: Uuid) -> DbResult<Option<Skill>> {
        // org_id = $1 (4x via filter), id = $2.
        let sql = format!(
            "SELECT {cols} {from} WHERE s.id = $2 AND s.deleted_at IS NULL {scope}",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
            scope = ORG_SCOPE_FILTER,
        );
        let result = sqlx::query(&sql)
            .bind(org_id)
            .bind(id)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
            Some(row) => {
                let (mut skill, version_id) = Self::parse_skill_row(&row)?;
                skill.files = self.load_files(version_id).await?;
                Ok(Some(skill))
            }
            None => Ok(None),
        }
    }

    async fn list_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        let limit = params.limit.unwrap_or(100);
        let fetch_limit = limit + 1;
        let deleted_filter = if params.include_deleted {
            ""
        } else {
            "AND s.deleted_at IS NULL"
        };

        if let Some(ref cursor) = params.cursor {
            let (cmp, order, should_reverse) =
                params.sort_order.cursor_query_params(params.direction);
            let sql = format!(
                "SELECT {cols} {from} \
                 WHERE s.owner_type = $1::skill_owner_type AND s.owner_id = $2 \
                 AND ROW(s.created_at, s.id) {cmp} ROW($3, $4) {deleted_filter} \
                 ORDER BY s.created_at {order}, s.id {order} LIMIT $5",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
            );
            let rows = sqlx::query(&sql)
                .bind(owner_type.as_str())
                .bind(owner_id)
                .bind(cursor.created_at)
                .bind(cursor.id)
                .bind(fetch_limit)
                .fetch_all(&self.read_pool)
                .await?;
            let has_more = rows.len() as i64 > limit;
            let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
            self.attach_skill_manifests(&mut items, &version_ids)
                .await?;
            if should_reverse {
                items.reverse();
            }
            let cursors =
                PageCursors::from_items(&items, has_more, params.direction, Some(cursor), |s| {
                    cursor_from_row(s.created_at, s.id)
                });
            return Ok(ListResult::new(items, has_more, cursors));
        }

        let sql = format!(
            "SELECT {cols} {from} \
             WHERE s.owner_type = $1::skill_owner_type AND s.owner_id = $2 {deleted_filter} \
             ORDER BY s.created_at DESC, s.id DESC LIMIT $3",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
        );
        let rows = sqlx::query(&sql)
            .bind(owner_type.as_str())
            .bind(owner_id)
            .bind(fetch_limit)
            .fetch_all(&self.read_pool)
            .await?;
        let has_more = rows.len() as i64 > limit;
        let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
        self.attach_skill_manifests(&mut items, &version_ids)
            .await?;
        let cursors =
            PageCursors::from_items(&items, has_more, CursorDirection::Forward, None, |s| {
                cursor_from_row(s.created_at, s.id)
            });
        Ok(ListResult::new(items, has_more, cursors))
    }

    async fn list_skills_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        let limit = params.limit.unwrap_or(100);
        let fetch_limit = limit + 1;

        if let Some(ref cursor) = params.cursor {
            let (cmp, order, should_reverse) =
                params.sort_order.cursor_query_params(params.direction);
            // org_id = $1 (4x), cursor = $2/$3, limit = $4.
            let sql = format!(
                "SELECT {cols} {from} \
                 WHERE s.deleted_at IS NULL AND ROW(s.created_at, s.id) {cmp} ROW($2, $3) {scope} \
                 ORDER BY s.created_at {order}, s.id {order} LIMIT $4",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
                scope = ORG_SCOPE_FILTER,
            );
            let rows = sqlx::query(&sql)
                .bind(org_id)
                .bind(cursor.created_at)
                .bind(cursor.id)
                .bind(fetch_limit)
                .fetch_all(&self.read_pool)
                .await?;
            let has_more = rows.len() as i64 > limit;
            let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
            self.attach_skill_manifests(&mut items, &version_ids)
                .await?;
            if should_reverse {
                items.reverse();
            }
            let cursors =
                PageCursors::from_items(&items, has_more, params.direction, Some(cursor), |s| {
                    cursor_from_row(s.created_at, s.id)
                });
            return Ok(ListResult::new(items, has_more, cursors));
        }

        let sql = format!(
            "SELECT {cols} {from} \
             WHERE s.deleted_at IS NULL {scope} \
             ORDER BY s.created_at DESC, s.id DESC LIMIT $2",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
            scope = ORG_SCOPE_FILTER,
        );
        let rows = sqlx::query(&sql)
            .bind(org_id)
            .bind(fetch_limit)
            .fetch_all(&self.read_pool)
            .await?;
        let has_more = rows.len() as i64 > limit;
        let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
        self.attach_skill_manifests(&mut items, &version_ids)
            .await?;
        let cursors =
            PageCursors::from_items(&items, has_more, CursorDirection::Forward, None, |s| {
                cursor_from_row(s.created_at, s.id)
            });
        Ok(ListResult::new(items, has_more, cursors))
    }

    async fn list_skills_accessible(
        &self,
        user_id: Option<Uuid>,
        org_ids: &[Uuid],
        team_ids: &[Uuid],
        project_ids: &[Uuid],
        params: ListParams,
    ) -> DbResult<ListResult<Skill>> {
        let limit = params.limit.unwrap_or(100);
        let fetch_limit = limit + 1;
        // user_id = $1, org/team/project arrays = $2/$3/$4.
        let owner_filter = "(\
            ($1::uuid IS NOT NULL AND s.owner_type = 'user' AND s.owner_id = $1) \
            OR (s.owner_type = 'organization' AND s.owner_id = ANY($2)) \
            OR (s.owner_type = 'team' AND s.owner_id = ANY($3)) \
            OR (s.owner_type = 'project' AND s.owner_id = ANY($4)) \
        )";
        let deleted_filter = if params.include_deleted {
            ""
        } else {
            "AND s.deleted_at IS NULL"
        };

        if let Some(ref cursor) = params.cursor {
            let (cmp, order, should_reverse) =
                params.sort_order.cursor_query_params(params.direction);
            let sql = format!(
                "SELECT {cols} {from} \
                 WHERE {owner_filter} {deleted_filter} \
                 AND ROW(s.created_at, s.id) {cmp} ROW($5, $6) \
                 ORDER BY s.created_at {order}, s.id {order} LIMIT $7",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
            );
            let rows = sqlx::query(&sql)
                .bind(user_id)
                .bind(org_ids)
                .bind(team_ids)
                .bind(project_ids)
                .bind(cursor.created_at)
                .bind(cursor.id)
                .bind(fetch_limit)
                .fetch_all(&self.read_pool)
                .await?;
            let has_more = rows.len() as i64 > limit;
            let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
            self.attach_skill_manifests(&mut items, &version_ids)
                .await?;
            if should_reverse {
                items.reverse();
            }
            let cursors =
                PageCursors::from_items(&items, has_more, params.direction, Some(cursor), |s| {
                    cursor_from_row(s.created_at, s.id)
                });
            return Ok(ListResult::new(items, has_more, cursors));
        }

        let sql = format!(
            "SELECT {cols} {from} \
             WHERE {owner_filter} {deleted_filter} \
             ORDER BY s.created_at DESC, s.id DESC LIMIT $5",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
        );
        let rows = sqlx::query(&sql)
            .bind(user_id)
            .bind(org_ids)
            .bind(team_ids)
            .bind(project_ids)
            .bind(fetch_limit)
            .fetch_all(&self.read_pool)
            .await?;
        let has_more = rows.len() as i64 > limit;
        let (mut items, version_ids) = Self::split_rows(&rows, limit)?;
        self.attach_skill_manifests(&mut items, &version_ids)
            .await?;
        let cursors =
            PageCursors::from_items(&items, has_more, CursorDirection::Forward, None, |s| {
                cursor_from_row(s.created_at, s.id)
            });
        Ok(ListResult::new(items, has_more, cursors))
    }

    async fn count_skills_by_owner(
        &self,
        owner_type: SkillOwnerType,
        owner_id: Uuid,
        include_deleted: bool,
    ) -> DbResult<i64> {
        let sql = if include_deleted {
            "SELECT COUNT(*) AS count FROM skills \
             WHERE owner_type = $1::skill_owner_type AND owner_id = $2"
        } else {
            "SELECT COUNT(*) AS count FROM skills \
             WHERE owner_type = $1::skill_owner_type AND owner_id = $2 AND deleted_at IS NULL"
        };
        let row = sqlx::query(sql)
            .bind(owner_type.as_str())
            .bind(owner_id)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(row.get::<i64, _>("count"))
    }

    async fn set_default_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<Skill> {
        // Lock the skill row so this serializes against delete_version; a
        // concurrent delete of the target version must not leave default_version
        // pointing at a soft-deleted version (which hides the skill from reads).
        let mut tx = self.write_pool.begin().await?;
        let skill_live =
            sqlx::query("SELECT 1 FROM skills WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
                .bind(skill_id)
                .fetch_optional(&mut *tx)
                .await?;
        if skill_live.is_none() {
            return Err(DbError::NotFound);
        }
        let exists = sqlx::query(
            "SELECT 1 FROM skill_versions \
             WHERE skill_id = $1 AND version_seq = $2 AND deleted_at IS NULL",
        )
        .bind(skill_id)
        .bind(version_seq)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            return Err(DbError::Validation(format!(
                "Skill has no version '{version_seq}'"
            )));
        }
        sqlx::query("UPDATE skills SET default_version_seq = $1, updated_at = NOW() WHERE id = $2")
            .bind(version_seq)
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        self.get_skill(skill_id).await?.ok_or(DbError::NotFound)
    }

    async fn delete_skill(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query(
            "UPDATE skills SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .execute(&self.write_pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    async fn create_version(
        &self,
        skill_id: Uuid,
        input: CreateSkillVersion,
    ) -> DbResult<SkillVersion> {
        let version_id = Uuid::new_v4();

        let allowed_tools_json: Option<serde_json::Value> = input
            .allowed_tools
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to serialize allowed_tools: {}", e)))?;
        let frontmatter_extra_json: Option<serde_json::Value> = input
            .frontmatter_extra
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| {
                DbError::Internal(format!("Failed to serialize frontmatter_extra: {}", e))
            })?;

        let files = Self::files_with_size(&input.files);
        let total_bytes: i64 = files.iter().map(|(_, s, _)| *s).sum();

        let mut tx = self.write_pool.begin().await?;

        // Atomically advance the counters. Postgres evaluates SET expressions
        // against the pre-update row, so RETURNING latest_version_seq is the
        // seq assigned to the new version.
        let row = sqlx::query(
            r#"
            UPDATE skills
            SET latest_version_seq = next_version_seq,
                default_version_seq = CASE WHEN $1 THEN next_version_seq ELSE default_version_seq END,
                next_version_seq = next_version_seq + 1,
                updated_at = NOW()
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING name, latest_version_seq
            "#,
        )
        .bind(input.make_default)
        .bind(skill_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DbError::NotFound)?;
        let skill_name: String = row.get("name");
        let seq: i64 = row.get("latest_version_seq");

        sqlx::query(
            r#"
            INSERT INTO skill_versions (
                id, skill_id, version_seq, name, description,
                user_invocable, disable_model_invocation, allowed_tools,
                argument_hint, source_url, source_ref, frontmatter_extra, total_bytes
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(version_id)
        .bind(skill_id)
        .bind(seq)
        .bind(&skill_name)
        .bind(&input.description)
        .bind(input.user_invocable)
        .bind(input.disable_model_invocation)
        .bind(&allowed_tools_json)
        .bind(&input.argument_hint)
        .bind(&input.source_url)
        .bind(&input.source_ref)
        .bind(&frontmatter_extra_json)
        .bind(total_bytes)
        .execute(&mut *tx)
        .await?;

        for (file, size, content_type) in files.iter() {
            sqlx::query(
                r#"
                INSERT INTO skill_version_files (
                    skill_version_id, path, content, byte_size, content_type
                )
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(version_id)
            .bind(&file.path)
            .bind(&file.content)
            .bind(*size)
            .bind(content_type)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        self.get_version(skill_id, seq)
            .await?
            .ok_or_else(|| DbError::Internal("Version vanished after create".into()))
    }

    async fn get_version(
        &self,
        skill_id: Uuid,
        version_seq: i64,
    ) -> DbResult<Option<SkillVersion>> {
        let sql = format!(
            "SELECT {cols} FROM skill_versions \
             WHERE skill_id = $1 AND version_seq = $2 AND deleted_at IS NULL",
            cols = VERSION_COLUMNS
        );
        let result = sqlx::query(&sql)
            .bind(skill_id)
            .bind(version_seq)
            .fetch_optional(&self.read_pool)
            .await?;
        match result {
            Some(row) => {
                let mut version = Self::parse_version_row(&row)?;
                version.files = self.load_files(version.id).await?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    async fn get_version_by_id(&self, version_id: Uuid) -> DbResult<Option<SkillVersion>> {
        let sql = format!(
            "SELECT {cols} FROM skill_versions WHERE id = $1 AND deleted_at IS NULL",
            cols = VERSION_COLUMNS
        );
        let result = sqlx::query(&sql)
            .bind(version_id)
            .fetch_optional(&self.read_pool)
            .await?;
        result.as_ref().map(Self::parse_version_row).transpose()
    }

    async fn list_versions(
        &self,
        skill_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<SkillVersion>> {
        let limit = params.limit.unwrap_or(100);
        let fetch_limit = limit + 1;
        let deleted_filter = if params.include_deleted {
            ""
        } else {
            "AND deleted_at IS NULL"
        };

        if let Some(ref cursor) = params.cursor {
            let (cmp, order, should_reverse) =
                params.sort_order.cursor_query_params(params.direction);
            let sql = format!(
                "SELECT {cols} FROM skill_versions \
                 WHERE skill_id = $1 AND ROW(created_at, id) {cmp} ROW($2, $3) {deleted_filter} \
                 ORDER BY created_at {order}, id {order} LIMIT $4",
                cols = VERSION_COLUMNS
            );
            let rows = sqlx::query(&sql)
                .bind(skill_id)
                .bind(cursor.created_at)
                .bind(cursor.id)
                .bind(fetch_limit)
                .fetch_all(&self.read_pool)
                .await?;
            let has_more = rows.len() as i64 > limit;
            let mut items: Vec<SkillVersion> = rows
                .iter()
                .take(limit as usize)
                .map(Self::parse_version_row)
                .collect::<DbResult<Vec<_>>>()?;
            self.attach_version_manifests(&mut items).await?;
            if should_reverse {
                items.reverse();
            }
            let cursors =
                PageCursors::from_items(&items, has_more, params.direction, Some(cursor), |v| {
                    cursor_from_row(v.created_at, v.id)
                });
            return Ok(ListResult::new(items, has_more, cursors));
        }

        let sql = format!(
            "SELECT {cols} FROM skill_versions \
             WHERE skill_id = $1 {deleted_filter} \
             ORDER BY created_at DESC, id DESC LIMIT $2",
            cols = VERSION_COLUMNS
        );
        let rows = sqlx::query(&sql)
            .bind(skill_id)
            .bind(fetch_limit)
            .fetch_all(&self.read_pool)
            .await?;
        let has_more = rows.len() as i64 > limit;
        let mut items: Vec<SkillVersion> = rows
            .iter()
            .take(limit as usize)
            .map(Self::parse_version_row)
            .collect::<DbResult<Vec<_>>>()?;
        self.attach_version_manifests(&mut items).await?;
        let cursors =
            PageCursors::from_items(&items, has_more, CursorDirection::Forward, None, |v| {
                cursor_from_row(v.created_at, v.id)
            });
        Ok(ListResult::new(items, has_more, cursors))
    }

    async fn count_versions(&self, skill_id: Uuid, include_deleted: bool) -> DbResult<i64> {
        let sql = if include_deleted {
            "SELECT COUNT(*) AS count FROM skill_versions WHERE skill_id = $1"
        } else {
            "SELECT COUNT(*) AS count FROM skill_versions WHERE skill_id = $1 AND deleted_at IS NULL"
        };
        let row = sqlx::query(sql)
            .bind(skill_id)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(row.get::<i64, _>("count"))
    }

    async fn delete_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<()> {
        let mut tx = self.write_pool.begin().await?;

        // FOR UPDATE locks the skill row so this serializes against
        // set_default_version (preventing a dangling default pointer).
        let pointers = sqlx::query(
            "SELECT default_version_seq, latest_version_seq FROM skills \
             WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(skill_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DbError::NotFound)?;
        let default_seq: i64 = pointers.get("default_version_seq");
        let latest_seq: i64 = pointers.get("latest_version_seq");

        if version_seq == default_seq {
            return Err(DbError::Conflict(
                "Cannot delete the default skill version; set another default first".into(),
            ));
        }

        let result = sqlx::query(
            "UPDATE skill_versions SET deleted_at = NOW() \
             WHERE skill_id = $1 AND version_seq = $2 AND deleted_at IS NULL",
        )
        .bind(skill_id)
        .bind(version_seq)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }

        if version_seq == latest_seq {
            // The default version is always live, so a live version remains;
            // COALESCE guards against an unexpected empty result.
            let max_row = sqlx::query(
                "SELECT COALESCE(MAX(version_seq), $2) AS m FROM skill_versions \
                 WHERE skill_id = $1 AND deleted_at IS NULL",
            )
            .bind(skill_id)
            .bind(default_seq)
            .fetch_one(&mut *tx)
            .await?;
            let new_latest: i64 = max_row.get("m");
            sqlx::query(
                "UPDATE skills SET latest_version_seq = $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(new_latest)
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn resolve_version_for_reference(
        &self,
        skill_ref: SkillRef,
        version: VersionSelector,
        org_id: Uuid,
    ) -> DbResult<Option<SkillVersion>> {
        // org_id = $1 (4x via filter), ref value = $2.
        let where_col = match &skill_ref {
            SkillRef::Id(_) => "s.id = $2",
            SkillRef::Name(_) => "s.name = $2",
        };
        let sql = format!(
            "SELECT s.id AS id, s.default_version_seq AS default_version_seq, \
                    s.latest_version_seq AS latest_version_seq \
             FROM skills s WHERE {where_col} AND s.deleted_at IS NULL {scope}",
            scope = ORG_SCOPE_FILTER,
        );
        let q = sqlx::query(&sql).bind(org_id);
        let q = match &skill_ref {
            SkillRef::Id(id) => q.bind(*id),
            SkillRef::Name(name) => q.bind(name.clone()),
        };
        let row = q.fetch_optional(&self.read_pool).await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let skill_id: Uuid = row.get("id");
        let default_seq: i64 = row.get("default_version_seq");
        let latest_seq: i64 = row.get("latest_version_seq");
        let seq = match version {
            VersionSelector::Default => default_seq,
            VersionSelector::Latest => latest_seq,
            VersionSelector::Exact(n) => n,
        };

        self.get_version(skill_id, seq).await
    }
}
