use std::time::Instant;

use axum::response::Response;

/// Extension to track usage metrics for a request
#[derive(Debug, Clone)]
pub struct UsageTracker {
    pub start_time: Instant,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub referer: Option<String>,
    /// Whether this is a streaming request
    pub streamed: bool,
    /// Whether the provider is "static" (config) or "dynamic" (DB)
    pub provider_source: Option<String>,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            model: None,
            provider: None,
            referer: None,
            streamed: false,
            provider_source: None,
        }
    }

    #[allow(dead_code)] // Used in tests; builder pattern for UsageTracker
    pub fn with_model(mut self, model: String) -> Self {
        self.model = Some(model);
        self
    }

    #[allow(dead_code)] // Used in tests; builder pattern for UsageTracker
    pub fn with_provider(mut self, provider: String) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_referer(mut self, referer: String) -> Self {
        self.referer = Some(referer);
        self
    }

    #[allow(dead_code)] // Used in tests; builder pattern for UsageTracker
    pub fn with_streamed(mut self, streamed: bool) -> Self {
        self.streamed = streamed;
        self
    }

    #[allow(dead_code)] // Available for non-header-based provider source tracking
    pub fn with_provider_source(mut self, source: &str) -> Self {
        self.provider_source = Some(source.to_string());
        self
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracted usage information from response headers
#[derive(Debug, Clone, Default)]
pub struct ExtractedUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost_microcents: Option<i64>,
    pub pricing_source: crate::pricing::CostPricingSource,
    pub image_count: Option<i32>,
    pub audio_seconds: Option<i32>,
    pub character_count: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub reasoning_tokens: Option<i32>,
    pub finish_reason: Option<String>,
}

/// Extract usage information from response headers
pub fn extract_full_usage_from_response(response: &Response) -> ExtractedUsage {
    let headers = response.headers();

    let input_tokens = headers
        .get("X-Input-Tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let output_tokens = headers
        .get("X-Output-Tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    // Cost in microcents (1/1,000,000 of a dollar)
    let cost_microcents = headers
        .get("X-Cost-Microcents")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let pricing_source = headers
        .get("X-Pricing-Source")
        .and_then(|v| v.to_str().ok())
        .map(crate::pricing::CostPricingSource::parse)
        .unwrap_or_default();

    let image_count = headers
        .get("X-Image-Count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let audio_seconds = headers
        .get("X-Audio-Seconds")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let character_count = headers
        .get("X-Character-Count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let cached_tokens = headers
        .get("X-Cached-Tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let reasoning_tokens = headers
        .get("X-Reasoning-Tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let finish_reason = headers
        .get("X-Finish-Reason")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    ExtractedUsage {
        input_tokens,
        output_tokens,
        cost_microcents,
        pricing_source,
        image_count,
        audio_seconds,
        character_count,
        cached_tokens,
        reasoning_tokens,
        finish_reason,
    }
}

/// Helper to create a usage tracker from request headers
/// Extract the client-supplied request correlation id, if any.
///
/// Honors `X-Request-Id` (the header the request-id middleware echoes) and
/// OpenAI's `X-Client-Request-Id`. The value is stored on usage rows purely
/// for correlation; it never keys them.
pub fn client_request_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("X-Request-Id")
        .or_else(|| headers.get("X-Client-Request-Id"))
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

pub fn tracker_from_headers(headers: &axum::http::HeaderMap) -> UsageTracker {
    let referer = headers
        .get("referer")
        .and_then(|h| h.to_str().ok())
        .map(String::from);

    let mut tracker = UsageTracker::new();
    if let Some(referer) = referer {
        tracker = tracker.with_referer(referer);
    }
    tracker
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_tracker_creation() {
        let tracker = UsageTracker::new();
        assert!(tracker.model.is_none());
        assert!(tracker.provider.is_none());
        assert!(tracker.referer.is_none());
        assert!(!tracker.streamed);
    }

    #[test]
    fn test_usage_tracker_with_fields() {
        let tracker = UsageTracker::new()
            .with_model("gpt-4".to_string())
            .with_provider("openai".to_string())
            .with_referer("https://example.com".to_string())
            .with_streamed(true);

        assert_eq!(tracker.model, Some("gpt-4".to_string()));
        assert_eq!(tracker.provider, Some("openai".to_string()));
        assert_eq!(tracker.referer, Some("https://example.com".to_string()));
        assert!(tracker.streamed);
    }

    #[test]
    fn test_usage_tracker_with_streamed() {
        // Default is false
        let tracker = UsageTracker::new();
        assert!(!tracker.streamed);

        // Can set to true
        let tracker = UsageTracker::new().with_streamed(true);
        assert!(tracker.streamed);

        // Can explicitly set to false
        let tracker = UsageTracker::new().with_streamed(false);
        assert!(!tracker.streamed);

        // Can toggle from true to false
        let tracker = UsageTracker::new().with_streamed(true).with_streamed(false);
        assert!(!tracker.streamed);
    }
}
