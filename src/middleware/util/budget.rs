use std::time::Duration;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{models::BudgetPeriod, observability::metrics, openapi::ErrorResponse};

#[derive(Debug, Clone)]
pub enum BudgetError {
    /// Budget limit exceeded
    LimitExceeded {
        limit_cents: i64,
        current_spend_cents: i64,
        period: BudgetPeriod,
    },

    /// No authentication present
    #[allow(dead_code)] // Error variant for completeness; handled by combined middleware
    NotAuthenticated,

    /// Cache is required for budget enforcement but not configured
    /// Budget enforcement uses atomic reservations to prevent race conditions
    /// Note: This error is rarely produced since the combined middleware checks
    /// cache availability before calling budget check functions.
    #[allow(dead_code)] // Error variant for completeness; combined middleware pre-checks cache
    CacheRequired {
        api_key_id: uuid::Uuid,
        period: BudgetPeriod,
    },

    /// Internal error checking budget
    Internal(String),
}

/// Result of budget check with reservation info for later adjustment
#[derive(Debug, Clone)]
pub struct BudgetCheckResult {
    /// Estimated cost that was reserved (in microcents)
    pub reserved_cost_microcents: i64,
    /// Cache key used for budget tracking
    pub cache_key: String,
    /// TTL for the cache entry
    pub cache_ttl: Duration,
}

impl IntoResponse for BudgetError {
    fn into_response(self) -> Response {
        let (status, code, message, details) = match &self {
            BudgetError::LimitExceeded {
                limit_cents,
                current_spend_cents,
                period,
            } => (
                StatusCode::PAYMENT_REQUIRED,
                "budget_exceeded",
                format!(
                    "Budget limit exceeded for {} period: current spend plus in-flight \
                     reservations ({} cents) leave no room for this request's estimated \
                     cost within the {} cent limit",
                    period.as_str(),
                    current_spend_cents,
                    limit_cents
                ),
                Some(json!({
                    "limit_cents": limit_cents,
                    "current_spend_cents": current_spend_cents,
                    "period": period.as_str(),
                })),
            ),
            BudgetError::NotAuthenticated => (
                StatusCode::UNAUTHORIZED,
                "not_authenticated",
                "Authentication required for budget check".to_string(),
                None,
            ),
            BudgetError::CacheRequired { api_key_id, period } => {
                tracing::error!(
                    api_key_id = %api_key_id,
                    period = %period.as_str(),
                    "Budget enforcement requires cache but none is configured. \
                     Configure a cache (memory or Redis) or remove budget limits from API keys."
                );
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cache_required",
                    "Budget enforcement requires cache to be configured".to_string(),
                    Some(json!({
                        "api_key_id": api_key_id.to_string(),
                        "period": period.as_str(),
                        "hint": "Configure [cache] in hadrian.toml or remove budget_limit_cents from the API key",
                    })),
                )
            }
            BudgetError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                msg.clone(),
                None,
            ),
        };

        // Record budget error metric
        metrics::record_gateway_error("budget_exceeded", code, None);

        let error_type = match &self {
            BudgetError::LimitExceeded { .. } => "budget_error",
            BudgetError::NotAuthenticated => "authentication_error",
            BudgetError::CacheRequired { .. } | BudgetError::Internal(_) => "server_error",
        };
        // Numeric context is folded into the message text; a structured
        // `details` object is deliberately omitted from the response body to
        // keep the OpenAI error shape. The audit log carries the structured
        // values (see log_budget_exceeded).
        let _ = details;
        let body = ErrorResponse::with_type(error_type, code, message);

        (status, Json(body)).into_response()
    }
}

/// Adjust the budget reservation after actual cost is known.
///
/// This should be called after the request completes to replace the estimated
/// cost with the actual cost. If actual cost is higher than estimated, the
/// overage is added. If lower, the difference is credited back.
/// Both values are in microcents.
///
/// Uses retry with exponential backoff to handle transient cache failures.
/// Returns true if the adjustment succeeded (or no adjustment was needed), false if it failed.
pub async fn adjust_budget_reservation(
    cache: &std::sync::Arc<dyn crate::cache::Cache>,
    reservation: &BudgetCheckResult,
    actual_cost_microcents: i64,
) -> bool {
    use std::time::Duration;

    const MAX_RETRIES: u32 = 3;
    const INITIAL_BACKOFF_MS: u64 = 10;

    let adjustment = actual_cost_microcents - reservation.reserved_cost_microcents;
    if adjustment == 0 {
        return true;
    }

    let mut last_error = None;
    for attempt in 0..MAX_RETRIES {
        match cache
            .incr_by(&reservation.cache_key, adjustment, reservation.cache_ttl)
            .await
        {
            Ok(_) => return true,
            Err(e) => {
                last_error = Some(e);
                if attempt < MAX_RETRIES - 1 {
                    tokio::time::sleep(Duration::from_millis(INITIAL_BACKOFF_MS * (1 << attempt)))
                        .await;
                }
            }
        }
    }

    if let Some(e) = last_error {
        tracing::error!(
            cache_key = %reservation.cache_key,
            adjustment = adjustment,
            error = %e,
            "Failed to adjust budget reservation after {} retries - budget tracking may be inaccurate",
            MAX_RETRIES
        );
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheKeys;

    #[test]
    fn test_budget_ttl_fixed_values() {
        // Daily budget uses fixed 24h TTL to prevent race conditions with long requests
        let daily_ttl = CacheKeys::budget_ttl(BudgetPeriod::Daily);
        assert_eq!(daily_ttl, Duration::from_secs(86400));

        // Monthly budget uses fixed 31d TTL
        let monthly_ttl = CacheKeys::budget_ttl(BudgetPeriod::Monthly);
        assert_eq!(monthly_ttl, Duration::from_secs(2678400));
    }

    #[test]
    fn test_budget_check_result_adjustment() {
        // Values in microcents: 100 microcents = $0.0001
        let result = BudgetCheckResult {
            reserved_cost_microcents: 100_000, // $0.001 in microcents
            cache_key: "test:key".to_string(),
            cache_ttl: Duration::from_secs(3600),
        };

        // Actual cost higher than reserved -> positive adjustment
        let adjustment = 150_000 - result.reserved_cost_microcents;
        assert_eq!(adjustment, 50_000);

        // Actual cost lower than reserved -> negative adjustment (credit back)
        let adjustment = 50_000 - result.reserved_cost_microcents;
        assert_eq!(adjustment, -50_000);

        // Actual cost equals reserved -> no adjustment
        let adjustment = 100_000 - result.reserved_cost_microcents;
        assert_eq!(adjustment, 0);
    }

    #[test]
    fn test_budget_cents_to_microcents_saturation() {
        // Verify saturating_mul prevents overflow when converting cents to microcents
        // i64::MAX / 10_000 ≈ 922 trillion, so any realistic budget limit is safe
        // But we should still handle the edge case gracefully

        // Normal case: $1 million budget = 100_000_000 cents
        let budget_cents: i64 = 100_000_000;
        let microcents = budget_cents.saturating_mul(10_000);
        assert_eq!(microcents, 1_000_000_000_000); // 1 trillion microcents

        // Edge case: would overflow if not saturated
        let huge_budget: i64 = i64::MAX / 10_000 + 1;
        let microcents = huge_budget.saturating_mul(10_000);
        // Should saturate to i64::MAX instead of wrapping to negative
        assert_eq!(microcents, i64::MAX);

        // Negative values (shouldn't happen in practice but handles edge case)
        let negative: i64 = i64::MIN / 10_000 - 1;
        let microcents = negative.saturating_mul(10_000);
        assert_eq!(microcents, i64::MIN);
    }
}
