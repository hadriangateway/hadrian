use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AdminError;
use crate::{
    AppState,
    db::{DateRange, repos::UsageLogQuery},
    middleware::{AdminAuth, AuthzContext},
    models::{
        CostForecast, DailyModelSpend, DailyOrgSpend, DailyPricingSourceSpend, DailyProjectSpend,
        DailyProviderSpend, DailySpend, DailyTeamSpend, DailyUserSpend, ModelSpend, OrgSpend,
        PricingSourceSpend, ProjectSpend, ProviderSpend, RefererSpend, TeamSpend, UsageLogRecord,
        UsageSummary, UserSpend,
    },
    openapi::PaginationMeta,
    services::Services,
};

/// Query parameters for usage endpoints
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema, utoipa::IntoParams))]
pub struct UsageQuery {
    /// Start date (YYYY-MM-DD)
    pub start_date: Option<String>,
    /// End date (YYYY-MM-DD)
    pub end_date: Option<String>,
    /// **Hadrian Extension:** Filter by record type: "model" (LLM requests) or
    /// "tool" (server-side tool invocations). Omit to include both.
    pub record_type: Option<String>,
}

impl UsageQuery {
    fn parse_date_range(&self) -> Result<DateRange, AdminError> {
        // Unparseable dates are a 400, not a silent fallback: substituting
        // "today" for a typo would return wrong data with a 200.
        let parse = |field: &str, value: &Option<String>| -> Result<NaiveDate, AdminError> {
            match value {
                Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
                    AdminError::BadRequest(format!(
                        "Invalid {} '{}': expected YYYY-MM-DD",
                        field, s
                    ))
                }),
                None => Ok(chrono::Utc::now().date_naive()),
            }
        };

        let start = parse("start_date", &self.start_date)?;
        let end = parse("end_date", &self.end_date)?;

        if end < start {
            return Err(AdminError::BadRequest(
                "end_date must be >= start_date".to_string(),
            ));
        }

        let record_type = match self.record_type.as_deref() {
            None => None,
            Some(rt @ ("model" | "tool")) => Some(rt.to_string()),
            Some(other) => {
                return Err(AdminError::BadRequest(format!(
                    "Invalid record_type '{}': expected \"model\" or \"tool\"",
                    other
                )));
            }
        };

        Ok(DateRange {
            start,
            end,
            record_type,
        })
    }
}

/// Usage summary response
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct UsageSummaryResponse {
    /// Total cost in dollars
    pub total_cost: f64,
    /// Input tokens used
    pub input_tokens: i64,
    /// Output tokens used
    pub output_tokens: i64,
    /// Total tokens used
    pub total_tokens: i64,
    /// Number of requests
    pub request_count: i64,
    /// First request timestamp (RFC3339)
    pub first_request_at: Option<String>,
    /// Last request timestamp (RFC3339)
    pub last_request_at: Option<String>,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<UsageSummary> for UsageSummaryResponse {
    fn from(summary: UsageSummary) -> Self {
        Self {
            // Convert microcents to dollars for API response
            total_cost: summary.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            total_tokens: summary.total_tokens,
            request_count: summary.request_count,
            first_request_at: summary.first_request_at.map(|dt| dt.to_rfc3339()),
            last_request_at: summary.last_request_at.map(|dt| dt.to_rfc3339()),
            image_count: summary.image_count,
            audio_seconds: summary.audio_seconds,
            character_count: summary.character_count,
        }
    }
}

/// Daily usage breakdown
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailySpendResponse {
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Total cost in dollars for this day
    pub total_cost: f64,
    /// Input tokens used this day
    pub input_tokens: i64,
    /// Output tokens used this day
    pub output_tokens: i64,
    /// Total tokens used this day
    pub total_tokens: i64,
    /// Number of requests this day
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<DailySpend> for DailySpendResponse {
    fn from(spend: DailySpend) -> Self {
        Self {
            date: spend.date.to_string(),
            // Convert microcents to dollars for API response
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Usage breakdown by model
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ModelSpendResponse {
    /// Model name
    pub model: String,
    /// Total cost in dollars for this model
    pub total_cost: f64,
    /// Input tokens used by this model
    pub input_tokens: i64,
    /// Output tokens used by this model
    pub output_tokens: i64,
    /// Total tokens used by this model
    pub total_tokens: i64,
    /// Number of requests to this model
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<ModelSpend> for ModelSpendResponse {
    fn from(spend: ModelSpend) -> Self {
        Self {
            model: spend.model,
            // Convert microcents to dollars for API response
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Usage breakdown by HTTP referer
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct RefererSpendResponse {
    /// HTTP referer header value
    pub http_referer: Option<String>,
    /// Total cost in dollars from this referer
    pub total_cost: f64,
    /// Input tokens used from this referer
    pub input_tokens: i64,
    /// Output tokens used from this referer
    pub output_tokens: i64,
    /// Total tokens used from this referer
    pub total_tokens: i64,
    /// Number of requests from this referer
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<RefererSpend> for RefererSpendResponse {
    fn from(spend: RefererSpend) -> Self {
        Self {
            http_referer: spend.referer,
            // Convert microcents to dollars for API response
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Usage breakdown by provider
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ProviderSpendResponse {
    /// Provider name
    pub provider: String,
    /// Total cost in dollars for this provider
    pub total_cost: f64,
    /// Input tokens used by this provider
    pub input_tokens: i64,
    /// Output tokens used by this provider
    pub output_tokens: i64,
    /// Total tokens used by this provider
    pub total_tokens: i64,
    /// Number of requests to this provider
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<ProviderSpend> for ProviderSpendResponse {
    fn from(spend: ProviderSpend) -> Self {
        Self {
            provider: spend.provider,
            // Convert microcents to dollars for API response
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Daily usage breakdown by model
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyModelSpendResponse {
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Model name
    pub model: String,
    /// Total cost in dollars for this day/model
    pub total_cost: f64,
    /// Input tokens used
    pub input_tokens: i64,
    /// Output tokens used
    pub output_tokens: i64,
    /// Total tokens used
    pub total_tokens: i64,
    /// Number of requests
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<DailyModelSpend> for DailyModelSpendResponse {
    fn from(spend: DailyModelSpend) -> Self {
        Self {
            date: spend.date.to_string(),
            model: spend.model,
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Daily usage breakdown by provider
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyProviderSpendResponse {
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Provider name
    pub provider: String,
    /// Total cost in dollars for this day/provider
    pub total_cost: f64,
    /// Input tokens used
    pub input_tokens: i64,
    /// Output tokens used
    pub output_tokens: i64,
    /// Total tokens used
    pub total_tokens: i64,
    /// Number of requests
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<DailyProviderSpend> for DailyProviderSpendResponse {
    fn from(spend: DailyProviderSpend) -> Self {
        Self {
            date: spend.date.to_string(),
            provider: spend.provider,
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Usage breakdown by pricing source
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PricingSourceSpendResponse {
    /// Pricing source (provider, admin, provider_config, pricing_config, catalog, none)
    pub pricing_source: String,
    /// Total cost in dollars for this pricing source
    pub total_cost: f64,
    /// Input tokens
    pub input_tokens: i64,
    /// Output tokens
    pub output_tokens: i64,
    /// Total tokens
    pub total_tokens: i64,
    /// Number of requests
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<PricingSourceSpend> for PricingSourceSpendResponse {
    fn from(spend: PricingSourceSpend) -> Self {
        Self {
            pricing_source: spend.pricing_source,
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Daily usage breakdown by pricing source
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyPricingSourceSpendResponse {
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Pricing source
    pub pricing_source: String,
    /// Total cost in dollars for this day/pricing source
    pub total_cost: f64,
    /// Input tokens
    pub input_tokens: i64,
    /// Output tokens
    pub output_tokens: i64,
    /// Total tokens
    pub total_tokens: i64,
    /// Number of requests
    pub request_count: i64,
    /// **Hadrian Extension:** Number of images generated
    pub image_count: i64,
    /// **Hadrian Extension:** Audio duration in seconds
    pub audio_seconds: i64,
    /// **Hadrian Extension:** Character count (TTS input)
    pub character_count: i64,
}

impl From<DailyPricingSourceSpend> for DailyPricingSourceSpendResponse {
    fn from(spend: DailyPricingSourceSpend) -> Self {
        Self {
            date: spend.date.to_string(),
            pricing_source: spend.pricing_source,
            total_cost: spend.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: spend.input_tokens,
            output_tokens: spend.output_tokens,
            total_tokens: spend.total_tokens,
            request_count: spend.request_count,
            image_count: spend.image_count,
            audio_seconds: spend.audio_seconds,
            character_count: spend.character_count,
        }
    }
}

/// Usage breakdown by user
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct UserSpendResponse {
    /// User ID (null for unattributed usage)
    pub user_id: Option<String>,
    /// User display name
    pub user_name: Option<String>,
    /// User email
    pub user_email: Option<String>,
    /// Total cost in dollars
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<UserSpend> for UserSpendResponse {
    fn from(s: UserSpend) -> Self {
        Self {
            user_id: s.user_id.map(|id| id.to_string()),
            user_name: s.user_name,
            user_email: s.user_email,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Daily usage breakdown by user
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyUserSpendResponse {
    pub date: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<DailyUserSpend> for DailyUserSpendResponse {
    fn from(s: DailyUserSpend) -> Self {
        Self {
            date: s.date.to_string(),
            user_id: s.user_id.map(|id| id.to_string()),
            user_name: s.user_name,
            user_email: s.user_email,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Usage breakdown by project
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ProjectSpendResponse {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<ProjectSpend> for ProjectSpendResponse {
    fn from(s: ProjectSpend) -> Self {
        Self {
            project_id: s.project_id.map(|id| id.to_string()),
            project_name: s.project_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Daily usage breakdown by project
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyProjectSpendResponse {
    pub date: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<DailyProjectSpend> for DailyProjectSpendResponse {
    fn from(s: DailyProjectSpend) -> Self {
        Self {
            date: s.date.to_string(),
            project_id: s.project_id.map(|id| id.to_string()),
            project_name: s.project_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Usage breakdown by team
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct TeamSpendResponse {
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<TeamSpend> for TeamSpendResponse {
    fn from(s: TeamSpend) -> Self {
        Self {
            team_id: s.team_id.map(|id| id.to_string()),
            team_name: s.team_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Daily usage breakdown by team
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyTeamSpendResponse {
    pub date: String,
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<DailyTeamSpend> for DailyTeamSpendResponse {
    fn from(s: DailyTeamSpend) -> Self {
        Self {
            date: s.date.to_string(),
            team_id: s.team_id.map(|id| id.to_string()),
            team_name: s.team_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Usage breakdown by organization
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct OrgSpendResponse {
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<OrgSpend> for OrgSpendResponse {
    fn from(s: OrgSpend) -> Self {
        Self {
            org_id: s.org_id.map(|id| id.to_string()),
            org_name: s.org_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Daily usage breakdown by organization
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DailyOrgSpendResponse {
    pub date: String,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub total_cost: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

impl From<DailyOrgSpend> for DailyOrgSpendResponse {
    fn from(s: DailyOrgSpend) -> Self {
        Self {
            date: s.date.to_string(),
            org_id: s.org_id.map(|id| id.to_string()),
            org_name: s.org_name,
            total_cost: s.total_cost_microcents as f64 / 1_000_000.0,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: s.total_tokens,
            request_count: s.request_count,
            image_count: s.image_count,
            audio_seconds: s.audio_seconds,
            character_count: s.character_count,
        }
    }
}

/// Query parameters for forecast endpoint
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema, utoipa::IntoParams))]
pub struct ForecastQuery {
    /// Number of days of historical data to use (default: 30)
    pub lookback_days: Option<i32>,
    /// Number of days to forecast ahead (default: 7)
    pub forecast_days: Option<usize>,
}

/// Cost forecast response
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CostForecastResponse {
    /// Current spend in the budget period (dollars)
    pub current_spend: f64,
    /// Budget limit (dollars), null if no budget configured
    pub budget_limit: Option<f64>,
    /// Budget period (daily or monthly)
    pub budget_period: Option<String>,
    /// Average daily spend (dollars) based on historical data
    pub avg_daily_spend: f64,
    /// Standard deviation of daily spend (dollars)
    pub std_dev_daily_spend: f64,
    /// Number of days of historical data used
    pub sample_days: i32,
    /// Projected days until budget exhaustion (null if no budget or zero spend rate)
    pub days_until_exhaustion: Option<f64>,
    /// Projected exhaustion date (YYYY-MM-DD, null if no budget or zero spend rate)
    pub projected_exhaustion_date: Option<String>,
    /// Lower bound days until exhaustion (95% confidence, assumes +1 std dev spend)
    pub days_until_exhaustion_lower: Option<f64>,
    /// Upper bound days until exhaustion (95% confidence, assumes -1 std dev spend)
    pub days_until_exhaustion_upper: Option<f64>,
    /// Percentage of budget used in current period
    pub budget_utilization_percent: Option<f64>,
    /// Projected end-of-period spend at current rate (dollars)
    pub projected_period_spend: Option<f64>,
    /// Time series forecast with prediction intervals (null if insufficient data)
    pub time_series_forecast: Option<TimeSeriesForecastResponse>,
}

/// Time series forecast with prediction intervals
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct TimeSeriesForecastResponse {
    /// Dates for each forecast point (YYYY-MM-DD)
    pub dates: Vec<String>,
    /// Point forecasts (daily spend in dollars)
    pub point_forecasts: Vec<f64>,
    /// Lower bound of prediction interval (dollars)
    pub lower_bounds: Vec<f64>,
    /// Upper bound of prediction interval (dollars)
    pub upper_bounds: Vec<f64>,
    /// Confidence level for prediction intervals (e.g., 0.95 for 95%)
    pub confidence_level: f64,
    /// Whether MSTL decomposition was used (false = simple ETS)
    pub used_seasonal_decomposition: bool,
}

fn get_services(state: &AppState) -> Result<&Services, AdminError> {
    state.services.as_ref().ok_or(AdminError::ServicesRequired)
}

/// Pre-fetch an API key and authorise the caller against the key's owner scope.
///
/// `/admin/v1/api-keys/{key_id}/usage/...` handlers used to pass `None` for
/// org/team/project, so cross-tenant org-admins satisfied policies that key on
/// tenant scope. This helper resolves the `ApiKeyOwner` to the matching
/// `(org, team, project)` triple before calling `authz.require`.
async fn usage_key_authz(
    services: &Services,
    authz: &AuthzContext,
    key_id: Uuid,
) -> Result<(), AdminError> {
    let key = services
        .api_keys
        .get_by_id(key_id)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("API key '{key_id}' not found")))?;
    let key_id_str = key_id.to_string();
    match key.owner {
        crate::models::ApiKeyOwner::Organization { org_id } => {
            authz.require(
                "usage",
                "read",
                Some(&key_id_str),
                Some(&org_id.to_string()),
                None,
                None,
            )?;
        }
        crate::models::ApiKeyOwner::Team { team_id } => {
            let team = services
                .teams
                .get_by_id(team_id)
                .await?
                .ok_or_else(|| AdminError::NotFound(format!("Team '{team_id}' not found")))?;
            authz.require(
                "usage",
                "read",
                Some(&key_id_str),
                Some(&team.org_id.to_string()),
                Some(&team_id.to_string()),
                None,
            )?;
        }
        crate::models::ApiKeyOwner::Project { project_id } => {
            let project = services
                .projects
                .get_by_id(project_id)
                .await?
                .ok_or_else(|| AdminError::NotFound(format!("Project '{project_id}' not found")))?;
            authz.require(
                "usage",
                "read",
                Some(&key_id_str),
                Some(&project.org_id.to_string()),
                None,
                Some(&project_id.to_string()),
            )?;
        }
        crate::models::ApiKeyOwner::User { .. } => {
            authz.require("usage", "read", Some(&key_id_str), None, None, None)?;
        }
        crate::models::ApiKeyOwner::ServiceAccount { service_account_id } => {
            let sa = services
                .service_accounts
                .get_by_id(service_account_id)
                .await?
                .ok_or_else(|| {
                    AdminError::NotFound(format!(
                        "Service account '{service_account_id}' not found"
                    ))
                })?;
            authz.require(
                "usage",
                "read",
                Some(&key_id_str),
                Some(&sa.org_id.to_string()),
                None,
                None,
            )?;
        }
    }
    Ok(())
}

/// Pre-fetch a user and authorise the caller against the user's organization scope.
///
/// Users in this codebase only ever belong to one organization, so the first
/// (and only) org membership is treated as the user's authoritative scope.
/// A user with no org membership is treated as platform-scoped (no org filter).
async fn usage_user_authz(
    services: &Services,
    authz: &AuthzContext,
    user_id: Uuid,
) -> Result<(), AdminError> {
    let _user = services
        .users
        .get_by_id(user_id)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("User not found: {user_id}")))?;
    let user_id_str = user_id.to_string();
    let org_id = services
        .users
        .get_org_memberships_for_user(user_id)
        .await?
        .into_iter()
        .next()
        .map(|m| m.org_id.to_string());
    authz.require(
        "usage",
        "read",
        Some(&user_id_str),
        org_id.as_deref(),
        None,
        None,
    )?;
    Ok(())
}

/// Get usage summary for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage",
    tag = "usage",
    operation_id = "usage_get_summary",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_summary(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;

    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary(key_id, range).await?;

    Ok(Json(summary.into()))
}

/// Get usage by date for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_by_date",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_by_date(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;

    let range = query.parse_date_range()?;
    let daily_spend = services.usage.get_by_date(key_id, range).await?;

    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_by_model",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_by_model(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;

    let range = query.parse_date_range()?;
    let model_spend = services.usage.get_by_model(key_id, range).await?;

    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by referer for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-referer",
    tag = "usage",
    operation_id = "usage_get_by_referer",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by HTTP referer", body = Vec<RefererSpendResponse>),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_by_referer(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<RefererSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;

    let range = query.parse_date_range()?;
    let referer_spend = services.usage.get_by_referer(key_id, range).await?;

    Ok(Json(referer_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for an API key
///
/// Uses historical usage data to predict when the budget will be exhausted.
/// Returns average daily spend, projected exhaustion date, and confidence intervals.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_forecast",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_forecast(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;

    let forecast = services
        .usage
        .get_forecast(key_id, query.lookback_days, query.forecast_days)
        .await?;

    // Convert time series forecast from microcents to dollars
    let time_series_forecast = forecast.time_series_forecast.map(|ts| {
        TimeSeriesForecastResponse {
            dates: ts.dates.iter().map(|d| d.to_string()).collect(),
            // Convert from microcents to dollars
            point_forecasts: ts.point_forecasts.iter().map(|v| v / 1_000_000.0).collect(),
            lower_bounds: ts.lower_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            upper_bounds: ts.upper_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            confidence_level: ts.confidence_level,
            used_seasonal_decomposition: ts.used_seasonal_decomposition,
        }
    });

    // Convert microcents to dollars for API response
    Ok(Json(CostForecastResponse {
        current_spend: forecast.current_spend_microcents as f64 / 1_000_000.0,
        budget_limit: forecast
            .budget_limit_microcents
            .map(|v| v as f64 / 1_000_000.0),
        budget_period: forecast.budget_period,
        avg_daily_spend: forecast.avg_daily_spend_microcents as f64 / 1_000_000.0,
        std_dev_daily_spend: forecast.std_dev_daily_spend_microcents as f64 / 1_000_000.0,
        sample_days: forecast.sample_days,
        days_until_exhaustion: forecast.days_until_exhaustion,
        projected_exhaustion_date: forecast.projected_exhaustion_date.map(|d| d.to_string()),
        days_until_exhaustion_lower: forecast.days_until_exhaustion_lower,
        days_until_exhaustion_upper: forecast.days_until_exhaustion_upper,
        budget_utilization_percent: forecast.budget_utilization_percent,
        projected_period_spend: forecast
            .projected_period_spend_microcents
            .map(|v| v as f64 / 1_000_000.0),
        time_series_forecast,
    }))
}

// ==================== Organization Usage Endpoints ====================

/// Get usage summary for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage",
    tag = "usage",
    operation_id = "usage_get_org_summary",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_summary(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let services = get_services(&state)?;

    // Resolve organization slug to ID
    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary_by_org(org.id, range).await?;

    Ok(Json(summary.into()))
}

/// Get usage by date for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_org_by_date",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_date(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let daily_spend = services.usage.get_by_date_by_org(org.id, range).await?;

    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_org_by_model",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_model(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let model_spend = services.usage.get_by_model_by_org(org.id, range).await?;

    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by provider for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_org_by_provider",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_provider(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let provider_spend = services.usage.get_by_provider_by_org(org.id, range).await?;

    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for an organization
///
/// Uses historical usage data across all API keys in the organization to predict future spending.
/// Returns average daily spend, confidence intervals, and time series forecasts.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_org_forecast",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_forecast(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let forecast = services
        .usage
        .get_forecast_by_org(org.id, query.lookback_days, query.forecast_days)
        .await?;

    // Convert time series forecast from microcents to dollars
    let time_series_forecast = forecast
        .time_series_forecast
        .map(|ts| TimeSeriesForecastResponse {
            dates: ts.dates.iter().map(|d| d.to_string()).collect(),
            point_forecasts: ts.point_forecasts.iter().map(|v| v / 1_000_000.0).collect(),
            lower_bounds: ts.lower_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            upper_bounds: ts.upper_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            confidence_level: ts.confidence_level,
            used_seasonal_decomposition: ts.used_seasonal_decomposition,
        });

    Ok(Json(CostForecastResponse {
        current_spend: forecast.current_spend_microcents as f64 / 1_000_000.0,
        budget_limit: forecast
            .budget_limit_microcents
            .map(|v| v as f64 / 1_000_000.0),
        budget_period: forecast.budget_period,
        avg_daily_spend: forecast.avg_daily_spend_microcents as f64 / 1_000_000.0,
        std_dev_daily_spend: forecast.std_dev_daily_spend_microcents as f64 / 1_000_000.0,
        sample_days: forecast.sample_days,
        days_until_exhaustion: forecast.days_until_exhaustion,
        projected_exhaustion_date: forecast.projected_exhaustion_date.map(|d| d.to_string()),
        days_until_exhaustion_lower: forecast.days_until_exhaustion_lower,
        days_until_exhaustion_upper: forecast.days_until_exhaustion_upper,
        budget_utilization_percent: forecast.budget_utilization_percent,
        projected_period_spend: forecast
            .projected_period_spend_microcents
            .map(|v| v as f64 / 1_000_000.0),
        time_series_forecast,
    }))
}

// ==================== Project Usage Endpoints ====================

/// Path parameters for project usage endpoints
#[derive(Debug, Deserialize)]
pub struct ProjectUsagePath {
    pub org_slug: String,
    pub project_slug: String,
}

/// Get usage summary for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage",
    tag = "usage",
    operation_id = "usage_get_project_summary",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_summary(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let services = get_services(&state)?;

    // Resolve organization slug to ID first
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    // Resolve project slug to ID
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let range = query.parse_date_range()?;
    let summary = services
        .usage
        .get_summary_by_project(project.id, range)
        .await?;

    Ok(Json(summary.into()))
}

/// Get usage by date for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_project_by_date",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_by_date(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    // Resolve organization slug to ID first
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let range = query.parse_date_range()?;
    let daily_spend = services
        .usage
        .get_by_date_by_project(project.id, range)
        .await?;

    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_project_by_model",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_by_model(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    // Resolve organization slug to ID first
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let range = query.parse_date_range()?;
    let model_spend = services
        .usage
        .get_by_model_by_project(project.id, range)
        .await?;

    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for a project
///
/// Uses historical usage data across all API keys in the project to predict future spending.
/// Returns average daily spend, confidence intervals, and time series forecasts.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_project_forecast",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_forecast(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let services = get_services(&state)?;

    // Resolve organization slug to ID first
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let forecast = services
        .usage
        .get_forecast_by_project(project.id, query.lookback_days, query.forecast_days)
        .await?;

    // Convert time series forecast from microcents to dollars
    let time_series_forecast = forecast
        .time_series_forecast
        .map(|ts| TimeSeriesForecastResponse {
            dates: ts.dates.iter().map(|d| d.to_string()).collect(),
            point_forecasts: ts.point_forecasts.iter().map(|v| v / 1_000_000.0).collect(),
            lower_bounds: ts.lower_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            upper_bounds: ts.upper_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            confidence_level: ts.confidence_level,
            used_seasonal_decomposition: ts.used_seasonal_decomposition,
        });

    Ok(Json(CostForecastResponse {
        current_spend: forecast.current_spend_microcents as f64 / 1_000_000.0,
        budget_limit: forecast
            .budget_limit_microcents
            .map(|v| v as f64 / 1_000_000.0),
        budget_period: forecast.budget_period,
        avg_daily_spend: forecast.avg_daily_spend_microcents as f64 / 1_000_000.0,
        std_dev_daily_spend: forecast.std_dev_daily_spend_microcents as f64 / 1_000_000.0,
        sample_days: forecast.sample_days,
        days_until_exhaustion: forecast.days_until_exhaustion,
        projected_exhaustion_date: forecast.projected_exhaustion_date.map(|d| d.to_string()),
        days_until_exhaustion_lower: forecast.days_until_exhaustion_lower,
        days_until_exhaustion_upper: forecast.days_until_exhaustion_upper,
        budget_utilization_percent: forecast.budget_utilization_percent,
        projected_period_spend: forecast
            .projected_period_spend_microcents
            .map(|v| v as f64 / 1_000_000.0),
        time_series_forecast,
    }))
}

// ==================== User Usage Endpoints ====================

/// Get usage summary for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage",
    tag = "usage",
    operation_id = "usage_get_user_summary",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 404, description = "User not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_user_summary(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary_by_user(user_id, range).await?;

    Ok(Json(summary.into()))
}

/// Get usage by date for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_user_by_date",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 404, description = "User not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_user_by_date(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    let range = query.parse_date_range()?;
    let daily_spend = services.usage.get_by_date_by_user(user_id, range).await?;

    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_user_by_model",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 404, description = "User not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_user_by_model(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    let range = query.parse_date_range()?;
    let model_spend = services.usage.get_by_model_by_user(user_id, range).await?;

    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for a user
///
/// Uses historical usage data across all API keys owned by the user to predict future spending.
/// Returns average daily spend, confidence intervals, and time series forecasts.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_user_forecast",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
        (status = 404, description = "User not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_user_forecast(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    let forecast = services
        .usage
        .get_forecast_by_user(user_id, query.lookback_days, query.forecast_days)
        .await?;

    // Convert time series forecast from microcents to dollars
    let time_series_forecast = forecast
        .time_series_forecast
        .map(|ts| TimeSeriesForecastResponse {
            dates: ts.dates.iter().map(|d| d.to_string()).collect(),
            point_forecasts: ts.point_forecasts.iter().map(|v| v / 1_000_000.0).collect(),
            lower_bounds: ts.lower_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            upper_bounds: ts.upper_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            confidence_level: ts.confidence_level,
            used_seasonal_decomposition: ts.used_seasonal_decomposition,
        });

    Ok(Json(CostForecastResponse {
        current_spend: forecast.current_spend_microcents as f64 / 1_000_000.0,
        budget_limit: forecast
            .budget_limit_microcents
            .map(|v| v as f64 / 1_000_000.0),
        budget_period: forecast.budget_period,
        avg_daily_spend: forecast.avg_daily_spend_microcents as f64 / 1_000_000.0,
        std_dev_daily_spend: forecast.std_dev_daily_spend_microcents as f64 / 1_000_000.0,
        sample_days: forecast.sample_days,
        days_until_exhaustion: forecast.days_until_exhaustion,
        projected_exhaustion_date: forecast.projected_exhaustion_date.map(|d| d.to_string()),
        days_until_exhaustion_lower: forecast.days_until_exhaustion_lower,
        days_until_exhaustion_upper: forecast.days_until_exhaustion_upper,
        budget_utilization_percent: forecast.budget_utilization_percent,
        projected_period_spend: forecast
            .projected_period_spend_microcents
            .map(|v| v as f64 / 1_000_000.0),
        time_series_forecast,
    }))
}

// ==================== Provider Usage Endpoints ====================

/// Get usage summary for a provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/providers/{provider}/usage",
    tag = "usage",
    operation_id = "usage_get_provider_summary",
    params(
        ("provider" = String, Path, description = "Provider name"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
    )
))]
pub async fn get_provider_summary(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let provider_str = provider.to_string();
    authz.require("usage", "read", Some(&provider_str), None, None, None)?;
    let services = get_services(&state)?;

    let range = query.parse_date_range()?;
    let summary = services
        .usage
        .get_summary_by_provider(&provider, range)
        .await?;

    Ok(Json(summary.into()))
}

/// Get usage by date for a provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/providers/{provider}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_provider_by_date",
    params(
        ("provider" = String, Path, description = "Provider name"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
    )
))]
pub async fn get_provider_by_date(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let provider_str = provider.to_string();
    authz.require("usage", "read", Some(&provider_str), None, None, None)?;
    let services = get_services(&state)?;

    let range = query.parse_date_range()?;
    let daily_spend = services
        .usage
        .get_by_date_by_provider(&provider, range)
        .await?;

    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for a provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/providers/{provider}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_provider_by_model",
    params(
        ("provider" = String, Path, description = "Provider name"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
    )
))]
pub async fn get_provider_by_model(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let provider_str = provider.to_string();
    authz.require("usage", "read", Some(&provider_str), None, None, None)?;
    let services = get_services(&state)?;

    let range = query.parse_date_range()?;
    let model_spend = services
        .usage
        .get_by_model_by_provider(&provider, range)
        .await?;

    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for a provider
///
/// Uses historical usage data across all API keys using this provider to predict future spending.
/// Returns average daily spend, confidence intervals, and time series forecasts.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/providers/{provider}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_provider_forecast",
    params(
        ("provider" = String, Path, description = "Provider name"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
    )
))]
pub async fn get_provider_forecast(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let provider_str = provider.to_string();
    authz.require("usage", "read", Some(&provider_str), None, None, None)?;
    let services = get_services(&state)?;

    let forecast = services
        .usage
        .get_forecast_by_provider(&provider, query.lookback_days, query.forecast_days)
        .await?;

    Ok(Json(forecast_to_response(forecast)))
}

/// Convert a CostForecast to a CostForecastResponse (microcents -> dollars)
fn forecast_to_response(forecast: CostForecast) -> CostForecastResponse {
    let time_series_forecast = forecast
        .time_series_forecast
        .map(|ts| TimeSeriesForecastResponse {
            dates: ts.dates.iter().map(|d| d.to_string()).collect(),
            point_forecasts: ts.point_forecasts.iter().map(|v| v / 1_000_000.0).collect(),
            lower_bounds: ts.lower_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            upper_bounds: ts.upper_bounds.iter().map(|v| v / 1_000_000.0).collect(),
            confidence_level: ts.confidence_level,
            used_seasonal_decomposition: ts.used_seasonal_decomposition,
        });

    CostForecastResponse {
        current_spend: forecast.current_spend_microcents as f64 / 1_000_000.0,
        budget_limit: forecast
            .budget_limit_microcents
            .map(|v| v as f64 / 1_000_000.0),
        budget_period: forecast.budget_period,
        avg_daily_spend: forecast.avg_daily_spend_microcents as f64 / 1_000_000.0,
        std_dev_daily_spend: forecast.std_dev_daily_spend_microcents as f64 / 1_000_000.0,
        sample_days: forecast.sample_days,
        days_until_exhaustion: forecast.days_until_exhaustion,
        projected_exhaustion_date: forecast.projected_exhaustion_date.map(|d| d.to_string()),
        days_until_exhaustion_lower: forecast.days_until_exhaustion_lower,
        days_until_exhaustion_upper: forecast.days_until_exhaustion_upper,
        budget_utilization_percent: forecast.budget_utilization_percent,
        projected_period_spend: forecast
            .projected_period_spend_microcents
            .map(|v| v as f64 / 1_000_000.0),
        time_series_forecast,
    }
}

// ==================== Team Usage Endpoints ====================

/// Path parameters for team usage endpoints
#[derive(Debug, Deserialize)]
pub struct TeamUsagePath {
    pub org_slug: String,
    pub team_slug: String,
}

/// Resolve org + team slugs to team ID
async fn resolve_team(
    services: &Services,
    org_slug: &str,
    team_slug: &str,
) -> Result<(Uuid, Uuid), AdminError> {
    let org = services
        .organizations
        .get_by_slug(org_slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {org_slug}")))?;

    let team = services
        .teams
        .get_by_slug(org.id, team_slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Team not found: {org_slug}/{team_slug}")))?;

    Ok((org.id, team.id))
}

/// Get usage summary for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage",
    tag = "usage",
    operation_id = "usage_get_team_summary",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_summary(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary_by_team(team_id, range).await?;
    Ok(Json(summary.into()))
}

/// Get usage by date for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_team_by_date",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_date(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let daily_spend = services.usage.get_by_date_by_team(team_id, range).await?;
    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by model for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_team_by_model",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_model(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let model_spend = services.usage.get_by_model_by_team(team_id, range).await?;
    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by provider for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_team_by_provider",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_provider(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let provider_spend = services
        .usage
        .get_by_provider_by_team(team_id, range)
        .await?;
    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get cost forecast for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/forecast",
    tag = "usage",
    operation_id = "usage_get_team_forecast",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        ForecastQuery,
    ),
    responses(
        (status = 200, description = "Cost forecast", body = CostForecastResponse),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_forecast(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<ForecastQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<CostForecastResponse>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let forecast = services
        .usage
        .get_forecast_by_team(team_id, query.lookback_days, query.forecast_days)
        .await?;
    Ok(Json(forecast_to_response(forecast)))
}

// ==================== Self-Service Usage Endpoints ====================

/// Get current user's usage summary
///
/// Returns usage data for the authenticated user. Does not require admin role.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage",
    tag = "me",
    operation_id = "me_usage_summary",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage summary", body = UsageSummaryResponse),
        (status = 401, description = "User not identified from session", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_me_summary(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary_by_user(user_id, range).await?;
    Ok(Json(summary.into()))
}

/// Get current user's usage by date
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-date",
    tag = "me",
    operation_id = "me_usage_by_date",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
        (status = 401, description = "User not identified from session", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_me_by_date(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let daily_spend = services.usage.get_by_date_by_user(user_id, range).await?;
    Ok(Json(daily_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by model
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-model",
    tag = "me",
    operation_id = "me_usage_by_model",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
        (status = 401, description = "User not identified from session", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_me_by_model(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let model_spend = services.usage.get_by_model_by_user(user_id, range).await?;
    Ok(Json(model_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by provider for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_by_provider",
    params(
        ("key_id" = Uuid, Path, description = "API key ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 404, description = "API key not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_by_provider(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;
    let range = query.parse_date_range()?;
    let provider_spend = services.usage.get_by_provider(key_id, range).await?;
    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by provider for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_project_by_provider",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_by_provider(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;
    let range = query.parse_date_range()?;
    let provider_spend = services
        .usage
        .get_by_provider_by_project(project.id, range)
        .await?;
    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by provider for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_user_by_provider",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 404, description = "User not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_user_by_provider(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let provider_spend = services
        .usage
        .get_by_provider_by_user(user_id, range)
        .await?;
    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-provider",
    tag = "me",
    operation_id = "me_usage_by_provider",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
        (status = 401, description = "User not identified from session", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_me_by_provider(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let provider_spend = services
        .usage
        .get_by_provider_by_user(user_id, range)
        .await?;
    Ok(Json(provider_spend.into_iter().map(|s| s.into()).collect()))
}

// ==================== Daily Model Breakdown Endpoints ====================

/// Get usage by date and model for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_by_date_model",
    params(("key_id" = Uuid, Path, description = "API key ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_by_date_model(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_model(key_id, range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and model for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_org_by_date_model",
    params(("slug" = String, Path, description = "Organization slug"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_org_by_date_model(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_model_by_org(org.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and model for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_project_by_date_model",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_project_by_date_model(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_model_by_project(project.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and model for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_user_by_date_model",
    params(("user_id" = Uuid, Path, description = "User ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_user_by_date_model(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_model_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and model for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_team_by_date_model",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_team_by_date_model(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_model_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by date and model
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-date-model",
    tag = "me",
    operation_id = "me_usage_by_date_model",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_me_by_date_model(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_model_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

// ==================== Daily Provider Breakdown Endpoints ====================

/// Get usage by date and provider for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_by_date_provider",
    params(("key_id" = Uuid, Path, description = "API key ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_by_date_provider(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_provider(key_id, range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and provider for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_org_by_date_provider",
    params(("slug" = String, Path, description = "Organization slug"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_org_by_date_provider(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_provider_by_org(org.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and provider for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_project_by_date_provider",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_project_by_date_provider(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_provider_by_project(project.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and provider for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_user_by_date_provider",
    params(("user_id" = Uuid, Path, description = "User ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_user_by_date_provider(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_provider_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and provider for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_team_by_date_provider",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_team_by_date_provider(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_provider_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by date and provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-date-provider",
    tag = "me",
    operation_id = "me_usage_by_date_provider",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_me_by_date_provider(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_provider_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

// ==================== Pricing Source Breakdown Endpoints ====================

/// Get usage by pricing source for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_by_pricing_source",
    params(("key_id" = Uuid, Path, description = "API key ID"), UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_by_pricing_source(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_pricing_source(key_id, range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by pricing source for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_org_by_pricing_source",
    params(("slug" = String, Path, description = "Organization slug"), UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_org_by_pricing_source(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_pricing_source_by_org(org.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by pricing source for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_project_by_pricing_source",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_project_by_pricing_source(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_pricing_source_by_project(project.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by pricing source for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_user_by_pricing_source",
    params(("user_id" = Uuid, Path, description = "User ID"), UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_user_by_pricing_source(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_pricing_source_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by pricing source for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_team_by_pricing_source",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_team_by_pricing_source(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_pricing_source_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by pricing source
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-pricing-source",
    tag = "me",
    operation_id = "me_usage_by_pricing_source",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_me_by_pricing_source(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_pricing_source_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

// ==================== Daily Pricing Source Breakdown Endpoints ====================

/// Get usage by date and pricing source for an API key
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/api-keys/{key_id}/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_by_date_pricing_source",
    params(("key_id" = Uuid, Path, description = "API key ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_by_date_pricing_source(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_key_authz(services, &authz, key_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source(key_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and pricing source for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_org_by_date_pricing_source",
    params(("slug" = String, Path, description = "Organization slug"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_org_by_date_pricing_source(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_by_org(org.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and pricing source for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_project_by_date_pricing_source",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_project_by_date_pricing_source(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;
    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_by_project(project.id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and pricing source for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_user_by_date_pricing_source",
    params(("user_id" = Uuid, Path, description = "User ID"), UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_user_by_date_pricing_source(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and pricing source for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_team_by_date_pricing_source",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_team_by_date_pricing_source(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get current user's usage by date and pricing source
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/by-date-pricing-source",
    tag = "me",
    operation_id = "me_usage_by_date_pricing_source",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_me_by_date_pricing_source(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_by_user(user_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by user for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-user",
    tag = "usage",
    operation_id = "usage_get_project_by_user",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by user", body = Vec<UserSpendResponse>),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_by_user(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<UserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_user_by_project(project.id, range)
        .await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and user for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/usage/by-date-user",
    tag = "usage",
    operation_id = "usage_get_project_by_date_user",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by user", body = Vec<DailyUserSpendResponse>),
        (status = 404, description = "Project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_project_by_date_user(
    State(state): State<AppState>,
    Path(path): Path<ProjectUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyUserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&path.org_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!("Organization not found: {}", path.org_slug))
        })?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let project = services
        .projects
        .get_by_slug(org.id, &path.project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project not found: {}/{}",
                path.org_slug, path.project_slug
            ))
        })?;

    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_user_by_project(project.id, range)
        .await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by user for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-user",
    tag = "usage",
    operation_id = "usage_get_team_by_user",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by user", body = Vec<UserSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_user(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<UserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_user_by_team(team_id, range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and user for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date-user",
    tag = "usage",
    operation_id = "usage_get_team_by_date_user",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by user", body = Vec<DailyUserSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_date_user(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyUserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_user_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by project for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-project",
    tag = "usage",
    operation_id = "usage_get_team_by_project",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by project", body = Vec<ProjectSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_project(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProjectSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_project_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and project for a team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/teams/{team_slug}/usage/by-date-project",
    tag = "usage",
    operation_id = "usage_get_team_by_date_project",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("team_slug" = String, Path, description = "Team slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by project", body = Vec<DailyProjectSpendResponse>),
        (status = 404, description = "Team not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_team_by_date_project(
    State(state): State<AppState>,
    Path(path): Path<TeamUsagePath>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProjectSpendResponse>>, AdminError> {
    let services = get_services(&state)?;
    let (org_id, team_id) = resolve_team(services, &path.org_slug, &path.team_slug).await?;
    authz.require("usage", "read", None, Some(&org_id.to_string()), None, None)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_project_by_team(team_id, range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by user for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-user",
    tag = "usage",
    operation_id = "usage_get_org_by_user",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by user", body = Vec<UserSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_user(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<UserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services.usage.get_by_user_by_org(org.id, range).await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and user for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-user",
    tag = "usage",
    operation_id = "usage_get_org_by_date_user",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by user", body = Vec<DailyUserSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_date_user(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyUserSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_user_by_org(org.id, range)
        .await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by project for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-project",
    tag = "usage",
    operation_id = "usage_get_org_by_project",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by project", body = Vec<ProjectSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProjectSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services.usage.get_by_project_by_org(org.id, range).await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and project for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-project",
    tag = "usage",
    operation_id = "usage_get_org_by_date_project",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by project", body = Vec<DailyProjectSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_date_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProjectSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_project_by_org(org.id, range)
        .await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by team for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-team",
    tag = "usage",
    operation_id = "usage_get_org_by_team",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Usage breakdown by team", body = Vec<TeamSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_team(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<TeamSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services.usage.get_by_team_by_org(org.id, range).await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get usage by date and team for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{slug}/usage/by-date-team",
    tag = "usage",
    operation_id = "usage_get_org_by_date_team",
    params(
        ("slug" = String, Path, description = "Organization slug"),
        UsageQuery,
    ),
    responses(
        (status = 200, description = "Daily usage breakdown by team", body = Vec<DailyTeamSpendResponse>),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get_org_by_date_team(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyTeamSpendResponse>>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization not found: {slug}")))?;
    authz.require("usage", "read", None, Some(&org.id.to_string()), None, None)?;

    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_team_by_org(org.id, range)
        .await?;

    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage summary
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage",
    tag = "usage",
    operation_id = "usage_get_global_summary",
    params(UsageQuery),
    responses(
        (status = 200, description = "Global usage summary", body = UsageSummaryResponse),
    )
))]
pub async fn get_global_summary(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<UsageSummaryResponse>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let summary = services.usage.get_summary_global(range).await?;
    Ok(Json(summary.into()))
}

/// Get global usage by date
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date",
    tag = "usage",
    operation_id = "usage_get_global_by_date",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown", body = Vec<DailySpendResponse>),
    )
))]
pub async fn get_global_by_date(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailySpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by model
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-model",
    tag = "usage",
    operation_id = "usage_get_global_by_model",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by model", body = Vec<ModelSpendResponse>),
    )
))]
pub async fn get_global_by_model(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ModelSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_model_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-provider",
    tag = "usage",
    operation_id = "usage_get_global_by_provider",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by provider", body = Vec<ProviderSpendResponse>),
    )
))]
pub async fn get_global_by_provider(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProviderSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_provider_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by pricing source
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-pricing-source",
    tag = "usage",
    operation_id = "usage_get_global_by_pricing_source",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by pricing source", body = Vec<PricingSourceSpendResponse>),
    )
))]
pub async fn get_global_by_pricing_source(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<PricingSourceSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_pricing_source_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and model
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-model",
    tag = "usage",
    operation_id = "usage_get_global_by_date_model",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by model", body = Vec<DailyModelSpendResponse>),
    )
))]
pub async fn get_global_by_date_model(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyModelSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_model_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and provider
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-provider",
    tag = "usage",
    operation_id = "usage_get_global_by_date_provider",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by provider", body = Vec<DailyProviderSpendResponse>),
    )
))]
pub async fn get_global_by_date_provider(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProviderSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_provider_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and pricing source
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-pricing-source",
    tag = "usage",
    operation_id = "usage_get_global_by_date_pricing_source",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by pricing source", body = Vec<DailyPricingSourceSpendResponse>),
    )
))]
pub async fn get_global_by_date_pricing_source(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyPricingSourceSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services
        .usage
        .get_by_date_pricing_source_global(range)
        .await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-user",
    tag = "usage",
    operation_id = "usage_get_global_by_user",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by user", body = Vec<UserSpendResponse>),
    )
))]
pub async fn get_global_by_user(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<UserSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_user_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-user",
    tag = "usage",
    operation_id = "usage_get_global_by_date_user",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by user", body = Vec<DailyUserSpendResponse>),
    )
))]
pub async fn get_global_by_date_user(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyUserSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_user_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-project",
    tag = "usage",
    operation_id = "usage_get_global_by_project",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by project", body = Vec<ProjectSpendResponse>),
    )
))]
pub async fn get_global_by_project(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<ProjectSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_project_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-project",
    tag = "usage",
    operation_id = "usage_get_global_by_date_project",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by project", body = Vec<DailyProjectSpendResponse>),
    )
))]
pub async fn get_global_by_date_project(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyProjectSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_project_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-team",
    tag = "usage",
    operation_id = "usage_get_global_by_team",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by team", body = Vec<TeamSpendResponse>),
    )
))]
pub async fn get_global_by_team(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<TeamSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_team_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and team
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-team",
    tag = "usage",
    operation_id = "usage_get_global_by_date_team",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by team", body = Vec<DailyTeamSpendResponse>),
    )
))]
pub async fn get_global_by_date_team(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyTeamSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_team_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-org",
    tag = "usage",
    operation_id = "usage_get_global_by_org",
    params(UsageQuery),
    responses(
        (status = 200, description = "Usage breakdown by organization", body = Vec<OrgSpendResponse>),
    )
))]
pub async fn get_global_by_org(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<OrgSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_org_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

/// Get global usage by date and organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/by-date-org",
    tag = "usage",
    operation_id = "usage_get_global_by_date_org",
    params(UsageQuery),
    responses(
        (status = 200, description = "Daily usage breakdown by organization", body = Vec<DailyOrgSpendResponse>),
    )
))]
pub async fn get_global_by_date_org(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
    Extension(authz): Extension<AuthzContext>,
) -> Result<Json<Vec<DailyOrgSpendResponse>>, AdminError> {
    authz.require("usage", "read", None, None, None, None)?;
    let services = get_services(&state)?;
    let range = query.parse_date_range()?;
    let data = services.usage.get_by_date_org_global(range).await?;
    Ok(Json(data.into_iter().map(|s| s.into()).collect()))
}

// ==================== Usage Log Endpoints ====================

/// Query parameters for usage log list endpoints
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::IntoParams))]
pub struct UsageLogQueryParams {
    pub org_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub provider_source: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub direction: Option<String>,
    /// Filter by record type: "model" (LLM requests) or "tool" (server-side
    /// tool invocations). Omit for both.
    pub record_type: Option<String>,
}

impl UsageLogQueryParams {
    fn into_query(self) -> UsageLogQuery {
        UsageLogQuery {
            org_id: self.org_id,
            user_id: self.user_id,
            project_id: self.project_id,
            team_id: self.team_id,
            api_key_id: self.api_key_id,
            service_account_id: self.service_account_id,
            model: self.model,
            provider: self.provider,
            provider_source: self.provider_source,
            from: self.from,
            to: self.to,
            limit: self.limit,
            cursor: self.cursor,
            direction: self.direction,
            record_type: self.record_type,
        }
    }

    fn into_export_query(self) -> UsageLogQuery {
        UsageLogQuery {
            org_id: self.org_id,
            user_id: self.user_id,
            project_id: self.project_id,
            team_id: self.team_id,
            api_key_id: self.api_key_id,
            service_account_id: self.service_account_id,
            model: self.model,
            provider: self.provider,
            provider_source: self.provider_source,
            from: self.from,
            to: self.to,
            limit: Some(EXPORT_ROW_LIMIT),
            cursor: None,
            direction: None,
            record_type: self.record_type,
        }
    }
}

/// Hard cap on export rows. Exports past this length set the
/// `X-Export-Truncated: true` response header; use the paginated
/// `/admin/v1/usage/logs` endpoint for complete data.
const EXPORT_ROW_LIMIT: i64 = 10_000;

/// Individual usage log entry response
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct UsageLogResponse {
    pub id: Uuid,
    pub recorded_at: String,
    pub request_id: String,
    /// Client-supplied correlation id (X-Request-Id / X-Client-Request-Id)
    pub client_request_id: Option<String>,
    pub api_key_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub model: String,
    pub provider: String,
    pub http_referer: Option<String>,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub reasoning_tokens: i32,
    /// Cost in dollars
    pub cost: f64,
    pub streamed: bool,
    pub finish_reason: Option<String>,
    pub latency_ms: Option<i32>,
    pub cancelled: bool,
    pub status_code: Option<i16>,
    pub pricing_source: String,
    pub image_count: Option<i32>,
    pub audio_seconds: Option<i32>,
    pub character_count: Option<i32>,
    pub provider_source: Option<String>,
}

impl From<UsageLogRecord> for UsageLogResponse {
    fn from(r: UsageLogRecord) -> Self {
        Self {
            id: r.id,
            recorded_at: r.recorded_at.to_rfc3339(),
            request_id: r.request_id,
            client_request_id: r.client_request_id,
            api_key_id: r.api_key_id,
            user_id: r.user_id,
            org_id: r.org_id,
            project_id: r.project_id,
            team_id: r.team_id,
            service_account_id: r.service_account_id,
            model: r.model,
            provider: r.provider,
            http_referer: r.http_referer,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cached_tokens: r.cached_tokens,
            reasoning_tokens: r.reasoning_tokens,
            cost: r.cost_microcents as f64 / 1_000_000.0,
            streamed: r.streamed,
            finish_reason: r.finish_reason,
            latency_ms: r.latency_ms,
            cancelled: r.cancelled,
            status_code: r.status_code,
            pricing_source: r.pricing_source,
            image_count: r.image_count,
            audio_seconds: r.audio_seconds,
            character_count: r.character_count,
            provider_source: r.provider_source,
        }
    }
}

/// Paginated list of usage log records
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct UsageLogListResponse {
    pub data: Vec<UsageLogResponse>,
    pub pagination: PaginationMeta,
}

/// Export format for usage logs
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum UsageLogExportFormat {
    #[default]
    Csv,
    Jsonl,
}

/// Query parameters for usage log export
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::IntoParams))]
pub struct UsageLogExportQuery {
    pub org_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub provider_source: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Filter by record type: "model" or "tool". Omit for both.
    pub record_type: Option<String>,
    #[serde(default)]
    pub format: UsageLogExportFormat,
}

impl UsageLogExportQuery {
    fn into_params(self) -> (UsageLogQueryParams, UsageLogExportFormat) {
        let format = self.format;
        (
            UsageLogQueryParams {
                org_id: self.org_id,
                user_id: self.user_id,
                project_id: self.project_id,
                team_id: self.team_id,
                api_key_id: self.api_key_id,
                service_account_id: self.service_account_id,
                model: self.model,
                provider: self.provider,
                provider_source: self.provider_source,
                from: self.from,
                to: self.to,
                limit: None,
                cursor: None,
                direction: None,
                record_type: self.record_type,
            },
            format,
        )
    }
}

/// List usage logs (admin global)
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/logs",
    tag = "usage",
    operation_id = "usage_log_list",
    params(UsageLogQueryParams),
    responses(
        (status = 200, description = "List of usage log records", body = UsageLogListResponse),
    )
))]
pub async fn list_logs(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Query(params): Query<UsageLogQueryParams>,
) -> Result<Json<UsageLogListResponse>, AdminError> {
    authz.require("usage", "list", None, None, None, None)?;
    let services = get_services(&state)?;

    // Clamp to [1, 1000]: limit=0 previously returned an empty page with
    // has_more=true (client infinite loop) and a negative limit became an
    // unbounded SQLite scan (`LIMIT -n` means no limit).
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    if let Some(ref dir) = params.direction
        && dir != "forward"
        && dir != "backward"
    {
        return Err(AdminError::BadRequest(format!(
            "Invalid direction '{}': must be 'forward' or 'backward'",
            dir
        )));
    }
    // Reject garbage cursors here with a 400; the repo layer treats decode
    // failures as internal database errors (500).
    if let Some(ref c) = params.cursor {
        crate::db::repos::Cursor::decode(c)
            .map_err(|e| AdminError::BadRequest(format!("Invalid cursor: {}", e)))?;
    }

    let mut query = params.into_query();
    query.limit = Some(limit);
    let result = services.usage.list_logs(query).await?;
    tracing::debug!(count = result.items.len(), "listed usage logs");

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(UsageLogListResponse {
        data: result.items.into_iter().map(|r| r.into()).collect(),
        pagination,
    }))
}

/// List current user's usage logs
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/logs",
    tag = "me",
    operation_id = "me_usage_log_list",
    params(UsageLogQueryParams),
    responses(
        (status = 200, description = "List of usage log records", body = UsageLogListResponse),
    )
))]
pub async fn list_me_logs(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Query(params): Query<UsageLogQueryParams>,
) -> Result<Json<UsageLogListResponse>, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    // Clamp to [1, 1000]: limit=0 previously returned an empty page with
    // has_more=true (client infinite loop) and a negative limit became an
    // unbounded SQLite scan (`LIMIT -n` means no limit).
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    if let Some(ref dir) = params.direction
        && dir != "forward"
        && dir != "backward"
    {
        return Err(AdminError::BadRequest(format!(
            "Invalid direction '{}': must be 'forward' or 'backward'",
            dir
        )));
    }
    // Reject garbage cursors here with a 400; the repo layer treats decode
    // failures as internal database errors (500).
    if let Some(ref c) = params.cursor {
        crate::db::repos::Cursor::decode(c)
            .map_err(|e| AdminError::BadRequest(format!("Invalid cursor: {}", e)))?;
    }

    let mut query = params.into_query();
    query.limit = Some(limit);
    query.user_id = Some(user_id);

    let result = services.usage.list_logs(query).await?;
    tracing::debug!(count = result.items.len(), %user_id, "listed user usage logs");

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(UsageLogListResponse {
        data: result.items.into_iter().map(|r| r.into()).collect(),
        pagination,
    }))
}

/// Flattened row for usage log CSV export
#[cfg(feature = "csv-export")]
#[derive(serde::Serialize)]
struct UsageLogCsvRow {
    id: String,
    recorded_at: String,
    request_id: String,
    client_request_id: String,
    model: String,
    provider: String,
    provider_source: String,
    input_tokens: i32,
    output_tokens: i32,
    cached_tokens: i32,
    reasoning_tokens: i32,
    cost: f64,
    streamed: bool,
    finish_reason: String,
    latency_ms: String,
    cancelled: bool,
    status_code: String,
    image_count: String,
    audio_seconds: String,
    character_count: String,
    http_referer: String,
    user_id: String,
    api_key_id: String,
    org_id: String,
    project_id: String,
    team_id: String,
    service_account_id: String,
    pricing_source: String,
}

fn build_export_response(
    records: Vec<UsageLogRecord>,
    format: UsageLogExportFormat,
    truncated: bool,
) -> Result<Response, AdminError> {
    let result: Result<Response, AdminError> = match format {
        #[cfg(feature = "csv-export")]
        UsageLogExportFormat::Csv => {
            use super::csv_export::CsvResponse;

            let mut wtr = csv::Writer::from_writer(vec![]);
            for r in records {
                let resp: UsageLogResponse = r.into();
                let row = UsageLogCsvRow {
                    id: resp.id.to_string(),
                    recorded_at: resp.recorded_at,
                    request_id: resp.request_id,
                    client_request_id: resp.client_request_id.unwrap_or_default(),
                    model: resp.model,
                    provider: resp.provider,
                    provider_source: resp.provider_source.unwrap_or_default(),
                    input_tokens: resp.input_tokens,
                    output_tokens: resp.output_tokens,
                    cached_tokens: resp.cached_tokens,
                    reasoning_tokens: resp.reasoning_tokens,
                    cost: resp.cost,
                    streamed: resp.streamed,
                    finish_reason: resp.finish_reason.unwrap_or_default(),
                    latency_ms: resp.latency_ms.map(|v| v.to_string()).unwrap_or_default(),
                    cancelled: resp.cancelled,
                    status_code: resp.status_code.map(|v| v.to_string()).unwrap_or_default(),
                    image_count: resp.image_count.map(|v| v.to_string()).unwrap_or_default(),
                    audio_seconds: resp
                        .audio_seconds
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    character_count: resp
                        .character_count
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    http_referer: resp.http_referer.unwrap_or_default(),
                    user_id: resp.user_id.map(|v| v.to_string()).unwrap_or_default(),
                    api_key_id: resp.api_key_id.map(|v| v.to_string()).unwrap_or_default(),
                    org_id: resp.org_id.map(|v| v.to_string()).unwrap_or_default(),
                    project_id: resp.project_id.map(|v| v.to_string()).unwrap_or_default(),
                    team_id: resp.team_id.map(|v| v.to_string()).unwrap_or_default(),
                    service_account_id: resp
                        .service_account_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    pricing_source: resp.pricing_source,
                };
                wtr.serialize(&row)
                    .map_err(|e| AdminError::Internal(format!("CSV serialization error: {}", e)))?;
            }
            let data = wtr
                .into_inner()
                .map_err(|e| AdminError::Internal(format!("CSV flush error: {}", e)))?;
            Ok(CsvResponse {
                data,
                filename: "usage-logs.csv".to_string(),
            }
            .into_response())
        }
        #[cfg(not(feature = "csv-export"))]
        UsageLogExportFormat::Csv => Err(AdminError::BadRequest(
            "CSV export is not available in this build".to_string(),
        )),
        UsageLogExportFormat::Jsonl => {
            let mut jsonl = String::new();
            for r in records {
                let resp: UsageLogResponse = r.into();
                jsonl.push_str(&serde_json::to_string(&resp).map_err(|e| {
                    AdminError::Internal(format!("JSON serialization error: {}", e))
                })?);
                jsonl.push('\n');
            }
            Ok((
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/x-ndjson; charset=utf-8",
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        "attachment; filename=\"usage-logs.jsonl\"",
                    ),
                ],
                jsonl,
            )
                .into_response())
        }
    };
    let mut response = result?;
    // The export is capped (EXPORT_ROW_LIMIT rows). Reconciliation callers
    // must be able to tell a complete export from a truncated one; use the
    // paginated JSON endpoints (/admin/v1/usage/logs) for complete data.
    if truncated {
        response.headers_mut().insert(
            "X-Export-Truncated",
            axum::http::HeaderValue::from_static("true"),
        );
    }
    Ok(response)
}

/// Export usage logs (admin global)
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/usage/logs/export",
    tag = "usage",
    operation_id = "usage_log_export",
    params(UsageLogExportQuery),
    responses(
        (status = 200, description = "Exported usage logs", content_type = "text/csv"),
        (status = 200, description = "Exported usage logs", content_type = "application/x-ndjson"),
    )
))]
pub async fn export_logs(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Query(export_query): Query<UsageLogExportQuery>,
) -> Result<Response, AdminError> {
    authz.require("usage", "list", None, None, None, None)?;
    let services = get_services(&state)?;

    let (params, format) = export_query.into_params();
    let result = services.usage.list_logs(params.into_export_query()).await?;
    tracing::debug!(count = result.items.len(), format = ?format, "exporting usage logs");

    build_export_response(result.items, format, result.has_more)
}

/// Export current user's usage logs
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/me/usage/logs/export",
    tag = "me",
    operation_id = "me_usage_log_export",
    params(UsageLogExportQuery),
    responses(
        (status = 200, description = "Exported usage logs", content_type = "text/csv"),
        (status = 200, description = "Exported usage logs", content_type = "application/x-ndjson"),
    )
))]
pub async fn export_me_logs(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Query(export_query): Query<UsageLogExportQuery>,
) -> Result<Response, AdminError> {
    let user_id = admin_auth.identity.user_id.ok_or(AdminError::NotFound(
        "User not found in database".to_string(),
    ))?;
    let services = get_services(&state)?;
    usage_user_authz(services, &authz, user_id).await?;

    let (params, format) = export_query.into_params();
    let mut query = params.into_export_query();
    query.user_id = Some(user_id);

    let result = services.usage.list_logs(query).await?;
    tracing::debug!(count = result.items.len(), %user_id, format = ?format, "exporting user usage logs");

    build_export_response(result.items, format, result.has_more)
}
