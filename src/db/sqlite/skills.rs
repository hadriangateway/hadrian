use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use super::{
    backend::{Pool, Row, RowExt, begin, map_unique_violation, query},
    common::parse_uuid,
};
use crate::{
    db::{
        error::{DbError, DbResult},
        repos::{
            CursorDirection, ListParams, ListResult, PageCursors, SkillRepo, cursor_from_row,
            truncate_to_millis,
        },
    },
    models::{
        CreateSkill, CreateSkillVersion, Skill, SkillFile, SkillFileInput, SkillFileManifest,
        SkillOwnerType, SkillRef, SkillVersion, VersionSelector,
    },
};

/// Columns for the skill projection: skill identity + pointers joined with the
/// default version's metadata. `version_id` is the default version's row id.
const SKILL_SELECT: &str = "s.id AS id, s.owner_type AS owner_type, s.owner_id AS owner_id, \
     s.name AS name, s.default_version_seq AS default_version_seq, \
     s.latest_version_seq AS latest_version_seq, s.created_at AS created_at, \
     s.updated_at AS updated_at, v.id AS version_id, v.description AS description, \
     v.user_invocable AS user_invocable, v.disable_model_invocation AS disable_model_invocation, \
     v.allowed_tools AS allowed_tools, v.argument_hint AS argument_hint, \
     v.source_url AS source_url, v.source_ref AS source_ref, \
     v.frontmatter_extra AS frontmatter_extra, v.total_bytes AS total_bytes";

/// FROM clause joining a skill to its default (live) version.
const SKILL_FROM: &str = "FROM skills s JOIN skill_versions v \
     ON v.skill_id = s.id AND v.version_seq = s.default_version_seq AND v.deleted_at IS NULL";

/// Columns for a [`SkillVersion`] selected directly from `skill_versions`.
const VERSION_COLUMNS: &str = "id, skill_id, version_seq, name, description, user_invocable, \
     disable_model_invocation, allowed_tools, argument_hint, source_url, source_ref, \
     frontmatter_extra, total_bytes, created_at";

pub struct SqliteSkillRepo {
    pool: Pool,
}

impl SqliteSkillRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Sniff a MIME type from a file path's extension. Falls back to
    /// `text/plain` for unknown extensions.
    fn sniff_content_type(path: &str) -> &'static str {
        super::super::repos::sniff_skill_content_type(path)
    }

    fn parse_json_array(s: Option<String>) -> DbResult<Option<Vec<String>>> {
        s.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to parse allowed_tools: {}", e)))
    }

    fn parse_json_obj(s: Option<String>) -> DbResult<Option<HashMap<String, serde_json::Value>>> {
        s.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to parse frontmatter_extra: {}", e)))
    }

    /// Parse a skill projection row. Returns the skill (no files) plus the
    /// default version's id (used to load files / manifests).
    fn parse_skill_row(row: &Row) -> DbResult<(Skill, Uuid)> {
        let owner_type: SkillOwnerType = row
            .col::<String>("owner_type")
            .parse()
            .map_err(DbError::Internal)?;
        let allowed_tools = Self::parse_json_array(row.col("allowed_tools"))?;
        let frontmatter_extra = Self::parse_json_obj(row.col("frontmatter_extra"))?;
        let user_invocable: Option<i64> = row.col("user_invocable");
        let disable_model_invocation: Option<i64> = row.col("disable_model_invocation");
        let version_id = parse_uuid(&row.col::<String>("version_id"))?;

        let skill = Skill {
            id: parse_uuid(&row.col::<String>("id"))?,
            owner_type,
            owner_id: parse_uuid(&row.col::<String>("owner_id"))?,
            name: row.col("name"),
            default_version_seq: row.col("default_version_seq"),
            latest_version_seq: row.col("latest_version_seq"),
            description: row.col("description"),
            user_invocable: user_invocable.map(|n| n != 0),
            disable_model_invocation: disable_model_invocation.map(|n| n != 0),
            allowed_tools,
            argument_hint: row.col("argument_hint"),
            source_url: row.col("source_url"),
            source_ref: row.col("source_ref"),
            frontmatter_extra,
            total_bytes: row.col("total_bytes"),
            files: Vec::new(),
            files_manifest: Vec::new(),
            created_at: row.col("created_at"),
            updated_at: row.col("updated_at"),
        };
        Ok((skill, version_id))
    }

    fn parse_version_row(row: &Row) -> DbResult<SkillVersion> {
        let allowed_tools = Self::parse_json_array(row.col("allowed_tools"))?;
        let frontmatter_extra = Self::parse_json_obj(row.col("frontmatter_extra"))?;
        let user_invocable: Option<i64> = row.col("user_invocable");
        let disable_model_invocation: Option<i64> = row.col("disable_model_invocation");

        Ok(SkillVersion {
            id: parse_uuid(&row.col::<String>("id"))?,
            skill_id: parse_uuid(&row.col::<String>("skill_id"))?,
            version_seq: row.col("version_seq"),
            name: row.col("name"),
            description: row.col("description"),
            user_invocable: user_invocable.map(|n| n != 0),
            disable_model_invocation: disable_model_invocation.map(|n| n != 0),
            allowed_tools,
            argument_hint: row.col("argument_hint"),
            source_url: row.col("source_url"),
            source_ref: row.col("source_ref"),
            frontmatter_extra,
            total_bytes: row.col("total_bytes"),
            files: Vec::new(),
            files_manifest: Vec::new(),
            created_at: row.col("created_at"),
        })
    }

    fn parse_file(row: &Row) -> SkillFile {
        SkillFile {
            path: row.col("path"),
            content: row.col("content"),
            byte_size: row.col("byte_size"),
            content_type: row.col("content_type"),
            created_at: row.col("created_at"),
        }
    }

    fn parse_manifest(row: &Row) -> SkillFileManifest {
        SkillFileManifest {
            path: row.col("path"),
            byte_size: row.col("byte_size"),
            content_type: row.col("content_type"),
        }
    }

    /// Load all files for a single version, sorted by path.
    async fn load_files(&self, version_id: Uuid) -> DbResult<Vec<SkillFile>> {
        let rows = query(
            r#"
            SELECT path, content, byte_size, content_type, created_at
            FROM skill_version_files
            WHERE skill_version_id = ?
            ORDER BY path ASC
            "#,
        )
        .bind(version_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(Self::parse_file).collect())
    }

    /// Attach each skill's default-version file manifest. `version_ids[i]` is
    /// the default version id of `skills[i]`.
    async fn attach_skill_manifests(
        &self,
        skills: &mut [Skill],
        version_ids: &[Uuid],
    ) -> DbResult<()> {
        if skills.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat_n("?", version_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT skill_version_id, path, byte_size, content_type FROM skill_version_files \
             WHERE skill_version_id IN ({}) ORDER BY path ASC",
            placeholders
        );
        let mut q = query(&sql);
        for vid in version_ids {
            q = q.bind(vid.to_string());
        }
        let rows = q.fetch_all(&self.pool).await?;

        let mut by_version: HashMap<Uuid, Vec<SkillFileManifest>> = HashMap::new();
        for row in rows.iter() {
            let vid = parse_uuid(&row.col::<String>("skill_version_id"))?;
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

    /// Attach each version's own file manifest.
    async fn attach_version_manifests(&self, versions: &mut [SkillVersion]) -> DbResult<()> {
        if versions.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat_n("?", versions.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT skill_version_id, path, byte_size, content_type FROM skill_version_files \
             WHERE skill_version_id IN ({}) ORDER BY path ASC",
            placeholders
        );
        let mut q = query(&sql);
        for v in versions.iter() {
            q = q.bind(v.id.to_string());
        }
        let rows = q.fetch_all(&self.pool).await?;

        let mut by_version: HashMap<Uuid, Vec<SkillFileManifest>> = HashMap::new();
        for row in rows.iter() {
            let vid = parse_uuid(&row.col::<String>("skill_version_id"))?;
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

    fn finalize_skill_rows(skills_and_versions: Vec<(Skill, Uuid)>) -> (Vec<Skill>, Vec<Uuid>) {
        let mut items = Vec::with_capacity(skills_and_versions.len());
        let mut version_ids = Vec::with_capacity(skills_and_versions.len());
        for (skill, vid) in skills_and_versions {
            items.push(skill);
            version_ids.push(vid);
        }
        (items, version_ids)
    }

    /// Compute (input, byte_size, resolved content_type) tuples for a file set.
    fn files_with_size(files: &[SkillFileInput]) -> Vec<(SkillFileInput, i64, String)> {
        files
            .iter()
            .map(|f| {
                let size = f.content.len() as i64;
                let ct = f
                    .content_type
                    .clone()
                    .unwrap_or_else(|| Self::sniff_content_type(&f.path).to_string());
                (f.clone(), size, ct)
            })
            .collect()
    }

    /// Org-scoped WHERE fragment for skills reachable within an organization.
    /// Binds the org id four times (one per owner kind).
    const ORG_SCOPE_FILTER: &'static str = r#"
        AND (
            (s.owner_type = 'organization' AND s.owner_id = ?)
            OR
            (s.owner_type = 'team' AND EXISTS (
                SELECT 1 FROM teams t WHERE t.id = s.owner_id AND t.org_id = ?
            ))
            OR
            (s.owner_type = 'project' AND EXISTS (
                SELECT 1 FROM projects pr WHERE pr.id = s.owner_id AND pr.org_id = ?
            ))
            OR
            (s.owner_type = 'user' AND EXISTS (
                SELECT 1 FROM org_memberships om WHERE om.user_id = s.owner_id AND om.org_id = ?
            ))
        )
    "#;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SkillRepo for SqliteSkillRepo {
    async fn create_skill(&self, input: CreateSkill) -> DbResult<Skill> {
        let id = Uuid::new_v4();
        let version_id = Uuid::new_v4();
        let now = truncate_to_millis(chrono::Utc::now());
        let owner_type = input.owner.owner_type();
        let owner_id = input.owner.owner_id();

        let allowed_tools_json = input
            .allowed_tools
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to serialize allowed_tools: {}", e)))?;
        let frontmatter_extra_json = input
            .frontmatter_extra
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                DbError::Internal(format!("Failed to serialize frontmatter_extra: {}", e))
            })?;

        let files = Self::files_with_size(&input.files);
        let total_bytes: i64 = files.iter().map(|(_, s, _)| *s).sum();

        let mut tx = begin(&self.pool).await?;

        query(
            r#"
            INSERT INTO skills (
                id, owner_type, owner_id, name,
                default_version_seq, latest_version_seq, next_version_seq,
                created_at, updated_at
            )
            VALUES (?, ?, ?, ?, 1, 1, 2, ?, ?)
            "#,
        )
        .bind(id.to_string())
        .bind(owner_type.as_str())
        .bind(owner_id.to_string())
        .bind(&input.name)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_unique_violation(format!(
            "Skill with name '{}' already exists for this owner",
            input.name
        )))?;

        query(
            r#"
            INSERT INTO skill_versions (
                id, skill_id, version_seq, name, description,
                user_invocable, disable_model_invocation, allowed_tools,
                argument_hint, source_url, source_ref, frontmatter_extra,
                total_bytes, created_at
            )
            VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(version_id.to_string())
        .bind(id.to_string())
        .bind(&input.name)
        .bind(&input.description)
        .bind(input.user_invocable.map(|b| if b { 1i64 } else { 0i64 }))
        .bind(
            input
                .disable_model_invocation
                .map(|b| if b { 1i64 } else { 0i64 }),
        )
        .bind(&allowed_tools_json)
        .bind(&input.argument_hint)
        .bind(&input.source_url)
        .bind(&input.source_ref)
        .bind(&frontmatter_extra_json)
        .bind(total_bytes)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for (file, size, content_type) in files.iter() {
            query(
                r#"
                INSERT INTO skill_version_files (
                    skill_version_id, path, content, byte_size, content_type, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(version_id.to_string())
            .bind(&file.path)
            .bind(&file.content)
            .bind(*size)
            .bind(content_type)
            .bind(now)
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
            "SELECT {cols} {from} WHERE s.id = ? AND s.deleted_at IS NULL",
            cols = SKILL_SELECT,
            from = SKILL_FROM
        );
        let result = query(&sql)
            .bind(id.to_string())
            .fetch_optional(&self.pool)
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
        let sql = format!(
            "SELECT {cols} {from} WHERE s.id = ? AND s.deleted_at IS NULL {scope}",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
            scope = Self::ORG_SCOPE_FILTER,
        );
        let result = query(&sql)
            .bind(id.to_string())
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .fetch_optional(&self.pool)
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
                 WHERE s.owner_type = ? AND s.owner_id = ? AND (s.created_at, s.id) {cmp} (?, ?) \
                 {deleted_filter} ORDER BY s.created_at {order}, s.id {order} LIMIT ?",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
            );
            let rows = query(&sql)
                .bind(owner_type.as_str())
                .bind(owner_id.to_string())
                .bind(cursor.created_at)
                .bind(cursor.id.to_string())
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?;

            let has_more = rows.len() as i64 > limit;
            let parsed = rows
                .iter()
                .take(limit as usize)
                .map(Self::parse_skill_row)
                .collect::<DbResult<Vec<_>>>()?;
            let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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
             WHERE s.owner_type = ? AND s.owner_id = ? {deleted_filter} \
             ORDER BY s.created_at DESC, s.id DESC LIMIT ?",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
        );
        let rows = query(&sql)
            .bind(owner_type.as_str())
            .bind(owner_id.to_string())
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?;

        let has_more = rows.len() as i64 > limit;
        let parsed = rows
            .iter()
            .take(limit as usize)
            .map(Self::parse_skill_row)
            .collect::<DbResult<Vec<_>>>()?;
        let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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
        let org_str = org_id.to_string();

        if let Some(ref cursor) = params.cursor {
            let (cmp, order, should_reverse) =
                params.sort_order.cursor_query_params(params.direction);
            let sql = format!(
                "SELECT {cols} {from} \
                 WHERE s.deleted_at IS NULL AND (s.created_at, s.id) {cmp} (?, ?) {scope} \
                 ORDER BY s.created_at {order}, s.id {order} LIMIT ?",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
                scope = Self::ORG_SCOPE_FILTER,
            );
            let rows = query(&sql)
                .bind(cursor.created_at)
                .bind(cursor.id.to_string())
                .bind(&org_str)
                .bind(&org_str)
                .bind(&org_str)
                .bind(&org_str)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?;

            let has_more = rows.len() as i64 > limit;
            let parsed = rows
                .iter()
                .take(limit as usize)
                .map(Self::parse_skill_row)
                .collect::<DbResult<Vec<_>>>()?;
            let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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
             ORDER BY s.created_at DESC, s.id DESC LIMIT ?",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
            scope = Self::ORG_SCOPE_FILTER,
        );
        let rows = query(&sql)
            .bind(&org_str)
            .bind(&org_str)
            .bind(&org_str)
            .bind(&org_str)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?;

        let has_more = rows.len() as i64 > limit;
        let parsed = rows
            .iter()
            .take(limit as usize)
            .map(Self::parse_skill_row)
            .collect::<DbResult<Vec<_>>>()?;
        let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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

        let mut conditions: Vec<String> = Vec::new();
        let mut bindings: Vec<String> = Vec::new();
        if let Some(uid) = user_id {
            conditions.push("(s.owner_type = ? AND s.owner_id = ?)".into());
            bindings.push("user".into());
            bindings.push(uid.to_string());
        }
        for (kind, ids) in [
            ("organization", org_ids),
            ("team", team_ids),
            ("project", project_ids),
        ] {
            for id in ids {
                conditions.push("(s.owner_type = ? AND s.owner_id = ?)".into());
                bindings.push(kind.into());
                bindings.push(id.to_string());
            }
        }
        if conditions.is_empty() {
            return Ok(ListResult::new(vec![], false, PageCursors::default()));
        }
        let owner_filter = conditions.join(" OR ");
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
                 WHERE ({owner_filter}) {deleted_filter} \
                 AND (s.created_at, s.id) {cmp} (?, ?) \
                 ORDER BY s.created_at {order}, s.id {order} LIMIT ?",
                cols = SKILL_SELECT,
                from = SKILL_FROM,
            );
            let mut q = query(&sql);
            for b in &bindings {
                q = q.bind(b);
            }
            q = q
                .bind(cursor.created_at)
                .bind(cursor.id.to_string())
                .bind(fetch_limit);
            let rows = q.fetch_all(&self.pool).await?;

            let has_more = rows.len() as i64 > limit;
            let parsed = rows
                .iter()
                .take(limit as usize)
                .map(Self::parse_skill_row)
                .collect::<DbResult<Vec<_>>>()?;
            let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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
             WHERE ({owner_filter}) {deleted_filter} \
             ORDER BY s.created_at DESC, s.id DESC LIMIT ?",
            cols = SKILL_SELECT,
            from = SKILL_FROM,
        );
        let mut q = query(&sql);
        for b in &bindings {
            q = q.bind(b);
        }
        q = q.bind(fetch_limit);
        let rows = q.fetch_all(&self.pool).await?;

        let has_more = rows.len() as i64 > limit;
        let parsed = rows
            .iter()
            .take(limit as usize)
            .map(Self::parse_skill_row)
            .collect::<DbResult<Vec<_>>>()?;
        let (mut items, version_ids) = Self::finalize_skill_rows(parsed);
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
            "SELECT COUNT(*) AS count FROM skills WHERE owner_type = ? AND owner_id = ?"
        } else {
            "SELECT COUNT(*) AS count FROM skills WHERE owner_type = ? AND owner_id = ? \
             AND deleted_at IS NULL"
        };
        let row = query(sql)
            .bind(owner_type.as_str())
            .bind(owner_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.col::<i64>("count"))
    }

    async fn set_default_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<Skill> {
        let now = truncate_to_millis(chrono::Utc::now());
        // BEGIN IMMEDIATE takes the write lock up front so this serializes
        // against delete_version (SQLite has no FOR UPDATE). Without it a
        // concurrent delete of the target version could leave default_version
        // pointing at a soft-deleted version, hiding the skill from all reads.
        let mut conn = self.pool.acquire().await?;
        query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let result = async {
            let skill_live =
                query("SELECT 1 AS one FROM skills WHERE id = ? AND deleted_at IS NULL")
                    .bind(skill_id.to_string())
                    .fetch_optional(&mut *conn)
                    .await?;
            if skill_live.is_none() {
                return Err(DbError::NotFound);
            }
            let exists = query(
                "SELECT 1 AS one FROM skill_versions \
                 WHERE skill_id = ? AND version_seq = ? AND deleted_at IS NULL",
            )
            .bind(skill_id.to_string())
            .bind(version_seq)
            .fetch_optional(&mut *conn)
            .await?;
            if exists.is_none() {
                return Err(DbError::Validation(format!(
                    "Skill has no version '{version_seq}'"
                )));
            }
            query("UPDATE skills SET default_version_seq = ?, updated_at = ? WHERE id = ?")
                .bind(version_seq)
                .bind(now)
                .bind(skill_id.to_string())
                .execute(&mut *conn)
                .await?;
            Ok(())
        }
        .await;
        match &result {
            Ok(_) => {
                query("COMMIT").execute(&mut *conn).await?;
            }
            Err(_) => {
                let _ = query("ROLLBACK").execute(&mut *conn).await;
            }
        }
        result?;
        // Release the connection before the follow-up read (pools may be
        // single-connection, e.g. in tests).
        drop(conn);

        self.get_skill(skill_id).await?.ok_or(DbError::NotFound)
    }

    async fn delete_skill(&self, id: Uuid) -> DbResult<()> {
        let now = truncate_to_millis(chrono::Utc::now());
        let result = query("UPDATE skills SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL")
            .bind(now)
            .bind(id.to_string())
            .execute(&self.pool)
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
        let now = truncate_to_millis(chrono::Utc::now());

        let allowed_tools_json = input
            .allowed_tools
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| DbError::Internal(format!("Failed to serialize allowed_tools: {}", e)))?;
        let frontmatter_extra_json = input
            .frontmatter_extra
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                DbError::Internal(format!("Failed to serialize frontmatter_extra: {}", e))
            })?;

        let files = Self::files_with_size(&input.files);
        let total_bytes: i64 = files.iter().map(|(_, s, _)| *s).sum();

        let mut tx = begin(&self.pool).await?;

        // Atomically advance the version counters. SQLite evaluates every SET
        // expression against the pre-update row, so `latest_version_seq` after
        // the update equals the seq assigned to the new version.
        let row = query(
            r#"
            UPDATE skills
            SET latest_version_seq = next_version_seq,
                default_version_seq = CASE WHEN ? THEN next_version_seq ELSE default_version_seq END,
                next_version_seq = next_version_seq + 1,
                updated_at = ?
            WHERE id = ? AND deleted_at IS NULL
            RETURNING name, latest_version_seq
            "#,
        )
        .bind(if input.make_default { 1i64 } else { 0i64 })
        .bind(now)
        .bind(skill_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let row = row.ok_or(DbError::NotFound)?;
        let skill_name: String = row.col("name");
        let seq: i64 = row.col("latest_version_seq");

        query(
            r#"
            INSERT INTO skill_versions (
                id, skill_id, version_seq, name, description,
                user_invocable, disable_model_invocation, allowed_tools,
                argument_hint, source_url, source_ref, frontmatter_extra,
                total_bytes, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(version_id.to_string())
        .bind(skill_id.to_string())
        .bind(seq)
        .bind(&skill_name)
        .bind(&input.description)
        .bind(input.user_invocable.map(|b| if b { 1i64 } else { 0i64 }))
        .bind(
            input
                .disable_model_invocation
                .map(|b| if b { 1i64 } else { 0i64 }),
        )
        .bind(&allowed_tools_json)
        .bind(&input.argument_hint)
        .bind(&input.source_url)
        .bind(&input.source_ref)
        .bind(&frontmatter_extra_json)
        .bind(total_bytes)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for (file, size, content_type) in files.iter() {
            query(
                r#"
                INSERT INTO skill_version_files (
                    skill_version_id, path, content, byte_size, content_type, created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(version_id.to_string())
            .bind(&file.path)
            .bind(&file.content)
            .bind(*size)
            .bind(content_type)
            .bind(now)
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
             WHERE skill_id = ? AND version_seq = ? AND deleted_at IS NULL",
            cols = VERSION_COLUMNS
        );
        let result = query(&sql)
            .bind(skill_id.to_string())
            .bind(version_seq)
            .fetch_optional(&self.pool)
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
            "SELECT {cols} FROM skill_versions WHERE id = ? AND deleted_at IS NULL",
            cols = VERSION_COLUMNS
        );
        let result = query(&sql)
            .bind(version_id.to_string())
            .fetch_optional(&self.pool)
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
                 WHERE skill_id = ? AND (created_at, id) {cmp} (?, ?) {deleted_filter} \
                 ORDER BY created_at {order}, id {order} LIMIT ?",
                cols = VERSION_COLUMNS
            );
            let rows = query(&sql)
                .bind(skill_id.to_string())
                .bind(cursor.created_at)
                .bind(cursor.id.to_string())
                .bind(fetch_limit)
                .fetch_all(&self.pool)
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
             WHERE skill_id = ? {deleted_filter} \
             ORDER BY created_at DESC, id DESC LIMIT ?",
            cols = VERSION_COLUMNS
        );
        let rows = query(&sql)
            .bind(skill_id.to_string())
            .bind(fetch_limit)
            .fetch_all(&self.pool)
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
            "SELECT COUNT(*) AS count FROM skill_versions WHERE skill_id = ?"
        } else {
            "SELECT COUNT(*) AS count FROM skill_versions WHERE skill_id = ? AND deleted_at IS NULL"
        };
        let row = query(sql)
            .bind(skill_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.col::<i64>("count"))
    }

    async fn delete_version(&self, skill_id: Uuid, version_seq: i64) -> DbResult<()> {
        let now = truncate_to_millis(chrono::Utc::now());
        // BEGIN IMMEDIATE so the default-pointer read and the version delete
        // serialize against set_default_version (no FOR UPDATE in SQLite).
        let mut conn = self.pool.acquire().await?;
        query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        let result = async {
            let pointers = query(
                "SELECT default_version_seq, latest_version_seq FROM skills \
                 WHERE id = ? AND deleted_at IS NULL",
            )
            .bind(skill_id.to_string())
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(DbError::NotFound)?;
            let default_seq: i64 = pointers.col("default_version_seq");
            let latest_seq: i64 = pointers.col("latest_version_seq");

            if version_seq == default_seq {
                return Err(DbError::Conflict(
                    "Cannot delete the default skill version; set another default first".into(),
                ));
            }

            let result = query(
                "UPDATE skill_versions SET deleted_at = ? \
                 WHERE skill_id = ? AND version_seq = ? AND deleted_at IS NULL",
            )
            .bind(now)
            .bind(skill_id.to_string())
            .bind(version_seq)
            .execute(&mut *conn)
            .await?;
            if result.rows_affected() == 0 {
                return Err(DbError::NotFound);
            }

            // If we removed the latest version, recompute the latest pointer.
            // The default version is always live, so at least one version
            // remains; COALESCE guards against an unexpected empty result.
            if version_seq == latest_seq {
                let max_row = query(
                    "SELECT COALESCE(MAX(version_seq), ?) AS m FROM skill_versions \
                     WHERE skill_id = ? AND deleted_at IS NULL",
                )
                .bind(default_seq)
                .bind(skill_id.to_string())
                .fetch_one(&mut *conn)
                .await?;
                let new_latest: i64 = max_row.col("m");
                query("UPDATE skills SET latest_version_seq = ?, updated_at = ? WHERE id = ?")
                    .bind(new_latest)
                    .bind(now)
                    .bind(skill_id.to_string())
                    .execute(&mut *conn)
                    .await?;
            }
            Ok(())
        }
        .await;
        match &result {
            Ok(_) => {
                query("COMMIT").execute(&mut *conn).await?;
            }
            Err(_) => {
                let _ = query("ROLLBACK").execute(&mut *conn).await;
            }
        }
        result
    }

    async fn resolve_version_for_reference(
        &self,
        skill_ref: SkillRef,
        version: VersionSelector,
        org_id: Uuid,
    ) -> DbResult<Option<SkillVersion>> {
        let (where_col, bind_val) = match &skill_ref {
            SkillRef::Id(id) => ("s.id = ?", id.to_string()),
            SkillRef::Name(name) => ("s.name = ?", name.clone()),
        };
        let sql = format!(
            "SELECT s.id AS id, s.default_version_seq AS default_version_seq, \
                    s.latest_version_seq AS latest_version_seq \
             FROM skills s WHERE {where_col} AND s.deleted_at IS NULL {scope}",
            scope = Self::ORG_SCOPE_FILTER,
        );
        let row = query(&sql)
            .bind(&bind_val)
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .bind(org_id.to_string())
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let skill_id = parse_uuid(&row.col::<String>("id"))?;
        let default_seq: i64 = row.col("default_version_seq");
        let latest_seq: i64 = row.col("latest_version_seq");
        let seq = match version {
            VersionSelector::Default => default_seq,
            VersionSelector::Latest => latest_seq,
            VersionSelector::Exact(n) => n,
        };

        self.get_version(skill_id, seq).await
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::models::{SkillFileInput, SkillOwner};

    async fn create_test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Failed to create in-memory SQLite pool");

        for ddl in [
            r#"
            CREATE TABLE skills (
                id TEXT PRIMARY KEY NOT NULL,
                owner_type TEXT NOT NULL CHECK (owner_type IN ('organization','team','project','user')),
                owner_id TEXT NOT NULL,
                name TEXT NOT NULL,
                default_version_seq INTEGER NOT NULL,
                latest_version_seq INTEGER NOT NULL,
                next_version_seq INTEGER NOT NULL DEFAULT 2,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                deleted_at TEXT
            )
            "#,
            "CREATE UNIQUE INDEX idx_skills_owner_name_active ON skills(owner_type, owner_id, name) WHERE deleted_at IS NULL",
            r#"
            CREATE TABLE skill_versions (
                id TEXT PRIMARY KEY NOT NULL,
                skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
                version_seq INTEGER NOT NULL,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                user_invocable INTEGER,
                disable_model_invocation INTEGER,
                allowed_tools TEXT,
                argument_hint TEXT,
                source_url TEXT,
                source_ref TEXT,
                frontmatter_extra TEXT,
                total_bytes INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                deleted_at TEXT
            )
            "#,
            "CREATE UNIQUE INDEX idx_skill_versions_skill_seq ON skill_versions(skill_id, version_seq)",
            r#"
            CREATE TABLE skill_version_files (
                skill_version_id TEXT NOT NULL REFERENCES skill_versions(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                content TEXT NOT NULL,
                byte_size INTEGER NOT NULL,
                content_type TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY(skill_version_id, path)
            )
            "#,
            // Minimal owner-relationship tables referenced by ORG_SCOPE_FILTER.
            "CREATE TABLE teams (id TEXT PRIMARY KEY, org_id TEXT NOT NULL)",
            "CREATE TABLE projects (id TEXT PRIMARY KEY, org_id TEXT NOT NULL)",
            "CREATE TABLE org_memberships (user_id TEXT NOT NULL, org_id TEXT NOT NULL)",
        ] {
            sqlx::query(ddl)
                .execute(&pool)
                .await
                .expect("Failed to create skill tables");
        }

        pool
    }

    fn main_file(body: &str) -> SkillFileInput {
        SkillFileInput {
            path: "SKILL.md".into(),
            content: body.into(),
            content_type: None,
        }
    }

    fn create_input(name: &str, body: &str, user_id: Uuid) -> CreateSkill {
        CreateSkill {
            owner: SkillOwner::User { user_id },
            name: name.into(),
            description: "Test skill description.".into(),
            files: vec![main_file(body)],
            user_invocable: None,
            disable_model_invocation: None,
            allowed_tools: None,
            argument_hint: None,
            source_url: None,
            source_ref: None,
            frontmatter_extra: None,
        }
    }

    fn version_input(body: &str, make_default: bool) -> CreateSkillVersion {
        CreateSkillVersion {
            files: vec![main_file(body)],
            description: "Next version.".into(),
            user_invocable: None,
            disable_model_invocation: None,
            allowed_tools: None,
            argument_hint: None,
            source_url: None,
            source_ref: None,
            frontmatter_extra: None,
            make_default,
        }
    }

    #[tokio::test]
    async fn create_skill_stores_first_version_and_files() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();

        let skill = repo
            .create_skill(create_input("code-review", "Review code.", user_id))
            .await
            .expect("create should succeed");

        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.default_version_seq, 1);
        assert_eq!(skill.latest_version_seq, 1);
        assert_eq!(skill.files.len(), 1);
        assert_eq!(skill.files[0].content, "Review code.");
        assert_eq!(skill.files[0].content_type, "text/markdown");
        assert_eq!(skill.total_bytes, "Review code.".len() as i64);
    }

    #[tokio::test]
    async fn duplicate_name_per_owner_fails_but_other_owner_ok() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();

        repo.create_skill(create_input("dup", "a", u1))
            .await
            .unwrap();
        let dup = repo.create_skill(create_input("dup", "b", u1)).await;
        assert!(matches!(dup, Err(DbError::Conflict(_))));
        repo.create_skill(create_input("dup", "c", u2))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_version_advances_pointers() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        let skill = repo
            .create_skill(create_input("vers", "v1", user_id))
            .await
            .unwrap();

        // Non-default version: latest advances, default stays.
        let v2 = repo
            .create_version(skill.id, version_input("v2", false))
            .await
            .unwrap();
        assert_eq!(v2.version_seq, 2);
        let after = repo.get_skill(skill.id).await.unwrap().unwrap();
        assert_eq!(after.latest_version_seq, 2);
        assert_eq!(after.default_version_seq, 1);
        assert_eq!(after.files[0].content, "v1");

        // Default version: both advance.
        let v3 = repo
            .create_version(skill.id, version_input("v3", true))
            .await
            .unwrap();
        assert_eq!(v3.version_seq, 3);
        let after = repo.get_skill(skill.id).await.unwrap().unwrap();
        assert_eq!(after.latest_version_seq, 3);
        assert_eq!(after.default_version_seq, 3);
        assert_eq!(after.files[0].content, "v3");
    }

    #[tokio::test]
    async fn set_default_version_repoints_and_validates() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        let skill = repo
            .create_skill(create_input("setdef", "v1", user_id))
            .await
            .unwrap();
        repo.create_version(skill.id, version_input("v2", false))
            .await
            .unwrap();

        let updated = repo.set_default_version(skill.id, 2).await.unwrap();
        assert_eq!(updated.default_version_seq, 2);
        assert_eq!(updated.files[0].content, "v2");

        let bad = repo.set_default_version(skill.id, 99).await;
        assert!(matches!(bad, Err(DbError::Validation(_))));
    }

    #[tokio::test]
    async fn delete_version_guards_default_and_recomputes_latest() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        let skill = repo
            .create_skill(create_input("del", "v1", user_id))
            .await
            .unwrap();
        repo.create_version(skill.id, version_input("v2", false))
            .await
            .unwrap();

        // Cannot delete the default (v1).
        let blocked = repo.delete_version(skill.id, 1).await;
        assert!(matches!(blocked, Err(DbError::Conflict(_))));

        // Delete the latest (v2) → latest recomputes to v1.
        repo.delete_version(skill.id, 2).await.unwrap();
        let after = repo.get_skill(skill.id).await.unwrap().unwrap();
        assert_eq!(after.latest_version_seq, 1);
        assert!(repo.get_version(skill.id, 2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_versions_returns_all_live() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        let skill = repo
            .create_skill(create_input("lv", "v1", user_id))
            .await
            .unwrap();
        repo.create_version(skill.id, version_input("v2", false))
            .await
            .unwrap();

        let versions = repo
            .list_versions(skill.id, ListParams::default())
            .await
            .unwrap();
        assert_eq!(versions.items.len(), 2);
        // Newest first.
        assert_eq!(versions.items[0].version_seq, 2);
        assert_eq!(versions.items[1].version_seq, 1);
        // Manifest populated, no content.
        assert!(versions.items[0].files.is_empty());
        assert_eq!(versions.items[0].files_manifest.len(), 1);
    }

    #[tokio::test]
    async fn list_by_owner_projects_default_version_manifest() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        repo.create_skill(create_input("s1", "body", user_id))
            .await
            .unwrap();

        let result = repo
            .list_skills_by_owner(SkillOwnerType::User, user_id, ListParams::default())
            .await
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].files.is_empty());
        assert_eq!(result.items[0].files_manifest.len(), 1);
    }

    #[tokio::test]
    async fn resolve_reference_by_name_and_version() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let org_id = Uuid::new_v4();
        let skill = repo
            .create_skill(CreateSkill {
                owner: SkillOwner::Organization {
                    organization_id: org_id,
                },
                name: "spreadsheets".into(),
                description: "d".into(),
                files: vec![main_file("v1")],
                user_invocable: None,
                disable_model_invocation: None,
                allowed_tools: None,
                argument_hint: None,
                source_url: None,
                source_ref: None,
                frontmatter_extra: None,
            })
            .await
            .unwrap();
        repo.create_version(skill.id, version_input("v2", false))
            .await
            .unwrap();

        // By name, default version.
        let by_name = repo
            .resolve_version_for_reference(
                SkillRef::Name("spreadsheets".into()),
                VersionSelector::Default,
                org_id,
            )
            .await
            .unwrap()
            .expect("resolves");
        assert_eq!(by_name.files[0].content, "v1");

        // By id, latest version.
        let by_id_latest = repo
            .resolve_version_for_reference(SkillRef::Id(skill.id), VersionSelector::Latest, org_id)
            .await
            .unwrap()
            .expect("resolves");
        assert_eq!(by_id_latest.files[0].content, "v2");

        // Exact version.
        let exact = repo
            .resolve_version_for_reference(
                SkillRef::Id(skill.id),
                VersionSelector::Exact(2),
                org_id,
            )
            .await
            .unwrap()
            .expect("resolves");
        assert_eq!(exact.version_seq, 2);

        // Wrong org → None.
        let wrong_org = repo
            .resolve_version_for_reference(
                SkillRef::Name("spreadsheets".into()),
                VersionSelector::Default,
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        assert!(wrong_org.is_none());
    }

    #[tokio::test]
    async fn delete_skill_soft_deletes_and_allows_recreate() {
        let repo = SqliteSkillRepo::new(create_test_pool().await);
        let user_id = Uuid::new_v4();
        let skill = repo
            .create_skill(create_input("gone", "body", user_id))
            .await
            .unwrap();

        repo.delete_skill(skill.id).await.unwrap();
        assert!(repo.get_skill(skill.id).await.unwrap().is_none());

        // Same name can be recreated after soft-delete (partial unique index).
        repo.create_skill(create_input("gone", "body2", user_id))
            .await
            .unwrap();
    }
}
