//! Provider fallback logic for handling failures with alternative providers.
//!
//! This module provides error classification to determine whether a failed request
//! should be retried with a fallback provider or returned immediately.
//!
//! # Fallback Strategy
//!
//! 1. Try primary model on primary provider
//! 2. On retryable failure, try model-specific fallbacks in order (if configured)
//! 3. If all model fallbacks fail (or none configured), try provider-level fallbacks
//! 4. Each provider fallback uses the originally requested model (with its own model fallbacks)
//!
//! # Error Classification
//!
//! **Retryable errors** (should try fallback):
//! - Circuit breaker open
//! - 5xx server errors (500, 502, 503, 504)
//! - Connection errors (network unreachable, connection refused)
//! - Timeouts
//!
//! **Non-retryable errors** (return immediately):
//! - 4xx client errors (bad request, validation errors)
//! - 401 Unauthorized / 403 Forbidden (authentication/authorization failures)
//! - 429 Too Many Requests (rate limiting is provider-specific, not our issue)
//! - Successful responses (even with unexpected content)

use http::StatusCode;

use super::ProviderError;

/// Result of classifying an error for fallback purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackDecision {
    /// Error is retryable - try next fallback provider/model.
    Retry,
    /// Error is not retryable - return immediately to client.
    NoRetry,
}

/// Classifies a `ProviderError` to determine if fallback should be attempted.
///
/// # Arguments
///
/// * `error` - The provider error to classify
///
/// # Returns
///
/// * `FallbackDecision::Retry` - The error is transient and fallback should be tried
/// * `FallbackDecision::NoRetry` - The error is permanent and should be returned to client
pub fn classify_provider_error(error: &ProviderError) -> FallbackDecision {
    match error {
        // Circuit breaker open - definitely retry with fallback
        ProviderError::CircuitBreakerOpen(_) => FallbackDecision::Retry,

        // HTTP request errors - check the underlying cause
        ProviderError::Request(reqwest_err) => classify_reqwest_error(reqwest_err),

        // Response builder errors are internal issues - retry might help if it's a transient issue
        ProviderError::ResponseBuilder(_) => FallbackDecision::Retry,

        // Internal errors are typically programming errors - don't retry
        ProviderError::Internal(_) => FallbackDecision::NoRetry,

        // Unsupported operations won't succeed on another provider of
        // the same type — bail out instead of cycling fallbacks.
        ProviderError::Unsupported(_) => FallbackDecision::NoRetry,

        // BadGateway covers gateway-side dependency failures (e.g. an
        // MCP server returning 503 during tools/list rewrite). Treat
        // like a transient upstream error and let the fallback chain
        // try the next provider — the next attempt re-runs the rewrite,
        // and if the dependency comes back the request can succeed.
        ProviderError::BadGateway(_, _) => FallbackDecision::Retry,

        // BadRequest is a caller-side problem detected in a pipeline
        // step (e.g. ambiguous MCP `tool_choice`). Retrying a different
        // provider won't help — the request itself is malformed.
        ProviderError::BadRequest(_, _) => FallbackDecision::NoRetry,
    }
}

/// Classifies a `reqwest::Error` for fallback purposes.
fn classify_reqwest_error(error: &reqwest::Error) -> FallbackDecision {
    // Connection errors are retryable - different provider might be reachable
    #[cfg(not(target_arch = "wasm32"))]
    if error.is_connect() {
        return FallbackDecision::Retry;
    }

    // Timeouts are retryable - different provider might respond faster
    if error.is_timeout() {
        return FallbackDecision::Retry;
    }

    // Request errors (failed to build/send) might be transient
    if error.is_request() {
        return FallbackDecision::Retry;
    }

    // Body errors during streaming might be transient network issues
    if error.is_body() {
        return FallbackDecision::Retry;
    }

    // If we got an HTTP status, classify based on the status code
    if let Some(status) = error.status() {
        return classify_http_status(status);
    }

    // Unknown error type - be conservative and retry
    FallbackDecision::Retry
}

/// Classifies an HTTP status code from a provider response for fallback purposes.
///
/// This is used both for reqwest errors with status codes and for successful
/// HTTP responses that contain error status codes (the provider returned an error).
///
/// # Arguments
///
/// * `status` - The HTTP status code
///
/// # Returns
///
/// * `FallbackDecision::Retry` - Server errors (5xx) should trigger fallback
/// * `FallbackDecision::NoRetry` - Client errors (4xx) should not trigger fallback
pub fn classify_http_status(status: StatusCode) -> FallbackDecision {
    // 5xx server errors are retryable - the provider is having issues
    if status.is_server_error() {
        return FallbackDecision::Retry;
    }

    // 4xx client errors are generally not retryable - the request is bad
    // and sending it to a different provider won't help
    if status.is_client_error() {
        return FallbackDecision::NoRetry;
    }

    // 2xx success - no fallback needed
    if status.is_success() {
        return FallbackDecision::NoRetry;
    }

    // 3xx redirects - shouldn't happen with API calls, but don't retry
    if status.is_redirection() {
        return FallbackDecision::NoRetry;
    }

    // 1xx informational - shouldn't happen, don't retry
    FallbackDecision::NoRetry
}

/// Checks if a response indicates an error that should trigger fallback.
///
/// This examines the HTTP status code of a successful HTTP response
/// (i.e., we got a response from the provider, but it might be an error response).
///
/// # Arguments
///
/// * `status` - The HTTP status code from the provider response
///
/// # Returns
///
/// * `true` - The response status indicates a retryable error
/// * `false` - The response is successful or has a non-retryable error
#[allow(dead_code)] // Useful for checking response status in future enhancements
pub fn should_fallback_on_response_status(status: StatusCode) -> bool {
    classify_http_status(status) == FallbackDecision::Retry
}

/// A target for fallback: a provider name and model name.
#[derive(Debug, Clone)]
pub struct FallbackTarget {
    /// Name of the provider to use.
    pub provider_name: String,
    /// Model name to use with this provider.
    pub model_name: String,
}

/// Hard cap on the number of fallback targets we'll try for a single request.
///
/// Without a cap, a misconfiguration where every provider lists every other
/// provider as a fallback can produce a very long chain (latency budget eaten
/// + amplified upstream pressure if many of them fail). 8 is generous in
///   practice — Hadrian's documented examples top out at 3-4.
pub const MAX_FALLBACK_CHAIN_LENGTH: usize = 8;

/// Builds the fallback chain for a request.
///
/// The chain is built in this order:
/// 1. Model-specific fallbacks (if any) - tried first
/// 2. Provider-level fallbacks - tried after model fallbacks are exhausted
///
/// `(provider, model)` pairs are deduplicated against the primary and against
/// each other so we never call the same target twice in a row, and the chain
/// is capped at `MAX_FALLBACK_CHAIN_LENGTH` entries.
///
/// # Arguments
///
/// * `primary_provider_name` - Name of the primary provider
/// * `primary_model_name` - Name of the model being requested
/// * `providers_config` - All provider configurations
///
/// # Returns
///
/// A vector of fallback targets to try in order. Does NOT include the primary provider.
pub fn build_fallback_chain(
    primary_provider_name: &str,
    primary_model_name: &str,
    providers_config: &crate::config::ProvidersConfig,
) -> Vec<FallbackTarget> {
    let mut chain = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    // Seed with the primary so we never retry the same (provider, model)
    // pair via a redundant model_fallbacks entry.
    seen.insert((
        primary_provider_name.to_string(),
        primary_model_name.to_string(),
    ));

    // Get the primary provider config
    let Some(primary_config) = providers_config.get(primary_provider_name) else {
        return chain;
    };

    let push_target = |chain: &mut Vec<FallbackTarget>,
                       seen: &mut std::collections::HashSet<(String, String)>,
                       provider: String,
                       model: String|
     -> bool {
        if chain.len() >= MAX_FALLBACK_CHAIN_LENGTH {
            tracing::warn!(
                cap = MAX_FALLBACK_CHAIN_LENGTH,
                "Fallback chain hit the per-request length cap; dropping further entries"
            );
            return false;
        }
        if !seen.insert((provider.clone(), model.clone())) {
            tracing::debug!(
                provider = %provider,
                model = %model,
                "Skipping duplicate fallback target"
            );
            return true;
        }
        chain.push(FallbackTarget {
            provider_name: provider,
            model_name: model,
        });
        true
    };

    // 1. Add model-specific fallbacks first
    if let Some(model_fallbacks) = primary_config.get_model_fallbacks(primary_model_name) {
        for fallback in model_fallbacks {
            let target_provider = fallback
                .provider
                .as_deref()
                .unwrap_or(primary_provider_name);

            // Skip if fallback provider doesn't exist
            if providers_config.get(target_provider).is_none() {
                tracing::warn!(
                    provider = target_provider,
                    model = %fallback.model,
                    "Skipping model fallback: provider not found"
                );
                continue;
            }

            if !push_target(
                &mut chain,
                &mut seen,
                target_provider.to_string(),
                fallback.model.clone(),
            ) {
                return chain;
            }
        }
    }

    // 2. Add provider-level fallbacks
    for fallback_provider_name in primary_config.fallback_providers() {
        // Skip if fallback provider doesn't exist
        if providers_config.get(fallback_provider_name).is_none() {
            tracing::warn!(
                provider = fallback_provider_name,
                "Skipping provider fallback: provider not found"
            );
            continue;
        }

        if !push_target(
            &mut chain,
            &mut seen,
            fallback_provider_name.clone(),
            // Use the original model name for provider fallbacks
            primary_model_name.to_string(),
        ) {
            return chain;
        }
    }

    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_http_status_5xx() {
        // All 5xx errors should trigger fallback
        assert_eq!(
            classify_http_status(StatusCode::INTERNAL_SERVER_ERROR),
            FallbackDecision::Retry
        );
        assert_eq!(
            classify_http_status(StatusCode::BAD_GATEWAY),
            FallbackDecision::Retry
        );
        assert_eq!(
            classify_http_status(StatusCode::SERVICE_UNAVAILABLE),
            FallbackDecision::Retry
        );
        assert_eq!(
            classify_http_status(StatusCode::GATEWAY_TIMEOUT),
            FallbackDecision::Retry
        );
        assert_eq!(
            classify_http_status(StatusCode::HTTP_VERSION_NOT_SUPPORTED),
            FallbackDecision::Retry
        );
    }

    #[test]
    fn test_classify_http_status_4xx() {
        // 4xx errors should NOT trigger fallback
        assert_eq!(
            classify_http_status(StatusCode::BAD_REQUEST),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::UNAUTHORIZED),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::FORBIDDEN),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::NOT_FOUND),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::TOO_MANY_REQUESTS),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::UNPROCESSABLE_ENTITY),
            FallbackDecision::NoRetry
        );
    }

    #[test]
    fn test_classify_http_status_2xx() {
        // Success should NOT trigger fallback
        assert_eq!(
            classify_http_status(StatusCode::OK),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::CREATED),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::ACCEPTED),
            FallbackDecision::NoRetry
        );
    }

    #[test]
    fn test_classify_http_status_3xx() {
        // Redirects should NOT trigger fallback
        assert_eq!(
            classify_http_status(StatusCode::MOVED_PERMANENTLY),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::FOUND),
            FallbackDecision::NoRetry
        );
        assert_eq!(
            classify_http_status(StatusCode::TEMPORARY_REDIRECT),
            FallbackDecision::NoRetry
        );
    }

    #[test]
    fn test_classify_provider_error_circuit_breaker() {
        use crate::providers::circuit_breaker::CircuitBreakerError;

        let error = ProviderError::CircuitBreakerOpen(CircuitBreakerError::Open {
            provider: "test".into(),
            retry_after_secs: 30,
        });

        assert_eq!(classify_provider_error(&error), FallbackDecision::Retry);
    }

    #[test]
    fn test_classify_provider_error_internal() {
        let error = ProviderError::Internal("programming error".to_string());
        assert_eq!(classify_provider_error(&error), FallbackDecision::NoRetry);
    }

    #[test]
    fn test_classify_provider_error_response_builder() {
        // http::Error doesn't have a public constructor, so we can't test this directly
        // The implementation returns Retry for response builder errors
    }

    #[test]
    fn test_should_fallback_on_response_status() {
        // 5xx should fallback
        assert!(should_fallback_on_response_status(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(should_fallback_on_response_status(StatusCode::BAD_GATEWAY));
        assert!(should_fallback_on_response_status(
            StatusCode::SERVICE_UNAVAILABLE
        ));

        // 4xx should NOT fallback
        assert!(!should_fallback_on_response_status(StatusCode::BAD_REQUEST));
        assert!(!should_fallback_on_response_status(
            StatusCode::UNAUTHORIZED
        ));
        assert!(!should_fallback_on_response_status(
            StatusCode::TOO_MANY_REQUESTS
        ));

        // 2xx should NOT fallback
        assert!(!should_fallback_on_response_status(StatusCode::OK));
    }

    #[test]
    fn test_build_fallback_chain_empty() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            model_name = "test-model"
        "#,
        )
        .unwrap();

        // No fallbacks configured
        let chain = build_fallback_chain("primary", "test-model", &config);
        assert!(chain.is_empty());
    }

    #[test]
    fn test_build_fallback_chain_model_fallbacks() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            model_name = "test-model"

            [primary.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" },
                { model = "gpt-4-turbo" }
            ]
        "#,
        )
        .unwrap();

        let chain = build_fallback_chain("primary", "gpt-4o", &config);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].provider_name, "primary");
        assert_eq!(chain[0].model_name, "gpt-4o-mini");
        assert_eq!(chain[1].provider_name, "primary");
        assert_eq!(chain[1].model_name, "gpt-4-turbo");
    }

    #[test]
    fn test_build_fallback_chain_provider_fallbacks() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        )
        .unwrap();

        let chain = build_fallback_chain("primary", "test-model", &config);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].provider_name, "backup");
        assert_eq!(chain[0].model_name, "test-model"); // Original model name preserved
    }

    #[test]
    fn test_build_fallback_chain_combined() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            fallback_providers = ["backup"]

            [primary.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" },
                { provider = "backup", model = "claude-sonnet" }
            ]

            [backup]
            type = "test"
        "#,
        )
        .unwrap();

        let chain = build_fallback_chain("primary", "gpt-4o", &config);
        // Order: model fallbacks first, then provider fallbacks
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].provider_name, "primary");
        assert_eq!(chain[0].model_name, "gpt-4o-mini");
        assert_eq!(chain[1].provider_name, "backup");
        assert_eq!(chain[1].model_name, "claude-sonnet");
        assert_eq!(chain[2].provider_name, "backup");
        assert_eq!(chain[2].model_name, "gpt-4o"); // Original model for provider fallback
    }

    #[test]
    fn test_build_fallback_chain_nonexistent_provider() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
        "#,
        )
        .unwrap();

        // Provider doesn't exist
        let chain = build_fallback_chain("nonexistent", "test-model", &config);
        assert!(chain.is_empty());
    }

    #[test]
    fn test_build_fallback_chain_dedupes_pairs() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            fallback_providers = ["backup", "backup"]

            [primary.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" },
                { model = "gpt-4o-mini" },
                { provider = "backup", model = "gpt-4o" },
            ]

            [backup]
            type = "test"
        "#,
        )
        .unwrap();

        let chain = build_fallback_chain("primary", "gpt-4o", &config);
        // Expected (post-dedup): primary/gpt-4o-mini, backup/gpt-4o (from
        // model_fallbacks). The duplicate model entry is dropped, the second
        // `backup` provider entry collides with the model_fallbacks entry, and
        // the (primary, gpt-4o) pair is the seeded primary.
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].provider_name, "primary");
        assert_eq!(chain[0].model_name, "gpt-4o-mini");
        assert_eq!(chain[1].provider_name, "backup");
        assert_eq!(chain[1].model_name, "gpt-4o");
    }

    #[test]
    fn test_build_fallback_chain_caps_length() {
        // Construct a primary with more model fallbacks than the cap allows.
        let mut toml = String::from(
            r#"
            [primary]
            type = "test"

            [primary.model_fallbacks]
            "gpt-4o" = [
            "#,
        );
        for i in 0..(MAX_FALLBACK_CHAIN_LENGTH + 5) {
            toml.push_str(&format!("                {{ model = \"m{}\" }},\n", i));
        }
        toml.push_str("            ]\n");

        let config: crate::config::ProvidersConfig = toml::from_str(&toml).unwrap();
        let chain = build_fallback_chain("primary", "gpt-4o", &config);
        assert_eq!(chain.len(), MAX_FALLBACK_CHAIN_LENGTH);
    }

    #[test]
    fn test_build_fallback_chain_no_model_match() {
        let config: crate::config::ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            fallback_providers = ["backup"]

            [primary.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" }
            ]

            [backup]
            type = "test"
        "#,
        )
        .unwrap();

        // Request different model - no model fallbacks, only provider fallback
        let chain = build_fallback_chain("primary", "other-model", &config);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].provider_name, "backup");
        assert_eq!(chain[0].model_name, "other-model");
    }
}
