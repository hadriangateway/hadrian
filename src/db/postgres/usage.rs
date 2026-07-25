use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    db::{
        error::DbResult,
        repos::{
            Cursor, CursorDirection, DateRange, ListResult, PageCursors, SortOrder, UsageLogQuery,
            UsageRepo, UsageStats, cursor_from_row, truncate_to_millis,
        },
    },
    models::{
        DailyModelSpend, DailyOrgSpend, DailyPricingSourceSpend, DailyProjectSpend,
        DailyProviderSpend, DailySpend, DailyTeamSpend, DailyUserSpend, ModelSpend, OrgSpend,
        PricingSourceSpend, ProjectSpend, ProviderSpend, RefererSpend, TeamSpend, UsageLogEntry,
        UsageLogRecord, UsageSummary, UserSpend,
    },
};

/// Common media-tracking columns for aggregation queries.
const MEDIA_AGGREGATE_COLS_PG: &str = "\
    COALESCE(SUM(image_count), 0) as image_count, \
    COALESCE(SUM(audio_seconds), 0) as audio_seconds, \
    COALESCE(SUM(character_count), 0) as character_count";

/// SQL fragment applying the [`DateRange`] record-type filter, or `""` when the
/// range has no filter. `prefix` is the query's table alias (e.g. `"u."`) and
/// `param` the next free `$n` placeholder index for the query.
/// Pair with [`BindRecordType::bind_record_type`] to bind the value.
fn record_type_sql(range: &DateRange, prefix: &str, param: usize) -> String {
    if range.record_type.is_some() {
        format!("AND {prefix}record_type = ${param}")
    } else {
        String::new()
    }
}

/// Chain-friendly conditional bind for the [`record_type_sql`] placeholder.
trait BindRecordType: Sized {
    fn bind_record_type(self, range: &DateRange) -> Self;
}

impl<'q> BindRecordType for sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    fn bind_record_type(self, range: &DateRange) -> Self {
        match &range.record_type {
            Some(rt) => self.bind(rt.clone()),
            None => self,
        }
    }
}

pub struct PostgresUsageRepo {
    write_pool: PgPool,
    read_pool: PgPool,
}

impl PostgresUsageRepo {
    pub fn new(write_pool: PgPool, read_pool: Option<PgPool>) -> Self {
        let read_pool = read_pool.unwrap_or_else(|| write_pool.clone());
        Self {
            write_pool,
            read_pool,
        }
    }

    fn media_fields(row: &sqlx::postgres::PgRow) -> (i64, i64, i64) {
        (
            row.get("image_count"),
            row.get("audio_seconds"),
            row.get("character_count"),
        )
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl UsageRepo for PostgresUsageRepo {
    async fn log(&self, entry: UsageLogEntry) -> DbResult<()> {
        let id = Uuid::new_v4();
        let total_tokens = entry.input_tokens + entry.output_tokens;

        // Use ON CONFLICT DO NOTHING for idempotency - duplicate request_ids are silently skipped
        sqlx::query(
            r#"
            INSERT INTO usage_records (
                id, request_id, client_request_id, api_key_id, model, provider, input_tokens, output_tokens,
                total_tokens, cost_microcents, http_referer, recorded_at,
                streamed, cached_tokens, reasoning_tokens, finish_reason,
                latency_ms, cancelled, status_code,
                user_id, org_id, project_id, team_id, service_account_id, pricing_source,
                image_count, audio_seconds, character_count, provider_source,
                record_type, tool_name, tool_query, tool_url,
                tool_bytes_fetched, tool_results_count, tool_runtime_seconds,
                tool_exit_code
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34, $35, $36, $37)
            ON CONFLICT (request_id) DO NOTHING
            "#,
        )
        .bind(id)
        .bind(&entry.request_id)
        .bind(&entry.client_request_id)
        .bind(entry.api_key_id)
        .bind(&entry.model)
        .bind(&entry.provider)
        .bind(entry.input_tokens)
        .bind(entry.output_tokens)
        .bind(total_tokens)
        .bind(entry.cost_microcents.unwrap_or(0))
        .bind(&entry.http_referer)
        // Millisecond truncation keeps stored values equal to cursor-encoded
        // boundaries (see db/repos/cursor.rs)
        .bind(truncate_to_millis(entry.request_at))
        .bind(entry.streamed)
        .bind(entry.cached_tokens)
        .bind(entry.reasoning_tokens)
        .bind(&entry.finish_reason)
        .bind(entry.latency_ms)
        .bind(entry.cancelled)
        .bind(entry.status_code)
        .bind(entry.user_id)
        .bind(entry.org_id)
        .bind(entry.project_id)
        .bind(entry.team_id)
        .bind(entry.service_account_id)
        .bind(entry.pricing_source.as_str())
        .bind(entry.image_count)
        .bind(entry.audio_seconds)
        .bind(entry.character_count)
        .bind(entry.provider_source)
        .bind(&entry.record_type)
        .bind(&entry.tool_name)
        .bind(&entry.tool_query)
        .bind(&entry.tool_url)
        .bind(entry.tool_bytes_fetched)
        .bind(entry.tool_results_count)
        .bind(entry.tool_runtime_seconds)
        .bind(entry.tool_exit_code)
        .execute(&self.write_pool)
        .await?;

        Ok(())
    }

    async fn log_batch(&self, entries: Vec<UsageLogEntry>) -> DbResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        // PostgreSQL allows up to 65535 parameters per query
        // Each entry uses 37 parameters, so we can insert ~1770 entries per batch
        // Use 1000 as a reasonable batch size for performance
        const MAX_ENTRIES_PER_BATCH: usize = 1000;

        let mut total_inserted = 0;

        // Wrap all chunks in a single transaction for atomicity.
        // On failure, the caller can safely retry the entire batch since
        // ON CONFLICT DO NOTHING makes re-insertion idempotent.
        let mut tx = self.write_pool.begin().await?;

        // Process in chunks
        for chunk in entries.chunks(MAX_ENTRIES_PER_BATCH) {
            // Build dynamic multi-row INSERT query with numbered placeholders
            let placeholders: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let o = i * 37;
                    format!(
                        "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                        o + 1, o + 2, o + 3, o + 4, o + 5, o + 6,
                        o + 7, o + 8, o + 9, o + 10, o + 11, o + 12,
                        o + 13, o + 14, o + 15, o + 16, o + 17, o + 18,
                        o + 19, o + 20, o + 21, o + 22, o + 23, o + 24,
                        o + 25, o + 26, o + 27, o + 28, o + 29, o + 30,
                        o + 31, o + 32, o + 33, o + 34, o + 35, o + 36,
                        o + 37
                    )
                })
                .collect();

            let query = format!(
                r#"
                INSERT INTO usage_records (
                    id, request_id, client_request_id, api_key_id, model, provider, input_tokens, output_tokens,
                    total_tokens, cost_microcents, http_referer, recorded_at,
                    streamed, cached_tokens, reasoning_tokens, finish_reason,
                    latency_ms, cancelled, status_code,
                    user_id, org_id, project_id, team_id, service_account_id, pricing_source,
                    image_count, audio_seconds, character_count, provider_source,
                    record_type, tool_name, tool_query, tool_url,
                    tool_bytes_fetched, tool_results_count, tool_runtime_seconds,
                    tool_exit_code
                )
                VALUES {}
                ON CONFLICT (request_id) DO NOTHING
                "#,
                placeholders.join(", ")
            );

            let mut query_builder = sqlx::query(&query);

            for entry in chunk {
                let id = Uuid::new_v4();
                let total_tokens = entry.input_tokens + entry.output_tokens;

                query_builder = query_builder
                    .bind(id)
                    .bind(&entry.request_id)
                    .bind(&entry.client_request_id)
                    .bind(entry.api_key_id)
                    .bind(&entry.model)
                    .bind(&entry.provider)
                    .bind(entry.input_tokens)
                    .bind(entry.output_tokens)
                    .bind(total_tokens)
                    .bind(entry.cost_microcents.unwrap_or(0))
                    .bind(&entry.http_referer)
                    .bind(truncate_to_millis(entry.request_at))
                    .bind(entry.streamed)
                    .bind(entry.cached_tokens)
                    .bind(entry.reasoning_tokens)
                    .bind(&entry.finish_reason)
                    .bind(entry.latency_ms)
                    .bind(entry.cancelled)
                    .bind(entry.status_code)
                    .bind(entry.user_id)
                    .bind(entry.org_id)
                    .bind(entry.project_id)
                    .bind(entry.team_id)
                    .bind(entry.service_account_id)
                    .bind(entry.pricing_source.as_str())
                    .bind(entry.image_count)
                    .bind(entry.audio_seconds)
                    .bind(entry.character_count)
                    .bind(&entry.provider_source)
                    .bind(&entry.record_type)
                    .bind(&entry.tool_name)
                    .bind(&entry.tool_query)
                    .bind(&entry.tool_url)
                    .bind(entry.tool_bytes_fetched)
                    .bind(entry.tool_results_count)
                    .bind(entry.tool_runtime_seconds)
                    .bind(entry.tool_exit_code);
            }

            let result = query_builder.execute(&mut *tx).await?;
            total_inserted += result.rows_affected() as usize;
        }

        tx.commit().await?;
        Ok(total_inserted)
    }

    async fn get_summary(&self, api_key_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        // Use range query instead of date casting to allow index usage on recorded_at
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_by_date(&self, api_key_id: Uuid, range: DateRange) -> DbResult<Vec<DailySpend>> {
        // Use range query in WHERE for index usage; date cast only needed in SELECT/GROUP BY
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_by_model(&self, api_key_id: Uuid, range: DateRange) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_by_referer(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<RefererSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                http_referer as referer,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY http_referer
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                RefererSpend {
                    referer: row.get("referer"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_usage_stats(&self, api_key_id: Uuid, range: DateRange) -> DbResult<UsageStats> {
        // PostgreSQL has native STDDEV_SAMP function
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            WITH daily_totals AS (
                SELECT
                    recorded_at::DATE as day,
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE api_key_id = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            )
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM daily_totals
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    async fn get_current_period_spend(&self, api_key_id: Uuid, period: &str) -> DbResult<i64> {
        // Use range queries to allow index usage on recorded_at
        let query = match period {
            "daily" => {
                r#"
                SELECT COALESCE(SUM(cost_microcents), 0)::BIGINT as total
                FROM usage_records
                WHERE api_key_id = $1
                    AND recorded_at >= CURRENT_DATE
                    AND recorded_at < (CURRENT_DATE + INTERVAL '1 day')
                "#
            }
            "monthly" => {
                r#"
                SELECT COALESCE(SUM(cost_microcents), 0)::BIGINT as total
                FROM usage_records
                WHERE api_key_id = $1
                    AND recorded_at >= DATE_TRUNC('month', CURRENT_DATE)
                    AND recorded_at < (DATE_TRUNC('month', CURRENT_DATE) + INTERVAL '1 month')
                "#
            }
            _ => {
                return Ok(0);
            }
        };

        let row = sqlx::query(query)
            .bind(api_key_id)
            .fetch_one(&self.read_pool)
            .await?;

        Ok(row.get("total"))
    }

    // ==================== Aggregated Usage Queries ====================

    async fn get_daily_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE ASC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE ASC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE ASC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_usage_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE provider = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE ASC
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_summary_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE provider = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_model_usage_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE provider = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_usage_stats_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM (
                SELECT
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE provider = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            ) daily_totals
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    async fn get_model_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_model_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_model_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_summary_by_org(&self, org_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_summary_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_summary_by_user(&self, user_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_usage_stats_by_org(&self, org_id: Uuid, range: DateRange) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            WITH daily_totals AS (
                SELECT
                    recorded_at::DATE as day,
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE org_id = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            )
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM daily_totals
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    async fn get_usage_stats_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            WITH daily_totals AS (
                SELECT
                    recorded_at::DATE as day,
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE project_id = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            )
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM daily_totals
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    async fn get_usage_stats_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            WITH daily_totals AS (
                SELECT
                    recorded_at::DATE as day,
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE user_id = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            )
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM daily_totals
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    // ==================== Team-Level Aggregated Queries ====================

    async fn get_daily_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE
            ORDER BY recorded_at::DATE ASC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_model_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_summary_by_team(&self, team_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_usage_stats_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let row = sqlx::query(&format!(
            r#"
            WITH daily_totals AS (
                SELECT
                    recorded_at::DATE as day,
                    COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE team_id = $1
                    AND recorded_at >= $2::DATE
                    AND recorded_at < ($3::DATE + INTERVAL '1 day')
                    {record_type_filter}
                GROUP BY recorded_at::DATE
            )
            SELECT
                COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend,
                COALESCE(STDDEV_SAMP(daily_cost), 0)::BIGINT as std_dev_daily_spend,
                COUNT(*)::INT as sample_days
            FROM daily_totals
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend"),
            sample_days: row.get("sample_days"),
        })
    }

    async fn get_provider_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, model
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, model
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, model
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, model
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, model
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, provider
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, provider
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, provider
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, provider
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, provider
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // ==================== Pricing Source Aggregated Queries ====================

    async fn get_pricing_source_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_pricing_source_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_pricing_source_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_pricing_source_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_pricing_source_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE
                AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE api_key_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE org_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE project_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE user_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE team_id = $1
                AND recorded_at >= $2::DATE AND recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source
            ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // ==================== Entity Breakdown Queries ====================

    // --- Project scope: by user ---

    async fn get_user_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.project_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_user_usage_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.project_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.user_id, users.name, users.email
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.get("date"),
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // --- Team scope: by user, by project ---

    async fn get_user_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.team_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_user_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.team_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.user_id, users.name, users.email
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.get("date"),
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_project_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.team_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.project_id, projects.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_project_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date,
                u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.team_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.project_id, projects.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.get("date"),
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // --- Org scope: by user, by project, by team ---
    // Follow the exact same pattern as above but with u.org_id = $1

    async fn get_user_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.org_id = $1
                AND u.recorded_at >= $2::DATE
                AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_user_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(r#"
            SELECT u.recorded_at::DATE as date, u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN users ON u.user_id = users.id
            WHERE u.org_id = $1 AND u.recorded_at >= $2::DATE AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.user_id, users.name, users.email
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(org_id).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.get("date"),
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_project_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.org_id = $1 AND u.recorded_at >= $2::DATE AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.project_id, projects.name ORDER BY total_cost_microcents DESC
            "#)).bind(org_id).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_project_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(r#"
            SELECT u.recorded_at::DATE as date, u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.org_id = $1 AND u.recorded_at >= $2::DATE AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.project_id, projects.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(org_id).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.get("date"),
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_team_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<TeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(r#"
            SELECT u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.org_id = $1 AND u.recorded_at >= $2::DATE AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.team_id, teams.name ORDER BY total_cost_microcents DESC
            "#)).bind(org_id).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                TeamSpend {
                    team_id: row.get("team_id"),
                    team_name: row.get("team_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_team_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyTeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 4);
        let rows = sqlx::query(&format!(r#"
            SELECT u.recorded_at::DATE as date, u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.org_id = $1 AND u.recorded_at >= $2::DATE AND u.recorded_at < ($3::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.team_id, teams.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(org_id).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyTeamSpend {
                    date: row.get("date"),
                    team_id: row.get("team_id"),
                    team_name: row.get("team_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // --- Global scope: base queries (no scope WHERE clause, just date range) ---

    async fn get_summary_global(&self, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let row = sqlx::query(&format!(
            r#"
            SELECT COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                MIN(recorded_at) as first_request_at, MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            "#
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.get("total_cost_microcents"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
            request_count: row.get("request_count"),
            first_request_at: row.get("first_request_at"),
            last_request_at: row.get("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_daily_usage_global(&self, range: DateRange) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT recorded_at::DATE as date,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY recorded_at::DATE ORDER BY recorded_at::DATE ASC
            "#
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.get("date"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_model_usage_global(&self, range: DateRange) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY model ORDER BY total_cost_microcents DESC
            "#
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_global(&self, range: DateRange) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY provider ORDER BY total_cost_microcents DESC
            "#
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_pricing_source_usage_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY pricing_source ORDER BY total_cost_microcents DESC
            "#
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_model_usage_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(r#"
            SELECT recorded_at::DATE as date, model,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY recorded_at::DATE, model ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.get("date"),
                    model: row.get("model"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_provider_usage_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(r#"
            SELECT recorded_at::DATE as date, provider,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY recorded_at::DATE, provider ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.get("date"),
                    provider: row.get("provider"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_pricing_source_usage_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let rows = sqlx::query(&format!(r#"
            SELECT recorded_at::DATE as date, pricing_source,
                COALESCE(SUM(cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                {MEDIA_AGGREGATE_COLS_PG}
            FROM usage_records
            WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY recorded_at::DATE, pricing_source ORDER BY recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.get("date"),
                    pricing_source: row.get("pricing_source"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_usage_stats_global(&self, range: DateRange) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "", 3);
        let row = sqlx::query(&format!(
            r#"
            SELECT COALESCE(AVG(daily_cost), 0)::BIGINT as avg_daily_spend_microcents,
                   COALESCE(STDDEV(daily_cost), 0)::BIGINT as std_dev_daily_spend_microcents,
                   COUNT(*)::INTEGER as sample_days
            FROM (
                SELECT COALESCE(SUM(cost_microcents), 0) as daily_cost
                FROM usage_records
                WHERE recorded_at >= $1::DATE AND recorded_at < ($2::DATE + INTERVAL '1 day')
                {record_type_filter}
                GROUP BY recorded_at::DATE
            ) daily_totals
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.read_pool)
        .await?;

        Ok(UsageStats {
            avg_daily_spend_microcents: row.get("avg_daily_spend_microcents"),
            std_dev_daily_spend_microcents: row.get("std_dev_daily_spend_microcents"),
            sample_days: row.get("sample_days"),
        })
    }

    // --- Global scope: entity breakdowns (no scope WHERE, just date range + entity grouping) ---

    async fn get_user_usage_global(&self, range: DateRange) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN users ON u.user_id = users.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.user_id, users.name, users.email ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_user_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyUserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(r#"
            SELECT u.recorded_at::DATE as date, u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN users ON u.user_id = users.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.user_id, users.name, users.email
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#)).bind(range.start).bind(range.end).bind_record_type(&range).fetch_all(&self.read_pool).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.get("date"),
                    user_id: row.get("user_id"),
                    user_name: row.get("user_name"),
                    user_email: row.get("user_email"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_project_usage_global(&self, range: DateRange) -> DbResult<Vec<ProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.project_id, projects.name ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_project_usage_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date, u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.project_id, projects.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.get("date"),
                    project_id: row.get("project_id"),
                    project_name: row.get("project_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_team_usage_global(&self, range: DateRange) -> DbResult<Vec<TeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.team_id, teams.name ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                TeamSpend {
                    team_id: row.get("team_id"),
                    team_name: row.get("team_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_team_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyTeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date, u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.team_id, teams.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyTeamSpend {
                    date: row.get("date"),
                    team_id: row.get("team_id"),
                    team_name: row.get("team_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_org_usage_global(&self, range: DateRange) -> DbResult<Vec<OrgSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.org_id, organizations.name as org_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN organizations ON u.org_id = organizations.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.org_id, organizations.name ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                OrgSpend {
                    org_id: row.get("org_id"),
                    org_name: row.get("org_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_org_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyOrgSpend>> {
        let record_type_filter = record_type_sql(&range, "u.", 3);
        let rows = sqlx::query(&format!(
            r#"
            SELECT u.recorded_at::DATE as date, u.org_id, organizations.name as org_name,
                COALESCE(SUM(u.cost_microcents), 0)::BIGINT as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0)::BIGINT as input_tokens,
                COALESCE(SUM(u.output_tokens), 0)::BIGINT as output_tokens,
                COALESCE(SUM(u.total_tokens), 0)::BIGINT as total_tokens,
                COUNT(*)::BIGINT as request_count,
                COALESCE(SUM(u.image_count), 0)::BIGINT as image_count,
                COALESCE(SUM(u.audio_seconds), 0)::BIGINT as audio_seconds,
                COALESCE(SUM(u.character_count), 0)::BIGINT as character_count
            FROM usage_records u LEFT JOIN organizations ON u.org_id = organizations.id
            WHERE u.recorded_at >= $1::DATE AND u.recorded_at < ($2::DATE + INTERVAL '1 day')
            {record_type_filter}
            GROUP BY u.recorded_at::DATE, u.org_id, organizations.name
            ORDER BY u.recorded_at::DATE ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.read_pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyOrgSpend {
                    date: row.get("date"),
                    org_id: row.get("org_id"),
                    org_name: row.get("org_name"),
                    total_cost_microcents: row.get("total_cost_microcents"),
                    input_tokens: row.get("input_tokens"),
                    output_tokens: row.get("output_tokens"),
                    total_tokens: row.get("total_tokens"),
                    request_count: row.get("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // ==================== Individual Log Queries ====================

    async fn list_logs(&self, query: UsageLogQuery) -> DbResult<ListResult<UsageLogRecord>> {
        let limit = query.limit.unwrap_or(100);
        let fetch_limit = limit + 1;

        let cursor = match &query.cursor {
            Some(c) => Some(Cursor::decode(c).map_err(|e| {
                crate::db::error::DbError::Internal(format!("Invalid cursor: {}", e))
            })?),
            None => None,
        };

        let direction = match query.direction.as_deref() {
            Some("backward") => CursorDirection::Backward,
            _ => CursorDirection::Forward,
        };

        let mut conditions = Vec::new();
        let mut param_idx = 1u32;

        if query.org_id.is_some() {
            conditions.push(format!("org_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.user_id.is_some() {
            conditions.push(format!("user_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.project_id.is_some() {
            conditions.push(format!("project_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.team_id.is_some() {
            conditions.push(format!("team_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.api_key_id.is_some() {
            conditions.push(format!("api_key_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.service_account_id.is_some() {
            conditions.push(format!("service_account_id = ${}", param_idx));
            param_idx += 1;
        }
        if query.model.is_some() {
            conditions.push(format!("model = ${}", param_idx));
            param_idx += 1;
        }
        if query.provider.is_some() {
            conditions.push(format!("provider = ${}", param_idx));
            param_idx += 1;
        }
        if query.provider_source.is_some() {
            conditions.push(format!("provider_source = ${}", param_idx));
            param_idx += 1;
        }
        if query.record_type.is_some() {
            conditions.push(format!("record_type = ${}", param_idx));
            param_idx += 1;
        }
        if query.from.is_some() {
            conditions.push(format!("recorded_at >= ${}", param_idx));
            param_idx += 1;
        }
        if query.to.is_some() {
            conditions.push(format!("recorded_at < ${}", param_idx));
            param_idx += 1;
        }

        let (comparison, order, should_reverse) = SortOrder::Desc.cursor_query_params(direction);

        let order = if cursor.is_some() {
            conditions.push(format!(
                "ROW(recorded_at, id) {} ROW(${}, ${})",
                comparison,
                param_idx,
                param_idx + 1
            ));
            param_idx += 2;
            order
        } else {
            "DESC"
        };

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT id, recorded_at, request_id, client_request_id, api_key_id, user_id, org_id,
                   project_id, team_id, service_account_id, model, provider,
                   http_referer, input_tokens, output_tokens, cached_tokens,
                   reasoning_tokens, cost_microcents, streamed, finish_reason,
                   latency_ms, cancelled, status_code, pricing_source,
                   image_count, audio_seconds, character_count, provider_source,
                   record_type, tool_name, tool_query, tool_url,
                   tool_bytes_fetched, tool_results_count, tool_runtime_seconds,
                   tool_exit_code
            FROM usage_records
            {}
            ORDER BY recorded_at {}, id {}
            LIMIT ${}
            "#,
            where_clause, order, order, param_idx
        );

        let mut qb = sqlx::query(&sql);

        if let Some(org_id) = &query.org_id {
            qb = qb.bind(org_id);
        }
        if let Some(user_id) = &query.user_id {
            qb = qb.bind(user_id);
        }
        if let Some(project_id) = &query.project_id {
            qb = qb.bind(project_id);
        }
        if let Some(team_id) = &query.team_id {
            qb = qb.bind(team_id);
        }
        if let Some(api_key_id) = &query.api_key_id {
            qb = qb.bind(api_key_id);
        }
        if let Some(service_account_id) = &query.service_account_id {
            qb = qb.bind(service_account_id);
        }
        if let Some(model) = &query.model {
            qb = qb.bind(model);
        }
        if let Some(provider) = &query.provider {
            qb = qb.bind(provider);
        }
        if let Some(provider_source) = &query.provider_source {
            qb = qb.bind(provider_source);
        }
        if let Some(record_type) = &query.record_type {
            qb = qb.bind(record_type);
        }
        if let Some(from) = &query.from {
            qb = qb.bind(from);
        }
        if let Some(to) = &query.to {
            qb = qb.bind(to);
        }
        if let Some(ref c) = cursor {
            qb = qb.bind(c.created_at).bind(c.id);
        }
        qb = qb.bind(fetch_limit);

        let rows = qb.fetch_all(&self.read_pool).await?;

        let has_more = rows.len() as i64 > limit;
        let mut items: Vec<UsageLogRecord> = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| UsageLogRecord {
                id: row.get("id"),
                recorded_at: row.get("recorded_at"),
                request_id: row.get("request_id"),
                client_request_id: row.get("client_request_id"),
                api_key_id: row.get("api_key_id"),
                user_id: row.get("user_id"),
                org_id: row.get("org_id"),
                project_id: row.get("project_id"),
                team_id: row.get("team_id"),
                service_account_id: row.get("service_account_id"),
                model: row.get("model"),
                provider: row.get("provider"),
                http_referer: row.get("http_referer"),
                input_tokens: row.get("input_tokens"),
                output_tokens: row.get("output_tokens"),
                cached_tokens: row.get("cached_tokens"),
                reasoning_tokens: row.get("reasoning_tokens"),
                cost_microcents: row.get("cost_microcents"),
                streamed: row.get("streamed"),
                finish_reason: row.get("finish_reason"),
                latency_ms: row.get("latency_ms"),
                cancelled: row.get("cancelled"),
                status_code: row.get("status_code"),
                pricing_source: row.get("pricing_source"),
                image_count: row.get("image_count"),
                audio_seconds: row.get("audio_seconds"),
                character_count: row.get("character_count"),
                provider_source: row.get("provider_source"),
                record_type: row
                    .try_get("record_type")
                    .unwrap_or_else(|_| "model".to_string()),
                tool_name: row.get("tool_name"),
                tool_query: row.get("tool_query"),
                tool_url: row.get("tool_url"),
                tool_bytes_fetched: row.get("tool_bytes_fetched"),
                tool_results_count: row.get("tool_results_count"),
                tool_runtime_seconds: row.get("tool_runtime_seconds"),
                tool_exit_code: row.get("tool_exit_code"),
            })
            .collect();

        if should_reverse {
            items.reverse();
        }

        let cursors =
            PageCursors::from_items(&items, has_more, direction, cursor.as_ref(), |rec| {
                cursor_from_row(rec.recorded_at, rec.id)
            });

        Ok(ListResult::new(items, has_more, cursors))
    }

    // ==================== Retention Operations ====================

    async fn delete_usage_records_before(
        &self,
        cutoff: DateTime<Utc>,
        batch_size: u32,
        max_deletes: u64,
    ) -> DbResult<u64> {
        let mut total_deleted: u64 = 0;

        loop {
            if total_deleted >= max_deletes {
                break;
            }

            let remaining = max_deletes - total_deleted;
            let limit = std::cmp::min(batch_size as u64, remaining) as i64;

            // PostgreSQL efficient batched deletion using ctid
            let result = sqlx::query(
                r#"
                DELETE FROM usage_records
                WHERE ctid IN (
                    SELECT ctid FROM usage_records
                    WHERE recorded_at < $1
                    LIMIT $2
                )
                "#,
            )
            .bind(cutoff)
            .bind(limit)
            .execute(&self.write_pool)
            .await?;

            let rows_deleted = result.rows_affected();
            total_deleted += rows_deleted;

            if rows_deleted < limit as u64 {
                break;
            }
        }

        Ok(total_deleted)
    }
}
