use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    backend::{Pool, RowExt, begin, query},
    common::parse_uuid,
};
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
const MEDIA_AGGREGATE_COLS: &str = "\
    COALESCE(SUM(image_count), 0) as image_count, \
    COALESCE(SUM(audio_seconds), 0) as audio_seconds, \
    COALESCE(SUM(character_count), 0) as character_count";

/// SQL fragment applying the [`DateRange`] record-type filter, or `""` when the
/// range has no filter. `prefix` is the query's table alias (e.g. `"u."`).
/// Pair with [`BindRecordType::bind_record_type`] to bind the value.
fn record_type_sql(range: &DateRange, prefix: &str) -> String {
    if range.record_type.is_some() {
        format!("AND {prefix}record_type = ?")
    } else {
        String::new()
    }
}

/// Chain-friendly conditional bind for the [`record_type_sql`] placeholder.
trait BindRecordType: Sized {
    fn bind_record_type(self, range: &DateRange) -> Self;
}

#[cfg(not(target_arch = "wasm32"))]
impl<'q> BindRecordType
    for sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>
{
    fn bind_record_type(self, range: &DateRange) -> Self {
        match &range.record_type {
            Some(rt) => self.bind(rt.clone()),
            None => self,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl BindRecordType for crate::db::wasm_sqlite::WasmQuery {
    fn bind_record_type(self, range: &DateRange) -> Self {
        match &range.record_type {
            Some(rt) => self.bind(rt.clone()),
            None => self,
        }
    }
}

pub struct SqliteUsageRepo {
    pool: Pool,
}

impl SqliteUsageRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    fn media_fields(row: &super::backend::Row) -> (i64, i64, i64) {
        (
            row.col("image_count"),
            row.col("audio_seconds"),
            row.col("character_count"),
        )
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl UsageRepo for SqliteUsageRepo {
    async fn log(&self, entry: UsageLogEntry) -> DbResult<()> {
        let id = Uuid::new_v4();
        let total_tokens = entry.input_tokens + entry.output_tokens;

        // Use INSERT OR IGNORE for idempotency - duplicate (server-generated)
        // request_ids from internal retries are silently skipped
        query(
            r#"
            INSERT OR IGNORE INTO usage_records (
                id, request_id, client_request_id, api_key_id, user_id, org_id, project_id, team_id,
                service_account_id, model, provider, input_tokens, output_tokens,
                total_tokens, cost_microcents, http_referer, recorded_at,
                streamed, cached_tokens, reasoning_tokens, finish_reason,
                latency_ms, cancelled, status_code, pricing_source,
                image_count, audio_seconds, character_count, provider_source,
                record_type, tool_name, tool_query, tool_url,
                tool_bytes_fetched, tool_results_count, tool_runtime_seconds,
                tool_exit_code
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id.to_string())
        .bind(&entry.request_id)
        .bind(&entry.client_request_id)
        .bind(entry.api_key_id.map(|id| id.to_string()))
        .bind(entry.user_id.map(|id| id.to_string()))
        .bind(entry.org_id.map(|id| id.to_string()))
        .bind(entry.project_id.map(|id| id.to_string()))
        .bind(entry.team_id.map(|id| id.to_string()))
        .bind(entry.service_account_id.map(|id| id.to_string()))
        .bind(&entry.model)
        .bind(&entry.provider)
        .bind(entry.input_tokens)
        .bind(entry.output_tokens)
        .bind(total_tokens)
        .bind(entry.cost_microcents.unwrap_or(0))
        .bind(&entry.http_referer)
        // Millisecond truncation is required for SQLite TEXT-comparison
        // cursor pagination (see db/repos/cursor.rs)
        .bind(truncate_to_millis(entry.request_at))
        .bind(entry.streamed)
        .bind(entry.cached_tokens)
        .bind(entry.reasoning_tokens)
        .bind(&entry.finish_reason)
        .bind(entry.latency_ms)
        .bind(entry.cancelled)
        .bind(entry.status_code)
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
        .bind(entry.tool_exit_code)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn log_batch(&self, entries: Vec<UsageLogEntry>) -> DbResult<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        // SQLite has a limit of 999 parameters per query (SQLITE_LIMIT_VARIABLE_NUMBER)
        // Each entry uses 37 parameters. Use 26 entries (37*26=962) to stay under limit.
        const MAX_ENTRIES_PER_BATCH: usize = 26;

        let mut total_inserted = 0;

        let mut tx = begin(&self.pool).await?;

        for chunk in entries.chunks(MAX_ENTRIES_PER_BATCH) {
            let placeholders: Vec<&str> = chunk
                .iter()
                .map(|_| "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .collect();

            let sql = format!(
                r#"
                INSERT OR IGNORE INTO usage_records (
                    id, request_id, client_request_id, api_key_id, user_id, org_id, project_id, team_id,
                    service_account_id, model, provider, input_tokens, output_tokens,
                    total_tokens, cost_microcents, http_referer, recorded_at,
                    streamed, cached_tokens, reasoning_tokens, finish_reason,
                    latency_ms, cancelled, status_code, pricing_source,
                    image_count, audio_seconds, character_count, provider_source,
                    record_type, tool_name, tool_query, tool_url,
                    tool_bytes_fetched, tool_results_count, tool_runtime_seconds,
                    tool_exit_code
                )
                VALUES {}
                "#,
                placeholders.join(", ")
            );

            let mut query_builder = query(&sql);

            for entry in chunk {
                let id = Uuid::new_v4();
                let total_tokens = entry.input_tokens + entry.output_tokens;

                query_builder = query_builder
                    .bind(id.to_string())
                    .bind(&entry.request_id)
                    .bind(&entry.client_request_id)
                    .bind(entry.api_key_id.map(|id| id.to_string()))
                    .bind(entry.user_id.map(|id| id.to_string()))
                    .bind(entry.org_id.map(|id| id.to_string()))
                    .bind(entry.project_id.map(|id| id.to_string()))
                    .bind(entry.team_id.map(|id| id.to_string()))
                    .bind(entry.service_account_id.map(|id| id.to_string()))
                    .bind(&entry.model)
                    .bind(&entry.provider)
                    .bind(entry.input_tokens)
                    .bind(entry.output_tokens)
                    .bind(total_tokens)
                    .bind(entry.cost_microcents.unwrap_or(0))
                    .bind(&entry.http_referer)
                    // Millisecond truncation required for cursor pagination
                    .bind(truncate_to_millis(entry.request_at))
                    .bind(entry.streamed)
                    .bind(entry.cached_tokens)
                    .bind(entry.reasoning_tokens)
                    .bind(&entry.finish_reason)
                    .bind(entry.latency_ms)
                    .bind(entry.cancelled)
                    .bind(entry.status_code)
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
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_by_date(&self, api_key_id: Uuid, range: DateRange) -> DbResult<Vec<DailySpend>> {
        // Use range query in WHERE for index usage; date cast only needed in SELECT/GROUP BY
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_by_model(&self, api_key_id: Uuid, range: DateRange) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                http_referer as referer,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY http_referer
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                RefererSpend {
                    referer: row.col("referer"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_usage_stats(&self, api_key_id: Uuid, range: DateRange) -> DbResult<UsageStats> {
        // Get daily totals first, then compute stats in Rust
        // This avoids SQLite's lack of native STDDEV function
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_current_period_spend(&self, api_key_id: Uuid, period: &str) -> DbResult<i64> {
        // Use range queries to allow index usage on recorded_at
        let sql = match period {
            "daily" => {
                r#"
                SELECT COALESCE(SUM(cost_microcents), 0) as total
                FROM usage_records
                WHERE api_key_id = ?
                    AND recorded_at >= date('now')
                    AND recorded_at < date('now', '+1 day')
                "#
            }
            "monthly" => {
                r#"
                SELECT COALESCE(SUM(cost_microcents), 0) as total
                FROM usage_records
                WHERE api_key_id = ?
                    AND recorded_at >= date('now', 'start of month')
                    AND recorded_at < date('now', 'start of month', '+1 month')
                "#
            }
            _ => {
                return Ok(0);
            }
        };

        let row = query(sql)
            .bind(api_key_id.to_string())
            .fetch_one(&self.pool)
            .await?;

        Ok(row.col("total"))
    }

    // ==================== Aggregated Usage Queries ====================

    async fn get_daily_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_usage_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE provider = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE provider = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE provider = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE provider = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(provider)
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_model_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_summary_by_org(&self, org_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
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
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_summary_by_user(&self, user_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_summary_by_team(&self, team_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_usage_stats_by_org(&self, org_id: Uuid, range: DateRange) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_usage_stats_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_usage_stats_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_usage_stats_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    async fn get_provider_usage(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE api_key_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(api_key_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE org_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE project_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE user_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(user_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE team_id = ?
                AND recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.project_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.project_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.user_id, users.name, users.email
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(project_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.col("date"),
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.team_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.team_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.user_id, users.name, users.email
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.col("date"),
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.team_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.project_id, projects.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.team_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.project_id, projects.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(team_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.col("date"),
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // --- Org scope: by user, by project, by team ---

    async fn get_user_usage_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.user_id, users.name, users.email
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.col("date"),
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.project_id, projects.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.project_id, projects.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.col("date"),
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.team_id, teams.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                TeamSpend {
                    team_id: row
                        .col::<Option<String>>("team_id")
                        .and_then(|s| s.parse().ok()),
                    team_name: row.col("team_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.org_id = ?
                AND u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.team_id, teams.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(org_id.to_string())
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyTeamSpend {
                    date: row.col("date"),
                    team_id: row
                        .col::<Option<String>>("team_id")
                        .and_then(|s| s.parse().ok()),
                    team_name: row.col("team_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // --- Global scope: base queries ---

    async fn get_summary_global(&self, range: DateRange) -> DbResult<UsageSummary> {
        let record_type_filter = record_type_sql(&range, "");
        let row = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                MIN(recorded_at) as first_request_at,
                MAX(recorded_at) as last_request_at,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_one(&self.pool)
        .await?;

        let (image_count, audio_seconds, character_count) = Self::media_fields(&row);
        Ok(UsageSummary {
            total_cost_microcents: row.col("total_cost_microcents"),
            input_tokens: row.col("input_tokens"),
            output_tokens: row.col("output_tokens"),
            total_tokens: row.col("total_tokens"),
            request_count: row.col("request_count"),
            first_request_at: row.col("first_request_at"),
            last_request_at: row.col("last_request_at"),
            image_count,
            audio_seconds,
            character_count,
        })
    }

    async fn get_daily_usage_global(&self, range: DateRange) -> DbResult<Vec<DailySpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            ORDER BY date(recorded_at) ASC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailySpend {
                    date: row.col("date"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_model_usage_global(&self, range: DateRange) -> DbResult<Vec<ModelSpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY model
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ModelSpend {
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_provider_usage_global(&self, range: DateRange) -> DbResult<Vec<ProviderSpend>> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY provider
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProviderSpend {
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY pricing_source
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                PricingSourceSpend {
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                model,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), model
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyModelSpend {
                    date: row.col("date"),
                    model: row.col("model"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                provider,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), provider
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProviderSpend {
                    date: row.col("date"),
                    provider: row.col("provider"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                date(recorded_at) as date,
                pricing_source,
                COALESCE(SUM(cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(input_tokens), 0) as input_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                {MEDIA_AGGREGATE_COLS}
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at), pricing_source
            ORDER BY date(recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyPricingSourceSpend {
                    date: row.col("date"),
                    pricing_source: row.col("pricing_source"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_usage_stats_global(&self, range: DateRange) -> DbResult<UsageStats> {
        let record_type_filter = record_type_sql(&range, "");
        let rows = query(&format!(
            r#"
            SELECT
                COALESCE(SUM(cost_microcents), 0) as daily_cost
            FROM usage_records
            WHERE recorded_at >= ?
                AND recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(recorded_at)
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        compute_stats_from_daily_costs(&rows)
    }

    // --- Global scope: entity breakdowns ---

    async fn get_user_usage_global(&self, range: DateRange) -> DbResult<Vec<UserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.user_id, users.name, users.email
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                UserSpend {
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_user_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyUserSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.user_id, users.name as user_name, users.email as user_email,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN users ON u.user_id = users.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.user_id, users.name, users.email
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyUserSpend {
                    date: row.col("date"),
                    user_id: row
                        .col::<Option<String>>("user_id")
                        .and_then(|s| s.parse().ok()),
                    user_name: row.col("user_name"),
                    user_email: row.col("user_email"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_project_usage_global(&self, range: DateRange) -> DbResult<Vec<ProjectSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.project_id, projects.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                ProjectSpend {
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
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
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.project_id, projects.name as project_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN projects ON u.project_id = projects.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.project_id, projects.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyProjectSpend {
                    date: row.col("date"),
                    project_id: row
                        .col::<Option<String>>("project_id")
                        .and_then(|s| s.parse().ok()),
                    project_name: row.col("project_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_team_usage_global(&self, range: DateRange) -> DbResult<Vec<TeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.team_id, teams.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                TeamSpend {
                    team_id: row
                        .col::<Option<String>>("team_id")
                        .and_then(|s| s.parse().ok()),
                    team_name: row.col("team_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_team_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyTeamSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.team_id, teams.name as team_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN teams ON u.team_id = teams.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.team_id, teams.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyTeamSpend {
                    date: row.col("date"),
                    team_id: row
                        .col::<Option<String>>("team_id")
                        .and_then(|s| s.parse().ok()),
                    team_name: row.col("team_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_org_usage_global(&self, range: DateRange) -> DbResult<Vec<OrgSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT u.org_id, organizations.name as org_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN organizations ON u.org_id = organizations.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY u.org_id, organizations.name
            ORDER BY total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                OrgSpend {
                    org_id: row
                        .col::<Option<String>>("org_id")
                        .and_then(|s| s.parse().ok()),
                    org_name: row.col("org_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    async fn get_daily_org_usage_global(&self, range: DateRange) -> DbResult<Vec<DailyOrgSpend>> {
        let record_type_filter = record_type_sql(&range, "u.");
        let rows = query(&format!(
            r#"
            SELECT date(u.recorded_at) as date,
                u.org_id, organizations.name as org_name,
                COALESCE(SUM(u.cost_microcents), 0) as total_cost_microcents,
                COALESCE(SUM(u.input_tokens), 0) as input_tokens,
                COALESCE(SUM(u.output_tokens), 0) as output_tokens,
                COALESCE(SUM(u.total_tokens), 0) as total_tokens,
                COUNT(*) as request_count,
                COALESCE(SUM(u.image_count), 0) as image_count,
                COALESCE(SUM(u.audio_seconds), 0) as audio_seconds,
                COALESCE(SUM(u.character_count), 0) as character_count
            FROM usage_records u
            LEFT JOIN organizations ON u.org_id = organizations.id
            WHERE u.recorded_at >= ?
                AND u.recorded_at < date(?, '+1 day')
                {record_type_filter}
            GROUP BY date(u.recorded_at), u.org_id, organizations.name
            ORDER BY date(u.recorded_at) ASC, total_cost_microcents DESC
            "#,
        ))
        .bind(range.start)
        .bind(range.end)
        .bind_record_type(&range)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let (image_count, audio_seconds, character_count) = Self::media_fields(row);
                DailyOrgSpend {
                    date: row.col("date"),
                    org_id: row
                        .col::<Option<String>>("org_id")
                        .and_then(|s| s.parse().ok()),
                    org_name: row.col("org_name"),
                    total_cost_microcents: row.col("total_cost_microcents"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    total_tokens: row.col("total_tokens"),
                    request_count: row.col("request_count"),
                    image_count,
                    audio_seconds,
                    character_count,
                }
            })
            .collect())
    }

    // ==================== Individual Log Queries ====================

    async fn list_logs(&self, filter: UsageLogQuery) -> DbResult<ListResult<UsageLogRecord>> {
        let limit = filter.limit.unwrap_or(100);
        let fetch_limit = limit + 1;

        let cursor = match &filter.cursor {
            Some(c) => Some(Cursor::decode(c).map_err(|e| {
                crate::db::error::DbError::Internal(format!("Invalid cursor: {}", e))
            })?),
            None => None,
        };

        let direction = match filter.direction.as_deref() {
            Some("backward") => CursorDirection::Backward,
            _ => CursorDirection::Forward,
        };

        let mut conditions = Vec::new();
        let mut params: Vec<String> = Vec::new();

        if let Some(org_id) = &filter.org_id {
            conditions.push("org_id = ?".to_string());
            params.push(org_id.to_string());
        }
        if let Some(user_id) = &filter.user_id {
            conditions.push("user_id = ?".to_string());
            params.push(user_id.to_string());
        }
        if let Some(project_id) = &filter.project_id {
            conditions.push("project_id = ?".to_string());
            params.push(project_id.to_string());
        }
        if let Some(team_id) = &filter.team_id {
            conditions.push("team_id = ?".to_string());
            params.push(team_id.to_string());
        }
        if let Some(api_key_id) = &filter.api_key_id {
            conditions.push("api_key_id = ?".to_string());
            params.push(api_key_id.to_string());
        }
        if let Some(service_account_id) = &filter.service_account_id {
            conditions.push("service_account_id = ?".to_string());
            params.push(service_account_id.to_string());
        }
        if let Some(model) = &filter.model {
            conditions.push("model = ?".to_string());
            params.push(model.clone());
        }
        if let Some(provider) = &filter.provider {
            conditions.push("provider = ?".to_string());
            params.push(provider.clone());
        }
        if let Some(provider_source) = &filter.provider_source {
            conditions.push("provider_source = ?".to_string());
            params.push(provider_source.clone());
        }
        if let Some(ref record_type) = filter.record_type {
            conditions.push("record_type = ?".to_string());
            params.push(record_type.clone());
        }
        if let Some(from) = &filter.from {
            conditions.push("recorded_at >= ?".to_string());
            params.push(from.to_rfc3339());
        }
        if let Some(to) = &filter.to {
            conditions.push("recorded_at < ?".to_string());
            params.push(to.to_rfc3339());
        }

        let (comparison, order, should_reverse) = SortOrder::Desc.cursor_query_params(direction);

        let (order, cursor_condition) = if cursor.is_some() {
            (
                order,
                Some(format!("(recorded_at, id) {} (?, ?)", comparison)),
            )
        } else {
            ("DESC", None)
        };

        if let Some(cond) = cursor_condition {
            conditions.push(cond);
        }

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
            LIMIT ?
            "#,
            where_clause, order, order
        );

        let mut qb = query(&sql);
        for param in &params {
            qb = qb.bind(param);
        }
        if let Some(ref c) = cursor {
            qb = qb.bind(c.created_at).bind(c.id.to_string());
        }
        qb = qb.bind(fetch_limit);

        let rows = qb.fetch_all(&self.pool).await?;

        let has_more = rows.len() as i64 > limit;
        let mut items: Vec<UsageLogRecord> = rows
            .into_iter()
            .take(limit as usize)
            .map(|row| {
                let id: String = row.col("id");
                let api_key_id: Option<String> = row.col("api_key_id");
                let user_id: Option<String> = row.col("user_id");
                let org_id: Option<String> = row.col("org_id");
                let project_id: Option<String> = row.col("project_id");
                let team_id: Option<String> = row.col("team_id");
                let service_account_id: Option<String> = row.col("service_account_id");

                Ok(UsageLogRecord {
                    id: parse_uuid(&id)?,
                    recorded_at: row.col("recorded_at"),
                    request_id: row.col("request_id"),
                    client_request_id: row.col("client_request_id"),
                    api_key_id: api_key_id.map(|s| parse_uuid(&s)).transpose()?,
                    user_id: user_id.map(|s| parse_uuid(&s)).transpose()?,
                    org_id: org_id.map(|s| parse_uuid(&s)).transpose()?,
                    project_id: project_id.map(|s| parse_uuid(&s)).transpose()?,
                    team_id: team_id.map(|s| parse_uuid(&s)).transpose()?,
                    service_account_id: service_account_id.map(|s| parse_uuid(&s)).transpose()?,
                    model: row.col("model"),
                    provider: row.col("provider"),
                    http_referer: row.col("http_referer"),
                    input_tokens: row.col("input_tokens"),
                    output_tokens: row.col("output_tokens"),
                    cached_tokens: row.col("cached_tokens"),
                    reasoning_tokens: row.col("reasoning_tokens"),
                    cost_microcents: row.col("cost_microcents"),
                    streamed: row.col("streamed"),
                    finish_reason: row.col("finish_reason"),
                    latency_ms: row.col("latency_ms"),
                    cancelled: row.col("cancelled"),
                    status_code: row.col::<Option<i32>>("status_code").map(|v| v as i16),
                    pricing_source: row.col("pricing_source"),
                    image_count: row.col("image_count"),
                    audio_seconds: row.col("audio_seconds"),
                    character_count: row.col("character_count"),
                    provider_source: row.col("provider_source"),
                    record_type: row
                        .col::<Option<String>>("record_type")
                        .unwrap_or_else(|| "model".to_string()),
                    tool_name: row.col("tool_name"),
                    tool_query: row.col("tool_query"),
                    tool_url: row.col("tool_url"),
                    tool_bytes_fetched: row.col("tool_bytes_fetched"),
                    tool_results_count: row.col("tool_results_count"),
                    tool_runtime_seconds: row.col("tool_runtime_seconds"),
                    tool_exit_code: row.col("tool_exit_code"),
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

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
            // Check if we've hit the max deletes limit
            if total_deleted >= max_deletes {
                break;
            }

            // Calculate remaining deletes allowed
            let remaining = max_deletes - total_deleted;
            let limit = std::cmp::min(batch_size as u64, remaining) as i64;

            // Delete a batch using subquery to select IDs (SQLite doesn't support LIMIT in DELETE directly)
            let result = query(
                r#"
                DELETE FROM usage_records
                WHERE id IN (
                    SELECT id FROM usage_records
                    WHERE recorded_at < ?
                    LIMIT ?
                )
                "#,
            )
            .bind(cutoff)
            .bind(limit)
            .execute(&self.pool)
            .await?;

            let rows_deleted = result.rows_affected();
            total_deleted += rows_deleted;

            // If we deleted fewer rows than the batch size, we're done
            if rows_deleted < limit as u64 {
                break;
            }
        }

        Ok(total_deleted)
    }
}

/// Helper function to compute usage stats from daily cost rows.
/// This avoids duplicating the statistics calculation logic.
fn compute_stats_from_daily_costs(rows: &[super::backend::Row]) -> DbResult<UsageStats> {
    let daily_costs: Vec<i64> = rows.iter().map(|row| row.col("daily_cost")).collect();
    let sample_days = daily_costs.len() as i32;

    if sample_days == 0 {
        return Ok(UsageStats {
            avg_daily_spend_microcents: 0,
            std_dev_daily_spend_microcents: 0,
            sample_days: 0,
        });
    }

    let total: i64 = daily_costs.iter().sum();
    let avg = total / sample_days as i64;

    // Calculate standard deviation
    let variance: f64 = if sample_days > 1 {
        let sum_sq_diff: f64 = daily_costs
            .iter()
            .map(|&cost| {
                let diff = cost as f64 - avg as f64;
                diff * diff
            })
            .sum();
        sum_sq_diff / (sample_days - 1) as f64
    } else {
        0.0
    };

    let std_dev = variance.sqrt() as i64;

    Ok(UsageStats {
        avg_daily_spend_microcents: avg,
        std_dev_daily_spend_microcents: std_dev,
        sample_days,
    })
}
