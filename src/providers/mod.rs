//! # Provider Implementations
//!
//! This module contains implementations for various LLM provider backends.
//!
//! ## Retry Loop Optimization Pattern
//!
//! When implementing provider methods that use `with_circuit_breaker_and_retry`,
//! follow this pattern to avoid repeated JSON serialization on each retry attempt:
//!
//! ```ignore
//! // 1. Build the request object
//! let request = ProviderRequest { ... };
//!
//! // 2. Pre-serialize BEFORE the retry loop
//! let body = serde_json::to_vec(&request).unwrap_or_default();
//!
//! // 3. Inside the retry closure, use .body() instead of .json()
//! let response = with_circuit_breaker_and_retry(
//!     ...,
//!     || async {
//!         client
//!             .post(&url)
//!             .header("content-type", "application/json")
//!             .body(body.clone())  // Clone bytes, don't re-serialize
//!             .send()
//!             .await
//!     },
//! ).await?;
//! ```
//!
//! This pattern is used consistently across Anthropic, Bedrock, Vertex, and Azure OpenAI
//! providers. Cloning `Vec<u8>` is much cheaper than re-serializing the request struct
//! on each retry attempt.
//!
//! For multipart form requests (like OpenAI image/audio endpoints), pre-serialize
//! enum values and other derived strings before the retry loop, as forms must be
//! rebuilt fresh on each attempt (they are consumed when sent).

pub mod anthropic;
#[cfg(feature = "provider-bedrock")]
pub mod aws;
#[cfg(feature = "provider-azure")]
pub mod azure_openai;
#[cfg(feature = "provider-bedrock")]
pub mod bedrock;
pub mod circuit_breaker;
pub(crate) mod convert_utils;
pub mod error;
pub mod fallback;
pub mod health_check;
pub mod image;
pub(crate) mod open_ai;
pub mod registry;
pub mod response;
pub mod retry;
pub mod test;
#[cfg(test)]
pub mod test_utils;
#[cfg(feature = "provider-vertex")]
pub mod vertex;

use async_trait::async_trait;
use axum::{
    body::Body,
    response::{IntoResponse, Response},
};
use bytes::Bytes;
pub use fallback::{
    FallbackDecision, build_fallback_chain, classify_provider_error,
    should_fallback_on_response_status,
};
use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
};
pub use registry::{CircuitBreakerRegistry, CircuitBreakerStatus};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "server")]
use tokio_util::task::TaskTracker;

use crate::{
    api_types::{
        CreateChatCompletionPayload, CreateCompletionPayload, CreateEmbeddingPayload,
        CreateImageRequest, CreateResponsesPayload, CreateSpeechRequest,
        CreateTranscriptionRequest, CreateTranslationRequest,
        images::{CreateImageEditRequest, CreateImageVariationRequest, ImagesResponse},
    },
    config::{ResponseValidationConfig, ResponseValidationMode},
    observability::metrics,
    validation::{ResponseType, SchemaId, validate_response},
};

/// Normalize a tool call ID for Anthropic/Bedrock compatibility.
///
/// - Strips pipe-separated format (keeps first part before `|`)
/// - Removes characters outside `[a-zA-Z0-9_-]`
/// - Truncates to 64 chars
/// - Falls back to a generated ID if the result is empty
pub fn normalize_tool_call_id(id: &str) -> String {
    // Take first segment before '|'
    let base = id.split('|').next().unwrap_or(id);

    // Remove invalid characters
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();

    if cleaned.is_empty() {
        format!("call_{}", uuid::Uuid::new_v4().simple())
    } else {
        cleaned
    }
}

/// Parameters for injecting cost calculation into a response
pub struct CostInjectionParams<'a> {
    pub response: Response,
    pub provider: &'a str,
    pub model: &'a str,
    pub pricing: &'a crate::pricing::PricingConfig,
    pub db: Option<&'a std::sync::Arc<crate::db::DbPool>>,
    pub usage_entry: Option<crate::models::UsageLogEntry>,
    #[cfg(feature = "server")]
    pub task_tracker: Option<&'a TaskTracker>,
    /// Handle to the usage-drain channel; used by `UsageTrackingStream` to
    /// log partial usage from `Drop` without spawning a task there directly.
    #[cfg(feature = "server")]
    pub usage_drain: Option<&'a crate::streaming::UsageDrainHandle>,
    pub max_response_body_bytes: usize,
    /// Idle timeout for streaming responses in seconds.
    /// If a streaming response doesn't receive a chunk within this timeout,
    /// the stream is terminated. Set to 0 to disable.
    pub streaming_idle_timeout_secs: u64,
    /// Response validation configuration.
    pub validation_config: &'a ResponseValidationConfig,
    /// Type of response for schema validation.
    pub response_type: ResponseType,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Failed to build response: {0}")]
    ResponseBuilder(#[from] http::Error),

    #[error("Internal provider error: {0}")]
    Internal(String),

    /// The provider does not implement the requested operation. Maps to
    /// HTTP 501 with `error_code = "not_supported"` so clients can
    /// distinguish from generic provider errors.
    #[error("{0}")]
    Unsupported(String),

    /// An upstream the gateway depends on (e.g. a remote MCP server
    /// during the `tools/list` rewrite, or a downstream sandbox API)
    /// failed in a caller-visible way. Maps to HTTP 502 with the
    /// supplied `(error_code, message)` so clients can distinguish
    /// "the gateway tried but the dependency was unreachable" from
    /// generic 500s. Use only for errors where exposing the message
    /// won't leak internal infrastructure detail.
    #[error("{1}")]
    BadGateway(&'static str, String),

    /// The caller's request is malformed in a way that pipeline steps
    /// only detect once they have context the route layer doesn't
    /// (e.g. which MCP tools survive the `allowed_tools` filter at
    /// rewrite time). Maps to HTTP 400 with the supplied
    /// `(error_code, message)`.
    #[error("{1}")]
    BadRequest(&'static str, String),

    #[error("{0}")]
    CircuitBreakerOpen(#[from] circuit_breaker::CircuitBreakerError),
}

impl From<ProviderError> for StatusCode {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Request(_) | ProviderError::BadGateway(_, _) => StatusCode::BAD_GATEWAY,
            ProviderError::ResponseBuilder(_) | ProviderError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ProviderError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            ProviderError::BadRequest(_, _) => StatusCode::BAD_REQUEST,
            ProviderError::CircuitBreakerOpen(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl IntoResponse for ProviderError {
    fn into_response(self) -> Response {
        // CircuitBreakerOpen is a curated message we own (no upstream detail
        // mixed in), so it's safe to expose. The other variants wrap reqwest
        // / http / arbitrary internal strings that may include hostnames,
        // file paths, or stack-trace fragments — keep those in logs only.
        let (status, error_code, public_message) = match &self {
            ProviderError::Request(_) => (
                StatusCode::BAD_GATEWAY,
                "request_failed",
                "Upstream provider request failed".to_string(),
            ),
            ProviderError::ResponseBuilder(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_builder",
                "Failed to build response".to_string(),
            ),
            ProviderError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Internal provider error".to_string(),
            ),
            ProviderError::Unsupported(msg) => {
                (StatusCode::NOT_IMPLEMENTED, "not_supported", msg.clone())
            }
            ProviderError::BadGateway(code, msg) => (StatusCode::BAD_GATEWAY, *code, msg.clone()),
            ProviderError::BadRequest(code, msg) => (StatusCode::BAD_REQUEST, *code, msg.clone()),
            ProviderError::CircuitBreakerOpen(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "circuit_breaker_open",
                e.to_string(),
            ),
        };

        tracing::error!(
            error_code = %error_code,
            error = %self,
            "Provider error returned to client"
        );

        // Record provider error metric
        // Note: Provider name is tracked via llm_requests_total with status="error"
        // This counter provides unified error categorization across all error types
        metrics::record_gateway_error("provider_error", error_code, None);

        (status, public_message).into_response()
    }
}

impl From<retry::ProviderRequestError> for ProviderError {
    fn from(err: retry::ProviderRequestError) -> Self {
        match err {
            retry::ProviderRequestError::CircuitBreakerOpen(e) => {
                ProviderError::CircuitBreakerOpen(e)
            }
            retry::ProviderRequestError::Request(e) => ProviderError::Request(e),
        }
    }
}

/// Trait for LLM provider implementations.
///
/// All methods receive a shared `&reqwest::Client` reference. The client is created once
/// at startup and shared across all providers. This works well because reqwest maintains
/// per-host connection pools internally, so each provider endpoint gets its own pool.
/// See [`crate::config::HttpClientConfig`] for connection pool tuning options.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait Provider: Send + Sync {
    async fn create_chat_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateChatCompletionPayload,
    ) -> Result<Response, ProviderError>;

    async fn create_responses(
        &self,
        client: &reqwest::Client,
        payload: CreateResponsesPayload,
    ) -> Result<Response, ProviderError>;

    /// Compact a context window via the provider's standalone compact
    /// endpoint. Only OpenAI-compatible providers implement this; the
    /// default returns `Unsupported` so non-OpenAI providers surface a
    /// clean 501 to the caller.
    async fn create_responses_compact(
        &self,
        _client: &reqwest::Client,
        _payload: crate::api_types::CompactRequest,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Unsupported(
            "compaction is not supported by this provider".to_string(),
        ))
    }

    async fn create_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateCompletionPayload,
    ) -> Result<Response, ProviderError>;

    async fn create_embedding(
        &self,
        client: &reqwest::Client,
        payload: CreateEmbeddingPayload,
    ) -> Result<Response, ProviderError>;

    async fn list_models(&self, client: &reqwest::Client) -> Result<ModelsResponse, ProviderError>;

    // =========================================================================
    // Image generation methods
    // =========================================================================

    /// Generate images from a text prompt.
    async fn create_image(
        &self,
        _client: &reqwest::Client,
        _payload: CreateImageRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support image generation".to_string(),
        ))
    }

    /// Edit an image given an original image and a prompt.
    ///
    /// # Arguments
    /// * `image` - The original image bytes (PNG, max 4MB)
    /// * `mask` - Optional mask image bytes indicating areas to edit
    /// * `request` - The edit request parameters
    async fn create_image_edit(
        &self,
        _client: &reqwest::Client,
        _image: Bytes,
        _mask: Option<Bytes>,
        _request: CreateImageEditRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support image editing".to_string(),
        ))
    }

    /// Create variations of an image.
    ///
    /// # Arguments
    /// * `image` - The original image bytes (PNG, max 4MB)
    /// * `request` - The variation request parameters
    async fn create_image_variation(
        &self,
        _client: &reqwest::Client,
        _image: Bytes,
        _request: CreateImageVariationRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support image variations".to_string(),
        ))
    }

    // =========================================================================
    // Audio methods
    // =========================================================================

    /// Generate audio from text (text-to-speech).
    ///
    /// Returns raw audio bytes in the requested format (mp3, opus, etc.).
    async fn create_speech(
        &self,
        _client: &reqwest::Client,
        _payload: CreateSpeechRequest,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support text-to-speech".to_string(),
        ))
    }

    /// Transcribe audio to text.
    ///
    /// # Arguments
    /// * `file` - The audio file bytes
    /// * `filename` - Original filename (used to determine format)
    /// * `request` - The transcription request parameters
    async fn create_transcription(
        &self,
        _client: &reqwest::Client,
        _file: Bytes,
        _filename: String,
        _request: CreateTranscriptionRequest,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support audio transcription".to_string(),
        ))
    }

    /// Translate audio to English text.
    ///
    /// # Arguments
    /// * `file` - The audio file bytes
    /// * `filename` - Original filename (used to determine format)
    /// * `request` - The translation request parameters
    async fn create_translation(
        &self,
        _client: &reqwest::Client,
        _file: Bytes,
        _filename: String,
        _request: CreateTranslationRequest,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "This provider does not support audio translation".to_string(),
        ))
    }

    // =========================================================================
    // Health check methods
    // =========================================================================

    /// Returns a provider-specific default model for inference health checks.
    ///
    /// Override in provider implementations to avoid requiring explicit
    /// `model` configuration for health checks. Returns `None` by default,
    /// which requires the user to configure a model explicitly.
    fn default_health_check_model(&self) -> Option<&str> {
        None
    }

    /// Perform a health check on the provider.
    ///
    /// The health check mode determines what kind of check is performed:
    /// - **Reachability**: Calls `list_models()` to verify connectivity and auth
    /// - **Inference**: Sends a minimal chat completion request
    ///
    /// Returns a `HealthCheckResult` indicating success or failure with latency.
    async fn health_check(
        &self,
        client: &reqwest::Client,
        config: &health_check::ProviderHealthCheckConfig,
    ) -> health_check::HealthCheckResult {
        use health_check::{HealthCheckResult, ProviderHealthCheckMode};

        let start = std::time::Instant::now();

        match config.mode {
            ProviderHealthCheckMode::Reachability => {
                // Use list_models as a cheap connectivity check
                match self.list_models(client).await {
                    Ok(_) => HealthCheckResult::healthy(start.elapsed().as_millis() as u64, 200),
                    Err(e) => HealthCheckResult::unhealthy(
                        start.elapsed().as_millis() as u64,
                        e.to_string(),
                        None,
                    ),
                }
            }
            ProviderHealthCheckMode::Inference => {
                use crate::api_types::chat_completion::{Message, MessageContent};

                let model = match config
                    .model
                    .as_deref()
                    .or_else(|| self.default_health_check_model())
                {
                    Some(m) => m,
                    None => {
                        tracing::warn!(
                            "No health check model configured and provider has no default; configure [providers.<name>.health_check.model]"
                        );
                        return HealthCheckResult::unhealthy(
                            start.elapsed().as_millis() as u64,
                            "No health check model configured".to_string(),
                            None,
                        );
                    }
                };
                let prompt = config.prompt();

                let payload = CreateChatCompletionPayload {
                    messages: vec![Message::User {
                        content: MessageContent::Text(prompt.to_string()),
                        name: None,
                    }],
                    model: Some(model.to_string()),
                    models: None,
                    max_tokens: Some(5), // Minimal tokens to reduce cost
                    max_completion_tokens: None,
                    temperature: None,
                    top_p: None,
                    stream: false,
                    stop: None,
                    presence_penalty: None,
                    frequency_penalty: None,
                    logit_bias: None,
                    user: None,
                    seed: None,
                    tools: None,
                    tool_choice: None,
                    response_format: None,
                    logprobs: None,
                    top_logprobs: None,
                    stream_options: None,
                    metadata: None,
                    reasoning: None,
                    sovereignty_requirements: None,
                };

                match self.create_chat_completion(client, payload).await {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        if response.status().is_success() {
                            HealthCheckResult::healthy(start.elapsed().as_millis() as u64, status)
                        } else {
                            HealthCheckResult::unhealthy(
                                start.elapsed().as_millis() as u64,
                                format!("HTTP {}", status),
                                Some(status),
                            )
                        }
                    }
                    Err(e) => HealthCheckResult::unhealthy(
                        start.elapsed().as_millis() as u64,
                        e.to_string(),
                        None,
                    ),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// List models for a resolved `ProviderConfig`.
///
/// Dispatches to the correct provider implementation's `list_models` method.
/// Used by both the `/v1/models` endpoint and connectivity tests.
pub async fn list_models_for_config(
    config: &crate::config::ProviderConfig,
    provider_name: &str,
    http_client: &reqwest::Client,
    circuit_breakers: &CircuitBreakerRegistry,
) -> Result<ModelsResponse, ProviderError> {
    use crate::config::ProviderConfig;
    match config {
        ProviderConfig::OpenAi(c) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                c,
                provider_name,
                circuit_breakers,
            )
            .list_models(http_client)
            .await
        }
        ProviderConfig::Anthropic(c) => {
            anthropic::AnthropicProvider::from_config_with_registry(
                c,
                provider_name,
                circuit_breakers,
            )
            .list_models(http_client)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(c) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                c,
                provider_name,
                circuit_breakers,
            )
            .list_models(http_client)
            .await
        }
        #[cfg(feature = "provider-bedrock")]
        ProviderConfig::Bedrock(c) => {
            bedrock::BedrockProvider::from_config_with_registry(c, provider_name, circuit_breakers)
                .list_models(http_client)
                .await
        }
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Vertex(c) => {
            vertex::VertexProvider::from_config_with_registry(c, provider_name, circuit_breakers)
                .list_models(http_client)
                .await
        }
        #[cfg(feature = "provider-vertex")]
        ProviderConfig::Gemini(c) => {
            vertex::VertexProvider::from_gemini_config_with_registry(
                c,
                provider_name,
                circuit_breakers,
            )
            .list_models(http_client)
            .await
        }
        ProviderConfig::Test(c) => {
            test::TestProvider::new(&c.model_name)
                .list_models(http_client)
                .await
        }
    }
}

async fn build_response(
    response: reqwest::Response,
    stream: bool,
) -> Result<Response, ProviderError> {
    let status = response.status();

    if stream {
        #[cfg(not(target_arch = "wasm32"))]
        let byte_stream = response.bytes_stream();
        #[cfg(target_arch = "wasm32")]
        let byte_stream = crate::compat::AssertSendStream(response.bytes_stream());

        response::streaming_response(status, byte_stream)
    } else {
        Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(response.bytes().await?))
            .map_err(ProviderError::ResponseBuilder)
    }
}

/// Inject cost calculation into an existing response
/// For non-streaming: adds usage/cost headers by parsing the body
/// For streaming: wraps body to track tokens as they arrive via SSE parsing
pub async fn inject_cost_into_response(params: CostInjectionParams<'_>) -> Response {
    #[cfg(feature = "server")]
    let task_tracker = params.task_tracker;
    #[cfg(feature = "server")]
    let usage_drain = params.usage_drain;
    let CostInjectionParams {
        response,
        provider,
        model,
        pricing,
        db,
        usage_entry,
        max_response_body_bytes,
        streaming_idle_timeout_secs,
        validation_config,
        response_type,
        ..
    } = params;
    // Only process successful JSON responses
    if !response.status().is_success() {
        return response;
    }

    // Check if response is JSON or SSE (streaming).
    // Providers that transform streams (Anthropic, Bedrock, Vertex) use text/event-stream,
    // while OpenAI-compatible providers use application/json even for streaming.
    // Both need cost injection: non-streaming for header-based tracking, streaming for
    // UsageTrackingStream wrapping.
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let is_trackable =
        content_type.contains("application/json") || content_type.contains("text/event-stream");

    if !is_trackable {
        return response;
    }

    // Check if this is a streaming response
    let is_streaming = content_type.contains("text/event-stream")
        || response
            .headers()
            .get("Transfer-Encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.contains("chunked"));

    if is_streaming {
        #[cfg(feature = "server")]
        {
            // For streaming responses, wrap the body to track tokens as they arrive
            if let (Some(db_pool), Some(entry), Some(tracker), Some(drain)) =
                (db, usage_entry, task_tracker, usage_drain)
            {
                use futures_util::StreamExt;

                let (parts, body) = response.into_parts();

                // Convert body to byte stream with proper type annotations
                let stream = body.into_data_stream().map(
                |result: Result<bytes::Bytes, axum::Error>| -> Result<bytes::Bytes, std::io::Error> {
                    result.map_err(std::io::Error::other)
                },
            );

                // Apply validation wrapper if enabled
                // This validates each SSE chunk against the OpenAPI schema
                let validated_stream = if validation_config.enabled {
                    let validating = crate::validation::stream::ValidatingStream::new(
                        stream,
                        response_type,
                        validation_config.mode,
                    );
                    // Box to unify the stream types
                    Box::new(validating)
                        as Box<
                            dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>>
                                + Send
                                + Unpin,
                        >
                } else {
                    Box::new(stream)
                        as Box<
                            dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>>
                                + Send
                                + Unpin,
                        >
                };

                // Apply idle timeout wrapper if enabled (timeout > 0)
                // This terminates the stream if no chunk is received within the timeout,
                // protecting against stalled providers and slow client attacks.
                let idle_timeout = std::time::Duration::from_secs(streaming_idle_timeout_secs);
                let timeout_stream =
                    crate::streaming::IdleTimeoutStream::new(validated_stream, idle_timeout);

                // Wrap with usage tracking (after idle timeout so usage is still logged on timeout)
                let tracking_stream = crate::streaming::UsageTrackingStream::new(
                    timeout_stream,
                    db_pool.clone(),
                    std::sync::Arc::new(pricing.clone()),
                    entry,
                    provider.to_string(),
                    model.to_string(),
                    tracker.clone(),
                    drain.clone(),
                );

                let new_body = axum::body::Body::from_stream(tracking_stream);
                if streaming_idle_timeout_secs > 0 {
                    tracing::debug!(
                        idle_timeout_secs = streaming_idle_timeout_secs,
                        validation_enabled = validation_config.enabled,
                        "Streaming response wrapped with idle timeout and usage tracking"
                    );
                } else {
                    tracing::debug!(
                        validation_enabled = validation_config.enabled,
                        "Streaming response wrapped with usage tracking (idle timeout disabled)"
                    );
                }
                return Response::from_parts(parts, new_body);
            } else {
                // No DB, entry, or tracker - return untracked streaming
                tracing::warn!(
                    "Streaming response without DB/entry/tracker - cost tracking disabled"
                );
                return response;
            }
        }
        #[cfg(not(feature = "server"))]
        {
            // No task tracker available - return untracked streaming
            return response;
        }
    }

    // For non-streaming, parse the body to extract usage
    let (parts, body) = response.into_parts();

    // Try to read and parse the body
    let bytes = match axum::body::to_bytes(body, max_response_body_bytes).await {
        Ok(b) => b,
        Err(_) => {
            // Failed to read body, return original response
            return Response::from_parts(parts, Body::empty());
        }
    };

    // Try to parse as JSON and extract usage
    let extracted = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(mut json) => {
            // Validate response schema if enabled
            if validation_config.enabled
                && let Some(schema_id) = SchemaId::from_response_type(response_type)
                && let Err(errors) = validate_response(schema_id, &json)
            {
                match validation_config.mode {
                    ResponseValidationMode::Warn => {
                        tracing::warn!(
                            response_type = ?response_type,
                            errors = %errors,
                            "Response schema validation failed"
                        );
                    }
                    ResponseValidationMode::Error => {
                        tracing::error!(
                            response_type = ?response_type,
                            "Response schema validation failed, returning error"
                        );
                        // Return a generic 500 error (no details exposed)
                        return Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .header(CONTENT_TYPE, "application/json")
                            .body(Body::from(
                                r#"{"error":{"type":"server_error","message":"Internal server error"}}"#,
                            ))
                            .unwrap_or_else(|_| {
                                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
                                    .into_response()
                            });
                    }
                }
            }

            let usage = json.get("usage");

            // Parse a JSON number as i64, accepting both integer and float representations
            let as_int =
                |v: &serde_json::Value| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64));

            // Support both OpenAI format (prompt_tokens) and newer format (input_tokens)
            let input = usage
                .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("input_tokens")))
                .and_then(as_int)
                .unwrap_or(0);
            let output = usage
                .and_then(|u| {
                    u.get("completion_tokens")
                        .or_else(|| u.get("output_tokens"))
                })
                .and_then(as_int)
                .unwrap_or(0);

            // Extract cached tokens from input_tokens_details or prompt_tokens_details
            let cached = usage
                .and_then(|u| {
                    u.get("input_tokens_details")
                        .or_else(|| u.get("prompt_tokens_details"))
                })
                .and_then(|d| d.get("cached_tokens"))
                .and_then(as_int)
                .unwrap_or(0);

            // Extract reasoning tokens from output_tokens_details or completion_tokens_details
            let reasoning = usage
                .and_then(|u| {
                    u.get("output_tokens_details")
                        .or_else(|| u.get("completion_tokens_details"))
                })
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(as_int)
                .unwrap_or(0);

            // Extract finish_reason from choices[0].finish_reason (OpenAI format)
            // or from output[].status (Responses API format)
            let finish_reason = json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("finish_reason"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    // Try Responses API format: look for message output with status
                    json.get("output")
                        .and_then(|o| o.as_array())
                        .and_then(|arr| {
                            arr.iter().find(|item| {
                                item.get("type").and_then(|t| t.as_str()) == Some("message")
                            })
                        })
                        .and_then(|msg| msg.get("status"))
                        .and_then(|v| v.as_str())
                        .map(|s| match s {
                            "completed" => "stop".to_string(),
                            other => other.to_string(),
                        })
                });

            // Calculate cost in microcents
            let cost_result = pricing.calculate_cost(provider, model, input, output);
            let cost_microcents = cost_result.map(|(c, _)| c);
            let pricing_source = cost_result
                .map(|(_, s)| s)
                .unwrap_or(crate::pricing::CostPricingSource::None);

            // Inject cost (in dollars) into the usage object in the response body.
            // Only re-serialize when we actually mutate the JSON; otherwise we'd
            // change the body length (whitespace, key order) and have to strip
            // Content-Length unnecessarily.
            let mut body_modified = false;
            if let Some(cost) = cost_microcents {
                let cost_dollars = crate::pricing::microcents_to_dollars(cost);
                if let Some(usage_obj) = json.get_mut("usage").and_then(|u| u.as_object_mut()) {
                    usage_obj.insert("cost".to_string(), serde_json::Value::from(cost_dollars));
                    body_modified = true;
                }
            }

            let body_bytes = if body_modified {
                serde_json::to_vec(&json).unwrap_or_else(|_| bytes.to_vec())
            } else {
                bytes.to_vec()
            };

            (
                Some(input),
                Some(output),
                cost_microcents,
                Some(cached),
                Some(reasoning),
                finish_reason,
                body_bytes,
                pricing_source,
                body_modified,
            )
        }
        Err(_) => (
            None,
            None,
            None,
            None,
            None,
            None,
            bytes.to_vec(),
            crate::pricing::CostPricingSource::None,
            false,
        ),
    };

    let (
        input_tokens,
        output_tokens,
        cost_microcents,
        cached_tokens,
        reasoning_tokens,
        finish_reason,
        body_bytes,
        pricing_source,
        body_modified,
    ) = extracted;

    // Rebuild response with headers
    let mut new_parts = parts;
    if let Some(input) = input_tokens
        && let Ok(value) = HeaderValue::try_from(input.to_string())
    {
        new_parts.headers.insert("X-Input-Tokens", value);
    }
    if let Some(output) = output_tokens
        && let Ok(value) = HeaderValue::try_from(output.to_string())
    {
        new_parts.headers.insert("X-Output-Tokens", value);
    }
    if let Some(cost) = cost_microcents
        && let Ok(value) = HeaderValue::try_from(cost.to_string())
    {
        new_parts.headers.insert("X-Cost-Microcents", value);
    }
    if let Some(cached) = cached_tokens
        && cached > 0
        && let Ok(value) = HeaderValue::try_from(cached.to_string())
    {
        new_parts.headers.insert("X-Cached-Tokens", value);
    }
    if let Some(reasoning) = reasoning_tokens
        && reasoning > 0
        && let Ok(value) = HeaderValue::try_from(reasoning.to_string())
    {
        new_parts.headers.insert("X-Reasoning-Tokens", value);
    }
    if let Some(ref reason) = finish_reason
        && let Ok(value) = HeaderValue::try_from(reason.as_str())
    {
        new_parts.headers.insert("X-Finish-Reason", value);
    }
    if let Ok(value) = HeaderValue::try_from(pricing_source.as_str()) {
        new_parts.headers.insert("X-Pricing-Source", value);
    }

    // Only strip Content-Length when we re-serialized the body. If the body is
    // passed through untouched, the upstream length is still authoritative.
    if body_modified {
        new_parts.headers.remove(CONTENT_LENGTH);
    }

    Response::from_parts(new_parts, Body::from(body_bytes))
}

/// Parameters for logging image/audio usage
pub struct MediaUsageParams<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub pricing: &'a crate::pricing::PricingConfig,
    pub db: Option<&'a std::sync::Arc<crate::db::DbPool>>,
    pub api_key_id: Option<uuid::Uuid>,
    #[cfg(feature = "server")]
    pub task_tracker: &'a TaskTracker,
    pub usage: crate::pricing::TokenUsage,
}

/// Log usage for image/audio endpoints and return cost info
///
/// This is used for endpoints that don't use the standard token-based usage tracking:
/// - Image generation (per-image pricing)
/// - Audio transcription/translation (per-second pricing)
/// - Text-to-speech (per-character pricing)
///
/// Returns (cost_microcents, usage_logged) tuple
pub async fn log_media_usage(params: MediaUsageParams<'_>) -> (Option<i64>, bool) {
    #[cfg(feature = "server")]
    let task_tracker = params.task_tracker;
    let MediaUsageParams {
        provider,
        model,
        pricing,
        db,
        api_key_id,
        usage,
        ..
    } = params;

    // Calculate cost
    let cost_result = pricing.calculate_cost_detailed(provider, model, &usage);
    let cost_microcents = cost_result.map(|(c, _)| c);
    let pricing_source = cost_result
        .map(|(_, s)| s)
        .unwrap_or(crate::pricing::CostPricingSource::None);

    // Log usage to database if we have all required components
    let usage_logged = if let (Some(db_pool), Some(key_id)) = (db, api_key_id) {
        let entry = crate::models::UsageLogEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: Some(key_id),
            user_id: None,
            org_id: None,
            project_id: None,
            team_id: None,
            service_account_id: None,
            model: model.to_string(),
            provider: provider.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents,
            http_referer: None,
            request_at: chrono::Utc::now(),
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: Some("stop".to_string()),
            latency_ms: None,
            cancelled: false,
            status_code: Some(200),
            pricing_source,
            image_count: usage.image_count.map(|v| v as i32),
            audio_seconds: usage.audio_seconds.map(|v| v as i32),
            character_count: usage.character_count.map(|v| v as i32),
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
            tool_exit_code: None,
        };

        let db = db_pool.clone();
        #[cfg(feature = "server")]
        task_tracker.spawn(async move {
            for attempt in 0..3 {
                match db.usage().log(entry.clone()).await {
                    Ok(_) => {
                        tracing::debug!(
                            "Logged media usage: model={}, cost_microcents={:?}",
                            entry.model,
                            entry.cost_microcents
                        );
                        break;
                    }
                    Err(e) if attempt == 2 => {
                        tracing::error!(
                            "Failed to log media usage after 3 attempts: {}. Entry: {:?}",
                            e,
                            entry
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to log media usage (attempt {}): {}. Retrying...",
                            attempt + 1,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(
                            100 * 2_u64.pow(attempt),
                        ))
                        .await;
                    }
                }
            }
        });
        true
    } else {
        tracing::debug!("Media usage not logged: missing db or api_key_id");
        false
    };

    (cost_microcents, usage_logged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{ProviderHealthCheckConfig, ProviderHealthCheckMode, TestFailureMode},
        providers::test::TestProvider,
    };

    // ============== Provider::health_check Tests ==============
    //
    // These tests verify the default health_check implementation in the Provider trait.
    // The TestProvider is used with various failure modes to simulate different scenarios.

    #[tokio::test]
    async fn test_health_check_reachability_mode_healthy() {
        // TestProvider with no failure mode should return healthy
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Reachability,
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Healthy);
        assert_eq!(result.status_code, Some(200));
        assert!(result.error.is_none());
        // latency_ms is an unsigned type, so any value is valid
    }

    #[tokio::test]
    async fn test_health_check_reachability_mode_unhealthy_connection_error() {
        // TestProvider with connection error should return unhealthy
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::ConnectionError {
                message: "Connection refused".to_string(),
            },
        );
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Reachability,
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(!result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Unhealthy);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Connection error"));
    }

    #[tokio::test]
    async fn test_health_check_reachability_mode_unhealthy_http_error() {
        // TestProvider with HTTP error should return unhealthy
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::HttpError {
                status_code: 503,
                message: Some("Service Unavailable".to_string()),
            },
        );
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Reachability,
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(!result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Unhealthy);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_healthy() {
        // TestProvider with no failure mode should return healthy for inference
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("test-model".to_string()),
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Healthy);
        assert_eq!(result.status_code, Some(200));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_unhealthy_http_error() {
        // TestProvider with HTTP error should return unhealthy for inference
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::HttpError {
                status_code: 500,
                message: Some("Internal Server Error".to_string()),
            },
        );
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("test-model".to_string()),
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(!result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Unhealthy);
        // HTTP 500 is returned as an error response, not a ProviderError
        assert_eq!(result.status_code, Some(500));
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("HTTP 500"));
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_unhealthy_connection_error() {
        // TestProvider with connection error should return unhealthy
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::ConnectionError {
                message: "Connection refused".to_string(),
            },
        );
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("test-model".to_string()),
            ..Default::default()
        };

        let result = provider.health_check(&client, &config).await;

        assert!(!result.is_healthy());
        assert_eq!(result.status, health_check::HealthStatus::Unhealthy);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Connection error"));
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_uses_configured_model() {
        // Verify that inference mode uses the model from config
        // TestProvider echoes the model name back in responses
        let provider = TestProvider::new("default-model");
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("custom-inference-model".to_string()),
            prompt: Some("Health check ping".to_string()),
            ..Default::default()
        };

        // The health check should succeed since TestProvider doesn't validate models
        let result = provider.health_check(&client, &config).await;
        assert!(result.is_healthy());
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_uses_default_prompt() {
        // Verify that inference mode uses default prompt when not configured
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("test-model".to_string()),
            prompt: None, // Should use default "ping"
            ..Default::default()
        };

        // The health check should succeed
        let result = provider.health_check(&client, &config).await;
        assert!(result.is_healthy());

        // Verify config.prompt() returns the default
        assert_eq!(config.prompt(), "ping");
    }

    #[tokio::test]
    async fn test_health_check_inference_mode_uses_custom_prompt() {
        // Verify that inference mode uses custom prompt when configured
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("test-model".to_string()),
            prompt: Some("Say OK".to_string()),
            ..Default::default()
        };

        // The health check should succeed
        let result = provider.health_check(&client, &config).await;
        assert!(result.is_healthy());

        // Verify config.prompt() returns the custom prompt
        assert_eq!(config.prompt(), "Say OK");
    }
}
