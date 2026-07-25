use std::sync::Arc;

use chrono::{Datelike, NaiveDate, Utc};
use uuid::Uuid;

#[cfg(feature = "forecasting")]
use super::forecasting::{self, DEFAULT_FORECAST_DAYS};
#[cfg(not(feature = "forecasting"))]
const DEFAULT_FORECAST_DAYS: usize = 7;
use crate::{
    db::{
        DateRange, DbPool, DbResult,
        repos::{ListResult, UsageLogQuery},
    },
    models::{
        CostForecast, DailyModelSpend, DailyOrgSpend, DailyPricingSourceSpend, DailyProjectSpend,
        DailyProviderSpend, DailySpend, DailyTeamSpend, DailyUserSpend, ModelSpend, OrgSpend,
        PricingSourceSpend, ProjectSpend, ProviderSpend, RefererSpend, TeamSpend, UsageLogEntry,
        UsageLogRecord, UsageSummary, UserSpend,
    },
};

/// Service layer for usage tracking and reporting
#[derive(Clone)]
pub struct UsageService {
    db: Arc<DbPool>,
}

impl UsageService {
    pub fn new(db: Arc<DbPool>) -> Self {
        Self { db }
    }

    /// Log a usage entry (async, fire-and-forget)
    pub async fn log(&self, entry: UsageLogEntry) -> DbResult<()> {
        self.db.usage().log(entry).await
    }

    /// List individual usage log records with optional filtering and cursor pagination.
    pub async fn list_logs(&self, query: UsageLogQuery) -> DbResult<ListResult<UsageLogRecord>> {
        self.db.usage().list_logs(query).await
    }

    /// Get usage summary for an API key within a date range
    pub async fn get_summary(&self, api_key_id: Uuid, range: DateRange) -> DbResult<UsageSummary> {
        self.db.usage().get_summary(api_key_id, range).await
    }

    /// Get daily usage breakdown for an API key
    pub async fn get_by_date(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db.usage().get_by_date(api_key_id, range).await
    }

    /// Get usage breakdown by model for an API key
    pub async fn get_by_model(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db.usage().get_by_model(api_key_id, range).await
    }

    /// Get usage breakdown by HTTP referer for an API key
    pub async fn get_by_referer(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<RefererSpend>> {
        self.db.usage().get_by_referer(api_key_id, range).await
    }

    // ==================== Organization-Level Analytics ====================

    /// Get usage summary for an organization within a date range
    pub async fn get_summary_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        self.db.usage().get_summary_by_org(org_id, range).await
    }

    /// Get daily usage breakdown for an organization
    pub async fn get_by_date_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db.usage().get_daily_usage_by_org(org_id, range).await
    }

    /// Get usage breakdown by model for an organization
    pub async fn get_by_model_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db.usage().get_model_usage_by_org(org_id, range).await
    }

    /// Get usage breakdown by provider for an organization
    pub async fn get_by_provider_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        self.db
            .usage()
            .get_provider_usage_by_org(org_id, range)
            .await
    }

    /// Get cost forecast for an organization
    ///
    /// Uses historical usage data to predict future spending.
    /// The lookback_days parameter controls how much historical data to use
    /// for computing the average daily spend (default: 30 days).
    /// The forecast_days parameter controls how many days to forecast ahead
    /// (default: 7 days).
    pub async fn get_forecast_by_org(
        &self,
        org_id: Uuid,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };

        // Get usage statistics
        let stats = self
            .db
            .usage()
            .get_usage_stats_by_org(org_id, range.clone())
            .await?;

        // Get daily spend for time series forecast
        let daily_spend = self
            .db
            .usage()
            .get_daily_usage_by_org(org_id, range)
            .await?;

        let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
        #[cfg(feature = "forecasting")]
        let time_series_forecast = forecasting::generate_forecast(&daily_spend, horizon, None)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast for org: {e}");
                None
            });
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = horizon;
            None
        };

        // Organizations don't have budget limits, so return forecast without budget info
        Ok(CostForecast {
            current_spend_microcents: daily_spend.iter().map(|d| d.total_cost_microcents).sum(),
            budget_limit_microcents: None,
            budget_period: None,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion: None,
            projected_exhaustion_date: None,
            days_until_exhaustion_lower: None,
            days_until_exhaustion_upper: None,
            budget_utilization_percent: None,
            projected_period_spend_microcents: None,
            time_series_forecast,
        })
    }

    // ==================== Project-Level Analytics ====================

    /// Get usage summary for a project within a date range
    pub async fn get_summary_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        self.db
            .usage()
            .get_summary_by_project(project_id, range)
            .await
    }

    /// Get daily usage breakdown for a project
    pub async fn get_by_date_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db
            .usage()
            .get_daily_usage_by_project(project_id, range)
            .await
    }

    /// Get usage breakdown by model for a project
    pub async fn get_by_model_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db
            .usage()
            .get_model_usage_by_project(project_id, range)
            .await
    }

    /// Get cost forecast for a project
    ///
    /// Uses historical usage data across all API keys in the project to predict future spending.
    /// The lookback_days parameter controls how much historical data to use
    /// for computing the average daily spend (default: 30 days).
    /// The forecast_days parameter controls how many days to forecast ahead
    /// (default: 7 days).
    pub async fn get_forecast_by_project(
        &self,
        project_id: Uuid,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };

        // Get usage statistics
        let stats = self
            .db
            .usage()
            .get_usage_stats_by_project(project_id, range.clone())
            .await?;

        // Get daily spend for time series forecast
        let daily_spend = self
            .db
            .usage()
            .get_daily_usage_by_project(project_id, range)
            .await?;

        let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
        #[cfg(feature = "forecasting")]
        let time_series_forecast = forecasting::generate_forecast(&daily_spend, horizon, None)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast for project: {e}");
                None
            });
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = horizon;
            None
        };

        // Projects don't have budget limits, so return forecast without budget info
        Ok(CostForecast {
            current_spend_microcents: daily_spend.iter().map(|d| d.total_cost_microcents).sum(),
            budget_limit_microcents: None,
            budget_period: None,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion: None,
            projected_exhaustion_date: None,
            days_until_exhaustion_lower: None,
            days_until_exhaustion_upper: None,
            budget_utilization_percent: None,
            projected_period_spend_microcents: None,
            time_series_forecast,
        })
    }

    // ==================== Team-Level Analytics ====================

    /// Get usage summary for a team within a date range
    pub async fn get_summary_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        self.db.usage().get_summary_by_team(team_id, range).await
    }

    /// Get daily usage breakdown for a team
    pub async fn get_by_date_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db
            .usage()
            .get_daily_usage_by_team(team_id, range)
            .await
    }

    /// Get usage breakdown by model for a team
    pub async fn get_by_model_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db
            .usage()
            .get_model_usage_by_team(team_id, range)
            .await
    }

    /// Get usage breakdown by provider for a team
    pub async fn get_by_provider_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        self.db
            .usage()
            .get_provider_usage_by_team(team_id, range)
            .await
    }

    /// Get cost forecast for a team
    pub async fn get_forecast_by_team(
        &self,
        team_id: Uuid,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };

        let stats = self
            .db
            .usage()
            .get_usage_stats_by_team(team_id, range.clone())
            .await?;

        let daily_spend = self
            .db
            .usage()
            .get_daily_usage_by_team(team_id, range)
            .await?;

        let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
        #[cfg(feature = "forecasting")]
        let time_series_forecast = forecasting::generate_forecast(&daily_spend, horizon, None)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast for team: {e}");
                None
            });
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = horizon;
            None
        };

        Ok(CostForecast {
            current_spend_microcents: daily_spend.iter().map(|d| d.total_cost_microcents).sum(),
            budget_limit_microcents: None,
            budget_period: None,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion: None,
            projected_exhaustion_date: None,
            days_until_exhaustion_lower: None,
            days_until_exhaustion_upper: None,
            budget_utilization_percent: None,
            projected_period_spend_microcents: None,
            time_series_forecast,
        })
    }

    // ==================== By-Provider for Missing Scopes ====================

    /// Get usage breakdown by provider for an API key
    pub async fn get_by_provider(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        self.db.usage().get_provider_usage(api_key_id, range).await
    }

    /// Get usage breakdown by provider for a project
    pub async fn get_by_provider_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        self.db
            .usage()
            .get_provider_usage_by_project(project_id, range)
            .await
    }

    /// Get usage breakdown by provider for a user
    pub async fn get_by_provider_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProviderSpend>> {
        self.db
            .usage()
            .get_provider_usage_by_user(user_id, range)
            .await
    }

    // ==================== Daily Time Series by Model/Provider ====================

    /// Get daily usage grouped by model for an API key
    pub async fn get_by_date_model(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db
            .usage()
            .get_daily_model_usage(api_key_id, range)
            .await
    }

    /// Get daily usage grouped by model for an organization
    pub async fn get_by_date_model_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db
            .usage()
            .get_daily_model_usage_by_org(org_id, range)
            .await
    }

    /// Get daily usage grouped by model for a project
    pub async fn get_by_date_model_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db
            .usage()
            .get_daily_model_usage_by_project(project_id, range)
            .await
    }

    /// Get daily usage grouped by model for a user
    pub async fn get_by_date_model_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db
            .usage()
            .get_daily_model_usage_by_user(user_id, range)
            .await
    }

    /// Get daily usage grouped by model for a team
    pub async fn get_by_date_model_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db
            .usage()
            .get_daily_model_usage_by_team(team_id, range)
            .await
    }

    /// Get daily usage grouped by provider for an API key
    pub async fn get_by_date_provider(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db
            .usage()
            .get_daily_provider_usage(api_key_id, range)
            .await
    }

    /// Get daily usage grouped by provider for an organization
    pub async fn get_by_date_provider_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db
            .usage()
            .get_daily_provider_usage_by_org(org_id, range)
            .await
    }

    /// Get daily usage grouped by provider for a project
    pub async fn get_by_date_provider_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db
            .usage()
            .get_daily_provider_usage_by_project(project_id, range)
            .await
    }

    /// Get daily usage grouped by provider for a user
    pub async fn get_by_date_provider_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db
            .usage()
            .get_daily_provider_usage_by_user(user_id, range)
            .await
    }

    /// Get daily usage grouped by provider for a team
    pub async fn get_by_date_provider_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db
            .usage()
            .get_daily_provider_usage_by_team(team_id, range)
            .await
    }

    // ==================== Pricing Source Analytics ====================

    /// Get usage breakdown by pricing source for an API key
    pub async fn get_by_pricing_source(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db
            .usage()
            .get_pricing_source_usage(api_key_id, range)
            .await
    }

    /// Get usage breakdown by pricing source for an organization
    pub async fn get_by_pricing_source_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db
            .usage()
            .get_pricing_source_usage_by_org(org_id, range)
            .await
    }

    /// Get usage breakdown by pricing source for a project
    pub async fn get_by_pricing_source_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db
            .usage()
            .get_pricing_source_usage_by_project(project_id, range)
            .await
    }

    /// Get usage breakdown by pricing source for a user
    pub async fn get_by_pricing_source_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db
            .usage()
            .get_pricing_source_usage_by_user(user_id, range)
            .await
    }

    /// Get usage breakdown by pricing source for a team
    pub async fn get_by_pricing_source_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db
            .usage()
            .get_pricing_source_usage_by_team(team_id, range)
            .await
    }

    /// Get daily usage grouped by pricing source for an API key
    pub async fn get_by_date_pricing_source(
        &self,
        api_key_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage(api_key_id, range)
            .await
    }

    /// Get daily usage grouped by pricing source for an organization
    pub async fn get_by_date_pricing_source_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage_by_org(org_id, range)
            .await
    }

    /// Get daily usage grouped by pricing source for a project
    pub async fn get_by_date_pricing_source_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage_by_project(project_id, range)
            .await
    }

    /// Get daily usage grouped by pricing source for a user
    pub async fn get_by_date_pricing_source_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage_by_user(user_id, range)
            .await
    }

    /// Get daily usage grouped by pricing source for a team
    pub async fn get_by_date_pricing_source_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage_by_team(team_id, range)
            .await
    }

    // ==================== Entity Breakdown Analytics ====================

    // --- Project scope: by user ---

    pub async fn get_by_user_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        self.db
            .usage()
            .get_user_usage_by_project(project_id, range)
            .await
    }

    pub async fn get_by_date_user_by_project(
        &self,
        project_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        self.db
            .usage()
            .get_daily_user_usage_by_project(project_id, range)
            .await
    }

    // --- Team scope: by user, by project ---

    pub async fn get_by_user_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        self.db.usage().get_user_usage_by_team(team_id, range).await
    }

    pub async fn get_by_date_user_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        self.db
            .usage()
            .get_daily_user_usage_by_team(team_id, range)
            .await
    }

    pub async fn get_by_project_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProjectSpend>> {
        self.db
            .usage()
            .get_project_usage_by_team(team_id, range)
            .await
    }

    pub async fn get_by_date_project_by_team(
        &self,
        team_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        self.db
            .usage()
            .get_daily_project_usage_by_team(team_id, range)
            .await
    }

    // --- Org scope: by user, by project, by team ---

    pub async fn get_by_user_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<UserSpend>> {
        self.db.usage().get_user_usage_by_org(org_id, range).await
    }

    pub async fn get_by_date_user_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyUserSpend>> {
        self.db
            .usage()
            .get_daily_user_usage_by_org(org_id, range)
            .await
    }

    pub async fn get_by_project_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ProjectSpend>> {
        self.db
            .usage()
            .get_project_usage_by_org(org_id, range)
            .await
    }

    pub async fn get_by_date_project_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        self.db
            .usage()
            .get_daily_project_usage_by_org(org_id, range)
            .await
    }

    pub async fn get_by_team_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<TeamSpend>> {
        self.db.usage().get_team_usage_by_org(org_id, range).await
    }

    pub async fn get_by_date_team_by_org(
        &self,
        org_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailyTeamSpend>> {
        self.db
            .usage()
            .get_daily_team_usage_by_org(org_id, range)
            .await
    }

    // --- Global scope ---

    pub async fn get_summary_global(&self, range: DateRange) -> DbResult<UsageSummary> {
        self.db.usage().get_summary_global(range).await
    }

    pub async fn get_by_date_global(&self, range: DateRange) -> DbResult<Vec<DailySpend>> {
        self.db.usage().get_daily_usage_global(range).await
    }

    pub async fn get_by_model_global(&self, range: DateRange) -> DbResult<Vec<ModelSpend>> {
        self.db.usage().get_model_usage_global(range).await
    }

    pub async fn get_by_provider_global(&self, range: DateRange) -> DbResult<Vec<ProviderSpend>> {
        self.db.usage().get_provider_usage_global(range).await
    }

    pub async fn get_by_pricing_source_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<PricingSourceSpend>> {
        self.db.usage().get_pricing_source_usage_global(range).await
    }

    pub async fn get_by_date_model_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyModelSpend>> {
        self.db.usage().get_daily_model_usage_global(range).await
    }

    pub async fn get_by_date_provider_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyProviderSpend>> {
        self.db.usage().get_daily_provider_usage_global(range).await
    }

    pub async fn get_by_date_pricing_source_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyPricingSourceSpend>> {
        self.db
            .usage()
            .get_daily_pricing_source_usage_global(range)
            .await
    }

    pub async fn get_by_user_global(&self, range: DateRange) -> DbResult<Vec<UserSpend>> {
        self.db.usage().get_user_usage_global(range).await
    }

    pub async fn get_by_date_user_global(&self, range: DateRange) -> DbResult<Vec<DailyUserSpend>> {
        self.db.usage().get_daily_user_usage_global(range).await
    }

    pub async fn get_by_project_global(&self, range: DateRange) -> DbResult<Vec<ProjectSpend>> {
        self.db.usage().get_project_usage_global(range).await
    }

    pub async fn get_by_date_project_global(
        &self,
        range: DateRange,
    ) -> DbResult<Vec<DailyProjectSpend>> {
        self.db.usage().get_daily_project_usage_global(range).await
    }

    pub async fn get_by_team_global(&self, range: DateRange) -> DbResult<Vec<TeamSpend>> {
        self.db.usage().get_team_usage_global(range).await
    }

    pub async fn get_by_date_team_global(&self, range: DateRange) -> DbResult<Vec<DailyTeamSpend>> {
        self.db.usage().get_daily_team_usage_global(range).await
    }

    pub async fn get_by_org_global(&self, range: DateRange) -> DbResult<Vec<OrgSpend>> {
        self.db.usage().get_org_usage_global(range).await
    }

    pub async fn get_by_date_org_global(&self, range: DateRange) -> DbResult<Vec<DailyOrgSpend>> {
        self.db.usage().get_daily_org_usage_global(range).await
    }

    // ==================== Provider-Level Analytics ====================

    /// Get usage summary for a provider within a date range
    pub async fn get_summary_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        self.db
            .usage()
            .get_summary_by_provider(provider, range)
            .await
    }

    /// Get daily usage breakdown for a provider
    pub async fn get_by_date_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db
            .usage()
            .get_daily_usage_by_provider(provider, range)
            .await
    }

    /// Get usage breakdown by model for a provider
    pub async fn get_by_model_by_provider(
        &self,
        provider: &str,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db
            .usage()
            .get_model_usage_by_provider(provider, range)
            .await
    }

    /// Get cost forecast for a provider
    ///
    /// Uses historical usage data across all API keys using this provider to predict future spending.
    /// The lookback_days parameter controls how much historical data to use
    /// for computing the average daily spend (default: 30 days).
    /// The forecast_days parameter controls how many days to forecast ahead
    /// (default: 7 days).
    pub async fn get_forecast_by_provider(
        &self,
        provider: &str,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };

        // Get usage statistics
        let stats = self
            .db
            .usage()
            .get_usage_stats_by_provider(provider, range.clone())
            .await?;

        // Get daily spend for time series forecast
        let daily_spend = self
            .db
            .usage()
            .get_daily_usage_by_provider(provider, range)
            .await?;

        let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
        #[cfg(feature = "forecasting")]
        let time_series_forecast = forecasting::generate_forecast(&daily_spend, horizon, None)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast for provider: {e}");
                None
            });
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = horizon;
            None
        };

        // Providers don't have budget limits, so return forecast without budget info
        Ok(CostForecast {
            current_spend_microcents: daily_spend.iter().map(|d| d.total_cost_microcents).sum(),
            budget_limit_microcents: None,
            budget_period: None,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion: None,
            projected_exhaustion_date: None,
            days_until_exhaustion_lower: None,
            days_until_exhaustion_upper: None,
            budget_utilization_percent: None,
            projected_period_spend_microcents: None,
            time_series_forecast,
        })
    }

    // ==================== User-Level Analytics ====================

    /// Get usage summary for a user within a date range
    pub async fn get_summary_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<UsageSummary> {
        self.db.usage().get_summary_by_user(user_id, range).await
    }

    /// Get daily usage breakdown for a user
    pub async fn get_by_date_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<DailySpend>> {
        self.db
            .usage()
            .get_daily_usage_by_user(user_id, range)
            .await
    }

    /// Get usage breakdown by model for a user
    pub async fn get_by_model_by_user(
        &self,
        user_id: Uuid,
        range: DateRange,
    ) -> DbResult<Vec<ModelSpend>> {
        self.db
            .usage()
            .get_model_usage_by_user(user_id, range)
            .await
    }

    /// Get cost forecast for a user
    ///
    /// Uses historical usage data across all API keys owned by the user to predict future spending.
    /// The lookback_days parameter controls how much historical data to use
    /// for computing the average daily spend (default: 30 days).
    /// The forecast_days parameter controls how many days to forecast ahead
    /// (default: 7 days).
    pub async fn get_forecast_by_user(
        &self,
        user_id: Uuid,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };

        // Get usage statistics
        let stats = self
            .db
            .usage()
            .get_usage_stats_by_user(user_id, range.clone())
            .await?;

        // Get daily spend for time series forecast
        let daily_spend = self
            .db
            .usage()
            .get_daily_usage_by_user(user_id, range)
            .await?;

        let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
        #[cfg(feature = "forecasting")]
        let time_series_forecast = forecasting::generate_forecast(&daily_spend, horizon, None)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast for user: {e}");
                None
            });
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = horizon;
            None
        };

        // Users don't have budget limits, so return forecast without budget info
        Ok(CostForecast {
            current_spend_microcents: daily_spend.iter().map(|d| d.total_cost_microcents).sum(),
            budget_limit_microcents: None,
            budget_period: None,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion: None,
            projected_exhaustion_date: None,
            days_until_exhaustion_lower: None,
            days_until_exhaustion_upper: None,
            budget_utilization_percent: None,
            projected_period_spend_microcents: None,
            time_series_forecast,
        })
    }

    /// Get cost forecast for an API key
    ///
    /// Uses historical usage data to predict when the budget will be exhausted.
    /// The lookback_days parameter controls how much historical data to use
    /// for computing the average daily spend (default: 30 days).
    /// The forecast_days parameter controls how many days to forecast ahead
    /// (default: 7 days).
    pub async fn get_forecast(
        &self,
        api_key_id: Uuid,
        lookback_days: Option<i32>,
        forecast_days: Option<usize>,
    ) -> DbResult<CostForecast> {
        use crate::db::DbError;

        let lookback = lookback_days.unwrap_or(30);
        let today = Utc::now().date_naive();
        let start_date = today - chrono::Duration::days(lookback as i64);

        // Get the API key to check budget configuration
        let api_key = self
            .db
            .api_keys()
            .get_by_id(api_key_id)
            .await?
            .ok_or(DbError::NotFound)?;

        // Get usage statistics (average and std dev of daily spend)
        let range = DateRange {
            start: start_date,
            end: today,
            record_type: None,
        };
        let stats = self
            .db
            .usage()
            .get_usage_stats(api_key_id, range.clone())
            .await?;

        // Get current period spend
        let (current_spend, budget_limit, budget_period): (i64, Option<i64>, Option<String>) =
            match (&api_key.budget_period, api_key.budget_limit_cents) {
                (Some(period), Some(limit_cents)) => {
                    let period_str = period.as_str();
                    let spend = self
                        .db
                        .usage()
                        .get_current_period_spend(api_key_id, period_str)
                        .await?;
                    // Convert budget from cents to microcents
                    (
                        spend,
                        Some(limit_cents * 10_000),
                        Some(period_str.to_string()),
                    )
                }
                _ => (0, None, None),
            };

        // Calculate forecast
        let (days_until_exhaustion, projected_exhaustion_date, days_lower, days_upper) =
            if let Some(budget) = budget_limit {
                if stats.avg_daily_spend_microcents > 0 {
                    let remaining = (budget - current_spend).max(0);
                    let days = remaining as f64 / stats.avg_daily_spend_microcents as f64;

                    // Calculate confidence bounds (+/- 1 std dev)
                    let high_spend =
                        stats.avg_daily_spend_microcents + stats.std_dev_daily_spend_microcents;
                    let low_spend = (stats.avg_daily_spend_microcents
                        - stats.std_dev_daily_spend_microcents)
                        .max(1);

                    let days_lower = if high_spend > 0 {
                        Some(remaining as f64 / high_spend as f64)
                    } else {
                        None
                    };
                    let days_upper = Some(remaining as f64 / low_spend as f64);

                    let exhaustion_date = today + chrono::Duration::days(days.ceil() as i64);

                    (Some(days), Some(exhaustion_date), days_lower, days_upper)
                } else {
                    // No spending, budget will never be exhausted
                    (None, None, None, None)
                }
            } else {
                // No budget configured
                (None, None, None, None)
            };

        // Calculate budget utilization percentage
        let budget_utilization_percent = budget_limit.map(|budget| {
            if budget > 0 {
                (current_spend as f64 / budget as f64) * 100.0
            } else {
                0.0
            }
        });

        // Calculate projected end-of-period spend
        let projected_period_spend_microcents = if let Some(ref period) = budget_period {
            let days_remaining = days_remaining_in_period(period, today);
            if days_remaining > 0 && stats.avg_daily_spend_microcents > 0 {
                Some(current_spend + (stats.avg_daily_spend_microcents * days_remaining as i64))
            } else {
                Some(current_spend)
            }
        } else {
            None
        };

        #[cfg(feature = "forecasting")]
        let time_series_forecast = {
            let horizon = forecast_days.unwrap_or(DEFAULT_FORECAST_DAYS);
            let daily_spend = self.db.usage().get_by_date(api_key_id, range).await?;
            forecasting::generate_forecast(&daily_spend, horizon, None).unwrap_or_else(|e| {
                tracing::warn!("Failed to generate time series forecast: {e}");
                None
            })
        };
        #[cfg(not(feature = "forecasting"))]
        let time_series_forecast = {
            let _ = forecast_days;
            None
        };

        Ok(CostForecast {
            current_spend_microcents: current_spend,
            budget_limit_microcents: budget_limit,
            budget_period,
            avg_daily_spend_microcents: stats.avg_daily_spend_microcents,
            std_dev_daily_spend_microcents: stats.std_dev_daily_spend_microcents,
            sample_days: stats.sample_days,
            days_until_exhaustion,
            projected_exhaustion_date,
            days_until_exhaustion_lower: days_lower,
            days_until_exhaustion_upper: days_upper,
            budget_utilization_percent,
            projected_period_spend_microcents,
            time_series_forecast,
        })
    }
}

/// Calculate days remaining in the current budget period
fn days_remaining_in_period(period: &str, today: NaiveDate) -> i32 {
    match period {
        "daily" => 0, // End of day
        "monthly" => {
            // Days until first of next month
            let year = today.year();
            let month = today.month();
            let next_month = if month == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)
            };
            next_month
                .map(|d| (d - today).num_days() as i32)
                .unwrap_or(0)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "database-sqlite")]
    use std::sync::Arc;

    use chrono::NaiveDate;
    #[cfg(feature = "database-sqlite")]
    use chrono::{Duration, Utc};
    #[cfg(feature = "database-sqlite")]
    use uuid::Uuid;

    use super::*;
    #[cfg(feature = "database-sqlite")]
    use crate::{
        db::{
            DbPool,
            tests::harness::{create_sqlite_pool, run_sqlite_migrations},
        },
        models::{ApiKeyOwner, BudgetPeriod, CreateApiKey, CreateOrganization, UsageLogEntry},
    };

    // ============================================================================
    // Unit Tests for days_remaining_in_period
    // ============================================================================

    #[test]
    fn test_days_remaining_daily_period() {
        let today = NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
        assert_eq!(days_remaining_in_period("daily", today), 0);
    }

    #[test]
    fn test_days_remaining_monthly_mid_month() {
        // June 15 -> July 1 = 16 days
        let today = NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 16);
    }

    #[test]
    fn test_days_remaining_monthly_first_of_month() {
        // June 1 -> July 1 = 30 days
        let today = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 30);
    }

    #[test]
    fn test_days_remaining_monthly_last_of_month() {
        // June 30 -> July 1 = 1 day
        let today = NaiveDate::from_ymd_opt(2025, 6, 30).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 1);
    }

    #[test]
    fn test_days_remaining_monthly_december() {
        // Dec 15 -> Jan 1 = 17 days (crosses year boundary)
        let today = NaiveDate::from_ymd_opt(2025, 12, 15).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 17);
    }

    #[test]
    fn test_days_remaining_monthly_february() {
        // Feb 15 -> Mar 1 = 14 days (28-day month in 2025, non-leap)
        let today = NaiveDate::from_ymd_opt(2025, 2, 15).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 14);
    }

    #[test]
    fn test_days_remaining_monthly_leap_year_february() {
        // Feb 15, 2024 -> Mar 1 = 15 days (29-day month in 2024, leap year)
        let today = NaiveDate::from_ymd_opt(2024, 2, 15).unwrap();
        assert_eq!(days_remaining_in_period("monthly", today), 15);
    }

    #[test]
    fn test_days_remaining_unknown_period() {
        let today = NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
        assert_eq!(days_remaining_in_period("weekly", today), 0);
        assert_eq!(days_remaining_in_period("yearly", today), 0);
        assert_eq!(days_remaining_in_period("", today), 0);
    }

    // ============================================================================
    // Helper Functions for Integration Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    async fn create_test_db() -> DbPool {
        let pool = create_sqlite_pool().await;
        run_sqlite_migrations(&pool).await;
        DbPool::from_sqlite(pool)
    }

    #[cfg(feature = "database-sqlite")]
    async fn create_test_org(db: &DbPool, slug: &str) -> Uuid {
        db.organizations()
            .create(CreateOrganization {
                slug: slug.to_string(),
                name: format!("Org {}", slug),
            })
            .await
            .expect("Failed to create org")
            .id
    }

    #[cfg(feature = "database-sqlite")]
    async fn create_test_api_key(db: &DbPool, org_id: Uuid, name: &str) -> Uuid {
        let hash = format!("hash_{}", Uuid::new_v4().to_string().replace("-", ""));
        db.api_keys()
            .create(
                CreateApiKey {
                    name: name.to_string(),
                    owner: ApiKeyOwner::Organization { org_id },
                    budget_limit_cents: None,
                    budget_period: None,
                    expires_at: None,
                    scopes: None,
                    allowed_models: None,
                    ip_allowlist: None,
                    rate_limit_rpm: None,
                    rate_limit_tpm: None,
                    sovereignty_requirements: None,
                },
                &hash,
            )
            .await
            .expect("Failed to create API key")
            .id
    }

    #[cfg(feature = "database-sqlite")]
    async fn create_test_api_key_with_budget(
        db: &DbPool,
        org_id: Uuid,
        name: &str,
        budget_limit_cents: i64,
        budget_period: BudgetPeriod,
    ) -> Uuid {
        let hash = format!("hash_{}", Uuid::new_v4().to_string().replace("-", ""));
        db.api_keys()
            .create(
                CreateApiKey {
                    name: name.to_string(),
                    owner: ApiKeyOwner::Organization { org_id },
                    budget_limit_cents: Some(budget_limit_cents),
                    budget_period: Some(budget_period),
                    expires_at: None,
                    scopes: None,
                    allowed_models: None,
                    ip_allowlist: None,
                    rate_limit_rpm: None,
                    rate_limit_tpm: None,
                    sovereignty_requirements: None,
                },
                &hash,
            )
            .await
            .expect("Failed to create API key with budget")
            .id
    }

    #[cfg(feature = "database-sqlite")]
    fn create_usage_entry(
        api_key_id: Uuid,
        model: &str,
        provider: &str,
        cost_microcents: i64,
    ) -> UsageLogEntry {
        UsageLogEntry {
            request_id: Uuid::new_v4().to_string(),
            client_request_id: None,
            api_key_id: Some(api_key_id),
            user_id: None,
            org_id: None,
            project_id: None,
            team_id: None,
            service_account_id: None,
            model: model.to_string(),
            provider: provider.to_string(),
            http_referer: None,
            input_tokens: 100,
            output_tokens: 50,
            cost_microcents: Some(cost_microcents),
            request_at: Utc::now(),
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: None,
            pricing_source: crate::pricing::CostPricingSource::None,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
            tool_exit_code: None,
        }
    }

    #[cfg(feature = "database-sqlite")]
    fn create_usage_entry_at_time(
        api_key_id: Uuid,
        model: &str,
        provider: &str,
        cost_microcents: i64,
        request_at: chrono::DateTime<Utc>,
    ) -> UsageLogEntry {
        UsageLogEntry {
            request_id: Uuid::new_v4().to_string(),
            client_request_id: None,
            api_key_id: Some(api_key_id),
            user_id: None,
            org_id: None,
            project_id: None,
            team_id: None,
            service_account_id: None,
            model: model.to_string(),
            provider: provider.to_string(),
            http_referer: None,
            input_tokens: 100,
            output_tokens: 50,
            cost_microcents: Some(cost_microcents),
            request_at,
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: None,
            pricing_source: crate::pricing::CostPricingSource::None,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
            tool_exit_code: None,
        }
    }

    // ============================================================================
    // UsageService Integration Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_log_and_get_summary() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Log usage
        let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 1000);
        service.log(entry).await.expect("Failed to log usage");

        // Get summary
        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let summary = service
            .get_summary(api_key_id, range)
            .await
            .expect("Failed to get summary");

        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.total_cost_microcents, 1000);
        assert_eq!(summary.total_tokens, 150); // 100 + 50
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_by_date() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        let today = Utc::now();
        let yesterday = today - Duration::days(1);

        // Log usage on different days
        service
            .log(create_usage_entry_at_time(
                api_key_id, "gpt-4", "openai", 500, yesterday,
            ))
            .await
            .unwrap();
        service
            .log(create_usage_entry_at_time(
                api_key_id, "gpt-4", "openai", 1000, today,
            ))
            .await
            .unwrap();

        let range = DateRange {
            start: yesterday.date_naive(),
            end: today.date_naive(),
            record_type: None,
        };
        let result = service.get_by_date(api_key_id, range).await.unwrap();

        assert_eq!(result.len(), 2);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_by_model() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Log usage for different models
        service
            .log(create_usage_entry(api_key_id, "gpt-4", "openai", 1000))
            .await
            .unwrap();
        service
            .log(create_usage_entry(
                api_key_id,
                "claude-3-opus",
                "anthropic",
                2000,
            ))
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let result = service.get_by_model(api_key_id, range).await.unwrap();

        assert_eq!(result.len(), 2);
        // Ordered by cost descending
        assert_eq!(result[0].model, "claude-3-opus");
        assert_eq!(result[0].total_cost_microcents, 2000);
        assert_eq!(result[1].model, "gpt-4");
        assert_eq!(result[1].total_cost_microcents, 1000);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_no_budget() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Log some usage
        service
            .log(create_usage_entry(api_key_id, "gpt-4", "openai", 1000))
            .await
            .unwrap();

        let forecast = service
            .get_forecast(api_key_id, None, None)
            .await
            .expect("Failed to get forecast");

        // Without budget, should have no exhaustion prediction
        assert!(forecast.budget_limit_microcents.is_none());
        assert!(forecast.budget_period.is_none());
        assert!(forecast.days_until_exhaustion.is_none());
        assert!(forecast.projected_exhaustion_date.is_none());
        assert!(forecast.budget_utilization_percent.is_none());
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_with_budget() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;

        // Create API key with $100 monthly budget (10000 cents)
        let api_key_id = create_test_api_key_with_budget(
            &db,
            org_id,
            "test-key",
            10000, // $100 in cents
            BudgetPeriod::Monthly,
        )
        .await;

        // Log usage over multiple days to get stats
        let today = Utc::now();
        for i in 0..7 {
            let day = today - Duration::days(i);
            // 100,000 microcents = $0.10 per day
            service
                .log(create_usage_entry_at_time(
                    api_key_id, "gpt-4", "openai", 100_000, day,
                ))
                .await
                .unwrap();
        }

        let forecast = service
            .get_forecast(api_key_id, Some(7), None)
            .await
            .expect("Failed to get forecast");

        // Should have budget info
        assert_eq!(forecast.budget_limit_microcents, Some(10000 * 10_000)); // cents to microcents
        assert_eq!(forecast.budget_period, Some("monthly".to_string()));

        // Should have utilization info
        assert!(forecast.budget_utilization_percent.is_some());
        let utilization = forecast.budget_utilization_percent.unwrap();
        assert!(utilization >= 0.0);

        // Should have average daily spend
        assert!(forecast.avg_daily_spend_microcents > 0);
        assert_eq!(forecast.sample_days, 7);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_with_custom_lookback() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Log usage over 14 days
        let today = Utc::now();
        for i in 0..14 {
            let day = today - Duration::days(i);
            service
                .log(create_usage_entry_at_time(
                    api_key_id, "gpt-4", "openai", 100_000, day,
                ))
                .await
                .unwrap();
        }

        // Get forecast with 7-day lookback (covers today - 7 days to today = 8 days)
        let forecast_7 = service
            .get_forecast(api_key_id, Some(7), None)
            .await
            .expect("Failed to get forecast");

        // Get forecast with 14-day lookback (covers today - 14 days to today = 15 days)
        let forecast_14 = service
            .get_forecast(api_key_id, Some(14), None)
            .await
            .expect("Failed to get forecast");

        // lookback_days=N creates range [today-N, today] which is N+1 days
        // But we only have data for days 0..14, so:
        // - 7-day lookback covers days 0-7 (8 days of data)
        // - 14-day lookback covers all 14 days of data
        assert!(forecast_7.sample_days <= 8);
        assert!(forecast_14.sample_days <= 15);
        assert!(forecast_14.sample_days >= forecast_7.sample_days);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_not_found() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());

        // Non-existent API key should return error
        let result = service.get_forecast(Uuid::new_v4(), None, None).await;

        assert!(result.is_err());
    }

    // ============================================================================
    // Organization-Level Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_summary_by_org() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;
        let key2 = create_test_api_key(&db, org_id, "key-2").await;

        // Log usage for both keys with org_id set
        let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 500);
        e1.org_id = Some(org_id);
        service.log(e1).await.unwrap();

        let mut e2 = create_usage_entry(key2, "gpt-4", "openai", 700);
        e2.org_id = Some(org_id);
        service.log(e2).await.unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let summary = service.get_summary_by_org(org_id, range).await.unwrap();

        assert_eq!(summary.total_cost_microcents, 1200); // 500 + 700
        assert_eq!(summary.request_count, 2);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_by_model_by_org() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 1000);
        e1.org_id = Some(org_id);
        service.log(e1).await.unwrap();

        let mut e2 = create_usage_entry(key1, "claude-3-opus", "anthropic", 2000);
        e2.org_id = Some(org_id);
        service.log(e2).await.unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let result = service.get_by_model_by_org(org_id, range).await.unwrap();

        assert_eq!(result.len(), 2);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_by_provider_by_org() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 1000);
        e1.org_id = Some(org_id);
        service.log(e1).await.unwrap();

        let mut e2 = create_usage_entry(key1, "claude-3-opus", "anthropic", 2000);
        e2.org_id = Some(org_id);
        service.log(e2).await.unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let result = service.get_by_provider_by_org(org_id, range).await.unwrap();

        assert_eq!(result.len(), 2);
        let anthropic = result.iter().find(|p| p.provider == "anthropic").unwrap();
        let openai = result.iter().find(|p| p.provider == "openai").unwrap();
        assert_eq!(anthropic.total_cost_microcents, 2000);
        assert_eq!(openai.total_cost_microcents, 1000);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_by_org() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        // Log usage with org_id set
        let today = Utc::now();
        for i in 0..7 {
            let day = today - Duration::days(i);
            let mut entry = create_usage_entry_at_time(key1, "gpt-4", "openai", 100_000, day);
            entry.org_id = Some(org_id);
            service.log(entry).await.unwrap();
        }

        let forecast = service
            .get_forecast_by_org(org_id, Some(7), None)
            .await
            .expect("Failed to get org forecast");

        // Org forecast should not have budget info
        assert!(forecast.budget_limit_microcents.is_none());
        assert!(forecast.budget_period.is_none());

        // Should have spend data
        assert!(forecast.current_spend_microcents > 0);
        assert!(forecast.avg_daily_spend_microcents > 0);
    }

    // ============================================================================
    // Provider-Level Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_summary_by_provider() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        service
            .log(create_usage_entry(key1, "gpt-4", "openai", 1000))
            .await
            .unwrap();
        service
            .log(create_usage_entry(key1, "gpt-3.5-turbo", "openai", 500))
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let summary = service
            .get_summary_by_provider("openai", range)
            .await
            .unwrap();

        assert_eq!(summary.total_cost_microcents, 1500);
        assert_eq!(summary.request_count, 2);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_by_model_by_provider() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        service
            .log(create_usage_entry(key1, "gpt-4", "openai", 1000))
            .await
            .unwrap();
        service
            .log(create_usage_entry(key1, "gpt-3.5-turbo", "openai", 500))
            .await
            .unwrap();

        let today = Utc::now().date_naive();
        let range = DateRange {
            start: today,
            end: today,
            record_type: None,
        };
        let result = service
            .get_by_model_by_provider("openai", range)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_by_provider() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let key1 = create_test_api_key(&db, org_id, "key-1").await;

        let today = Utc::now();
        for i in 0..7 {
            let day = today - Duration::days(i);
            service
                .log(create_usage_entry_at_time(
                    key1, "gpt-4", "openai", 100_000, day,
                ))
                .await
                .unwrap();
        }

        let forecast = service
            .get_forecast_by_provider("openai", Some(7), None)
            .await
            .expect("Failed to get provider forecast");

        assert!(forecast.current_spend_microcents > 0);
        assert!(forecast.avg_daily_spend_microcents > 0);
        assert!(forecast.budget_limit_microcents.is_none());
    }

    // ============================================================================
    // Time Series Forecast Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_with_time_series() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Need at least 7 days of data for time series forecast
        let today = Utc::now();
        for i in 0..10 {
            let day = today - Duration::days(i);
            // Varying amounts to give the forecaster something to work with
            let cost = 100_000 + (i * 10_000);
            service
                .log(create_usage_entry_at_time(
                    api_key_id, "gpt-4", "openai", cost, day,
                ))
                .await
                .unwrap();
        }

        let forecast = service
            .get_forecast(api_key_id, Some(10), Some(5))
            .await
            .expect("Failed to get forecast");

        // Should have time series forecast with 5 days
        if let Some(ts) = forecast.time_series_forecast {
            assert_eq!(ts.dates.len(), 5);
            assert_eq!(ts.point_forecasts.len(), 5);
            assert_eq!(ts.lower_bounds.len(), 5);
            assert_eq!(ts.upper_bounds.len(), 5);
        }
        // Note: forecast may be None if insufficient data - that's OK
    }

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_get_forecast_insufficient_data_for_time_series() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;
        let api_key_id = create_test_api_key(&db, org_id, "test-key").await;

        // Only 3 days of data - not enough for time series
        let today = Utc::now();
        for i in 0..3 {
            let day = today - Duration::days(i);
            service
                .log(create_usage_entry_at_time(
                    api_key_id, "gpt-4", "openai", 100_000, day,
                ))
                .await
                .unwrap();
        }

        let forecast = service
            .get_forecast(api_key_id, Some(30), None)
            .await
            .expect("Failed to get forecast");

        // Time series forecast should be None with insufficient data
        assert!(forecast.time_series_forecast.is_none());
    }

    // ============================================================================
    // Budget Exhaustion Calculation Tests
    // ============================================================================

    #[cfg(feature = "database-sqlite")]
    #[tokio::test]
    async fn test_service_budget_exhaustion_calculation() {
        let db = Arc::new(create_test_db().await);
        let service = UsageService::new(db.clone());
        let org_id = create_test_org(&db, "test-org").await;

        // $10 daily budget (1000 cents)
        let api_key_id =
            create_test_api_key_with_budget(&db, org_id, "test-key", 1000, BudgetPeriod::Daily)
                .await;

        // Log usage: $1 today (1,000,000 microcents = $1)
        let today = Utc::now();
        service
            .log(create_usage_entry_at_time(
                api_key_id, "gpt-4", "openai", 1_000_000, today,
            ))
            .await
            .unwrap();

        let forecast = service
            .get_forecast(api_key_id, Some(7), None)
            .await
            .expect("Failed to get forecast");

        // Budget is $10 (10,000,000 microcents), spend is $1
        // Utilization should be ~10%
        assert!(forecast.budget_utilization_percent.is_some());
        let utilization = forecast.budget_utilization_percent.unwrap();
        assert!(utilization > 0.0 && utilization <= 100.0);
    }
}
