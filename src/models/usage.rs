use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pricing::CostPricingSource;

/// Individual usage log record — the read model for `usage_records` rows.
///
/// Distinct from `UsageLogEntry` (the write model) — includes database-assigned
/// `id` and `recorded_at` fields.
#[derive(Debug, Clone, Serialize)]
pub struct UsageLogRecord {
    pub id: Uuid,
    pub recorded_at: DateTime<Utc>,
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
    /// Cost in microcents (1/1,000,000 of a dollar)
    pub cost_microcents: i64,
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
    /// Record type: "model" for LLM requests, "tool" for tool invocations
    pub record_type: String,
    /// Tool name (e.g. "web_search", "web_fetch") — only for tool records
    pub tool_name: Option<String>,
    /// Search query or target URL — only for tool records
    pub tool_query: Option<String>,
    /// URL fetched — only for web_fetch records
    pub tool_url: Option<String>,
    /// Response body size in bytes — only for tool records
    pub tool_bytes_fetched: Option<i64>,
    /// Number of search results returned — only for web_search records
    pub tool_results_count: Option<i32>,
    /// Wall-clock runtime in seconds — only for shell tool records.
    pub tool_runtime_seconds: Option<f64>,
    /// Process exit code — only for shell tool records. Distinct from
    /// `status_code` (which is the HTTP status of the API request that
    /// drove the tool); a shell can exit `0` while the wrapping request
    /// returns 200, or exit `7` while the request still returns 200.
    pub tool_exit_code: Option<i32>,
}

/// Usage log entry for a single API request.
///
/// Costs are stored in microcents (1/1,000,000 of a dollar) for precision.
/// For example, $0.000207 = 207 microcents.
///
/// Attribution context is stored at write time for efficient aggregation queries.
/// `api_key_id` is None for session-based users (OIDC/SAML/proxy auth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogEntry {
    /// Unique request identifier for idempotency (prevents duplicate charges).
    /// Always server-generated: a client-controllable value here would let a
    /// client suppress its own billing rows via the duplicate-id dedupe.
    pub request_id: String,
    /// Client-supplied correlation id (X-Request-Id / X-Client-Request-Id).
    /// Correlation only — never keys or dedupes rows.
    pub client_request_id: Option<String>,
    /// API key used (None for session-based auth)
    pub api_key_id: Option<Uuid>,
    /// User who made the request (from session or user-owned API key)
    pub user_id: Option<Uuid>,
    /// Organization context (always present when determinable)
    pub org_id: Option<Uuid>,
    /// Project context (from project-scoped key or X-Hadrian-Project header)
    pub project_id: Option<Uuid>,
    /// Team context (from team-scoped API key only)
    pub team_id: Option<Uuid>,
    /// Service account that owns the API key (if applicable)
    pub service_account_id: Option<Uuid>,
    pub model: String,
    pub provider: String,
    pub http_referer: Option<String>,
    pub input_tokens: i32,
    pub output_tokens: i32,
    /// Cost in microcents (1/1,000,000 of a dollar)
    pub cost_microcents: Option<i64>,
    pub request_at: DateTime<Utc>,
    /// Whether this was a streaming request
    pub streamed: bool,
    /// Cached prompt tokens (for Anthropic prompt caching, OpenAI cached context, etc.)
    pub cached_tokens: i32,
    /// Reasoning tokens (for o1, Claude extended thinking, etc.)
    pub reasoning_tokens: i32,
    /// How the generation ended (stop, length, content_filter, tool_calls, error, cancelled)
    pub finish_reason: Option<String>,
    /// Total request latency in milliseconds
    pub latency_ms: Option<i32>,
    /// Whether the request was cancelled mid-stream
    pub cancelled: bool,
    /// HTTP status code of the response
    pub status_code: Option<i16>,
    /// Where the cost data came from (provider, admin, provider_config, pricing_config, catalog, none)
    #[serde(default)]
    pub pricing_source: CostPricingSource,
    /// Number of images generated (for image generation requests)
    #[serde(default)]
    pub image_count: Option<i32>,
    /// Audio duration in seconds (for TTS, transcription, translation requests)
    #[serde(default)]
    pub audio_seconds: Option<i32>,
    /// Character count (for TTS input text)
    #[serde(default)]
    pub character_count: Option<i32>,
    /// Whether this request used a static or dynamic provider
    #[serde(default)]
    pub provider_source: Option<String>,
    /// Record type: "model" for LLM requests, "tool" for tool invocations
    #[serde(default = "default_record_type")]
    pub record_type: String,
    /// Tool name (e.g. "web_search", "web_fetch") — only for tool records
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Search query or target URL — only for tool records
    #[serde(default)]
    pub tool_query: Option<String>,
    /// URL fetched — only for web_fetch records
    #[serde(default)]
    pub tool_url: Option<String>,
    /// Response body size in bytes — only for tool records
    #[serde(default)]
    pub tool_bytes_fetched: Option<i64>,
    /// Number of search results returned — only for web_search records
    #[serde(default)]
    pub tool_results_count: Option<i32>,
    /// Wall-clock runtime in seconds — only for shell tool records.
    /// Multiplied by the rate from `[features.server_tools.pricing]`
    /// to populate `cost_microcents` for billing.
    #[serde(default)]
    pub tool_runtime_seconds: Option<f64>,
    /// Process exit code for shell tool records. `None` when the
    /// command never reached an `Exit` event (e.g. client disconnect
    /// before the runtime signaled completion); clients see this
    /// surfaced as `status: "incomplete"` with a `timeout` outcome on
    /// the `shell_call_output` item, but the usage record preserves
    /// the "never reported" vs "exited 0" distinction.
    #[serde(default)]
    pub tool_exit_code: Option<i32>,
}

fn default_record_type() -> String {
    "model".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct DailySpend {
    pub date: NaiveDate,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSpend {
    pub model: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefererSpend {
    pub referer: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderSpend {
    pub provider: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub first_request_at: Option<DateTime<Utc>>,
    pub last_request_at: Option<DateTime<Utc>>,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyModelSpend {
    pub date: NaiveDate,
    pub model: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyProviderSpend {
    pub date: NaiveDate,
    pub provider: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingSourceSpend {
    pub pricing_source: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyPricingSourceSpend {
    pub date: NaiveDate,
    pub pricing_source: String,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Usage breakdown by user
#[derive(Debug, Clone, Serialize)]
pub struct UserSpend {
    pub user_id: Option<Uuid>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Daily usage breakdown by user
#[derive(Debug, Clone, Serialize)]
pub struct DailyUserSpend {
    pub date: NaiveDate,
    pub user_id: Option<Uuid>,
    pub user_name: Option<String>,
    pub user_email: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Usage breakdown by project
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSpend {
    pub project_id: Option<Uuid>,
    pub project_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Daily usage breakdown by project
#[derive(Debug, Clone, Serialize)]
pub struct DailyProjectSpend {
    pub date: NaiveDate,
    pub project_id: Option<Uuid>,
    pub project_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Usage breakdown by team
#[derive(Debug, Clone, Serialize)]
pub struct TeamSpend {
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Daily usage breakdown by team
#[derive(Debug, Clone, Serialize)]
pub struct DailyTeamSpend {
    pub date: NaiveDate,
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Usage breakdown by organization
#[derive(Debug, Clone, Serialize)]
pub struct OrgSpend {
    pub org_id: Option<Uuid>,
    pub org_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Daily usage breakdown by organization
#[derive(Debug, Clone, Serialize)]
pub struct DailyOrgSpend {
    pub date: NaiveDate,
    pub org_id: Option<Uuid>,
    pub org_name: Option<String>,
    /// Total cost in microcents (1/1,000,000 of a dollar)
    pub total_cost_microcents: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub image_count: i64,
    pub audio_seconds: i64,
    pub character_count: i64,
}

/// Cost forecast for predicting remaining budget lifespan
#[derive(Debug, Clone, Serialize)]
pub struct CostForecast {
    /// Current spend in the budget period (microcents)
    pub current_spend_microcents: i64,
    /// Budget limit (microcents), None if no budget configured
    pub budget_limit_microcents: Option<i64>,
    /// Budget period (daily or monthly)
    pub budget_period: Option<String>,
    /// Average daily spend (microcents) based on historical data
    pub avg_daily_spend_microcents: i64,
    /// Standard deviation of daily spend (microcents)
    pub std_dev_daily_spend_microcents: i64,
    /// Number of days of historical data used
    pub sample_days: i32,
    /// Projected days until budget exhaustion (None if no budget or zero spend rate)
    pub days_until_exhaustion: Option<f64>,
    /// Projected exhaustion date (None if no budget or zero spend rate)
    pub projected_exhaustion_date: Option<NaiveDate>,
    /// Lower bound days until exhaustion (95% confidence, assumes +1 std dev spend)
    pub days_until_exhaustion_lower: Option<f64>,
    /// Upper bound days until exhaustion (95% confidence, assumes -1 std dev spend)
    pub days_until_exhaustion_upper: Option<f64>,
    /// Percentage of budget used in current period
    pub budget_utilization_percent: Option<f64>,
    /// Projected end-of-period spend at current rate (microcents)
    pub projected_period_spend_microcents: Option<i64>,
    /// Multi-step time series forecast (None if insufficient data for forecasting)
    pub time_series_forecast: Option<ForecastTimeSeries>,
}

/// Multi-step time series forecast with prediction intervals
#[derive(Debug, Clone, Serialize)]
pub struct ForecastTimeSeries {
    /// Dates for each forecast point
    pub dates: Vec<NaiveDate>,
    /// Point forecasts (daily spend in microcents)
    pub point_forecasts: Vec<f64>,
    /// Lower bound of prediction interval (microcents)
    pub lower_bounds: Vec<f64>,
    /// Upper bound of prediction interval (microcents)
    pub upper_bounds: Vec<f64>,
    /// Confidence level for prediction intervals (e.g., 0.95 for 95%)
    pub confidence_level: f64,
    /// Whether MSTL decomposition was used (false = simple ETS)
    pub used_seasonal_decomposition: bool,
}
