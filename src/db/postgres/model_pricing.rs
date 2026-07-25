use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    db::{
        error::{DbError, DbResult},
        repos::{
            Cursor, CursorDirection, ListParams, ListResult, ModelPricingRepo, PageCursors,
            cursor_from_row,
        },
    },
    models::{CreateModelPricing, DbModelPricing, PricingOwner, PricingSource, UpdateModelPricing},
};

pub struct PostgresModelPricingRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresModelPricingRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
    }

    fn parse_owner(owner_type: Option<&str>, owner_id: Option<Uuid>) -> DbResult<PricingOwner> {
        match (owner_type, owner_id) {
            (None, _) => Ok(PricingOwner::Global),
            (Some("organization"), Some(id)) => Ok(PricingOwner::Organization { org_id: id }),
            (Some("team"), Some(id)) => Ok(PricingOwner::Team { team_id: id }),
            (Some("project"), Some(id)) => Ok(PricingOwner::Project { project_id: id }),
            (Some("user"), Some(id)) => Ok(PricingOwner::User { user_id: id }),
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

    fn row_to_pricing(row: &sqlx::postgres::PgRow) -> DbResult<DbModelPricing> {
        let owner_type: Option<String> = row.get("owner_type");
        let owner_id: Option<Uuid> = row.get("owner_id");
        let source_str: String = row.get("source");

        Ok(DbModelPricing {
            id: row.get("id"),
            owner: Self::parse_owner(owner_type.as_deref(), owner_id)?,
            provider: row.get("provider"),
            model: row.get("model"),
            input_per_1m_tokens: row.get("input_per_1m_tokens"),
            output_per_1m_tokens: row.get("output_per_1m_tokens"),
            per_image: row.get("per_image"),
            per_request: row.get("per_request"),
            cached_input_per_1m_tokens: row.get("cached_input_per_1m_tokens"),
            cache_write_per_1m_tokens: row.get("cache_write_per_1m_tokens"),
            reasoning_per_1m_tokens: row.get("reasoning_per_1m_tokens"),
            per_second: row.get("per_second"),
            per_1m_characters: row.get("per_1m_characters"),
            source: PricingSource::parse(&source_str),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    /// Helper method for cursor-based pagination with a custom WHERE clause.
    async fn list_with_cursor(
        &self,
        where_clause: &str,
        cursor: &Cursor,
        params: &ListParams,
        limit: i64,
        bind_uuid: Option<Uuid>,
        bind_str: Option<&str>,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let fetch_limit = limit + 1;
        let (comparison, order, should_reverse) =
            params.sort_order.cursor_query_params(params.direction);

        // Determine parameter numbering based on binds
        let (cursor_ts_param, cursor_id_param, limit_param) = match (&bind_uuid, &bind_str) {
            (Some(_), None) => ("$2", "$3", "$4"),
            (None, Some(_)) => ("$2", "$3", "$4"),
            (None, None) => ("$1", "$2", "$3"),
            _ => ("$3", "$4", "$5"),
        };

        let cursor_condition = if where_clause.is_empty() {
            format!(
                "WHERE ROW(created_at, id) {} ROW({}, {})",
                comparison, cursor_ts_param, cursor_id_param
            )
        } else {
            format!(
                "{} AND ROW(created_at, id) {} ROW({}, {})",
                where_clause, comparison, cursor_ts_param, cursor_id_param
            )
        };

        let query = format!(
            r#"
            SELECT id, owner_type::TEXT, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source::TEXT, created_at, updated_at
            FROM model_pricing
            {}
            ORDER BY created_at {}, id {}
            LIMIT {}
            "#,
            cursor_condition, order, order, limit_param
        );

        let mut query_builder = sqlx::query(&query);
        if let Some(uuid) = bind_uuid {
            query_builder = query_builder.bind(uuid);
        }
        if let Some(s) = bind_str {
            query_builder = query_builder.bind(s);
        }
        query_builder = query_builder
            .bind(cursor.created_at)
            .bind(cursor.id)
            .bind(fetch_limit);

        let rows = query_builder.fetch_all(&self.read_pool).await?;

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

    /// Helper method for first page pagination (no cursor provided).
    async fn list_first_page(
        &self,
        where_clause: &str,
        limit: i64,
        bind_uuid: Option<Uuid>,
        bind_str: Option<&str>,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let fetch_limit = limit + 1;

        // Determine parameter numbering based on binds
        let limit_param = match (&bind_uuid, &bind_str) {
            (Some(_), None) => "$2",
            (None, Some(_)) => "$2",
            (None, None) => "$1",
            _ => "$3",
        };

        let query = format!(
            r#"
            SELECT id, owner_type::TEXT, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source::TEXT, created_at, updated_at
            FROM model_pricing
            {}
            ORDER BY created_at DESC, id DESC
            LIMIT {}
            "#,
            where_clause, limit_param
        );

        let mut query_builder = sqlx::query(&query);
        if let Some(uuid) = bind_uuid {
            query_builder = query_builder.bind(uuid);
        }
        if let Some(s) = bind_str {
            query_builder = query_builder.bind(s);
        }
        query_builder = query_builder.bind(fetch_limit);

        let rows = query_builder.fetch_all(&self.read_pool).await?;

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
impl ModelPricingRepo for PostgresModelPricingRepo {
    async fn create(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        let id = Uuid::new_v4();
        let (owner_type, owner_id) = Self::owner_to_parts(&input.owner);

        let row = sqlx::query(
            r#"
            INSERT INTO model_pricing (
                id, owner_type, owner_id, provider, model,
                input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                per_second, per_1m_characters, source
            )
            VALUES ($1, $2::model_pricing_owner_type, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::pricing_source)
            RETURNING id, owner_type::TEXT, owner_id, provider, model,
                      input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                      cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                      per_second, per_1m_characters, source::TEXT, created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(owner_type)
        .bind(owner_id)
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
        .fetch_one(&self.write_pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db_err) if db_err.is_unique_violation() => DbError::Conflict(
                format!(
                    "Pricing for provider '{}' model '{}' already exists",
                    input.provider, input.model
                ),
            ),
            _ => DbError::from(e),
        })?;

        Self::row_to_pricing(&row)
    }

    async fn get_by_id(&self, id: Uuid) -> DbResult<Option<DbModelPricing>> {
        let row = sqlx::query(
            r#"
            SELECT id, owner_type::TEXT, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source::TEXT, created_at, updated_at
            FROM model_pricing
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.read_pool)
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
            sqlx::query(
                r#"
                SELECT id, owner_type::TEXT, owner_id, provider, model,
                       input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                       cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                       per_second, per_1m_characters, source::TEXT, created_at, updated_at
                FROM model_pricing
                WHERE owner_type IS NULL AND provider = $1 AND model = $2
                "#,
            )
            .bind(provider)
            .bind(model)
            .fetch_optional(&self.read_pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, owner_type::TEXT, owner_id, provider, model,
                       input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                       cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                       per_second, per_1m_characters, source::TEXT, created_at, updated_at
                FROM model_pricing
                WHERE owner_type = $1::model_pricing_owner_type AND owner_id = $2 AND provider = $3 AND model = $4
                "#,
            )
            .bind(owner_type)
            .bind(owner_id)
            .bind(provider)
            .bind(model)
            .fetch_optional(&self.read_pool)
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
        let row = sqlx::query(
            r#"
            SELECT id, owner_type::TEXT, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source::TEXT, created_at, updated_at
            FROM model_pricing
            WHERE provider = $1 AND model = $2
              AND (
                ($3::uuid IS NOT NULL AND owner_type = 'user' AND owner_id = $3)
                OR ($4::uuid IS NOT NULL AND owner_type = 'project' AND owner_id = $4)
                OR ($5::uuid IS NOT NULL AND owner_type = 'organization' AND owner_id = $5)
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
        .bind(user_id)
        .bind(project_id)
        .bind(org_id)
        .fetch_optional(&self.read_pool)
        .await?;

        row.as_ref().map(Self::row_to_pricing).transpose()
    }

    async fn list_by_org(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'organization' AND owner_id = $1";

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, cursor, &params, limit, Some(org_id), None)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, limit, Some(org_id), None)
            .await
    }

    async fn count_by_org(&self, org_id: Uuid) -> DbResult<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'organization' AND owner_id = $1
            "#,
        )
        .bind(org_id)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn list_by_project(
        &self,
        project_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'project' AND owner_id = $1";

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, cursor, &params, limit, Some(project_id), None)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, limit, Some(project_id), None)
            .await
    }

    async fn count_by_project(&self, project_id: Uuid) -> DbResult<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'project' AND owner_id = $1
            "#,
        )
        .bind(project_id)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn list_by_user(
        &self,
        user_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type = 'user' AND owner_id = $1";

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, cursor, &params, limit, Some(user_id), None)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, limit, Some(user_id), None)
            .await
    }

    async fn count_by_user(&self, user_id: Uuid) -> DbResult<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type = 'user' AND owner_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn list_global(&self, params: ListParams) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE owner_type IS NULL";

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, cursor, &params, limit, None, None)
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, limit, None, None).await
    }

    async fn count_global(&self) -> DbResult<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE owner_type IS NULL
            "#,
        )
        .fetch_one(&self.read_pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn list_by_provider(
        &self,
        provider: &str,
        params: ListParams,
    ) -> DbResult<ListResult<DbModelPricing>> {
        let limit = params.limit.unwrap_or(100);
        let where_clause = "WHERE provider = $1";

        if let Some(ref cursor) = params.cursor {
            return self
                .list_with_cursor(where_clause, cursor, &params, limit, None, Some(provider))
                .await;
        }

        // First page (no cursor provided)
        self.list_first_page(where_clause, limit, None, Some(provider))
            .await
    }

    async fn count_by_provider(&self, provider: &str) -> DbResult<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM model_pricing
            WHERE provider = $1
            "#,
        )
        .bind(provider)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn list_billing_overlay_rows(&self) -> DbResult<Vec<DbModelPricing>> {
        let rows = sqlx::query(
            r#"
            SELECT id, owner_type::TEXT, owner_id, provider, model,
                   input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                   cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                   per_second, per_1m_characters, source::TEXT, created_at, updated_at
            FROM model_pricing
            WHERE owner_type IS NULL OR owner_type = 'organization'
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;

        rows.iter().map(Self::row_to_pricing).collect()
    }

    async fn update(&self, id: Uuid, input: UpdateModelPricing) -> DbResult<DbModelPricing> {
        let row = sqlx::query(
            r#"
            UPDATE model_pricing SET
                input_per_1m_tokens = COALESCE($1, input_per_1m_tokens),
                output_per_1m_tokens = COALESCE($2, output_per_1m_tokens),
                per_image = COALESCE($3, per_image),
                per_request = COALESCE($4, per_request),
                cached_input_per_1m_tokens = COALESCE($5, cached_input_per_1m_tokens),
                cache_write_per_1m_tokens = COALESCE($6, cache_write_per_1m_tokens),
                reasoning_per_1m_tokens = COALESCE($7, reasoning_per_1m_tokens),
                per_second = COALESCE($8, per_second),
                per_1m_characters = COALESCE($9, per_1m_characters),
                source = COALESCE($10::pricing_source, source)
            WHERE id = $11
            RETURNING id, owner_type::TEXT, owner_id, provider, model,
                      input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                      cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                      per_second, per_1m_characters, source::TEXT, created_at, updated_at
            "#,
        )
        .bind(input.input_per_1m_tokens)
        .bind(input.output_per_1m_tokens)
        .bind(input.per_image)
        .bind(input.per_request)
        .bind(input.cached_input_per_1m_tokens)
        .bind(input.cache_write_per_1m_tokens)
        .bind(input.reasoning_per_1m_tokens)
        .bind(input.per_second)
        .bind(input.per_1m_characters)
        .bind(input.source.map(|s| s.as_str()))
        .bind(id)
        .fetch_optional(&self.write_pool)
        .await?;

        row.as_ref()
            .map(Self::row_to_pricing)
            .transpose()?
            .ok_or(DbError::NotFound)
    }

    async fn delete(&self, id: Uuid) -> DbResult<()> {
        sqlx::query("DELETE FROM model_pricing WHERE id = $1")
            .bind(id)
            .execute(&self.write_pool)
            .await?;

        Ok(())
    }

    async fn upsert(&self, input: CreateModelPricing) -> DbResult<DbModelPricing> {
        let id = Uuid::new_v4();
        let (owner_type, owner_id) = Self::owner_to_parts(&input.owner);

        // Use INSERT ... ON CONFLICT DO UPDATE RETURNING for atomic single-query upsert
        // PostgreSQL uses UNIQUE NULLS NOT DISTINCT constraint for proper NULL handling
        let row = if owner_type.is_none() {
            // Global pricing: conflict on (provider, model) where owner_type IS NULL
            sqlx::query(
                r#"
                INSERT INTO model_pricing (
                    id, owner_type, owner_id, provider, model,
                    input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                    cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                    per_second, per_1m_characters, source
                )
                VALUES ($1, NULL, NULL, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::pricing_source)
                ON CONFLICT (owner_type, owner_id, provider, model)
                DO UPDATE SET
                    input_per_1m_tokens = EXCLUDED.input_per_1m_tokens,
                    output_per_1m_tokens = EXCLUDED.output_per_1m_tokens,
                    per_image = EXCLUDED.per_image,
                    per_request = EXCLUDED.per_request,
                    cached_input_per_1m_tokens = EXCLUDED.cached_input_per_1m_tokens,
                    cache_write_per_1m_tokens = EXCLUDED.cache_write_per_1m_tokens,
                    reasoning_per_1m_tokens = EXCLUDED.reasoning_per_1m_tokens,
                    per_second = EXCLUDED.per_second,
                    per_1m_characters = EXCLUDED.per_1m_characters,
                    source = EXCLUDED.source,
                    updated_at = NOW()
                RETURNING id, owner_type::TEXT, owner_id, provider, model,
                          input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                          cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                          per_second, per_1m_characters, source::TEXT, created_at, updated_at
                "#,
            )
            .bind(id)
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
            .fetch_one(&self.write_pool)
            .await?
        } else {
            // Scoped pricing: conflict on (owner_type, owner_id, provider, model)
            sqlx::query(
                r#"
                INSERT INTO model_pricing (
                    id, owner_type, owner_id, provider, model,
                    input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                    cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                    per_second, per_1m_characters, source
                )
                VALUES ($1, $2::model_pricing_owner_type, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::pricing_source)
                ON CONFLICT (owner_type, owner_id, provider, model)
                DO UPDATE SET
                    input_per_1m_tokens = EXCLUDED.input_per_1m_tokens,
                    output_per_1m_tokens = EXCLUDED.output_per_1m_tokens,
                    per_image = EXCLUDED.per_image,
                    per_request = EXCLUDED.per_request,
                    cached_input_per_1m_tokens = EXCLUDED.cached_input_per_1m_tokens,
                    cache_write_per_1m_tokens = EXCLUDED.cache_write_per_1m_tokens,
                    reasoning_per_1m_tokens = EXCLUDED.reasoning_per_1m_tokens,
                    per_second = EXCLUDED.per_second,
                    per_1m_characters = EXCLUDED.per_1m_characters,
                    source = EXCLUDED.source,
                    updated_at = NOW()
                RETURNING id, owner_type::TEXT, owner_id, provider, model,
                          input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                          cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                          per_second, per_1m_characters, source::TEXT, created_at, updated_at
                "#,
            )
            .bind(id)
            .bind(owner_type)
            .bind(owner_id)
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
            .fetch_one(&self.write_pool)
            .await?
        };

        Self::row_to_pricing(&row)
    }

    async fn bulk_upsert(&self, entries: Vec<CreateModelPricing>) -> DbResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let count = entries.len();

        // Process all entries in a single transaction for atomicity
        // If any entry fails, the entire batch is rolled back
        let mut tx = self.write_pool.begin().await?;

        for entry in entries {
            let id = Uuid::new_v4();
            let (owner_type, owner_id) = Self::owner_to_parts(&entry.owner);

            if owner_type.is_none() {
                sqlx::query(
                    r#"
                    INSERT INTO model_pricing (
                        id, owner_type, owner_id, provider, model,
                        input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                        cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                        per_second, per_1m_characters, source
                    )
                    VALUES ($1, NULL, NULL, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::pricing_source)
                    ON CONFLICT (owner_type, owner_id, provider, model)
                    DO UPDATE SET
                        input_per_1m_tokens = EXCLUDED.input_per_1m_tokens,
                        output_per_1m_tokens = EXCLUDED.output_per_1m_tokens,
                        per_image = EXCLUDED.per_image,
                        per_request = EXCLUDED.per_request,
                        cached_input_per_1m_tokens = EXCLUDED.cached_input_per_1m_tokens,
                        cache_write_per_1m_tokens = EXCLUDED.cache_write_per_1m_tokens,
                        reasoning_per_1m_tokens = EXCLUDED.reasoning_per_1m_tokens,
                        per_second = EXCLUDED.per_second,
                        per_1m_characters = EXCLUDED.per_1m_characters,
                        source = EXCLUDED.source,
                        updated_at = NOW()
                    "#,
                )
                .bind(id)
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
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    r#"
                    INSERT INTO model_pricing (
                        id, owner_type, owner_id, provider, model,
                        input_per_1m_tokens, output_per_1m_tokens, per_image, per_request,
                        cached_input_per_1m_tokens, cache_write_per_1m_tokens, reasoning_per_1m_tokens,
                        per_second, per_1m_characters, source
                    )
                    VALUES ($1, $2::model_pricing_owner_type, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::pricing_source)
                    ON CONFLICT (owner_type, owner_id, provider, model)
                    DO UPDATE SET
                        input_per_1m_tokens = EXCLUDED.input_per_1m_tokens,
                        output_per_1m_tokens = EXCLUDED.output_per_1m_tokens,
                        per_image = EXCLUDED.per_image,
                        per_request = EXCLUDED.per_request,
                        cached_input_per_1m_tokens = EXCLUDED.cached_input_per_1m_tokens,
                        cache_write_per_1m_tokens = EXCLUDED.cache_write_per_1m_tokens,
                        reasoning_per_1m_tokens = EXCLUDED.reasoning_per_1m_tokens,
                        per_second = EXCLUDED.per_second,
                        per_1m_characters = EXCLUDED.per_1m_characters,
                        source = EXCLUDED.source,
                        updated_at = NOW()
                    "#,
                )
                .bind(id)
                .bind(owner_type)
                .bind(owner_id)
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
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(count)
    }
}
