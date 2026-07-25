use async_trait::async_trait;
use uuid::Uuid;

use super::{
    backend::{Pool, RowExt, begin, map_unique_violation, query},
    common::parse_uuid,
};
use crate::{
    db::{
        error::{DbError, DbResult},
        repos::{
            Cursor, CursorDirection, ListParams, ListResult, ModelPricingRepo, PageCursors,
            cursor_from_row, truncate_to_millis,
        },
    },
    models::{CreateModelPricing, DbModelPricing, PricingOwner, PricingSource, UpdateModelPricing},
};

pub struct SqliteModelPricingRepo {
    pool: Pool,
}

impl SqliteModelPricingRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    fn parse_owner(owner_type: Option<&str>, owner_id: Option<&str>) -> DbResult<PricingOwner> {
        match (owner_type, owner_id) {
            (None, _) | (Some(""), _) => Ok(PricingOwner::Global),
            (Some("organization"), Some(id)) => Ok(PricingOwner::Organization {
                org_id: parse_uuid(id)?,
            }),
            (Some("team"), Some(id)) => Ok(PricingOwner::Team {
                team_id: parse_uuid(id)?,
            }),
            (Some("project"), Some(id)) => Ok(PricingOwner::Project {
                project_id: parse_uuid(id)?,
            }),
            (Some("user"), Some(id)) => Ok(PricingOwner::User {
                user_id: parse_uuid(id)?,
            }),
            _ => Err(DbError::Internal("Invalid pricing owner".to_string())),
        }
    }

    fn owner_to_parts(owner: &PricingOwner) -> (Option<&'static str>, Option<Uuid>) {
        match owner {
            PricingOwner::Global => (None, None),
            PricingOwner::Organization { org_id } => (Some("organization"), Some(*org_id)),
            PricingOwner::Team { team_id } => (Some("team"), Some(*team_id)),
            PricingOwner::Project { project_id } => (Some("project"), Some(*project_id)),
            PricingOwner::User { user_id } => (Some("user"), Some(*user_id)),
        }
    }

    fn row_to_pricing(row: &super::backend::Row) -> DbResult<DbModelPricing> {
        let owner_type: Option<String> = row.col("owner_type");
        let owner_id: Option<String> = row.col("owner_id");
        let source_str: String = row.col("source");

        Ok(DbModelPricing {
            id: parse_uuid(&row.col::<String>("id"))?,
            owner: Self::parse_owner(owner_type.as_deref(), owner_id.as_deref())?,
            provider: row.col("provider"),
            model: row.col("model"),
            input_per_1m_tokens: row.col("input_per_1m_tokens"),
            output_per_1m_tokens: row.col("output_per_1m_tokens"),
            per_image: row.col("per_image"),
            per_request: row.col("per_request"),
            cached_input_per_1m_tokens: row.col("cached_input_per_1m_tokens"),
            cache_write_per_1m_tokens: row.col("cache_write_per_1m_tokens"),
            reasoning_per_1m_tokens: row.col("reasoning_per_1m_tokens"),
            per_second: row.col("per_second"),
            per_1m_characters: row.col("per_1m_characters"),
            source: PricingSource::parse(&source_str),
            created_at: row.col("created_at"),
            updated_at: row.col("updated_at"),
        })
    }

    /// Helper method for cursor-based pagination with a custom WHERE clause.
    async fn list_with_cursor(
        &self,
        where_clause: &str,
        binds: Vec<String>,
        params: &ListParams,
        cursor: &Cursor,
        limit: i64,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let fetch_limit = limit + 1;
        let (comparison, order, should_reverse) =
            params.sort_order.cursor_query_params(params.direction);

        let cursor_condition = if where_clause.is_empty() {
            format!("WHERE (created_at, id) {} (?, ?)", comparison)
        } else {
            format!(
                "{} AND (created_at, id) {} (?, ?)",
                where_clause, comparison
            )
        };

        let sql = format!(
            r#"
            SELECT id, owner_type, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source, created_at, updated_at
            FROM model_pricing
            {}
            ORDER BY created_at {}, id {}
            LIMIT ?
            "#,
            cursor_condition, order, order
        );

        let mut query_builder = query(&sql);
        for bind in &binds {
            query_builder = query_builder.bind(bind);
        }
        query_builder = query_builder
            .bind(cursor.created_at)
            .bind(cursor.id.to_string())
            .bind(fetch_limit);

        let rows = query_builder.fetch_all(&self.pool).await?;

        let has_more = rows.len() as i64 > limit;
        let mut items: Vec<DbModelPricing> = rows
            .iter()
            .take(limit as usize)
            .map(Self::row_to_pricing)
            .collect::<DbResult<Vec<_>>>()?;

        if should_reverse {
            items.reverse();
        }

        // Generate cursors
        let cursors = PageCursors::from_items(
            &items,
            has_more,
            params.direction,
            Some(cursor),
            |pricing| cursor_from_row(pricing.created_at, pricing.id),
        );

        Ok(ListResult::new(items, has_more, cursors))
    }

    /// Helper method for first page (no cursor provided).
    async fn list_first_page(
        &self,
        where_clause: &str,
        binds: Vec<String>,
        limit: i64,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let fetch_limit = limit + 1;

        let sql = format!(
            r#"
            SELECT id, owner_type, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source, created_at, updated_at
            FROM model_pricing
            {}
            ORDER BY created_at DESC, id DESC
            LIMIT ?
            "#,
            where_clause
        );

        let mut query_builder = query(&sql);
        for bind in &binds {
            query_builder = query_builder.bind(bind);
        }
        query_builder = query_builder.bind(fetch_limit);

        let rows = query_builder.fetch_all(&self.pool).await?;

        let has_more = rows.len() as i64 > limit;
        let items: Vec<DbModelPricing> = rows
            .iter()
            .take(limit as usize)
            .map(Self::row_to_pricing)
            .collect::<DbResult<Vec<_>>>()?;

        // Generate cursors for pagination
        let cursors = PageCursors::from_items(
            &items,
            has_more,
            CursorDirection::Forward,
            None,
            |pricing| cursor_from_row(pricing.created_at, pricing.id),
        );

        Ok(ListResult::new(items, has_more, cursors))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ModelPricingRepo for SqliteModelPricingRepo {
    async fn create(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        let id = Uuid::new_v4();
        let now = truncate_to_millis(chrono::Utc::now());
        let (owner_type, owner_id) = Self::owner_to_parts(&input.owner);

        query(
            r#"
            INSERT INTO model_pricing (
                id, owner_type, owner_id, provider, model,
                input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                per_second, per_1m_characters,
                source, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id.to_string())
        .bind(owner_type)
        .bind(owner_id.map(|u| u.to_string()))
        .bind(&input.provider)
        .bind(&input.model)
        .bind(input.input_per_1m_tokens)
        .bind(input.output_per_1m_tokens)
        .bind(input.per_image)
        .bind(input.per_request)
        .bind(input.cached_input_per_1m_tokens)
        .bind(input.cache_write_per_1m_tokens)
        .bind(input.reasoning_per_1m_tokens)
        .bind(input.per_second)
        .bind(input.per_1m_characters)
        .bind(input.source.as_str())
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_unique_violation(format!(
            "Pricing for provider '{}' model '{}' already exists",
            input.provider, input.model
        )))?;

        Ok(DbModelPricing {
            id,
            owner: input.owner,
            provider: input.provider,
            model: input.model,
            input_per_1m_tokens: input.input_per_1m_tokens,
            output_per_1m_tokens: input.output_per_1m_tokens,
            per_image: input.per_image,
            per_request: input.per_request,
            cached_input_per_1m_tokens: input.cached_input_per_1m_tokens,
            cache_write_per_1m_tokens: input.cache_write_per_1m_tokens,
            reasoning_per_1m_tokens: input.reasoning_per_1m_tokens,
            per_second: input.per_second,
            per_1m_characters: input.per_1m_characters,
            source: input.source,
            created_at: now,
            updated_at: now,
        })
    }

    async fn get_by_id(&self, id: Uuid) -> DbResult<Option<DbModelPricing>> {
        let row = query(
            r#"
            SELECT id, owner_type, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source, created_at, updated_at
            FROM model_pricing
            WHERE id = ?
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(Self::row_to_pricing).transpose()
    }

    async fn get_by_provider_model(
        &self,
        owner: &PricingOwner,
        provider: &str,
        model: &str,
    ) -> DbResult<Option<DbModelPricing>> {
        let (owner_type, owner_id) = Self::owner_to_parts(owner);

        let row = if owner_type.is_none() {
            query(
                r#"
                SELECT id, owner_type, owner_id, provider, model,
                       input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                       cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                       per_second, per_1m_characters, source, created_at, updated_at
                FROM model_pricing
                WHERE owner_type IS NULL AND provider = ? AND model = ?
                "#,
            )
            .bind(provider)
            .bind(model)
            .fetch_optional(&self.pool)
            .await?
        } else {
            query(
                r#"
                SELECT id, owner_type, owner_id, provider, model,
                       input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                       cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                       per_second, per_1m_characters, source, created_at, updated_at
                FROM model_pricing
                WHERE owner_type = ? AND owner_id = ? AND provider = ? AND model = ?
                "#,
            )
            .bind(owner_type)
            .bind(owner_id.map(|u| u.to_string()))
            .bind(provider)
            .bind(model)
            .fetch_optional(&self.pool)
            .await?
        };

        row.as_ref().map(Self::row_to_pricing).transpose()
    }

    async fn get_effective_pricing(
        &self,
        provider: &str,
        model: &str,
        user_id: Option<Uuid>,
        project_id: Option<Uuid>,
        org_id: Option<Uuid>,
    ) -> DbResult<Option<DbModelPricing>> {
        // Single query with priority ordering: user > project > org > global
        // Uses CASE expression to assign priority and LIMIT 1 to get highest priority match
        let row = query(
            r#"
            SELECT id, owner_type, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source, created_at, updated_at
            FROM model_pricing
            WHERE provider = ? AND model = ?
              AND (
                (? IS NOT NULL AND owner_type = 'user' AND owner_id = ?)
                OR (? IS NOT NULL AND owner_type = 'project' AND owner_id = ?)
                OR (? IS NOT NULL AND owner_type = 'organization' AND owner_id = ?)
                OR owner_type IS NULL
              )
            ORDER BY CASE
              WHEN owner_type = 'user' THEN 1
              WHEN owner_type = 'project' THEN 2
              WHEN owner_type = 'organization' THEN 3
              ELSE 4
            END
            LIMIT 1
            "#,
        )
        .bind(provider)
        .bind(model)
        .bind(user_id.map(|u| u.to_string()))
        .bind(user_id.map(|u| u.to_string()))
        .bind(project_id.map(|p| p.to_string()))
        .bind(project_id.map(|p| p.to_string()))
        .bind(org_id.map(|o| o.to_string()))
        .bind(org_id.map(|o| o.to_string()))
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(Self::row_to_pricing).transpose()
    }

    async fn list_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'organization' AND owner_id = ?";
        let binds = vec![org_id.to_string()];

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, binds, &params, cursor, limit)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, binds, limit).await
    }

    async fn count_by_org(&self, org_id: Uuid) -> DbResult<i64> {
        let row = query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'organization' AND owner_id = ?
            "#,
        )
        .bind(org_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        Ok(row.col::<i64>("count"))
    }

    async fn list_by_project(
        &self,
        project_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'project' AND owner_id = ?";
        let binds = vec![project_id.to_string()];

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, binds, &params, cursor, limit)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, binds, limit).await
    }

    async fn count_by_project(&self, project_id: Uuid) -> DbResult<i64> {
        let row = query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'project' AND owner_id = ?
            "#,
        )
        .bind(project_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        Ok(row.col::<i64>("count"))
    }

    async fn list_by_user(
        &self,
        user_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'user' AND owner_id = ?";
        let binds = vec![user_id.to_string()];

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, binds, &params, cursor, limit)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, binds, limit).await
    }

    async fn count_by_user(&self, user_id: Uuid) -> DbResult<i64> {
        let row = query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'user' AND owner_id = ?
            "#,
        )
        .bind(user_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        Ok(row.col::<i64>("count"))
    }

    async fn list_global(&self, params: ListParams) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type IS NULL";
        let binds = vec![];

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, binds, &params, cursor, limit)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, binds, limit).await
    }

    async fn count_global(&self) -> DbResult<i64> {
        let row = query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type IS NULL
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.col::<i64>("count"))
    }

    async fn list_by_provider(
        &self,
        provider: &str,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE provider = ?";
        let binds = vec![provider.to_string()];

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, binds, &params, cursor, limit)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, binds, limit).await
    }

    async fn count_by_provider(&self, provider: &str) -> DbResult<i64> {
        let row = query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE provider = ?
            "#,
        )
        .bind(provider)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.col::<i64>("count"))
    }

    async fn list_billing_overlay_rows(&self) -> DbResult<Vec<DbModelPricing>> {
        let rows = query(
            r#"
            SELECT id, owner_type, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source, created_at, updated_at
            FROM model_pricing
            WHERE owner_type IS NULL OR owner_type = 'organization'
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(Self::row_to_pricing).collect()
    }

    async fn update(&self, id: Uuid, input: UpdateModelPricing) -> DbResult<DbModelPricing> {
        let now = truncate_to_millis(chrono::Utc::now());

        query(
            r#"
            UPDATE model_pricing SET
                input_per_1m_tokens = COALESCE(?, input_per_1m_tokens),
                output_per_1m_tokens = COALESCE(?, output_per_1m_tokens),
                per_image = COALESCE(?, per_image),
                per_request = COALESCE(?, per_request),
                cached_input_per_1m_tokens = COALESCE(?, cached_input_per_1m_tokens),
                cache_write_per_1m_tokens = COALESCE(?, cache_write_per_1m_tokens),
                reasoning_per_1m_tokens = COALESCE(?, reasoning_per_1m_tokens),
                source = COALESCE(?, source),
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(input.input_per_1m_tokens)
        .bind(input.output_per_1m_tokens)
        .bind(input.per_image)
        .bind(input.per_request)
        .bind(input.cached_input_per_1m_tokens)
        .bind(input.cache_write_per_1m_tokens)
        .bind(input.reasoning_per_1m_tokens)
        .bind(input.source.map(|s| s.as_str()))
        .bind(now)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;

        self.get_by_id(id).await?.ok_or(DbError::NotFound)
    }

    async fn delete(&self, id: Uuid) -> DbResult<()> {
        query("DELETE FROM model_pricing WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    async fn upsert(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        let id = Uuid::new_v4();
        let now = truncate_to_millis(chrono::Utc::now());
        let (owner_type, owner_id) = Self::owner_to_parts(&input.owner);

        // Use INSERT ... ON CONFLICT DO UPDATE for atomic upsert
        // Different conflict targets for global vs scoped pricing due to partial indexes
        if owner_type.is_none() {
            // Global pricing: conflict on (provider, model) where owner_type IS NULL
            query(
                r#"
                INSERT INTO model_pricing (
                    id, owner_type, owner_id, provider, model,
                    input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                    cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                    per_second, per_1m_characters, source, created_at, updated_at
                )
                VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (provider, model) WHERE owner_type IS NULL
                DO UPDATE SET
                    input_per_1m_tokens = excluded.input_per_1m_tokens,
                    output_per_1m_tokens = excluded.output_per_1m_tokens,
                    per_image = excluded.per_image,
                    per_request = excluded.per_request,
                    cached_input_per_1m_tokens = excluded.cached_input_per_1m_tokens,
                    cache_write_per_1m_tokens = excluded.cache_write_per_1m_tokens,
                    reasoning_per_1m_tokens = excluded.reasoning_per_1m_tokens,
                    per_second = excluded.per_second,
                    per_1m_characters = excluded.per_1m_characters,
                    source = excluded.source,
                    updated_at = excluded.updated_at
                "#,
            )
            .bind(id.to_string())
            .bind(&input.provider)
            .bind(&input.model)
            .bind(input.input_per_1m_tokens)
            .bind(input.output_per_1m_tokens)
            .bind(input.per_image)
            .bind(input.per_request)
            .bind(input.cached_input_per_1m_tokens)
            .bind(input.cache_write_per_1m_tokens)
            .bind(input.reasoning_per_1m_tokens)
            .bind(input.per_second)
            .bind(input.per_1m_characters)
            .bind(input.source.as_str())
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        } else {
            // Scoped pricing: conflict on (owner_type, owner_id, provider, model)
            query(
                r#"
                INSERT INTO model_pricing (
                    id, owner_type, owner_id, provider, model,
                    input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                    cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                    per_second, per_1m_characters, source, created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (owner_type, owner_id, provider, model) WHERE owner_type IS NOT NULL
                DO UPDATE SET
                    input_per_1m_tokens = excluded.input_per_1m_tokens,
                    output_per_1m_tokens = excluded.output_per_1m_tokens,
                    per_image = excluded.per_image,
                    per_request = excluded.per_request,
                    cached_input_per_1m_tokens = excluded.cached_input_per_1m_tokens,
                    cache_write_per_1m_tokens = excluded.cache_write_per_1m_tokens,
                    reasoning_per_1m_tokens = excluded.reasoning_per_1m_tokens,
                    per_second = excluded.per_second,
                    per_1m_characters = excluded.per_1m_characters,
                    source = excluded.source,
                    updated_at = excluded.updated_at
                "#,
            )
            .bind(id.to_string())
            .bind(owner_type)
            .bind(owner_id.map(|u| u.to_string()))
            .bind(&input.provider)
            .bind(&input.model)
            .bind(input.input_per_1m_tokens)
            .bind(input.output_per_1m_tokens)
            .bind(input.per_image)
            .bind(input.per_request)
            .bind(input.cached_input_per_1m_tokens)
            .bind(input.cache_write_per_1m_tokens)
            .bind(input.reasoning_per_1m_tokens)
            .bind(input.per_second)
            .bind(input.per_1m_characters)
            .bind(input.source.as_str())
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        }

        // Fetch the result (either newly inserted or updated row)
        self.get_by_provider_model(&input.owner, &input.provider, &input.model)
            .await?
            .ok_or_else(|| DbError::Internal("Upsert failed to retrieve result".to_string()))
    }

    async fn bulk_upsert(&self, entries: Vec<CreateModelPricing>) -> DbResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let now = truncate_to_millis(chrono::Utc::now());
        let count = entries.len();

        let mut tx = begin(&self.pool).await?;

        for entry in entries {
            let id = Uuid::new_v4();
            let (owner_type, owner_id) = Self::owner_to_parts(&entry.owner);

            if owner_type.is_none() {
                query(
                    r#"
                    INSERT INTO model_pricing (
                        id, owner_type, owner_id, provider, model,
                        input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                        cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                        per_second, per_1m_characters, source, created_at, updated_at
                    )
                    VALUES (?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT (provider, model) WHERE owner_type IS NULL
                    DO UPDATE SET
                        input_per_1m_tokens = excluded.input_per_1m_tokens,
                        output_per_1m_tokens = excluded.output_per_1m_tokens,
                        per_image = excluded.per_image,
                        per_request = excluded.per_request,
                        cached_input_per_1m_tokens = excluded.cached_input_per_1m_tokens,
                        cache_write_per_1m_tokens = excluded.cache_write_per_1m_tokens,
                        reasoning_per_1m_tokens = excluded.reasoning_per_1m_tokens,
                        per_second = excluded.per_second,
                        per_1m_characters = excluded.per_1m_characters,
                        source = excluded.source,
                        updated_at = excluded.updated_at
                    "#,
                )
                .bind(id.to_string())
                .bind(&entry.provider)
                .bind(&entry.model)
                .bind(entry.input_per_1m_tokens)
                .bind(entry.output_per_1m_tokens)
                .bind(entry.per_image)
                .bind(entry.per_request)
                .bind(entry.cached_input_per_1m_tokens)
                .bind(entry.cache_write_per_1m_tokens)
                .bind(entry.reasoning_per_1m_tokens)
                .bind(entry.per_second)
                .bind(entry.per_1m_characters)
                .bind(entry.source.as_str())
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            } else {
                query(
                    r#"
                    INSERT INTO model_pricing (
                        id, owner_type, owner_id, provider, model,
                        input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                        cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                        per_second, per_1m_characters, source, created_at, updated_at
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT (owner_type, owner_id, provider, model) WHERE owner_type IS NOT NULL
                    DO UPDATE SET
                        input_per_1m_tokens = excluded.input_per_1m_tokens,
                        output_per_1m_tokens = excluded.output_per_1m_tokens,
                        per_image = excluded.per_image,
                        per_request = excluded.per_request,
                        cached_input_per_1m_tokens = excluded.cached_input_per_1m_tokens,
                        cache_write_per_1m_tokens = excluded.cache_write_per_1m_tokens,
                        reasoning_per_1m_tokens = excluded.reasoning_per_1m_tokens,
                        per_second = excluded.per_second,
                        per_1m_characters = excluded.per_1m_characters,
                        source = excluded.source,
                        updated_at = excluded.updated_at
                    "#,
                )
                .bind(id.to_string())
                .bind(owner_type)
                .bind(owner_id.map(|u| u.to_string()))
                .bind(&entry.provider)
                .bind(&entry.model)
                .bind(entry.input_per_1m_tokens)
                .bind(entry.output_per_1m_tokens)
                .bind(entry.per_image)
                .bind(entry.per_request)
                .bind(entry.cached_input_per_1m_tokens)
                .bind(entry.cache_write_per_1m_tokens)
                .bind(entry.reasoning_per_1m_tokens)
                .bind(entry.per_second)
                .bind(entry.per_1m_characters)
                .bind(entry.source.as_str())
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::repos::{ListParams, ModelPricingRepo};

    async fn create_test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Failed to create in-memory SQLite pool");

        sqlx::query(
            r#"
            CREATE TABLE model_pricing (
                id TEXT PRIMARY KEY NOT NULL,
                owner_type TEXT,
                owner_id TEXT,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                input_per_1m_tokens INTEGER NOT NULL DEFAULT 0,
                output_per_1m_tokens INTEGER NOT NULL DEFAULT 0,
                per_image INTEGER,
                per_request INTEGER,
                cached_input_per_1m_tokens INTEGER,
                cache_write_per_1m_tokens INTEGER,
                reasoning_per_1m_tokens INTEGER,
                per_second INTEGER,
                per_1m_characters INTEGER,
                source TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create model_pricing table");

        // Use partial unique indexes to handle SQLite's NULL distinctness in UNIQUE constraints
        sqlx::query(
            r#"
            CREATE UNIQUE INDEX idx_model_pricing_unique_global
                ON model_pricing(provider, model) WHERE owner_type IS NULL
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create global pricing unique index");

        sqlx::query(
            r#"
            CREATE UNIQUE INDEX idx_model_pricing_unique_scoped
                ON model_pricing(owner_type, owner_id, provider, model) WHERE owner_type IS NOT NULL
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create scoped pricing unique index");

        pool
    }

    fn create_global_pricing(provider: &str, model: &str) -> CreateModelPricing {
        CreateModelPricing {
            owner: PricingOwner::Global,
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1m_tokens: 1000,
            output_per_1m_tokens: 2000,
            per_image: None,
            per_request: None,
            cached_input_per_1m_tokens: None,
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: PricingSource::Manual,
        }
    }

    fn create_org_pricing(org_id: Uuid, provider: &str, model: &str) -> CreateModelPricing {
        CreateModelPricing {
            owner: PricingOwner::Organization { org_id },
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1m_tokens: 1500,
            output_per_1m_tokens: 2500,
            per_image: Some(100),
            per_request: None,
            cached_input_per_1m_tokens: Some(500),
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: PricingSource::Manual,
        }
    }

    fn create_project_pricing(project_id: Uuid, provider: &str, model: &str) -> CreateModelPricing {
        CreateModelPricing {
            owner: PricingOwner::Project { project_id },
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1m_tokens: 1800,
            output_per_1m_tokens: 2800,
            per_image: None,
            per_request: Some(50),
            cached_input_per_1m_tokens: None,
            cache_write_per_1m_tokens: Some(200),
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: PricingSource::ProviderApi,
        }
    }

    fn create_user_pricing(user_id: Uuid, provider: &str, model: &str) -> CreateModelPricing {
        CreateModelPricing {
            owner: PricingOwner::User { user_id },
            provider: provider.to_string(),
            model: model.to_string(),
            input_per_1m_tokens: 2000,
            output_per_1m_tokens: 3000,
            per_image: Some(200),
            per_request: Some(75),
            cached_input_per_1m_tokens: Some(800),
            cache_write_per_1m_tokens: Some(300),
            reasoning_per_1m_tokens: Some(4000),
            per_second: None,
            per_1m_characters: None,
            source: PricingSource::Default,
        }
    }

    // ===================
    // Basic CRUD tests
    // ===================

    #[tokio::test]
    async fn test_create_global_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let pricing = repo.create(input).await.expect("Failed to create pricing");

        assert!(!pricing.id.is_nil());
        assert!(matches!(pricing.owner, PricingOwner::Global));
        assert_eq!(pricing.provider, "openai");
        assert_eq!(pricing.model, "gpt-4");
        assert_eq!(pricing.input_per_1m_tokens, 1000);
        assert_eq!(pricing.output_per_1m_tokens, 2000);
        assert_eq!(pricing.source, PricingSource::Manual);
    }

    #[tokio::test]
    async fn test_create_org_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let org_id = Uuid::new_v4();

        let input = create_org_pricing(org_id, "anthropic", "claude-3");
        let pricing = repo.create(input).await.expect("Failed to create pricing");

        assert!(matches!(pricing.owner, PricingOwner::Organization { org_id: id } if id == org_id));
        assert_eq!(pricing.provider, "anthropic");
        assert_eq!(pricing.per_image, Some(100));
        assert_eq!(pricing.cached_input_per_1m_tokens, Some(500));
    }

    #[tokio::test]
    async fn test_create_project_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let project_id = Uuid::new_v4();

        let input = create_project_pricing(project_id, "google", "gemini-pro");
        let pricing = repo.create(input).await.expect("Failed to create pricing");

        assert!(
            matches!(pricing.owner, PricingOwner::Project { project_id: id } if id == project_id)
        );
        assert_eq!(pricing.per_request, Some(50));
        assert_eq!(pricing.cache_write_per_1m_tokens, Some(200));
        assert_eq!(pricing.source, PricingSource::ProviderApi);
    }

    #[tokio::test]
    async fn test_create_user_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let user_id = Uuid::new_v4();

        let input = create_user_pricing(user_id, "openai", "gpt-4-turbo");
        let pricing = repo.create(input).await.expect("Failed to create pricing");

        assert!(matches!(pricing.owner, PricingOwner::User { user_id: id } if id == user_id));
        assert_eq!(pricing.reasoning_per_1m_tokens, Some(4000));
        assert_eq!(pricing.source, PricingSource::Default);
    }

    #[tokio::test]
    async fn test_create_duplicate_pricing_fails() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let org_id = Uuid::new_v4();

        let input = create_org_pricing(org_id, "openai", "gpt-4");
        repo.create(input)
            .await
            .expect("First create should succeed");

        let input2 = create_org_pricing(org_id, "openai", "gpt-4");
        let result = repo.create(input2).await;

        assert!(matches!(result, Err(DbError::Conflict(_))));
    }

    #[tokio::test]
    async fn test_create_duplicate_global_pricing_fails() {
        // This test verifies that duplicate global pricing entries are rejected.
        // SQLite uses partial unique indexes to handle NULL values correctly:
        // - idx_model_pricing_unique_global: UNIQUE(provider, model) WHERE owner_type IS NULL
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        repo.create(input)
            .await
            .expect("First create should succeed");

        let input2 = create_global_pricing("openai", "gpt-4");
        let result = repo.create(input2).await;

        // Duplicate global pricing should fail with Conflict
        assert!(matches!(result, Err(DbError::Conflict(_))));
    }

    #[tokio::test]
    async fn test_same_model_different_owners_allowed() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let org_id = Uuid::new_v4();

        // Create global pricing
        let input1 = create_global_pricing("openai", "gpt-4");
        repo.create(input1)
            .await
            .expect("Global create should succeed");

        // Create org pricing for same model
        let input2 = create_org_pricing(org_id, "openai", "gpt-4");
        repo.create(input2)
            .await
            .expect("Org create should succeed for same model");

        // Both should exist
        let global_pricing = repo
            .get_by_provider_model(&PricingOwner::Global, "openai", "gpt-4")
            .await
            .expect("Query should succeed")
            .expect("Global pricing should exist");
        assert!(matches!(global_pricing.owner, PricingOwner::Global));

        let org_pricing = repo
            .get_by_provider_model(&PricingOwner::Organization { org_id }, "openai", "gpt-4")
            .await
            .expect("Query should succeed")
            .expect("Org pricing should exist");
        assert!(matches!(
            org_pricing.owner,
            PricingOwner::Organization { .. }
        ));
    }

    #[tokio::test]
    async fn test_get_by_id() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let created = repo.create(input).await.expect("Failed to create pricing");

        let fetched = repo
            .get_by_id(created.id)
            .await
            .expect("Query should succeed")
            .expect("Pricing should exist");

        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.provider, "openai");
        assert_eq!(fetched.model, "gpt-4");
    }

    #[tokio::test]
    async fn test_get_by_id_not_found() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let result = repo
            .get_by_id(Uuid::new_v4())
            .await
            .expect("Query should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_by_provider_model_global() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        repo.create(input).await.expect("Failed to create pricing");

        let fetched = repo
            .get_by_provider_model(&PricingOwner::Global, "openai", "gpt-4")
            .await
            .expect("Query should succeed")
            .expect("Pricing should exist");

        assert!(matches!(fetched.owner, PricingOwner::Global));
        assert_eq!(fetched.provider, "openai");
    }

    #[tokio::test]
    async fn test_get_by_provider_model_org() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);
        let org_id = Uuid::new_v4();

        let input = create_org_pricing(org_id, "anthropic", "claude-3");
        repo.create(input).await.expect("Failed to create pricing");

        let fetched = repo
            .get_by_provider_model(
                &PricingOwner::Organization { org_id },
                "anthropic",
                "claude-3",
            )
            .await
            .expect("Query should succeed")
            .expect("Pricing should exist");

        assert!(matches!(fetched.owner, PricingOwner::Organization { org_id: id } if id == org_id));
    }

    #[tokio::test]
    async fn test_get_by_provider_model_not_found() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let result = repo
            .get_by_provider_model(&PricingOwner::Global, "nonexistent", "model")
            .await
            .expect("Query should succeed");

        assert!(result.is_none());
    }

    // ===================
    // Effective pricing tests (hierarchical lookup)
    // ===================

    #[tokio::test]
    async fn test_get_effective_pricing_returns_user_level_first() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        // Create pricing at all levels
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_org_pricing(org_id, "openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_project_pricing(project_id, "openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_user_pricing(user_id, "openai", "gpt-4"))
            .await
            .unwrap();

        let effective = repo
            .get_effective_pricing(
                "openai",
                "gpt-4",
                Some(user_id),
                Some(project_id),
                Some(org_id),
            )
            .await
            .expect("Query should succeed")
            .expect("Should find pricing");

        // Should return user-level (highest priority)
        assert!(matches!(effective.owner, PricingOwner::User { .. }));
        assert_eq!(effective.input_per_1m_tokens, 2000); // User pricing values
    }

    #[tokio::test]
    async fn test_get_effective_pricing_returns_project_level_when_no_user() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        // Create pricing at org, project, and global levels only
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_org_pricing(org_id, "openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_project_pricing(project_id, "openai", "gpt-4"))
            .await
            .unwrap();

        let effective = repo
            .get_effective_pricing(
                "openai",
                "gpt-4",
                Some(user_id),
                Some(project_id),
                Some(org_id),
            )
            .await
            .expect("Query should succeed")
            .expect("Should find pricing");

        // Should return project-level
        assert!(matches!(effective.owner, PricingOwner::Project { .. }));
        assert_eq!(effective.input_per_1m_tokens, 1800); // Project pricing values
    }

    #[tokio::test]
    async fn test_get_effective_pricing_returns_org_level_when_no_project() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        // Create pricing at org and global levels only
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_org_pricing(org_id, "openai", "gpt-4"))
            .await
            .unwrap();

        let effective = repo
            .get_effective_pricing(
                "openai",
                "gpt-4",
                Some(user_id),
                Some(project_id),
                Some(org_id),
            )
            .await
            .expect("Query should succeed")
            .expect("Should find pricing");

        // Should return org-level
        assert!(matches!(effective.owner, PricingOwner::Organization { .. }));
        assert_eq!(effective.input_per_1m_tokens, 1500); // Org pricing values
    }

    #[tokio::test]
    async fn test_get_effective_pricing_returns_global_as_fallback() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        // Create only global pricing
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();

        let effective = repo
            .get_effective_pricing(
                "openai",
                "gpt-4",
                Some(user_id),
                Some(project_id),
                Some(org_id),
            )
            .await
            .expect("Query should succeed")
            .expect("Should find pricing");

        // Should return global
        assert!(matches!(effective.owner, PricingOwner::Global));
        assert_eq!(effective.input_per_1m_tokens, 1000); // Global pricing values
    }

    #[tokio::test]
    async fn test_get_effective_pricing_returns_none_when_no_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let result = repo
            .get_effective_pricing(
                "openai",
                "gpt-4",
                Some(Uuid::new_v4()),
                Some(Uuid::new_v4()),
                Some(Uuid::new_v4()),
            )
            .await
            .expect("Query should succeed");

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_effective_pricing_with_no_user_id() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();

        repo.create(create_project_pricing(project_id, "openai", "gpt-4"))
            .await
            .unwrap();

        let effective = repo
            .get_effective_pricing("openai", "gpt-4", None, Some(project_id), Some(org_id))
            .await
            .expect("Query should succeed")
            .expect("Should find pricing");

        assert!(matches!(effective.owner, PricingOwner::Project { .. }));
    }

    // ===================
    // List operations tests
    // ===================

    #[tokio::test]
    async fn test_list_by_org() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();
        let other_org_id = Uuid::new_v4();

        repo.create(create_org_pricing(org_id, "openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_org_pricing(org_id, "anthropic", "claude-3"))
            .await
            .unwrap();
        repo.create(create_org_pricing(other_org_id, "google", "gemini"))
            .await
            .unwrap();

        let result = repo
            .list_by_org(org_id, ListParams::default())
            .await
            .expect("Query should succeed");

        assert_eq!(result.items.len(), 2);
        // Sorted by created_at DESC, id DESC (most recently created first)
    }

    #[tokio::test]
    async fn test_list_by_org_empty() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let result = repo
            .list_by_org(Uuid::new_v4(), ListParams::default())
            .await
            .expect("Query should succeed");

        assert!(result.items.is_empty());
    }

    #[tokio::test]
    async fn test_list_by_project() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let project_id = Uuid::new_v4();

        repo.create(create_project_pricing(project_id, "openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_project_pricing(project_id, "openai", "gpt-3.5"))
            .await
            .unwrap();

        let result = repo
            .list_by_project(project_id, ListParams::default())
            .await
            .expect("Query should succeed");

        assert_eq!(result.items.len(), 2);
        // Sorted by created_at DESC, id DESC (most recently created first)
    }

    #[tokio::test]
    async fn test_list_by_user() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let user_id = Uuid::new_v4();

        repo.create(create_user_pricing(user_id, "openai", "gpt-4"))
            .await
            .unwrap();

        let result = repo
            .list_by_user(user_id, ListParams::default())
            .await
            .expect("Query should succeed");

        assert_eq!(result.items.len(), 1);
        assert!(matches!(result.items[0].owner, PricingOwner::User { .. }));
    }

    #[tokio::test]
    async fn test_list_global() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_global_pricing("anthropic", "claude-3"))
            .await
            .unwrap();
        repo.create(create_global_pricing("google", "gemini"))
            .await
            .unwrap();

        let result = repo
            .list_global(ListParams::default())
            .await
            .expect("Query should succeed");

        assert_eq!(result.items.len(), 3);
        // Sorted by created_at DESC, id DESC (most recently created first)
    }

    #[tokio::test]
    async fn test_list_by_provider() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();

        // Create pricing for same provider at different levels
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();
        repo.create(create_global_pricing("openai", "gpt-3.5"))
            .await
            .unwrap();
        repo.create(create_org_pricing(org_id, "openai", "gpt-4-turbo"))
            .await
            .unwrap();
        repo.create(create_global_pricing("anthropic", "claude"))
            .await
            .unwrap();

        let result = repo
            .list_by_provider("openai", ListParams::default())
            .await
            .expect("Query should succeed");

        assert_eq!(result.items.len(), 3);
        // All should be openai
        assert!(result.items.iter().all(|p| p.provider == "openai"));
    }

    // ===================
    // Update tests
    // ===================

    #[tokio::test]
    async fn test_update_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let created = repo.create(input).await.expect("Failed to create pricing");

        let update = UpdateModelPricing {
            input_per_1m_tokens: Some(5000),
            output_per_1m_tokens: Some(10000),
            per_image: Some(500),
            per_request: None,
            cached_input_per_1m_tokens: Some(2500),
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: Some(15000),
            per_second: None,
            per_1m_characters: None,
            source: Some(PricingSource::ProviderApi),
        };

        let updated = repo
            .update(created.id, update)
            .await
            .expect("Update should succeed");

        assert_eq!(updated.input_per_1m_tokens, 5000);
        assert_eq!(updated.output_per_1m_tokens, 10000);
        assert_eq!(updated.per_image, Some(500));
        assert_eq!(updated.cached_input_per_1m_tokens, Some(2500));
        assert_eq!(updated.reasoning_per_1m_tokens, Some(15000));
        assert_eq!(updated.source, PricingSource::ProviderApi);
        // Owner, provider, model should remain unchanged
        assert!(matches!(updated.owner, PricingOwner::Global));
        assert_eq!(updated.provider, "openai");
        assert_eq!(updated.model, "gpt-4");
    }

    #[tokio::test]
    async fn test_update_partial_fields() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let created = repo.create(input).await.expect("Failed to create pricing");

        // Only update input price
        let update = UpdateModelPricing {
            input_per_1m_tokens: Some(9999),
            output_per_1m_tokens: None,
            per_image: None,
            per_request: None,
            cached_input_per_1m_tokens: None,
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: None,
        };

        let updated = repo
            .update(created.id, update)
            .await
            .expect("Update should succeed");

        assert_eq!(updated.input_per_1m_tokens, 9999);
        // Other fields should remain unchanged
        assert_eq!(updated.output_per_1m_tokens, 2000); // Original value
        assert_eq!(updated.source, PricingSource::Manual); // Original value
    }

    #[tokio::test]
    async fn test_update_not_found() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let update = UpdateModelPricing {
            input_per_1m_tokens: Some(5000),
            output_per_1m_tokens: None,
            per_image: None,
            per_request: None,
            cached_input_per_1m_tokens: None,
            cache_write_per_1m_tokens: None,
            reasoning_per_1m_tokens: None,
            per_second: None,
            per_1m_characters: None,
            source: None,
        };

        let result = repo.update(Uuid::new_v4(), update).await;

        assert!(matches!(result, Err(DbError::NotFound)));
    }

    // ===================
    // Delete tests
    // ===================

    #[tokio::test]
    async fn test_delete_pricing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let created = repo.create(input).await.expect("Failed to create pricing");

        repo.delete(created.id)
            .await
            .expect("Delete should succeed");

        let result = repo
            .get_by_id(created.id)
            .await
            .expect("Query should succeed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_succeeds() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        // Delete of nonexistent ID should not fail
        let result = repo.delete(Uuid::new_v4()).await;
        assert!(result.is_ok());
    }

    // ===================
    // Upsert tests
    // ===================

    #[tokio::test]
    async fn test_upsert_creates_when_not_exists() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let input = create_global_pricing("openai", "gpt-4");
        let result = repo.upsert(input).await.expect("Upsert should succeed");

        assert_eq!(result.provider, "openai");
        assert_eq!(result.model, "gpt-4");
        assert_eq!(result.input_per_1m_tokens, 1000);
    }

    #[tokio::test]
    async fn test_upsert_updates_when_exists() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        // First create
        let input1 = create_global_pricing("openai", "gpt-4");
        let created = repo
            .upsert(input1)
            .await
            .expect("First upsert should succeed");

        // Second upsert with different values
        let mut input2 = create_global_pricing("openai", "gpt-4");
        input2.input_per_1m_tokens = 9999;
        input2.output_per_1m_tokens = 8888;

        let updated = repo
            .upsert(input2)
            .await
            .expect("Second upsert should succeed");

        // Should be same ID (updated, not created)
        assert_eq!(updated.id, created.id);
        // Values should be updated
        assert_eq!(updated.input_per_1m_tokens, 9999);
        assert_eq!(updated.output_per_1m_tokens, 8888);
    }

    #[tokio::test]
    async fn test_upsert_with_different_owner_creates_new() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let org_id = Uuid::new_v4();

        // Create global
        let global = repo
            .upsert(create_global_pricing("openai", "gpt-4"))
            .await
            .expect("Upsert should succeed");

        // Upsert org-level (different owner)
        let org = repo
            .upsert(create_org_pricing(org_id, "openai", "gpt-4"))
            .await
            .expect("Upsert should succeed");

        // Should be different IDs
        assert_ne!(global.id, org.id);
        assert!(matches!(global.owner, PricingOwner::Global));
        assert!(matches!(org.owner, PricingOwner::Organization { .. }));
    }

    // ===================
    // Bulk upsert tests
    // ===================

    #[tokio::test]
    async fn test_bulk_upsert_creates_all() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let entries = vec![
            create_global_pricing("openai", "gpt-4"),
            create_global_pricing("anthropic", "claude-3"),
            create_global_pricing("google", "gemini"),
        ];

        let count = repo
            .bulk_upsert(entries)
            .await
            .expect("Bulk upsert should succeed");

        assert_eq!(count, 3);

        let result = repo
            .list_global(ListParams::default())
            .await
            .expect("List should succeed");
        assert_eq!(result.items.len(), 3);
    }

    #[tokio::test]
    async fn test_bulk_upsert_updates_existing() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        // Create initial entries
        repo.create(create_global_pricing("openai", "gpt-4"))
            .await
            .unwrap();

        // Bulk upsert with mix of new and existing
        let mut updated_entry = create_global_pricing("openai", "gpt-4");
        updated_entry.input_per_1m_tokens = 9999;

        let entries = vec![
            updated_entry,
            create_global_pricing("anthropic", "claude-3"),
        ];

        let count = repo
            .bulk_upsert(entries)
            .await
            .expect("Bulk upsert should succeed");

        assert_eq!(count, 2);

        // Verify update happened
        let openai = repo
            .get_by_provider_model(&PricingOwner::Global, "openai", "gpt-4")
            .await
            .expect("Query should succeed")
            .expect("Should exist");
        assert_eq!(openai.input_per_1m_tokens, 9999);
    }

    #[tokio::test]
    async fn test_bulk_upsert_empty() {
        let pool = create_test_pool().await;
        let repo = SqliteModelPricingRepo::new(pool);

        let count = repo
            .bulk_upsert(vec![])
            .await
            .expect("Bulk upsert should succeed");

        assert_eq!(count, 0);
    }

    // ===================
    // Helper function tests
    // ===================

    #[test]
    fn test_parse_owner_global() {
        let result = SqliteModelPricingRepo::parse_owner(None, None);
        assert!(matches!(result, Ok(PricingOwner::Global)));

        let result = SqliteModelPricingRepo::parse_owner(Some(""), None);
        assert!(matches!(result, Ok(PricingOwner::Global)));
    }

    #[test]
    fn test_parse_owner_organization() {
        let org_id = Uuid::new_v4();
        let result =
            SqliteModelPricingRepo::parse_owner(Some("organization"), Some(&org_id.to_string()));
        assert!(matches!(result, Ok(PricingOwner::Organization { .. })));
    }

    #[test]
    fn test_parse_owner_project() {
        let project_id = Uuid::new_v4();
        let result =
            SqliteModelPricingRepo::parse_owner(Some("project"), Some(&project_id.to_string()));
        assert!(matches!(result, Ok(PricingOwner::Project { .. })));
    }

    #[test]
    fn test_parse_owner_user() {
        let user_id = Uuid::new_v4();
        let result = SqliteModelPricingRepo::parse_owner(Some("user"), Some(&user_id.to_string()));
        assert!(matches!(result, Ok(PricingOwner::User { .. })));
    }

    #[test]
    fn test_parse_owner_invalid() {
        let result = SqliteModelPricingRepo::parse_owner(Some("invalid"), Some("id"));
        assert!(matches!(result, Err(DbError::Internal(_))));

        let result = SqliteModelPricingRepo::parse_owner(Some("organization"), None);
        assert!(matches!(result, Err(DbError::Internal(_))));
    }

    #[test]
    fn test_owner_to_parts_global() {
        let (owner_type, owner_id) = SqliteModelPricingRepo::owner_to_parts(&PricingOwner::Global);
        assert!(owner_type.is_none());
        assert!(owner_id.is_none());
    }

    #[test]
    fn test_owner_to_parts_organization() {
        let org_id = Uuid::new_v4();
        let (owner_type, owner_id) =
            SqliteModelPricingRepo::owner_to_parts(&PricingOwner::Organization { org_id });
        assert_eq!(owner_type, Some("organization"));
        assert_eq!(owner_id, Some(org_id));
    }

    #[test]
    fn test_owner_to_parts_project() {
        let project_id = Uuid::new_v4();
        let (owner_type, owner_id) =
            SqliteModelPricingRepo::owner_to_parts(&PricingOwner::Project { project_id });
        assert_eq!(owner_type, Some("project"));
        assert_eq!(owner_id, Some(project_id));
    }

    #[test]
    fn test_owner_to_parts_user() {
        let user_id = Uuid::new_v4();
        let (owner_type, owner_id) =
            SqliteModelPricingRepo::owner_to_parts(&PricingOwner::User { user_id });
        assert_eq!(owner_type, Some("user"));
        assert_eq!(owner_id, Some(user_id));
    }
}
