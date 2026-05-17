use std::{net::IpAddr, sync::Arc, time::Duration};

#[cfg(feature = "server")]
use axum::extract::ConnectInfo;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::Utc;

use super::rate_limit::{
    RateLimitError, TokenRateLimitCheckResult, TokenRateLimitResult, TokenReservation,
    add_rate_limit_headers, add_token_rate_limit_headers, adjust_token_reservation,
};
use crate::{
    AppState,
    auth::{ApiKeyAuth, AuthError, AuthenticatedRequest, Identity, IdentityKind},
    cache::{BudgetCheckParams, Cache, CacheKeys, RateLimitCheckParams, RateLimitResult},
    events::{BudgetType, ServerEvent},
    middleware::{
        RequestId,
        util::{
            budget::{BudgetCheckResult, BudgetError, adjust_budget_reservation},
            scope::required_scope_for_path,
            usage::{UsageTracker, extract_full_usage_from_response, tracker_from_headers},
        },
    },
    models::{AuditActorType, BudgetPeriod, CreateAuditLog, has_valid_prefix, hash_api_key},
    observability::metrics,
};

/// Input parameters for combined limit checking
pub struct LimitsCheckInput<'a> {
    pub cache: &'a Arc<dyn Cache>,
    pub api_key: &'a ApiKeyAuth,
    pub estimated_cost_cents: i64,
    pub tpm_limit: u32,
    pub tpd_limit: Option<u32>,
    pub estimated_tokens: i64,
    pub rpm_limit: u32,
    pub rpd_limit: Option<u32>,
    /// Warning threshold as a percentage (0.0-1.0)
    pub budget_warning_threshold: f64,
}

/// Context for async usage tracking
pub struct UsageTrackingContext<'a> {
    pub state: AppState,
    pub auth: AuthenticatedRequest,
    pub tracker: UsageTracker,
    pub response: &'a Response,
    pub request_id: Option<String>,
    pub budget_reservation: Option<BudgetCheckResult>,
    pub token_reservation: Option<TokenRateLimitCheckResult>,
    /// Project ID from the X-Hadrian-Project request header (validated)
    pub header_project_id: Option<uuid::Uuid>,
}

/// Event data for budget exceeded audit logging
pub struct BudgetExceededEvent<'a> {
    pub state: &'a AppState,
    pub api_key_id: uuid::Uuid,
    pub org_id: Option<uuid::Uuid>,
    pub project_id: Option<uuid::Uuid>,
    pub limit_cents: i64,
    pub current_spend_cents: i64,
    pub period: BudgetPeriod,
    pub request_path: &'a str,
    pub request_id: Option<&'a str>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

/// Event data for budget warning audit logging
pub struct BudgetWarningEvent<'a> {
    pub state: &'a AppState,
    pub api_key_id: uuid::Uuid,
    pub org_id: Option<uuid::Uuid>,
    pub project_id: Option<uuid::Uuid>,
    pub spend_percentage: f64,
    pub current_spend_cents: i64,
    pub limit_cents: i64,
    pub period: BudgetPeriod,
    pub request_path: &'a str,
    pub request_id: Option<&'a str>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

/// Result of combined budget and token limit checks
pub struct CombinedLimitResult {
    /// Budget reservation result (if budget limits are configured)
    pub budget: Option<BudgetCheckResult>,
    /// Token rate limit reservation (if API key is present)
    pub token: Option<TokenRateLimitCheckResult>,
    /// Request rate limit result (for response headers)
    pub request_rate_limit: Option<RateLimitResult>,
    /// Budget warning info if threshold exceeded (but not yet at limit)
    pub budget_warning: Option<BudgetWarning>,
}

/// Budget warning when spending approaches the limit
#[derive(Debug, Clone)]
pub struct BudgetWarning {
    /// Current spend percentage (0.0 - 1.0+)
    pub spend_percentage: f64,
    /// Current spend in cents
    pub current_spend_cents: i64,
    /// Budget limit in cents
    pub limit_cents: i64,
    /// Budget period
    pub period: BudgetPeriod,
}

/// Error from combined limit checking
pub enum CombinedLimitError {
    Budget(BudgetError),
    RateLimit(super::rate_limit::RateLimitError),
}

impl IntoResponse for CombinedLimitError {
    fn into_response(self) -> Response {
        match self {
            CombinedLimitError::Budget(e) => e.into_response(),
            CombinedLimitError::RateLimit(e) => e.into_response(),
        }
    }
}

/// Check all limits (budget + token + request rate limits) in a single batched operation.
///
/// This uses Redis pipelining to reduce network round trips when using Redis cache.
/// For in-memory cache, this runs checks sequentially (already fast).
///
/// Batches the following checks into a single Redis round trip:
/// - Budget check (if configured on API key)
/// - Token per-minute limit
/// - Token per-day limit (if configured)
/// - Request per-minute limit
/// - Request per-day limit (if configured)
///
/// Returns CombinedLimitResult with reservation info for later adjustment.
async fn check_all_limits_batch(
    input: LimitsCheckInput<'_>,
) -> Result<CombinedLimitResult, CombinedLimitError> {
    let LimitsCheckInput {
        cache,
        api_key,
        estimated_cost_cents,
        tpm_limit,
        tpd_limit,
        estimated_tokens,
        rpm_limit,
        rpd_limit,
        budget_warning_threshold,
    } = input;
    // Prepare all the budget check parameters (for budget + token limits)
    let mut budget_checks = Vec::with_capacity(3);
    // Prepare rate limit check parameters (for request rate limits)
    let mut rate_limit_checks = Vec::with_capacity(2);

    let api_key_id = api_key.key.id;

    // Budget check parameters (if budget is configured)
    let budget_info = if let (Some(limit_cents), Some(period)) =
        (api_key.key.budget_limit_cents, api_key.key.budget_period)
    {
        // Use saturating_mul to prevent overflow (cents * 10_000 = microcents)
        let estimated_cost_microcents = estimated_cost_cents.saturating_mul(10_000);
        let limit_microcents = limit_cents.saturating_mul(10_000);
        let cache_key = CacheKeys::spend(api_key_id, period);
        // Use fixed full-period TTL to prevent race conditions with long-running requests
        let cache_ttl = CacheKeys::budget_ttl(period);

        budget_checks.push(BudgetCheckParams {
            key: cache_key.clone(),
            estimated_cost: estimated_cost_microcents,
            limit: limit_microcents,
            ttl: cache_ttl,
        });

        Some((
            cache_key,
            cache_ttl,
            estimated_cost_microcents,
            limit_cents,
            period,
        ))
    } else {
        None
    };

    // Token per-minute check parameters
    let token_minute_cache_key = CacheKeys::rate_limit_tokens(api_key_id, "minute");
    let token_minute_ttl = Duration::from_secs(60);
    budget_checks.push(BudgetCheckParams {
        key: token_minute_cache_key.clone(),
        estimated_cost: estimated_tokens,
        limit: tpm_limit as i64,
        ttl: token_minute_ttl,
    });

    // Token per-day check parameters (if configured)
    let token_day_info = if let Some(limit) = tpd_limit {
        let day_cache_key = CacheKeys::rate_limit_tokens(api_key_id, "day");
        let day_ttl = Duration::from_secs(86400);
        budget_checks.push(BudgetCheckParams {
            key: day_cache_key.clone(),
            estimated_cost: estimated_tokens,
            limit: limit as i64,
            ttl: day_ttl,
        });
        Some((day_cache_key, day_ttl, limit))
    } else {
        None
    };

    // Request per-minute rate limit
    rate_limit_checks.push(RateLimitCheckParams {
        key: CacheKeys::rate_limit(api_key_id, "minute"),
        limit: rpm_limit,
        window_secs: 60,
    });

    // Request per-day rate limit (if configured)
    let has_rpd = rpd_limit.is_some();
    if let Some(limit) = rpd_limit {
        rate_limit_checks.push(RateLimitCheckParams {
            key: CacheKeys::rate_limit(api_key_id, "day"),
            limit,
            window_secs: 86400,
        });
    }

    // Execute all checks in a single batch (1 RTT for Redis)
    let results = cache
        .check_limits_batch(&budget_checks, &rate_limit_checks)
        .await
        .map_err(|e| {
            CombinedLimitError::Budget(BudgetError::Internal(format!("Cache error: {}", e)))
        })?;

    let mut budget_iter = results.budget_results.into_iter();
    let mut rate_limit_iter = results.rate_limit_results.into_iter();

    // Helper to refund all successful reservations on failure
    // Uses retry with backoff to handle transient cache failures
    async fn refund_reservations(
        cache: &Arc<dyn Cache>,
        budget: &Option<BudgetCheckResult>,
        token_minute_key: &str,
        token_minute_reserved: bool,
        estimated_tokens: i64,
    ) {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_BACKOFF_MS: u64 = 10;

        if let Some(b) = budget {
            let mut last_error = None;
            for attempt in 0..MAX_RETRIES {
                match cache
                    .incr_by(&b.cache_key, -b.reserved_cost_microcents, b.cache_ttl)
                    .await
                {
                    Ok(_) => {
                        metrics::record_cache_operation("budget", "refund", "success");
                        last_error = None;
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);
                        if attempt < MAX_RETRIES - 1 {
                            tokio::time::sleep(Duration::from_millis(
                                INITIAL_BACKOFF_MS * (1 << attempt),
                            ))
                            .await;
                        }
                    }
                }
            }
            if let Some(e) = last_error {
                tracing::error!(
                    cache_key = %b.cache_key,
                    refund_amount = -b.reserved_cost_microcents,
                    error = %e,
                    "Failed to refund budget reservation after {} retries - budget tracking may be inaccurate",
                    MAX_RETRIES
                );
                metrics::record_cache_operation("budget", "refund", "error");
            }
        }

        if token_minute_reserved {
            let mut last_error = None;
            for attempt in 0..MAX_RETRIES {
                match cache
                    .incr_by(token_minute_key, -estimated_tokens, Duration::from_secs(60))
                    .await
                {
                    Ok(_) => {
                        metrics::record_cache_operation("token_rate_limit", "refund", "success");
                        last_error = None;
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);
                        if attempt < MAX_RETRIES - 1 {
                            tokio::time::sleep(Duration::from_millis(
                                INITIAL_BACKOFF_MS * (1 << attempt),
                            ))
                            .await;
                        }
                    }
                }
            }
            if let Some(e) = last_error {
                tracing::error!(
                    cache_key = %token_minute_key,
                    refund_amount = -estimated_tokens,
                    error = %e,
                    "Failed to refund token reservation after {} retries - rate limit tracking may be inaccurate",
                    MAX_RETRIES
                );
                metrics::record_cache_operation("token_rate_limit", "refund", "error");
            }
        }
    }

    // Helper to refund token day reservation with retry
    async fn refund_token_day_reservation(
        cache: &Arc<dyn Cache>,
        day_res: &TokenReservation,
        estimated_tokens: i64,
    ) {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_BACKOFF_MS: u64 = 10;

        let mut last_error = None;
        for attempt in 0..MAX_RETRIES {
            match cache
                .incr_by(
                    &day_res.cache_key,
                    -estimated_tokens,
                    Duration::from_secs(day_res.ttl_secs),
                )
                .await
            {
                Ok(_) => {
                    metrics::record_cache_operation("token_rate_limit", "refund_day", "success");
                    return;
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRIES - 1 {
                        tokio::time::sleep(Duration::from_millis(
                            INITIAL_BACKOFF_MS * (1 << attempt),
                        ))
                        .await;
                    }
                }
            }
        }
        if let Some(e) = last_error {
            tracing::error!(
                cache_key = %day_res.cache_key,
                refund_amount = -estimated_tokens,
                error = %e,
                "Failed to refund token day reservation after {} retries - rate limit tracking may be inaccurate",
                MAX_RETRIES
            );
            metrics::record_cache_operation("token_rate_limit", "refund_day", "error");
        }
    }

    // Process budget check result
    let (budget_result, budget_warning) =
        if let Some((cache_key, cache_ttl, estimated_cost_microcents, limit_cents, period)) =
            budget_info
        {
            let reservation = budget_iter.next().ok_or_else(|| {
                CombinedLimitError::Budget(BudgetError::Internal(
                    "Missing budget check result".to_string(),
                ))
            })?;

            if !reservation.allowed {
                return Err(CombinedLimitError::Budget(BudgetError::LimitExceeded {
                    limit_cents,
                    current_spend_cents: reservation.current_spend / 10_000,
                    period,
                }));
            }

            // Check if we've exceeded the warning threshold
            // current_spend is in microcents, limit_cents needs to be converted
            let limit_microcents = limit_cents * 10_000;
            let spend_percentage = if limit_microcents > 0 {
                reservation.current_spend as f64 / limit_microcents as f64
            } else {
                0.0
            };

            let warning = if spend_percentage >= budget_warning_threshold {
                Some(BudgetWarning {
                    spend_percentage,
                    current_spend_cents: reservation.current_spend / 10_000,
                    limit_cents,
                    period,
                })
            } else {
                None
            };

            (
                Some(BudgetCheckResult {
                    reserved_cost_microcents: estimated_cost_microcents,
                    cache_key,
                    cache_ttl,
                }),
                warning,
            )
        } else {
            (None, None)
        };

    // Process token per-minute result
    let token_minute_result = budget_iter.next().ok_or_else(|| {
        CombinedLimitError::RateLimit(RateLimitError::Internal(
            "Missing minute token check result".to_string(),
        ))
    })?;

    if !token_minute_result.allowed {
        refund_reservations(cache, &budget_result, &token_minute_cache_key, false, 0).await;
        metrics::record_rate_limit("limited", Some(api_key_id));
        return Err(CombinedLimitError::RateLimit(RateLimitError::Exceeded {
            limit: tpm_limit,
            current: token_minute_result.current_spend,
            window: "tokens per minute".to_string(),
            retry_after: 60,
        }));
    }

    let token_minute_reservation = TokenReservation {
        cache_key: token_minute_cache_key.clone(),
        reserved_tokens: estimated_tokens,
        current_tokens: token_minute_result.current_spend,
        limit: tpm_limit,
        ttl_secs: 60,
    };

    // Process token per-day result (if configured)
    let token_day_reservation = if let Some((day_cache_key, _day_ttl, limit)) = token_day_info {
        let day_result = budget_iter.next().ok_or_else(|| {
            CombinedLimitError::RateLimit(RateLimitError::Internal(
                "Missing day token check result".to_string(),
            ))
        })?;

        if !day_result.allowed {
            refund_reservations(
                cache,
                &budget_result,
                &token_minute_cache_key,
                true,
                estimated_tokens,
            )
            .await;
            metrics::record_rate_limit("limited", Some(api_key_id));
            return Err(CombinedLimitError::RateLimit(RateLimitError::Exceeded {
                limit,
                current: day_result.current_spend,
                window: "tokens per day".to_string(),
                retry_after: 86400,
            }));
        }

        Some(TokenReservation {
            cache_key: day_cache_key,
            reserved_tokens: estimated_tokens,
            current_tokens: day_result.current_spend,
            limit,
            ttl_secs: 86400,
        })
    } else {
        None
    };

    // Process request per-minute rate limit
    let rpm_result = rate_limit_iter.next().ok_or_else(|| {
        CombinedLimitError::RateLimit(RateLimitError::Internal(
            "Missing request per-minute rate limit result".to_string(),
        ))
    })?;

    if !rpm_result.allowed {
        refund_reservations(
            cache,
            &budget_result,
            &token_minute_cache_key,
            true,
            estimated_tokens,
        )
        .await;
        // Also refund token day if it was reserved
        if let Some(ref day_res) = token_day_reservation {
            refund_token_day_reservation(cache, day_res, estimated_tokens).await;
        }
        metrics::record_rate_limit("limited", Some(api_key_id));
        return Err(CombinedLimitError::RateLimit(RateLimitError::Exceeded {
            limit: rpm_limit,
            current: rpm_result.current,
            window: "minute".to_string(),
            retry_after: rpm_result.reset_secs,
        }));
    }

    // Process request per-day rate limit (if configured)
    if has_rpd {
        let rpd_result = rate_limit_iter.next().ok_or_else(|| {
            CombinedLimitError::RateLimit(RateLimitError::Internal(
                "Missing request per-day rate limit result".to_string(),
            ))
        })?;

        if !rpd_result.allowed {
            refund_reservations(
                cache,
                &budget_result,
                &token_minute_cache_key,
                true,
                estimated_tokens,
            )
            .await;
            if let Some(ref day_res) = token_day_reservation {
                refund_token_day_reservation(cache, day_res, estimated_tokens).await;
            }
            // Note: RPM was already incremented, but it's not a reservation pattern
            // so we don't need to refund it (it's a count, not a cost reservation)
            metrics::record_rate_limit("limited", Some(api_key_id));
            return Err(CombinedLimitError::RateLimit(RateLimitError::Exceeded {
                limit: rpd_limit.unwrap(),
                current: rpd_result.current,
                window: "day".to_string(),
                retry_after: rpd_result.reset_secs,
            }));
        }
    }

    Ok(CombinedLimitResult {
        budget: budget_result,
        token: Some(TokenRateLimitCheckResult {
            minute_reservation: token_minute_reservation,
            day_reservation: token_day_reservation,
        }),
        request_rate_limit: Some(rpm_result),
        budget_warning,
    })
}

/// Combined middleware that handles auth, budget checking, and usage tracking
/// This is applied to all API routes
pub async fn api_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let headers = req.headers().clone();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let start_time = std::time::Instant::now();

    // Get request ID if available (set by request_id_middleware)
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.as_str().to_string());

    // 1. Initialize usage tracker from headers
    let tracker = tracker_from_headers(&headers);
    req.extensions_mut().insert(tracker.clone());

    // Extract connecting IP for trusted proxy validation
    #[cfg(feature = "server")]
    let connecting_ip = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    #[cfg(not(feature = "server"))]
    let connecting_ip: Option<std::net::IpAddr> = None;

    // Insert client info for audit logging
    let client_info = crate::middleware::ClientInfo {
        ip_address: connecting_ip.map(|ip| ip.to_string()),
        user_agent: headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    };
    req.extensions_mut().insert(client_info.clone());

    // Extract cookies for session-based auth (set by CookieManagerLayer)
    let cookies = req.extensions().get::<tower_cookies::Cookies>().cloned();

    // 2. Try to authenticate (optional - doesn't fail if no auth)
    // Short-circuit: in None mode with no credential headers, skip auth entirely.
    // This makes anonymous access explicit rather than relying on MissingCredentials
    // being caught downstream. Credentials present in None mode are still validated.
    let has_credentials = headers
        .contains_key(state.config.auth.api_key_config().header_name.as_str())
        || headers.contains_key(axum::http::header::AUTHORIZATION);
    let auth_result = if !state.config.auth.is_auth_enabled() && !has_credentials {
        Err(AuthError::MissingCredentials)
    } else {
        try_authenticate(&headers, cookies.as_ref(), connecting_ip, &state).await
    };

    // Budget reservation (if applicable)
    let mut budget_reservation: Option<BudgetCheckResult> = None;
    // Budget warning (if threshold exceeded)
    let mut budget_warning: Option<BudgetWarning> = None;
    // Token rate limit reservation (for atomic reservation pattern)
    let mut token_reservation: Option<TokenRateLimitCheckResult> = None;
    // Token rate limit result for headers
    let mut token_rate_limit: Option<TokenRateLimitResult> = None;
    // Request rate limit result for headers
    let mut request_rate_limit: Option<RateLimitResult> = None;
    // Rate limit config
    let tpm_limit = state.config.limits.rate_limits.tokens_per_minute;
    let tpd_limit = state.config.limits.rate_limits.tokens_per_day;
    let estimated_tokens = state.config.limits.rate_limits.estimated_tokens_per_request;
    let rpm_limit = state.config.limits.rate_limits.requests_per_minute;
    let rpd_limit = state.config.limits.rate_limits.requests_per_day;
    let budget_warning_threshold = state.config.limits.budgets.warning_threshold;

    let (auth_clone, _api_key_id) = if let Ok(ref auth) = auth_result {
        let api_key_id = auth.api_key().map(|k| k.key.id);

        // Record successful auth
        let auth_method = match &auth.kind {
            IdentityKind::ApiKey(_) => "api_key",
            IdentityKind::Identity(_) => "identity",
            IdentityKind::Both { .. } => "both",
        };
        metrics::record_auth_attempt(auth_method, true);

        tracing::debug!(
            request_id = ?request_id,
            auth_method = auth_method,
            api_key_id = ?api_key_id,
            "Request authenticated"
        );

        // Add auth to request
        req.extensions_mut().insert(auth.clone());

        // 2.5. Check API key scopes (if API key auth and path requires a scope)
        if let Some(api_key) = auth.api_key()
            && let Some(required_scope) = required_scope_for_path(&path)
            && !api_key.key.has_scope(required_scope)
        {
            tracing::warn!(
                request_id = ?request_id,
                api_key_id = %api_key.key.id,
                required_scope = %required_scope,
                available_scopes = ?api_key.key.scopes,
                path = %path,
                "API key lacks required scope"
            );
            return AuthError::InsufficientScope {
                required: required_scope.to_string(),
                available: api_key.key.scopes.clone().unwrap_or_default(),
            }
            .into_response();
        }

        // 2.6. Check API key IP allowlist (if API key auth and IP allowlist is configured)
        if let Some(api_key) = auth.api_key()
            && let Some(ref allowlist) = api_key.key.ip_allowlist
            && !allowlist.is_empty()
        {
            // Use extract_client_ip to get real client IP (handles trusted proxies)
            let client_ip =
                super::rate_limit::extract_client_ip(&req, &state.config.server.trusted_proxies);

            match client_ip {
                Some(ip) if !api_key.key.is_ip_allowed(ip) => {
                    tracing::warn!(
                        request_id = ?request_id,
                        api_key_id = %api_key.key.id,
                        client_ip = %ip,
                        allowlist = ?allowlist,
                        "API key IP not allowed"
                    );
                    return AuthError::IPNotAllowed {
                        ip: ip.to_string(),
                        allowlist: allowlist.clone(),
                    }
                    .into_response();
                }
                None => {
                    // IP allowlist is configured but we couldn't determine client IP
                    tracing::warn!(
                        request_id = ?request_id,
                        api_key_id = %api_key.key.id,
                        "API key has IP allowlist but client IP could not be determined"
                    );
                    return AuthError::IPNotAllowed {
                        ip: "unknown".to_string(),
                        allowlist: allowlist.clone(),
                    }
                    .into_response();
                }
                _ => {} // IP allowed
            }
        }

        // 3. Check all limits (budget + token + request) in a single batched operation
        // This uses Redis pipelining to reduce network round trips (1 RTT instead of 4-5)
        if let (Some(cache), Some(api_key)) = (&state.cache, auth.api_key()) {
            let estimated_cost_cents = state.config.limits.budgets.estimated_cost_cents;

            // Use per-key rate limits if configured, otherwise fall back to global defaults
            let effective_rpm = api_key
                .key
                .rate_limit_rpm
                .map(|r| r as u32)
                .unwrap_or(rpm_limit);
            let effective_tpm = api_key
                .key
                .rate_limit_tpm
                .map(|t| t as u32)
                .unwrap_or(tpm_limit);

            match check_all_limits_batch(LimitsCheckInput {
                cache,
                api_key,
                estimated_cost_cents,
                tpm_limit: effective_tpm,
                tpd_limit,
                estimated_tokens,
                rpm_limit: effective_rpm,
                rpd_limit,
                budget_warning_threshold,
            })
            .await
            {
                Ok(result) => {
                    if result.budget.is_some() {
                        metrics::record_budget_check("allowed", api_key_id);
                    }
                    metrics::record_rate_limit("allowed", api_key_id);

                    budget_reservation = result.budget;
                    budget_warning = result.budget_warning.clone();

                    // Record budget warning metric and audit log
                    if let Some(ref warning) = result.budget_warning {
                        metrics::record_budget_warning(
                            api_key.key.id,
                            warning.spend_percentage,
                            warning.period.as_str(),
                        );

                        // Log audit event once per budget period (uses cache to deduplicate)
                        log_budget_warning(BudgetWarningEvent {
                            state: &state,
                            api_key_id: api_key.key.id,
                            org_id: api_key.org_id,
                            project_id: api_key.project_id,
                            spend_percentage: warning.spend_percentage,
                            current_spend_cents: warning.current_spend_cents,
                            limit_cents: warning.limit_cents,
                            period: warning.period,
                            request_path: &path,
                            request_id: request_id.as_deref(),
                            ip_address: client_info.ip_address.clone(),
                            user_agent: client_info.user_agent.clone(),
                        });
                    }

                    if let Some(token_result) = result.token {
                        token_rate_limit = Some(TokenRateLimitResult::from(&token_result));
                        token_reservation = Some(token_result);
                    }
                    request_rate_limit = result.request_rate_limit;
                }
                Err(CombinedLimitError::Budget(ref e)) => {
                    metrics::record_budget_check("exceeded", api_key_id);
                    tracing::warn!(
                        request_id = ?request_id,
                        api_key_id = ?api_key_id,
                        "Budget exceeded"
                    );

                    // Log audit event for compliance
                    if let BudgetError::LimitExceeded {
                        limit_cents,
                        current_spend_cents,
                        period,
                    } = e
                    {
                        log_budget_exceeded(BudgetExceededEvent {
                            state: &state,
                            api_key_id: api_key.key.id,
                            org_id: api_key.org_id,
                            project_id: api_key.project_id,
                            limit_cents: *limit_cents,
                            current_spend_cents: *current_spend_cents,
                            period: *period,
                            request_path: &path,
                            request_id: request_id.as_deref(),
                            ip_address: client_info.ip_address.clone(),
                            user_agent: client_info.user_agent.clone(),
                        });
                    }

                    return e.clone().into_response();
                }
                Err(CombinedLimitError::RateLimit(e)) => {
                    // Note: "limited" metric is recorded inside check_all_limits_batch
                    tracing::warn!(
                        request_id = ?request_id,
                        api_key_id = ?api_key_id,
                        "Rate limit exceeded"
                    );
                    return e.into_response();
                }
            }
        }

        (Some(auth.clone()), api_key_id)
    } else if headers.contains_key("X-API-Key") || headers.contains_key("Authorization") {
        // Credentials were provided but invalid — reject with the original error
        metrics::record_auth_attempt("api_key", false);
        tracing::warn!(
            request_id = ?request_id,
            "Authentication failed: invalid credentials provided"
        );
        return auth_result.unwrap_err().into_response();
    } else {
        // No credentials provided — allow anonymous access
        (None, None)
    };

    // 4. Execute the request
    let mut response = next.run(req).await;

    // Record HTTP metrics
    let duration = start_time.elapsed();
    let status = response.status().as_u16();
    metrics::record_http_request(&method, &path, status, duration.as_secs_f64());

    // 5. Add rate limit headers if we have them
    if let Some(ref token_limit) = token_rate_limit {
        response = add_token_rate_limit_headers(response, token_limit);
    }
    if let Some(ref rate_limit) = request_rate_limit {
        response = add_rate_limit_headers(response, rate_limit);
    }
    // Add budget warning header if threshold exceeded
    if let Some(ref warning) = budget_warning {
        response = add_budget_warning_headers(response, warning);
    }

    // 6. Track usage (async, non-blocking) and adjust budget/token reservations
    if let Some(auth) = auth_clone {
        // Extract project context from request header (for session-based users)
        let header_project_id = headers
            .get("X-Hadrian-Project")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| uuid::Uuid::parse_str(v).ok());

        track_usage_async(UsageTrackingContext {
            state,
            auth,
            tracker,
            response: &response,
            request_id,
            budget_reservation,
            token_reservation,
            header_project_id,
        });
    } else {
        #[cfg(feature = "concurrency")]
        if let Some(buffer) = &state.usage_buffer {
            // Track anonymous usage when auth is disabled (local dev / no-auth mode).
            // Attribute to the default anonymous user/org created on startup.
            let has_model = response.headers().contains_key("X-Model");
            let is_streaming = response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| s.contains("text/event-stream"))
                || response
                    .headers()
                    .get("Transfer-Encoding")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s.contains("chunked"));

            // Only track LLM requests (those with X-Model header).
            // Skip streaming responses here — UsageTrackingStream handles them
            // with actual token counts after the stream completes.
            if has_model && !is_streaming {
                let usage = extract_full_usage_from_response(&response);

                let model = response
                    .headers()
                    .get("X-Model")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from)
                    .or(tracker.model)
                    .unwrap_or_else(|| "unknown".to_string());
                let provider = response
                    .headers()
                    .get("X-Provider")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from)
                    .or(tracker.provider)
                    .unwrap_or_else(|| "unknown".to_string());

                let elapsed = tracker.start_time.elapsed();
                let latency_ms = elapsed.as_millis().min(i32::MAX as u128) as i32;

                let status = if response.status().is_success() {
                    "success"
                } else {
                    "error"
                };
                metrics::record_llm_request(metrics::LlmRequestMetrics {
                    provider: &provider,
                    model: &model,
                    status,
                    status_code: Some(response.status().as_u16()),
                    duration_secs: elapsed.as_secs_f64(),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cost_microcents: usage.cost_microcents,
                });

                let header_project_id = headers
                    .get("X-Hadrian-Project")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| uuid::Uuid::parse_str(v).ok());

                buffer.push(crate::models::UsageLogEntry {
                    request_id: request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    api_key_id: None,
                    user_id: state.default_user_id,
                    org_id: state.default_org_id,
                    project_id: header_project_id,
                    team_id: None,
                    service_account_id: None,
                    model,
                    provider,
                    input_tokens: saturate_i64_to_i32(usage.input_tokens.unwrap_or(0)),
                    output_tokens: saturate_i64_to_i32(usage.output_tokens.unwrap_or(0)),
                    cost_microcents: usage.cost_microcents,
                    http_referer: tracker.referer.clone(),
                    request_at: chrono::Utc::now(),
                    streamed: tracker.streamed,
                    cached_tokens: 0,
                    reasoning_tokens: 0,
                    finish_reason: None,
                    latency_ms: Some(latency_ms),
                    cancelled: false,
                    status_code: Some(response.status().as_u16() as i16),
                    pricing_source: usage.pricing_source,
                    image_count: usage.image_count,
                    audio_seconds: usage.audio_seconds,
                    character_count: usage.character_count,
                    provider_source: tracker.provider_source.clone(),
                    record_type: "model".to_string(),
                    tool_name: None,
                    tool_query: None,
                    tool_url: None,
                    tool_bytes_fetched: None,
                    tool_results_count: None,
                    tool_runtime_seconds: None,
                });
            }
        }
    }

    response
}

/// Track usage asynchronously (fire and forget)
///
/// Uses the usage buffer for batched database writes when available,
/// falling back to per-request writes otherwise.
///
/// Tracks usage for all authenticated requests (API key, session, or both).
/// Attribution context (org, user, project, team) is populated at write time
/// for efficient aggregation queries.
#[allow(clippy::collapsible_if)]
fn track_usage_async(ctx: UsageTrackingContext<'_>) {
    let UsageTrackingContext {
        state,
        auth,
        tracker,
        response,
        request_id,
        budget_reservation,
        token_reservation,
        header_project_id,
    } = ctx;

    let api_key = auth.api_key();
    let elapsed = tracker.start_time.elapsed();
    // Saturate latency to i32::MAX to prevent overflow on very long requests
    let latency_ms = elapsed.as_millis().min(i32::MAX as u128) as i32;

    // Extract full usage info from response headers
    let usage = extract_full_usage_from_response(response);
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    let cost_microcents = usage.cost_microcents;

    // Detect whether UsageTrackingStream will handle tracking for this response.
    // Streaming responses use Transfer-Encoding: chunked or text/event-stream content type
    // and are wrapped by inject_cost_into_response (providers/mod.rs) with a
    // UsageTrackingStream that writes correct token counts after the stream completes.
    let is_streaming = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("text/event-stream"))
        || response
            .headers()
            .get("Transfer-Encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.contains("chunked"));
    let has_model = response.headers().contains_key("X-Model");

    // Read provider and model from response headers (set by route handlers)
    // Fall back to tracker values (for backwards compatibility) or "unknown"
    let model = response
        .headers()
        .get("X-Model")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or(tracker.model)
        .unwrap_or_else(|| "unknown".to_string());
    let provider = response
        .headers()
        .get("X-Provider")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or(tracker.provider)
        .unwrap_or_else(|| "unknown".to_string());

    // Read provider source from response header (set by route handler)
    let provider_source = response
        .headers()
        .get("X-Provider-Source")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or(tracker.provider_source);

    // Only record LLM metrics for actual LLM requests (those with X-Model header)
    if has_model {
        let status_code = response.status().as_u16();
        let status = if response.status().is_success() {
            "success"
        } else {
            "error"
        };
        metrics::record_llm_request(metrics::LlmRequestMetrics {
            provider: &provider,
            model: &model,
            status,
            status_code: Some(status_code),
            duration_secs: elapsed.as_secs_f64(),
            input_tokens,
            output_tokens,
            cost_microcents,
        });
    }

    // Derive principal-based attribution context
    // org_id: from API key's resolved org, or from principal's org (user's single org)
    let org_id = api_key
        .and_then(|k| k.org_id)
        .or_else(|| auth.principal().org_id());

    // user_id: from identity (session) or user-owned API key
    let user_id = auth.user_id();

    // project_id: from API key scope, or from X-Hadrian-Project header
    let project_id = api_key.and_then(|k| k.project_id).or(header_project_id);

    // team_id: from team-scoped API key only (not a session selection)
    let team_id = api_key.and_then(|k| k.team_id);

    // api_key_id: present only for API key auth
    let api_key_id = api_key.map(|k| k.key.id);

    // service_account_id: from SA-owned keys
    let service_account_id = api_key.and_then(|k| k.service_account_id);

    // Generate request_id if not provided (should always be set by middleware)
    let usage_request_id = request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let entry = crate::models::UsageLogEntry {
        request_id: usage_request_id,
        api_key_id,
        user_id,
        org_id,
        project_id,
        team_id,
        service_account_id,
        model,
        provider,
        // Saturate tokens to i32::MAX to prevent overflow with extremely large values
        input_tokens: saturate_i64_to_i32(input_tokens.unwrap_or(0)),
        output_tokens: saturate_i64_to_i32(output_tokens.unwrap_or(0)),
        cost_microcents,
        http_referer: tracker.referer,
        request_at: chrono::Utc::now(),
        streamed: tracker.streamed,
        cached_tokens: 0,    // Populated by streaming parser from response body
        reasoning_tokens: 0, // Populated by streaming parser from response body
        finish_reason: None, // Populated by streaming parser from response body
        latency_ms: Some(latency_ms),
        cancelled: false,
        status_code: Some(response.status().as_u16() as i16),
        pricing_source: usage.pricing_source,
        image_count: usage.image_count,
        audio_seconds: usage.audio_seconds,
        character_count: usage.character_count,
        provider_source,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
    };

    let is_success = response.status().is_success();

    // Push to usage buffer for batched writes (if available).
    // Skip for streaming responses (UsageTrackingStream writes correct values)
    // and non-LLM requests (no X-Model header means this isn't an LLM call).
    #[cfg(feature = "concurrency")]
    if has_model && !is_streaming {
        if let Some(buffer) = &state.usage_buffer {
            tracing::debug!(
                api_key_id = ?api_key_id,
                user_id = ?user_id,
                org_id = ?org_id,
                model = %entry.model,
                input_tokens = entry.input_tokens,
                output_tokens = entry.output_tokens,
                cost_microcents = ?entry.cost_microcents,
                "Pushing usage entry to buffer"
            );
            buffer.push(entry);
        } else {
            tracing::warn!("Usage buffer not available, usage entry not tracked");
        }
    }

    // Budget and token adjustments remain API-key-scoped
    // (session users have no API key budget configured)
    if api_key.is_some() {
        if let Some(cache) = state.cache {
            // Use task_tracker to ensure this task completes during graceful shutdown
            #[cfg(feature = "server")]
            state.task_tracker.spawn(async move {
                // Adjust budget reservation with actual cost (for successful responses)
                // This replaces the estimated cost that was reserved before the request
                if is_success {
                    if let Some(reservation) = &budget_reservation {
                        // Get actual cost (or 0 if not available) - in microcents
                        let actual_cost = cost_microcents.unwrap_or(0);
                        let succeeded =
                            adjust_budget_reservation(&cache, reservation, actual_cost).await;
                        metrics::record_cache_operation(
                            "budget",
                            "adjust",
                            if succeeded { "success" } else { "error" },
                        );
                    }
                } else if let Some(reservation) = &budget_reservation {
                    // Request failed - refund the entire reservation
                    // (we reserved estimated cost, now we're removing it since request didn't count)
                    let succeeded = adjust_budget_reservation(&cache, reservation, 0).await;
                    metrics::record_cache_operation(
                        "budget",
                        "refund",
                        if succeeded { "success" } else { "error" },
                    );
                }

                // Adjust token rate limit reservation with actual token count
                if let Some(reservation) = &token_reservation {
                    let (succeeded, operation) = if is_success {
                        // Request succeeded - adjust with actual tokens
                        let total_tokens = input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0);
                        (
                            adjust_token_reservation(&cache, reservation, total_tokens).await,
                            "adjust",
                        )
                    } else {
                        // Request failed - refund the entire reservation
                        (
                            adjust_token_reservation(&cache, reservation, 0).await,
                            "refund",
                        )
                    };
                    metrics::record_cache_operation(
                        "token_rate_limit",
                        operation,
                        if succeeded { "success" } else { "error" },
                    );
                }
            });
        }
    }
}

/// Try to authenticate from headers based on the configured `AuthMode`.
///
/// - `None` — optional auth (try API key if present, don't require it)
/// - `ApiKey` — require API key
/// - `Idp` — try session/API key/JWT with format-based detection; rejects ambiguous dual credentials
/// - `Iap` — try proxy identity headers, also accept API key
///
/// In `Idp` mode, **format-based detection** is used:
/// - Tokens in `Authorization: Bearer` starting with the API key prefix are validated as API keys
/// - Other Bearer tokens are validated as JWTs
/// - `X-API-Key` header is always validated as an API key
async fn try_authenticate(
    headers: &axum::http::HeaderMap,
    cookies: Option<&tower_cookies::Cookies>,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<AuthenticatedRequest, AuthError> {
    use crate::config::AuthMode;

    let api_key_config = state.config.auth.api_key_config();
    #[cfg(feature = "sso")]
    let api_key_header = api_key_config.header_name.as_str();
    #[cfg(not(feature = "sso"))]
    let _ = (cookies, &api_key_config);

    match &state.config.auth.mode {
        AuthMode::None => {
            // Optional auth: try API key if header present, don't require it
            let api_key = try_api_key_auth(headers, state).await?;
            match api_key {
                Some(api_key) => Ok(AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(
                    api_key,
                )))),
                None => Err(AuthError::MissingCredentials),
            }
        }
        AuthMode::ApiKey => {
            // Require API key
            let api_key = try_api_key_auth(headers, state).await?;
            match api_key {
                Some(api_key) => Ok(AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(
                    api_key,
                )))),
                None => Err(AuthError::MissingCredentials),
            }
        }
        #[cfg(feature = "sso")]
        AuthMode::Idp => {
            // Idp mode: reject ambiguous dual credentials
            // (both X-API-Key and Authorization headers present)
            let has_api_key_header = headers.contains_key(api_key_header);
            let has_auth_header = headers.contains_key(axum::http::header::AUTHORIZATION);
            if has_api_key_header && has_auth_header {
                return Err(AuthError::AmbiguousCredentials);
            }

            // Try session cookie → API key → JWT
            // Session first because it's cheapest (no JWKS fetch, no DB hash lookup)
            let api_key = try_api_key_auth(headers, state).await?;
            let identity = if let Some(id) = try_session_api_auth(cookies, state).await? {
                Some(id)
            } else {
                try_jwt_api_auth(headers, connecting_ip, state).await?
            };
            let kind = match (api_key, identity) {
                (Some(api_key), Some(identity)) => IdentityKind::Both {
                    api_key: Box::new(api_key),
                    identity,
                },
                (Some(api_key), None) => IdentityKind::ApiKey(Box::new(api_key)),
                (None, Some(identity)) => IdentityKind::Identity(identity),
                (None, None) => return Err(AuthError::MissingCredentials),
            };
            Ok(AuthenticatedRequest::new(kind))
        }
        AuthMode::Iap(_) => {
            // Try proxy headers, also accept API key
            let api_key = try_api_key_auth(headers, state).await?;
            let identity = try_identity_auth(headers, connecting_ip, state).await?;
            let kind = match (api_key, identity) {
                (Some(api_key), Some(identity)) => IdentityKind::Both {
                    api_key: Box::new(api_key),
                    identity,
                },
                (Some(api_key), None) => IdentityKind::ApiKey(Box::new(api_key)),
                (None, Some(identity)) => IdentityKind::Identity(identity),
                (None, None) => return Err(AuthError::MissingCredentials),
            };
            Ok(AuthenticatedRequest::new(kind))
        }
    }
}

/// Try to authenticate via API key.
///
/// Checks for API keys in the following order:
/// 1. `X-API-Key` header (or configured header name)
/// 2. `Authorization: Bearer` header (only if token starts with API key prefix)
///
/// In idp mode, format-based detection allows API keys in the Bearer header:
/// tokens starting with the configured prefix (e.g., `gw_`) are treated as API keys.
#[allow(clippy::collapsible_if)]
pub(crate) async fn try_api_key_auth(
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> Result<Option<ApiKeyAuth>, AuthError> {
    // Get header name and key prefix from config
    let api_key_config = state.config.auth.api_key_config();
    let (header_name, key_prefix) = (
        api_key_config.header_name.as_str(),
        api_key_config.key_prefix.as_str(),
    );

    use std::borrow::Cow;

    // Try X-API-Key header first, then check Authorization: Bearer for API key format.
    // Format-based detection for API keys in Bearer token:
    // Tokens starting with the configured prefix (default: "gw_") are treated as API keys.
    // This allows clients to use the standard Authorization header with either:
    //   - API keys: "Authorization: Bearer gw_xxx..."
    //   - JWTs: "Authorization: Bearer eyJxxx..."
    // The prefix-based discrimination is deterministic and avoids ambiguity.
    let raw_key: Cow<'_, str> = if let Some(h) = headers.get(header_name) {
        Cow::Borrowed(h.to_str().map_err(|_| AuthError::InvalidApiKeyFormat)?)
    } else if let Some(h) = headers.get(axum::http::header::AUTHORIZATION) {
        let value = h.to_str().map_err(|_| AuthError::InvalidApiKeyFormat)?;
        // Use ASCII case-insensitive comparison instead of to_lowercase() allocation
        if value.len() >= 7 && value[..7].eq_ignore_ascii_case("bearer ") {
            let token = &value[7..];
            if token.starts_with(key_prefix) {
                Cow::Borrowed(token)
            } else {
                // Not an API key format, let JWT handler try
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    } else {
        return Ok(None);
    };

    // Use constant-time comparison to prevent timing attacks
    if !has_valid_prefix(&raw_key, key_prefix) {
        return Err(AuthError::InvalidApiKeyFormat);
    }

    let key_hash = hash_api_key(&raw_key);

    // Try cache first if available
    // Cache is invalidated on revoke, so we can trust cached data for the TTL
    if let Some(cache) = &state.cache {
        use crate::{cache::CacheKeys, models::CachedApiKey};

        let cache_key = CacheKeys::api_key(&key_hash);

        // Try to get from cache
        match cache.get_bytes(&cache_key).await {
            Ok(Some(bytes)) => {
                if let Ok(cached) = serde_json::from_slice::<CachedApiKey>(&bytes) {
                    metrics::record_cache_operation("api_key", "get", "hit");

                    let api_key_auth = ApiKeyAuth {
                        key: cached.key.clone(),
                        org_id: cached.org_id,
                        team_id: cached.team_id,
                        project_id: cached.project_id,
                        user_id: cached.user_id,
                        service_account_id: cached.service_account_id,
                        service_account_roles: cached.service_account_roles,
                    };

                    // Check revocation and expiration from cached data
                    // Note: Cache is invalidated on revoke (see routes/admin/api_keys.rs)
                    // so if we have a cache hit, the key was valid when cached
                    if api_key_auth.is_revoked() {
                        // This shouldn't happen since we invalidate on revoke,
                        // but handle it gracefully just in case
                        let _ = cache.delete(&cache_key).await;
                        return Err(AuthError::InvalidApiKey);
                    }

                    if api_key_auth.is_expired() {
                        // Key expired since it was cached - clean up and reject
                        let _ = cache.delete(&cache_key).await;
                        return Err(AuthError::ExpiredApiKey);
                    }

                    // Cache hit with valid key - skip DB query entirely
                    tracing::trace!(
                        api_key_id = %api_key_auth.key.id,
                        "API key authenticated from cache"
                    );
                    return Ok(Some(api_key_auth));
                } else {
                    // Deserialization failed - treat as miss
                    metrics::record_cache_operation("api_key", "get", "miss");
                }
            }
            Ok(None) => {
                metrics::record_cache_operation("api_key", "get", "miss");
            }
            Err(_) => {
                metrics::record_cache_operation("api_key", "get", "error");
            }
        }
    }

    // Cache miss or not configured - fetch from database
    let db = state
        .db
        .as_ref()
        .ok_or_else(|| AuthError::Internal("Database not configured".to_string()))?;

    let key_with_owner = db
        .api_keys()
        .get_by_hash(&key_hash)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
        .ok_or(AuthError::InvalidApiKey)?;

    let api_key_auth = ApiKeyAuth {
        key: key_with_owner.key.clone(),
        org_id: key_with_owner.org_id,
        team_id: key_with_owner.team_id,
        project_id: key_with_owner.project_id,
        user_id: key_with_owner.user_id,
        service_account_id: key_with_owner.service_account_id,
        service_account_roles: key_with_owner.service_account_roles,
    };

    if api_key_auth.is_revoked() {
        return Err(AuthError::InvalidApiKey);
    }

    if api_key_auth.is_expired() {
        return Err(AuthError::ExpiredApiKey);
    }

    // Cache the API key for future requests (configurable TTL, default 5 min)
    // This caches all data needed for auth, so cache hits skip the DB entirely
    // Note: Cache is invalidated immediately on revoke (see routes/admin/api_keys.rs)
    // The TTL only matters for multi-node deployments without Redis
    if let Some(cache) = &state.cache {
        use crate::{cache::CacheKeys, models::CachedApiKey};

        let cached = CachedApiKey {
            key: api_key_auth.key.clone(),
            org_id: api_key_auth.org_id,
            team_id: api_key_auth.team_id,
            project_id: api_key_auth.project_id,
            user_id: api_key_auth.user_id,
            service_account_id: api_key_auth.service_account_id,
            service_account_roles: api_key_auth.service_account_roles.clone(),
        };

        let ttl = std::time::Duration::from_secs(state.config.cache.ttl().api_key_secs);

        if let Ok(bytes) = serde_json::to_vec(&cached) {
            let cache_key = CacheKeys::api_key(&key_hash);
            match cache.set_bytes(&cache_key, &bytes, ttl).await {
                Ok(_) => metrics::record_cache_operation("api_key", "set", "success"),
                Err(_) => metrics::record_cache_operation("api_key", "set", "error"),
            }
        }

        // Store reverse mapping (ID -> hash) for cache invalidation on revoke
        let reverse_key = CacheKeys::api_key_reverse(api_key_auth.key.id);
        match cache
            .set_bytes(&reverse_key, key_hash.as_bytes(), ttl)
            .await
        {
            Ok(_) => metrics::record_cache_operation("api_key", "set", "success"),
            Err(_) => metrics::record_cache_operation("api_key", "set", "error"),
        }
    }

    // Fire-and-forget update of last_used_at, debounced to once per 5 minutes.
    // Uses a cache key to avoid redundant DB writes on high-traffic keys.
    if let Some(db) = state.db.clone() {
        let key_id = api_key_auth.key.id;
        let cache = state.cache.clone();
        tokio::spawn(async move {
            const DEBOUNCE_SECS: u64 = 300; // 5 minutes

            // If cache is available, check/set a debounce key to skip redundant writes
            if let Some(ref cache) = cache {
                let debounce_key = CacheKeys::api_key_last_used(key_id);
                // Try to read the debounce key — if present, skip the DB write
                if cache
                    .get_bytes(&debounce_key)
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    return;
                }
                // Set the debounce key (best-effort, ignore errors)
                let _ = cache
                    .set_bytes(
                        &debounce_key,
                        b"1",
                        std::time::Duration::from_secs(DEBOUNCE_SECS),
                    )
                    .await;
            }

            if let Err(e) = db.api_keys().update_last_used(key_id).await {
                tracing::debug!(error = %e, api_key_id = %key_id, "Failed to update API key last_used_at");
            }
        });
    }

    Ok(Some(api_key_auth))
}

/// Try to authenticate via session cookie for API endpoints.
///
/// Validates OIDC/SAML session cookies so users who logged in via SSO can
/// use the chat UI on `/v1/*` endpoints without needing a separate API key.
/// Session cookies are cheaper to validate than JWTs (no JWKS fetch).
#[cfg(feature = "sso")]
async fn try_session_api_auth(
    cookies: Option<&tower_cookies::Cookies>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    // Get the OIDC registry which holds the shared session store
    let registry = match &state.oidc_registry {
        Some(reg) => reg,
        None => return Ok(None),
    };

    let session_config = state.config.auth.session_config_or_default();

    let cookies = match cookies {
        Some(c) => c,
        None => return Ok(None),
    };

    // Get session ID from cookie
    let session_cookie = match cookies.get(&session_config.cookie_name) {
        Some(c) => c,
        None => return Ok(None),
    };

    let session_id: uuid::Uuid = match session_cookie.value().parse() {
        Ok(id) => id,
        Err(_) => return Ok(None),
    };

    // Validate session (checks expiration, inactivity timeout, updates last_activity)
    let session = match crate::auth::session_store::validate_and_refresh_session(
        registry.session_store().as_ref(),
        session_id,
        &session_config.enhanced,
    )
    .await
    {
        Ok(s) => s,
        Err(
            crate::auth::session_store::SessionError::NotFound
            | crate::auth::session_store::SessionError::Expired,
        ) => return Ok(None),
        Err(e) => {
            tracing::debug!(session_id = %session_id, error = %e, "Session validation failed");
            return Ok(None);
        }
    };

    // Look up internal user and memberships from the database
    let (user_id, org_ids, team_ids, project_ids) = if let Some(db) = &state.db {
        match db
            .users()
            .get_by_external_id(&session.external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
        {
            Some(user) => {
                let user_id = user.id;

                let org_ids: Vec<String> = db
                    .users()
                    .get_org_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?
                    .iter()
                    .map(|m| m.org_id.to_string())
                    .collect();

                let team_ids: Vec<String> = db
                    .users()
                    .get_team_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?
                    .iter()
                    .map(|m| m.team_id.to_string())
                    .collect();

                let project_ids: Vec<String> = db
                    .users()
                    .get_project_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?
                    .iter()
                    .map(|m| m.project_id.to_string())
                    .collect();

                (Some(user_id), org_ids, team_ids, project_ids)
            }
            None => {
                // User not found in DB — they may need to log in via admin first
                // to trigger JIT provisioning. Don't provision here on API path.
                return Ok(None);
            }
        }
    } else {
        return Ok(None);
    };

    let roles = if session.roles.is_empty() {
        session.groups.clone()
    } else {
        session.roles.clone()
    };

    Ok(Some(Identity {
        external_id: session.external_id,
        email: session.email,
        name: session.name,
        user_id,
        roles,
        idp_groups: session.groups.clone(),
        org_ids,
        team_ids,
        project_ids,
    }))
}

/// Try to authenticate via identity headers
///
/// **Security:** This function validates that the connecting IP is from a trusted
/// proxy before trusting identity headers. This prevents header spoofing attacks
/// where an attacker connects directly to the gateway and sets fake identity headers.
async fn try_identity_auth(
    headers: &axum::http::HeaderMap,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    let config = match state.config.auth.iap_config() {
        Some(config) => config,
        None => return Ok(None),
    };

    // SECURITY: Validate that the request comes from a trusted proxy before trusting headers.
    // If trusted_proxies is configured, we MUST verify the connecting IP is trusted.
    // If trusted_proxies is NOT configured, we trust all sources (for backwards compatibility
    // and development environments where the gateway is behind a trusted network boundary).
    let trusted_proxies = &state.config.server.trusted_proxies;
    if trusted_proxies.is_configured() {
        let parsed_cidrs = trusted_proxies.parsed_cidrs();

        let is_trusted = match connecting_ip {
            Some(ip) => trusted_proxies.is_trusted_ip(ip, &parsed_cidrs),
            // No connecting IP available - only trust if dangerously_trust_all is explicitly set
            None => trusted_proxies.dangerously_trust_all,
        };

        if !is_trusted {
            // Request is not from a trusted proxy - do not trust identity headers
            if let Some(ip) = connecting_ip
                && headers.contains_key(&config.identity_header)
            {
                tracing::warn!(
                    connecting_ip = %ip,
                    identity_header = %config.identity_header,
                    "Ignoring identity header from untrusted IP - \
                     configure server.trusted_proxies to trust this source"
                );
            }
            return Ok(None);
        }
    }

    let external_id = match headers.get(&config.identity_header) {
        Some(h) => h
            .to_str()
            .map_err(|_| AuthError::Internal("Invalid identity header".to_string()))?
            .to_string(),
        None => return Ok(None),
    };

    let iap = state.config.auth.iap_config();
    let email = extract_header(headers, iap, "email");
    let name = extract_header(headers, iap, "name");

    let user_id = if let Some(db) = &state.db {
        db.users()
            .get_by_external_id(&external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
            .map(|u| u.id)
    } else {
        None
    };

    // Extract roles from groups header if configured
    let roles = extract_groups(headers, iap);

    // For proxy auth, groups header serves as both roles and raw groups
    Ok(Some(Identity {
        external_id,
        email,
        name,
        user_id,
        roles: roles.clone(),
        idp_groups: roles,
        org_ids: Vec::new(),
        team_ids: Vec::new(),
        project_ids: Vec::new(),
    }))
}

/// Try to authenticate via JWT for API endpoints.
///
/// This handles Bearer token authentication in `Idp` mode, validating JWTs
/// via per-org SSO configurations in the `GatewayJwtRegistry`.
/// Unlike `try_identity_auth` which handles proxy-forwarded headers,
/// this validates JWT tokens directly.
///
/// Tokens starting with the API key prefix are skipped
/// (they're already handled by `try_api_key_auth`).
#[cfg(feature = "sso")]
async fn try_jwt_api_auth(
    headers: &axum::http::HeaderMap,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    // JWT auth is only available via per-org GatewayJwtRegistry (Idp mode)
    let is_idp = matches!(state.config.auth.mode, crate::config::AuthMode::Idp);
    if !is_idp {
        return Ok(None);
    }

    // Use API key prefix for format-based detection to skip API key tokens
    let key_prefix = Some(state.config.auth.api_key_config().key_prefix.as_str());

    // Extract Bearer token from Authorization header
    let auth_header = match headers.get(axum::http::header::AUTHORIZATION) {
        Some(h) => h,
        None => return Ok(None),
    };

    let auth_value = auth_header.to_str().map_err(|_| AuthError::InvalidToken)?;

    // Check for Bearer prefix (case-insensitive, no allocation)
    let token = if auth_value.len() >= 7 && auth_value[..7].eq_ignore_ascii_case("bearer ") {
        &auth_value[7..]
    } else {
        return Ok(None); // Not a Bearer token
    };

    // In idp mode, skip JWT validation if token has API key prefix
    // (already handled by try_api_key_auth via format-based detection)
    if key_prefix.is_some_and(|prefix| token.starts_with(prefix)) {
        return Ok(None);
    }

    // Try per-org SSO JWT validators first (by issuer), then fall back to global config.
    // This supports multi-tenant JWT auth where each org has its own IdP.
    if let Some(registry) = &state.gateway_jwt_registry {
        // Decode the issuer from the token without verification (cheap base64 decode)
        if let Some(iss) = decode_jwt_issuer(token) {
            // Look up validators, lazy-loading from DB on cache miss.
            // find_or_load_by_issuer deduplicates concurrent loads and caches
            // negative results to prevent DB query amplification.
            let validators = if let Some(db) = &state.db {
                let rate_limit = match (&state.cache, connecting_ip) {
                    (Some(cache), Some(ip)) => {
                        Some(crate::auth::gateway_jwt::LazyLoadRateLimit { cache, ip })
                    }
                    _ => None,
                };
                match registry
                    .find_or_load_by_issuer(
                        &iss,
                        db,
                        &state.http_client,
                        state.config.server.allow_loopback_urls,
                        state.config.server.allow_private_urls,
                        rate_limit,
                    )
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            issuer = %iss,
                            error = %e,
                            "Per-org JWT registry lookup failed"
                        );
                        Vec::new()
                    }
                }
            } else {
                registry.find_validators_by_issuer(&iss).await
            };

            // Try each matching validator; first success wins.
            // Disambiguation for shared-issuer orgs works naturally: each validator
            // enforces its own audience (the org's client_id), so a token issued for
            // org A's client will fail audience validation on org B's validator.
            for (org_id, validator) in &validators {
                match validator.validate(token).await {
                    Ok(claims) => {
                        return build_jwt_identity(&claims, validator, state, Some(*org_id))
                            .await
                            .map(Some);
                    }
                    Err(e) => {
                        tracing::debug!(
                            org_id = %org_id,
                            error = %e,
                            "Per-org JWT validation failed"
                        );
                    }
                }
            }
        }
    }

    // No per-org match — not a JWT we can validate.
    // In the new AuthMode system, JWT is only available via per-org GatewayJwtRegistry.
    Ok(None)
}

/// Decode the `iss` claim from a JWT without verifying the signature.
/// This is a cheap base64 decode of the payload used for routing to the right validator.
#[cfg(any(feature = "sso", test))]
fn decode_jwt_issuer(token: &str) -> Option<String> {
    use base64::Engine;

    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() < 2 {
        return None;
    }
    // JWT payloads use base64url without padding per RFC 7519 §3,
    // but some IdPs (e.g. older Azure AD) emit padded tokens.
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    value.get("iss")?.as_str().map(String::from)
}

/// Build an `Identity` from validated JWT claims. Shared by per-org and global paths.
#[cfg(feature = "sso")]
async fn build_jwt_identity(
    claims: &crate::auth::jwt::JwtClaims,
    validator: &crate::auth::jwt::JwtValidator,
    state: &AppState,
    known_org_id: Option<uuid::Uuid>,
) -> Result<Identity, AuthError> {
    let external_id = validator.extract_identity(claims);

    // Look up user in database to get internal user_id
    let user_id = if let Some(db) = &state.db {
        db.users()
            .get_by_external_id(&external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
            .map(|u| u.id)
    } else {
        None
    };

    let roles = claims.roles.clone().unwrap_or_default();

    // Fetch org/team memberships from database (more reliable than JWT claims)
    let (mut org_ids, team_ids) = if let Some(db) = &state.db
        && let Some(user_id) = user_id
    {
        let org_memberships = db
            .users()
            .get_org_memberships_for_user(user_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        let org_ids: Vec<String> = org_memberships
            .iter()
            .map(|m| m.org_id.to_string())
            .collect();

        let team_memberships = db
            .users()
            .get_team_memberships_for_user(user_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        let team_ids: Vec<String> = team_memberships
            .iter()
            .map(|m| m.team_id.to_string())
            .collect();

        (org_ids, team_ids)
    } else {
        (Vec::new(), Vec::new())
    };

    // If we matched a per-org SSO config, ensure that org is included in org_ids.
    // This is intentional: a valid JWT from an org's configured IdP proves the user
    // belongs to that org, enabling JIT provisioning and per-org RBAC evaluation
    // even before the membership is persisted in the database.
    if let Some(org_id) = known_org_id {
        let org_str = org_id.to_string();
        if !org_ids.contains(&org_str) {
            org_ids.push(org_str);
        }
    }

    tracing::debug!(
        sub = %claims.sub,
        external_id = %external_id,
        roles = ?roles,
        user_id = ?user_id,
        known_org_id = ?known_org_id,
        "API request authenticated via JWT"
    );

    Ok(Identity {
        external_id,
        email: claims.email.clone(),
        name: claims.name.clone(),
        user_id,
        roles,
        idp_groups: claims.groups.clone().unwrap_or_default(),
        org_ids,
        team_ids,
        project_ids: Vec::new(),
    })
}

fn extract_groups(
    headers: &axum::http::HeaderMap,
    iap_config: Option<&crate::config::IapConfig>,
) -> Vec<String> {
    if let Some(config) = iap_config
        && let Some(header_name) = &config.groups_header
        && let Some(value) = headers.get(header_name).and_then(|v| v.to_str().ok())
    {
        if let Ok(groups) = serde_json::from_str::<Vec<String>>(value) {
            return groups;
        }
        return value.split(',').map(|s| s.trim().to_string()).collect();
    }
    Vec::new()
}

fn extract_header(
    headers: &axum::http::HeaderMap,
    iap_config: Option<&crate::config::IapConfig>,
    field: &str,
) -> Option<String> {
    let config = iap_config?;
    let header_name = match field {
        "email" => config.email_header.as_ref()?,
        "name" => config.name_header.as_ref()?,
        _ => return None,
    };
    headers.get(header_name)?.to_str().ok().map(String::from)
}

/// Log a budget exceeded event to the audit log (fire-and-forget)
fn log_budget_exceeded(event: BudgetExceededEvent<'_>) {
    let BudgetExceededEvent {
        state,
        api_key_id,
        org_id,
        project_id,
        limit_cents,
        current_spend_cents,
        period,
        request_path,
        request_id,
        ip_address,
        user_agent,
    } = event;

    // Publish budget threshold reached event to WebSocket subscribers
    state
        .event_bus
        .publish(ServerEvent::BudgetThresholdReached {
            timestamp: Utc::now(),
            budget_type: period_to_budget_type(period),
            threshold_percent: 100, // 100% = exceeded
            current_amount_microcents: current_spend_cents * 10_000, // Convert cents to microcents
            limit_microcents: limit_cents * 10_000,
            user_id: None, // API key doesn't directly map to user_id
            org_id,
            project_id,
        });

    let Some(db) = &state.db else { return };
    let db = db.clone();
    let path = request_path.to_string();
    let req_id = request_id.map(String::from);

    // Fire-and-forget: spawn a task to log the audit event
    // This ensures we don't block the response on audit logging
    #[cfg(feature = "server")]
    state.task_tracker.spawn(async move {
        let result = db
            .audit_logs()
            .create(CreateAuditLog {
                actor_type: AuditActorType::ApiKey,
                actor_id: Some(api_key_id),
                action: "budget.exceeded".to_string(),
                resource_type: "api_key".to_string(),
                resource_id: api_key_id,
                org_id,
                project_id,
                details: serde_json::json!({
                    "limit_cents": limit_cents,
                    "current_spend_cents": current_spend_cents,
                    "period": period.as_str(),
                    "request_path": path,
                    "request_id": req_id,
                }),
                ip_address,
                user_agent,
            })
            .await;

        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                api_key_id = %api_key_id,
                "Failed to log budget.exceeded audit event"
            );
        }
    });
}

/// Convert BudgetPeriod to BudgetType for events.
fn period_to_budget_type(period: BudgetPeriod) -> BudgetType {
    match period {
        BudgetPeriod::Daily => BudgetType::Daily,
        BudgetPeriod::Monthly => BudgetType::Monthly,
    }
}

/// Log a budget warning event to the audit log (fire-and-forget, once per period)
///
/// Uses cache to deduplicate: only logs once per API key per budget period.
/// This prevents flooding the audit log with repeated warnings.
/// Note: WebSocket events are always published for real-time monitoring.
fn log_budget_warning(event: BudgetWarningEvent<'_>) {
    let BudgetWarningEvent {
        state,
        api_key_id,
        org_id,
        project_id,
        spend_percentage,
        current_spend_cents,
        limit_cents,
        period,
        request_path,
        request_id,
        ip_address,
        user_agent,
    } = event;

    // Publish budget threshold warning event to WebSocket subscribers
    // This is published on every warning for real-time dashboards
    state
        .event_bus
        .publish(ServerEvent::BudgetThresholdReached {
            timestamp: Utc::now(),
            budget_type: period_to_budget_type(period),
            threshold_percent: (spend_percentage * 100.0) as u8,
            current_amount_microcents: current_spend_cents * 10_000, // Convert cents to microcents
            limit_microcents: limit_cents * 10_000,
            user_id: None,
            org_id,
            project_id,
        });

    let Some(db) = &state.db else { return };
    let Some(cache) = &state.cache else { return };

    let db = db.clone();
    let cache = cache.clone();
    let path = request_path.to_string();
    let req_id = request_id.map(String::from);

    // Fire-and-forget: spawn a task to log the audit event
    #[cfg(feature = "server")]
    state.task_tracker.spawn(async move {
        // Check if we've already logged a warning for this API key in this budget period
        // Cache key format: budget_warning_logged:{api_key_id}:{period}
        let cache_key = format!("budget_warning_logged:{}:{}", api_key_id, period.as_str());
        let ttl = CacheKeys::ttl_until_period_end(period);

        // Try to set the flag - if it already exists, we've already logged
        match cache.set_nx(&cache_key, b"1", ttl).await {
            Ok(true) => {
                // We set the flag, so this is the first warning this period - log it
                tracing::info!(
                    api_key_id = %api_key_id,
                    spend_percentage = %format!("{:.1}%", spend_percentage * 100.0),
                    current_spend_cents = current_spend_cents,
                    limit_cents = limit_cents,
                    period = %period.as_str(),
                    "Budget warning threshold exceeded"
                );

                let result = db
                    .audit_logs()
                    .create(CreateAuditLog {
                        actor_type: AuditActorType::ApiKey,
                        actor_id: Some(api_key_id),
                        action: "budget.warning".to_string(),
                        resource_type: "api_key".to_string(),
                        resource_id: api_key_id,
                        org_id,
                        project_id,
                        details: serde_json::json!({
                            "spend_percentage": spend_percentage,
                            "current_spend_cents": current_spend_cents,
                            "limit_cents": limit_cents,
                            "period": period.as_str(),
                            "request_path": path,
                            "request_id": req_id,
                        }),
                        ip_address,
                        user_agent,
                    })
                    .await;

                if let Err(e) = result {
                    tracing::warn!(
                        error = %e,
                        api_key_id = %api_key_id,
                        "Failed to log budget.warning audit event"
                    );
                }
            }
            Ok(false) => {
                // Flag already exists - we've already logged this period
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    api_key_id = %api_key_id,
                    "Failed to check budget warning flag in cache"
                );
            }
        }
    });
}

/// Add budget warning headers to the response
fn add_budget_warning_headers(response: Response, warning: &BudgetWarning) -> Response {
    use axum::http::HeaderValue;

    let (mut parts, body) = response.into_parts();

    // Add warning headers
    // X-Budget-Warning: true
    parts
        .headers
        .insert("X-Budget-Warning", HeaderValue::from_static("true"));

    // X-Budget-Spend-Percentage: 0.85 (as percentage of limit)
    if let Ok(v) = HeaderValue::from_str(&format!("{:.2}", warning.spend_percentage)) {
        parts.headers.insert("X-Budget-Spend-Percentage", v);
    }

    // X-Budget-Current-Spend-Cents: 850 (current spend in cents)
    if let Ok(v) = HeaderValue::from_str(&warning.current_spend_cents.to_string()) {
        parts.headers.insert("X-Budget-Current-Spend-Cents", v);
    }

    // X-Budget-Limit-Cents: 1000 (budget limit in cents)
    if let Ok(v) = HeaderValue::from_str(&warning.limit_cents.to_string()) {
        parts.headers.insert("X-Budget-Limit-Cents", v);
    }

    // X-Budget-Period: daily or monthly
    if let Ok(v) = HeaderValue::from_str(warning.period.as_str()) {
        parts.headers.insert("X-Budget-Period", v);
    }

    Response::from_parts(parts, body)
}

/// Saturate an i64 value to fit in an i32.
///
/// Returns `i32::MAX` if the value exceeds the i32 range,
/// `i32::MIN` if below, or the value as i32 otherwise.
#[inline]
fn saturate_i64_to_i32(value: i64) -> i32 {
    if value > i32::MAX as i64 {
        i32::MAX
    } else if value < i32::MIN as i64 {
        i32::MIN
    } else {
        value as i32
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::task::TaskTracker;

    use super::*;
    use crate::config::{ApiKeyAuthConfig, AuthMode, GatewayConfig, HashAlgorithm};

    /// Create AppState with Idp configuration
    fn create_multi_auth_state(header_name: &str, key_prefix: &str) -> AppState {
        let mut config = GatewayConfig::parse("").unwrap();
        #[cfg(feature = "sso")]
        {
            config.auth.mode = AuthMode::Idp;
        }
        #[cfg(not(feature = "sso"))]
        {
            config.auth.mode = AuthMode::ApiKey;
        }
        config.auth.api_key = Some(ApiKeyAuthConfig {
            header_name: header_name.to_string(),
            key_prefix: key_prefix.to_string(),
            generation_prefix: None,
            hash_algorithm: HashAlgorithm::default(),
            cache_ttl_secs: 300,
        });

        AppState {
            config: Arc::new(config),
            db: None,
            services: None,
            cache: None,
            secrets: None,
            dlq: None,
            pricing: Arc::new(crate::pricing::PricingConfig::default()),
            circuit_breakers: crate::providers::CircuitBreakerRegistry::new(),
            provider_health: crate::jobs::ProviderHealthStateRegistry::new(),
            task_tracker: TaskTracker::new(),
            usage_drain: {
                let tracker = TaskTracker::new();
                crate::streaming::UsageDrainHandle::spawn(&tracker, 16)
            },
            #[cfg(feature = "sso")]
            oidc_registry: None,
            #[cfg(feature = "saml")]
            saml_registry: None,
            gateway_jwt_registry: None,
            policy_registry: None,
            usage_buffer: None,
            response_cache: None,
            semantic_cache: None,
            input_guardrails: None,
            output_guardrails: None,
            event_bus: Arc::new(crate::events::EventBus::new()),
            file_search_service: None,
            shell_runtime: None,
            responses_store: None,
            containers_service: None,
            container_session_registry: std::sync::Arc::new(
                crate::services::container_session::ContainerSessionRegistry::new(),
            ),
            response_event_buffer: None,
            #[cfg(any(
                feature = "document-extraction-basic",
                feature = "document-extraction-full"
            ))]
            document_processor: None,
            http_client: reqwest::Client::new(),
            default_user_id: None,
            default_org_id: None,
            provider_metrics: Arc::new(
                crate::services::ProviderMetricsService::with_local_metrics(|| None),
            ),
            model_catalog: crate::catalog::ModelCatalogRegistry::new(),
            static_models_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// Create AppState with API key only authentication
    fn create_api_key_only_state(header_name: &str, key_prefix: &str) -> AppState {
        let mut config = GatewayConfig::parse("").unwrap();
        config.auth.mode = AuthMode::ApiKey;
        config.auth.api_key = Some(ApiKeyAuthConfig {
            header_name: header_name.to_string(),
            key_prefix: key_prefix.to_string(),
            generation_prefix: None,
            hash_algorithm: HashAlgorithm::default(),
            cache_ttl_secs: 300,
        });

        AppState {
            config: Arc::new(config),
            db: None,
            services: None,
            cache: None,
            secrets: None,
            dlq: None,
            pricing: Arc::new(crate::pricing::PricingConfig::default()),
            circuit_breakers: crate::providers::CircuitBreakerRegistry::new(),
            provider_health: crate::jobs::ProviderHealthStateRegistry::new(),
            task_tracker: TaskTracker::new(),
            usage_drain: {
                let tracker = TaskTracker::new();
                crate::streaming::UsageDrainHandle::spawn(&tracker, 16)
            },
            #[cfg(feature = "sso")]
            oidc_registry: None,
            #[cfg(feature = "saml")]
            saml_registry: None,
            gateway_jwt_registry: None,
            policy_registry: None,
            usage_buffer: None,
            response_cache: None,
            semantic_cache: None,
            input_guardrails: None,
            output_guardrails: None,
            event_bus: Arc::new(crate::events::EventBus::new()),
            file_search_service: None,
            shell_runtime: None,
            responses_store: None,
            containers_service: None,
            container_session_registry: std::sync::Arc::new(
                crate::services::container_session::ContainerSessionRegistry::new(),
            ),
            response_event_buffer: None,
            #[cfg(any(
                feature = "document-extraction-basic",
                feature = "document-extraction-full"
            ))]
            document_processor: None,
            http_client: reqwest::Client::new(),
            default_user_id: None,
            default_org_id: None,
            provider_metrics: Arc::new(
                crate::services::ProviderMetricsService::with_local_metrics(|| None),
            ),
            model_catalog: crate::catalog::ModelCatalogRegistry::new(),
            static_models_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    fn make_headers(headers: Vec<(&str, &str)>) -> axum::http::HeaderMap {
        let mut map = axum::http::HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                axum::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::header::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    // ========== Idp mode ambiguous credentials tests ==========

    #[cfg(feature = "sso")]
    #[tokio::test]
    async fn test_idp_auth_ambiguous_credentials_rejected() {
        // In Idp mode, providing both X-API-Key and Authorization headers
        // should be rejected as ambiguous
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![
            ("X-API-Key", "gw_test_key_12345"),
            (
                "Authorization",
                "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.test",
            ),
        ]);

        let result = try_authenticate(&headers, None, None, &state).await;

        assert!(result.is_err());
        assert!(matches!(result, Err(AuthError::AmbiguousCredentials)));
    }

    #[cfg(feature = "sso")]
    #[tokio::test]
    async fn test_idp_auth_custom_header_ambiguous_credentials_rejected() {
        // Ambiguous credentials check should respect custom header name
        let state = create_multi_auth_state("Api-Key", "hadrian_");
        let headers = make_headers(vec![
            ("Api-Key", "hadrian_test_key"),
            ("Authorization", "Bearer some.jwt.token"),
        ]);

        let result = try_authenticate(&headers, None, None, &state).await;

        assert!(result.is_err());
        assert!(matches!(result, Err(AuthError::AmbiguousCredentials)));
    }

    #[tokio::test]
    async fn test_non_idp_allows_both_headers() {
        // In non-idp mode (API key only), having both headers is not rejected
        // (Authorization header is simply ignored)
        let state = create_api_key_only_state("X-API-Key", "gw_");
        let headers = make_headers(vec![
            ("X-API-Key", "gw_test_key_12345"),
            ("Authorization", "Bearer some.jwt.token"),
        ]);

        // This won't return AmbiguousCredentials error
        // (will fail later due to missing DB, but that's expected)
        let result = try_authenticate(&headers, None, None, &state).await;

        // Should not be AmbiguousCredentials - it should be a different error
        // (InvalidApiKey since DB lookup fails)
        assert!(!matches!(result, Err(AuthError::AmbiguousCredentials)));
    }

    // ========== Format-based detection tests ==========

    #[tokio::test]
    async fn test_try_api_key_auth_bearer_with_prefix_extracts_key() {
        // Bearer token with API key prefix should be extracted by try_api_key_auth
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![("Authorization", "Bearer gw_test_key_12345")]);

        // This will fail at DB lookup, but should attempt to validate the key
        // (not return Ok(None) which would pass it to JWT handler)
        let result = try_api_key_auth(&headers, &state).await;

        // Should be an error (InvalidApiKey since no DB), not Ok(None)
        // Ok(None) would mean "not an API key, try JWT"
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.is_some()));
    }

    #[tokio::test]
    async fn test_try_api_key_auth_bearer_without_prefix_returns_none() {
        // Bearer token without API key prefix should return Ok(None)
        // to allow JWT handler to process it
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![(
            "Authorization",
            "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.test",
        )]);

        let result = try_api_key_auth(&headers, &state).await;

        // Should return Ok(None) - not an API key format, let JWT handler try
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_try_api_key_auth_custom_prefix_detection() {
        // Custom prefix should be respected for format-based detection
        let state = create_multi_auth_state("X-API-Key", "hadrian_");
        let headers = make_headers(vec![("Authorization", "Bearer hadrian_custom_key")]);

        // This will fail at DB lookup, but should attempt to validate the key
        let result = try_api_key_auth(&headers, &state).await;

        // Should be an error (no DB) or Some (if it got that far), not Ok(None)
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.is_some()));
    }

    #[tokio::test]
    async fn test_try_api_key_auth_default_prefix_not_matched() {
        // Bearer token with gw_ prefix should NOT be detected as API key
        // when a different prefix is configured
        let state = create_multi_auth_state("X-API-Key", "hadrian_");
        let headers = make_headers(vec![("Authorization", "Bearer gw_wrong_prefix")]);

        let result = try_api_key_auth(&headers, &state).await;

        // Should return Ok(None) - prefix doesn't match
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(feature = "sso")]
    #[tokio::test]
    async fn test_try_jwt_api_auth_skips_api_key_format() {
        // JWT handler should skip tokens that have API key prefix
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![("Authorization", "Bearer gw_test_api_key")]);

        let result = try_jwt_api_auth(&headers, None, &state).await;

        // Should return Ok(None) - has API key prefix, handled by API key auth
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_try_api_key_auth_x_api_key_header() {
        // X-API-Key header should be tried first (primary API key header)
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![("X-API-Key", "gw_test_key_from_header")]);

        // This will fail at DB lookup, but should attempt to validate
        let result = try_api_key_auth(&headers, &state).await;

        // Should attempt validation (error or Some), not return Ok(None)
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.is_some()));
    }

    #[tokio::test]
    async fn test_try_api_key_auth_custom_header_name() {
        // Custom header name should be respected
        let state = create_multi_auth_state("Api-Key", "gw_");
        let headers = make_headers(vec![("Api-Key", "gw_custom_header_key")]);

        // This will fail at DB lookup, but should attempt to validate
        let result = try_api_key_auth(&headers, &state).await;

        // Should attempt validation (error or Some), not return Ok(None)
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.is_some()));
    }

    #[tokio::test]
    async fn test_try_api_key_auth_wrong_header_returns_none() {
        // Using wrong header name should return None (no API key found)
        let state = create_multi_auth_state("Api-Key", "gw_");
        let headers = make_headers(vec![("X-API-Key", "gw_wrong_header")]);

        let result = try_api_key_auth(&headers, &state).await;

        // Should return Ok(None) - header not found
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // ========== Case insensitivity tests ==========

    #[tokio::test]
    async fn test_bearer_case_insensitive() {
        // "bearer" prefix should be case-insensitive
        let state = create_multi_auth_state("X-API-Key", "gw_");

        for bearer_case in ["Bearer", "bearer", "BEARER", "BeArEr"] {
            let auth_value = format!("{} gw_test_key", bearer_case);
            let headers = make_headers(vec![("Authorization", &auth_value)]);

            let result = try_api_key_auth(&headers, &state).await;

            // All cases should attempt validation (not return Ok(None))
            assert!(
                result.is_err() || result.as_ref().is_ok_and(|r| r.is_some()),
                "Bearer case '{}' should be accepted",
                bearer_case
            );
        }
    }

    #[tokio::test]
    async fn test_non_bearer_auth_returns_none() {
        // Non-bearer Authorization headers should return None
        let state = create_multi_auth_state("X-API-Key", "gw_");

        for auth_type in ["Basic", "Digest", "ApiKey"] {
            let auth_value = format!("{} gw_test_key", auth_type);
            let headers = make_headers(vec![("Authorization", &auth_value)]);

            let result = try_api_key_auth(&headers, &state).await;

            // Non-bearer should return Ok(None)
            assert!(result.is_ok(), "Auth type '{}' should not error", auth_type);
            assert!(
                result.unwrap().is_none(),
                "Auth type '{}' should return None",
                auth_type
            );
        }
    }

    // ========== Missing credentials tests ==========

    #[tokio::test]
    async fn test_multi_auth_no_credentials_returns_missing() {
        // No credentials at all should return MissingCredentials error
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![]);

        let result = try_authenticate(&headers, None, None, &state).await;

        assert!(result.is_err());
        assert!(matches!(result, Err(AuthError::MissingCredentials)));
    }

    #[tokio::test]
    async fn test_multi_auth_only_irrelevant_headers() {
        // Headers that don't match either auth method should return MissingCredentials
        let state = create_multi_auth_state("X-API-Key", "gw_");
        let headers = make_headers(vec![
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
        ]);

        let result = try_authenticate(&headers, None, None, &state).await;

        assert!(result.is_err());
        assert!(matches!(result, Err(AuthError::MissingCredentials)));
    }

    #[test]
    fn test_decode_jwt_issuer_valid() {
        // Build a JWT-like payload with iss claim: {"iss":"https://idp.acme.com","sub":"user1"}
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"iss":"https://idp.acme.com","sub":"user1"}"#);
        let token = format!("{header}.{payload}.fake_sig");

        assert_eq!(
            decode_jwt_issuer(&token),
            Some("https://idp.acme.com".to_string())
        );
    }

    #[test]
    fn test_decode_jwt_issuer_missing_iss() {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"sub":"user1"}"#);
        let token = format!("{header}.{payload}.sig");

        assert_eq!(decode_jwt_issuer(&token), None);
    }

    #[test]
    fn test_decode_jwt_issuer_invalid_token() {
        assert_eq!(decode_jwt_issuer("not-a-jwt"), None);
        assert_eq!(decode_jwt_issuer(""), None);
    }

    #[test]
    fn test_decode_jwt_issuer_padded() {
        // Some IdPs (e.g. older Azure AD) emit base64url with padding
        use base64::Engine;
        let header =
            base64::engine::general_purpose::URL_SAFE.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE
            .encode(r#"{"iss":"https://login.microsoftonline.com/tenant","sub":"user1"}"#);
        let token = format!("{header}.{payload}.fake_sig");

        assert_eq!(
            decode_jwt_issuer(&token),
            Some("https://login.microsoftonline.com/tenant".to_string())
        );
    }
}
