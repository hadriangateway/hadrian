//! End-to-end provider tests using wiremock.
//!
//! This module provides parameterized tests that run against all configured providers.
//! Each provider defines a `ProviderTestSpec` that declares:
//! - How to configure the provider
//! - Which fixtures exist
//! - Any provider-specific headers or behavior
//!
//! Adding a new provider = add one `ProviderTestSpec` + fixture files.
//! Adding a new test = add one test function -> all providers get tested.
//!
//! # Example
//!
//! ```ignore
//! #[rstest]
//! #[case::openai(&OPENAI_SPEC)]
//! #[case::openrouter(&OPENROUTER_SPEC)]
//! #[tokio::test]
//! async fn test_chat_completion_success(#[case] spec: &ProviderTestSpec) {
//!     let harness = E2ETestHarness::new(spec).await;
//!     // ... test logic
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rstest::rstest;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::MockServer;

/// Debug output directory for test responses.
/// Set HADRIAN_TEST_DEBUG=1 to enable saving responses to this directory.
const DEBUG_OUTPUT_DIR: &str = "tests/fixtures/providers/_debug";

#[cfg(feature = "response-validation")]
use crate::providers::test_utils::schema;
use crate::{
    config::GatewayConfig,
    providers::test_utils::{
        FixtureId, load_fixture, mount_fixture_data, responses_weather_tool, validators,
        weather_tool,
    },
};

// =============================================================================
// Provider Test Specification
// =============================================================================

/// Defines what a provider must support for testing.
/// Each provider has a spec that declares its fixtures and configuration.
#[derive(Debug)]
pub struct ProviderTestSpec {
    /// Provider name (e.g., "openai", "openrouter")
    pub name: &'static str,

    /// Provider type for configuration (e.g., "open_ai")
    pub provider_type: &'static str,

    /// Which fixtures exist for this provider
    pub fixtures: ProviderFixtures,

    /// Extra TOML configuration for the provider (e.g., custom headers)
    pub extra_config: &'static str,

    /// Default model name for this provider (used for Chat Completions)
    pub default_model: &'static str,

    /// Model for Responses API (if different from default_model)
    /// Falls back to default_model if None
    pub responses_model: Option<&'static str>,

    /// Expected HTTP status for bad_request fixture (default: 400)
    /// Anthropic returns 404 for invalid model, OpenAI/Bedrock return 400
    pub expected_bad_request_status: u16,

    /// Expected HTTP status for unauthorized fixture (default: 401)
    /// AWS/Bedrock returns 403 for auth errors, OpenAI/Anthropic return 401
    pub expected_unauthorized_status: u16,

    /// Whether this provider supports reasoning tokens in usage.
    /// OpenAI models like o3-mini expose reasoning_tokens.
    /// Anthropic uses "thinking" but doesn't expose it as reasoning_tokens.
    pub supports_reasoning_tokens: bool,

    /// Minimum expected prompt_tokens for vision requests.
    /// Vision requests include image tokens, so prompt_tokens should be high.
    /// Set to 0 to skip this validation.
    pub min_vision_prompt_tokens: u64,

    /// Whether to validate responses against the OpenAI OpenAPI schema.
    /// Enable to catch schema drift and ensure spec conformance.
    /// Default: true
    #[cfg_attr(not(feature = "response-validation"), allow(dead_code))]
    pub validate_against_schema: bool,
}

impl ProviderTestSpec {
    /// Get the model to use for Responses API tests
    pub fn get_responses_model(&self) -> &'static str {
        self.responses_model.unwrap_or(self.default_model)
    }
}

/// Declares which fixtures exist for a provider.
/// `None` means the provider doesn't support that endpoint.
#[derive(Debug, Default)]
pub struct ProviderFixtures {
    // Chat Completions API
    pub chat_completion_success: Option<FixtureId>,
    pub chat_completion_streaming: Option<FixtureId>,

    // Error cases
    pub rate_limit: Option<FixtureId>,
    pub server_error: Option<FixtureId>,
    pub bad_request: Option<FixtureId>,
    pub unauthorized: Option<FixtureId>,

    // Embeddings
    pub embedding_success: Option<FixtureId>,

    // Models
    pub models_list: Option<FixtureId>,

    // Responses API
    pub responses_success: Option<FixtureId>,
    pub responses_streaming: Option<FixtureId>,

    // Completions API (legacy)
    pub completion_success: Option<FixtureId>,
    pub completion_streaming: Option<FixtureId>,

    // Tool Calling (Chat Completions)
    pub tool_call_success: Option<FixtureId>,
    pub tool_call_streaming: Option<FixtureId>,
    pub tool_call_parallel: Option<FixtureId>,
    pub tool_call_with_result: Option<FixtureId>,

    // Reasoning Models (o3-mini, o1, etc.)
    pub reasoning_success: Option<FixtureId>,
    pub reasoning_streaming: Option<FixtureId>,

    // Responses API - Tool Calling
    pub responses_tool_call_success: Option<FixtureId>,
    pub responses_tool_call_streaming: Option<FixtureId>,
    pub responses_tool_call_parallel: Option<FixtureId>,
    pub responses_tool_call_with_result: Option<FixtureId>,

    // Responses API - Reasoning
    pub responses_reasoning_success: Option<FixtureId>,
    pub responses_reasoning_streaming: Option<FixtureId>,

    // Vision (Chat Completions)
    pub vision_success: Option<FixtureId>,
    pub vision_url_success: Option<FixtureId>,

    // Responses API - Vision
    pub responses_vision_success: Option<FixtureId>,
    pub responses_vision_url_success: Option<FixtureId>,

    // Image Generation
    pub image_generation_success: Option<FixtureId>,
    pub image_edit_success: Option<FixtureId>,
    pub image_variation_success: Option<FixtureId>,

    // Audio
    pub audio_speech_success: Option<FixtureId>,
    pub audio_transcription_success: Option<FixtureId>,
    pub audio_translation_success: Option<FixtureId>,
}

/// Empty fixtures constant for use with struct update syntax.
/// Use `..EMPTY_FIXTURES` to fill remaining fields with None.
const EMPTY_FIXTURES: ProviderFixtures = ProviderFixtures {
    chat_completion_success: None,
    chat_completion_streaming: None,
    rate_limit: None,
    server_error: None,
    bad_request: None,
    unauthorized: None,
    embedding_success: None,
    models_list: None,
    responses_success: None,
    responses_streaming: None,
    completion_success: None,
    completion_streaming: None,
    tool_call_success: None,
    tool_call_streaming: None,
    tool_call_parallel: None,
    tool_call_with_result: None,
    reasoning_success: None,
    reasoning_streaming: None,
    responses_tool_call_success: None,
    responses_tool_call_streaming: None,
    responses_tool_call_parallel: None,
    responses_tool_call_with_result: None,
    responses_reasoning_success: None,
    responses_reasoning_streaming: None,
    vision_success: None,
    vision_url_success: None,
    responses_vision_success: None,
    responses_vision_url_success: None,
    image_generation_success: None,
    image_edit_success: None,
    image_variation_success: None,
    audio_speech_success: None,
    audio_transcription_success: None,
    audio_translation_success: None,
};

// =============================================================================
// Provider Specifications
// =============================================================================

pub static OPENAI_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "openai",
    provider_type: "open_ai",
    default_model: "gpt-4o-mini",
    responses_model: None,
    extra_config: "",
    fixtures: ProviderFixtures {
        // Chat Completions
        chat_completion_success: Some(FixtureId::OpenAiChatCompletionSuccess),
        chat_completion_streaming: Some(FixtureId::OpenAiChatCompletionStreaming),

        // Errors
        rate_limit: Some(FixtureId::OpenAiRateLimit),
        server_error: Some(FixtureId::OpenAiServerError),
        bad_request: Some(FixtureId::OpenAiBadRequest),
        unauthorized: Some(FixtureId::OpenAiUnauthorized),

        // Embeddings & Models
        embedding_success: Some(FixtureId::OpenAiEmbeddingSuccess),
        models_list: Some(FixtureId::OpenAiModelsList),

        // Responses API
        responses_success: Some(FixtureId::OpenAiResponsesSuccess),
        responses_streaming: Some(FixtureId::OpenAiResponsesStreaming),

        // Completions API
        completion_success: Some(FixtureId::OpenAiCompletionSuccess),
        completion_streaming: Some(FixtureId::OpenAiCompletionStreaming),

        // Tool Calling
        tool_call_success: Some(FixtureId::OpenAiToolCallSuccess),
        tool_call_streaming: Some(FixtureId::OpenAiToolCallStreaming),
        tool_call_parallel: Some(FixtureId::OpenAiToolCallParallel),
        tool_call_with_result: Some(FixtureId::OpenAiToolCallWithResult),

        // Reasoning
        reasoning_success: Some(FixtureId::OpenAiReasoningSuccess),
        reasoning_streaming: Some(FixtureId::OpenAiReasoningStreaming),

        // Responses API - Tool Calling
        responses_tool_call_success: Some(FixtureId::OpenAiResponsesToolCallSuccess),
        responses_tool_call_streaming: Some(FixtureId::OpenAiResponsesToolCallStreaming),
        responses_tool_call_parallel: Some(FixtureId::OpenAiResponsesToolCallParallel),
        responses_tool_call_with_result: Some(FixtureId::OpenAiResponsesToolCallWithResult),

        // Responses API - Reasoning
        responses_reasoning_success: Some(FixtureId::OpenAiResponsesReasoningSuccess),
        responses_reasoning_streaming: Some(FixtureId::OpenAiResponsesReasoningStreaming),

        // Vision
        vision_success: Some(FixtureId::OpenAiVisionSuccess),
        vision_url_success: Some(FixtureId::OpenAiVisionUrlSuccess),

        // Responses API - Vision
        responses_vision_success: Some(FixtureId::OpenAiResponsesVisionSuccess),
        responses_vision_url_success: Some(FixtureId::OpenAiResponsesVisionUrlSuccess),

        // Image Generation
        image_generation_success: Some(FixtureId::OpenAiImageGenerationSuccess),
        image_edit_success: Some(FixtureId::OpenAiImageEditSuccess),
        image_variation_success: Some(FixtureId::OpenAiImageVariationSuccess),

        // Audio
        audio_speech_success: Some(FixtureId::OpenAiAudioSpeechSuccess),
        audio_transcription_success: Some(FixtureId::OpenAiAudioTranscriptionSuccess),
        audio_translation_success: Some(FixtureId::OpenAiAudioTranslationSuccess),
    },
    expected_bad_request_status: 400,
    expected_unauthorized_status: 401,
    supports_reasoning_tokens: true,
    min_vision_prompt_tokens: 100, // Images add significant tokens
    validate_against_schema: true,
};

pub static OPENROUTER_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "openrouter",
    provider_type: "open_ai",
    // Note: We use gpt-4o-mini without provider prefix to avoid Hadrian's
    // provider routing (which would interpret "openai/" as a provider name).
    // The fixture responses still contain "openai/gpt-4o-mini" format.
    default_model: "gpt-4o-mini",
    responses_model: None,
    extra_config: r#"
[providers.mock-provider.headers]
HTTP-Referer = "https://hadriangateway.com"
X-Title = "Hadrian Gateway Tests"
"#,
    fixtures: ProviderFixtures {
        chat_completion_success: Some(FixtureId::OpenRouterChatCompletionSuccess),
        chat_completion_streaming: Some(FixtureId::OpenRouterChatCompletionStreaming),
        responses_success: Some(FixtureId::OpenRouterResponsesSuccess),
        responses_streaming: Some(FixtureId::OpenRouterResponsesStreaming),
        // All others use Default (None)
        ..EMPTY_FIXTURES
    },
    expected_bad_request_status: 400,
    expected_unauthorized_status: 401,
    supports_reasoning_tokens: false, // Not tested with OpenRouter
    min_vision_prompt_tokens: 0,      // Vision not tested with OpenRouter
    validate_against_schema: true,
};

/// Anthropic provider specification.
/// Uses native Anthropic Messages API format in fixtures.
/// The gateway converts these to OpenAI-compatible responses.
pub static ANTHROPIC_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "anthropic",
    provider_type: "anthropic",
    default_model: "claude-sonnet-4-20250514",
    responses_model: None,
    extra_config: "",
    fixtures: ProviderFixtures {
        // Chat Completions (Anthropic /v1/messages -> OpenAI format)
        chat_completion_success: Some(FixtureId::AnthropicMessagesSuccess),
        chat_completion_streaming: Some(FixtureId::AnthropicMessagesStreaming),
        // Responses API (uses same /v1/messages endpoint with conversion)
        responses_success: Some(FixtureId::AnthropicMessagesSuccess),
        responses_streaming: Some(FixtureId::AnthropicMessagesStreaming),
        // Tool Calling
        tool_call_success: Some(FixtureId::AnthropicToolCallSuccess),
        tool_call_streaming: Some(FixtureId::AnthropicToolCallStreaming),
        tool_call_parallel: Some(FixtureId::AnthropicToolCallParallel),
        tool_call_with_result: Some(FixtureId::AnthropicToolCallWithResult),
        // Extended Thinking (Anthropic's reasoning feature)
        reasoning_success: Some(FixtureId::AnthropicThinkingSuccess),
        reasoning_streaming: Some(FixtureId::AnthropicThinkingStreaming),
        // Responses API - Tool Calling (same fixtures as chat completions)
        responses_tool_call_success: Some(FixtureId::AnthropicToolCallSuccess),
        responses_tool_call_streaming: Some(FixtureId::AnthropicToolCallStreaming),
        responses_tool_call_parallel: Some(FixtureId::AnthropicToolCallParallel),
        responses_tool_call_with_result: Some(FixtureId::AnthropicToolCallWithResult),
        // Responses API - Reasoning (same fixtures as chat completions)
        responses_reasoning_success: Some(FixtureId::AnthropicThinkingSuccess),
        responses_reasoning_streaming: Some(FixtureId::AnthropicThinkingStreaming),
        // Vision (Anthropic only supports base64 images)
        vision_success: Some(FixtureId::AnthropicVisionSuccess),
        responses_vision_success: Some(FixtureId::AnthropicVisionSuccess),
        // Errors
        bad_request: Some(FixtureId::AnthropicBadRequest),
        unauthorized: Some(FixtureId::AnthropicUnauthorized),
        ..EMPTY_FIXTURES
    },
    // Anthropic returns 404 for invalid model (not_found_error)
    expected_bad_request_status: 404,
    expected_unauthorized_status: 401,
    // Anthropic uses "thinking" but doesn't expose reasoning_tokens in usage
    supports_reasoning_tokens: false,
    // Anthropic uses fewer tokens for tiny images
    min_vision_prompt_tokens: 20,
    validate_against_schema: true,
};

/// Bedrock provider specification.
/// Uses AWS Bedrock Converse API format (converted to OpenAI) for Chat Completions.
/// Uses Converse API for all endpoints including Responses API.
#[cfg(feature = "provider-bedrock")]
pub static BEDROCK_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "bedrock",
    provider_type: "bedrock",
    // Use Nova Lite for Bedrock tests (via Converse API)
    default_model: "us.amazon.nova-2-lite-v1:0",
    responses_model: None,
    extra_config: "",
    fixtures: ProviderFixtures {
        // Chat Completions (Bedrock Converse API -> OpenAI format)
        chat_completion_success: Some(FixtureId::BedrockConverseSuccess),
        chat_completion_streaming: Some(FixtureId::BedrockConverseStreaming),
        // Responses API (routes through Converse API internally)
        responses_success: Some(FixtureId::BedrockResponsesSuccess),
        responses_streaming: Some(FixtureId::BedrockResponsesStreaming),
        // Tool Calling (Converse API)
        tool_call_success: Some(FixtureId::BedrockToolCallSuccess),
        tool_call_streaming: Some(FixtureId::BedrockToolCallStreaming),
        tool_call_parallel: Some(FixtureId::BedrockToolCallParallel),
        tool_call_with_result: Some(FixtureId::BedrockToolCallWithResult),
        // Vision (Bedrock requires base64 images)
        vision_success: Some(FixtureId::BedrockVisionSuccess),
        // Errors
        bad_request: Some(FixtureId::BedrockBadRequest),
        unauthorized: Some(FixtureId::BedrockUnauthorized),
        ..EMPTY_FIXTURES
    },
    expected_bad_request_status: 400,
    // AWS returns 403 (Forbidden) for auth errors
    expected_unauthorized_status: 403,
    supports_reasoning_tokens: false, // Bedrock Converse doesn't expose reasoning tokens
    // Bedrock uses fewer tokens for tiny images
    min_vision_prompt_tokens: 20,
    validate_against_schema: true,
};

/// Gemini Developer API provider specification.
/// Uses Google Gemini API format (converted to OpenAI) for Chat Completions.
/// Uses generateContent API for all endpoints including Responses API.
/// The `gemini` provider reuses the Vertex runtime, so the same wire fixtures apply.
#[cfg(feature = "provider-vertex")]
pub static GEMINI_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "gemini",
    provider_type: "gemini",
    // Use Gemini 2.0 Flash for tests
    default_model: "gemini-2.0-flash",
    responses_model: None,
    extra_config: "",
    fixtures: ProviderFixtures {
        // Chat Completions (Vertex generateContent -> OpenAI format)
        chat_completion_success: Some(FixtureId::VertexGenerateContentSuccess),
        chat_completion_streaming: Some(FixtureId::VertexGenerateContentStreaming),
        // Responses API (routes through generateContent internally)
        responses_success: Some(FixtureId::VertexResponsesSuccess),
        responses_streaming: Some(FixtureId::VertexResponsesStreaming),
        // Tool Calling (generateContent with tools)
        tool_call_success: Some(FixtureId::VertexToolCallSuccess),
        tool_call_streaming: Some(FixtureId::VertexToolCallStreaming),
        tool_call_parallel: Some(FixtureId::VertexToolCallParallel),
        tool_call_with_result: Some(FixtureId::VertexToolCallWithResult),
        // Vision (Vertex supports inline base64 images)
        vision_success: Some(FixtureId::VertexVisionSuccess),
        // Errors
        bad_request: Some(FixtureId::VertexBadRequest),
        unauthorized: Some(FixtureId::VertexUnauthorized),
        ..EMPTY_FIXTURES
    },
    // Vertex returns 404 for invalid model, 401 for invalid API key
    expected_bad_request_status: 404,
    expected_unauthorized_status: 401,
    supports_reasoning_tokens: false, // Gemini thinking not exposed as reasoning_tokens
    // Vertex uses fewer tokens for tiny images
    min_vision_prompt_tokens: 20,
    validate_against_schema: true,
};

/// Ollama provider specification.
/// Uses OpenAI-compatible API format (local server, no auth required).
/// Uses qwen3:4b for text and gemma3:4b for vision.
pub static OLLAMA_SPEC: ProviderTestSpec = ProviderTestSpec {
    name: "ollama",
    provider_type: "open_ai",
    // Use qwen3:4b for Ollama tests (local model)
    default_model: "qwen3:4b",
    responses_model: None,
    extra_config: "",
    fixtures: ProviderFixtures {
        // Chat Completions (OpenAI-compatible)
        chat_completion_success: Some(FixtureId::OllamaChatCompletionSuccess),
        chat_completion_streaming: Some(FixtureId::OllamaChatCompletionStreaming),
        // Tool Calling
        tool_call_success: Some(FixtureId::OllamaToolCallSuccess),
        tool_call_streaming: Some(FixtureId::OllamaToolCallStreaming),
        vision_success: Some(FixtureId::OllamaVisionSuccess),
        // Errors
        bad_request: Some(FixtureId::OllamaBadRequest),
        ..EMPTY_FIXTURES
    },
    // Ollama returns 404 for invalid model
    expected_bad_request_status: 404,
    // Ollama doesn't require auth, so unauthorized tests are not applicable
    expected_unauthorized_status: 401,
    supports_reasoning_tokens: false, // Local models typically don't expose reasoning tokens
    // Vision tokens vary by model
    min_vision_prompt_tokens: 20,
    validate_against_schema: true,
};

// =============================================================================
// Test Harness
// =============================================================================

/// Check if debug output is enabled via HADRIAN_TEST_DEBUG env var.
/// Only `1`/`true` (case-insensitive) count — `HADRIAN_TEST_DEBUG=0` should
/// not turn debug on.
fn is_debug_enabled() -> bool {
    matches!(
        std::env::var("HADRIAN_TEST_DEBUG")
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(ref s) if s == "1" || s == "true"
    )
}

/// Save a debug response to the debug output directory.
/// Only saves if HADRIAN_TEST_DEBUG=1 is set.
fn save_debug_response(provider: &str, test_name: &str, status: StatusCode, body: &str) {
    if !is_debug_enabled() {
        return;
    }

    let debug_dir = std::path::Path::new(DEBUG_OUTPUT_DIR).join(provider);
    if let Err(e) = std::fs::create_dir_all(&debug_dir) {
        eprintln!("Failed to create debug dir: {}", e);
        return;
    }

    let filename = format!("{}.json", test_name.replace("::", "_"));
    let filepath = debug_dir.join(&filename);

    // Try to parse body as JSON for pretty printing
    let body_value: Value = serde_json::from_str(body).unwrap_or_else(|_| {
        json!({
            "_raw_body": body,
            "_parse_error": "Body was not valid JSON"
        })
    });

    let debug_output = json!({
        "test": test_name,
        "provider": provider,
        "status": status.as_u16(),
        "status_text": status.canonical_reason().unwrap_or("Unknown"),
        "body": body_value
    });

    if let Ok(content) = serde_json::to_string_pretty(&debug_output)
        && let Err(e) = std::fs::write(&filepath, content)
    {
        eprintln!("Failed to write debug file {}: {}", filepath.display(), e);
    }
}

/// Test harness for running e2e tests against a provider.
pub struct E2ETestHarness {
    pub app: axum::Router,
    pub mock_server: MockServer,
    pub spec: &'static ProviderTestSpec,
}

impl E2ETestHarness {
    /// Create a new test harness for the given provider spec.
    pub async fn new(spec: &'static ProviderTestSpec) -> Self {
        let mock_server = MockServer::start().await;
        let app = create_test_app(spec, &mock_server).await;
        Self {
            app,
            mock_server,
            spec,
        }
    }

    /// Mount a fixture on the mock server.
    pub async fn mount_fixture(&self, id: FixtureId, expected_calls: u64) {
        let fixture = load_fixture(id);
        mount_fixture_data(&self.mock_server, &fixture, expected_calls).await;
    }

    /// POST JSON to the app and return status + JSON body.
    pub async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.post_json_debug(uri, body, None).await
    }

    /// POST JSON with debug output saved to file.
    pub async fn post_json_debug(
        &self,
        uri: &str,
        body: Value,
        test_name: Option<&str>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();

        if let Some(name) = test_name {
            save_debug_response(self.spec.name, name, status, &body_str);
        }

        let json: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
            panic!(
                "Failed to parse response as JSON: {e}\nstatus: {status}\nbody: {}",
                String::from_utf8_lossy(&body_bytes)
            )
        });
        (status, json)
    }

    /// POST JSON to the app and return status + raw string body.
    pub async fn post_json_raw(&self, uri: &str, body: Value) -> (StatusCode, String) {
        self.post_json_raw_debug(uri, body, None).await
    }

    /// POST JSON with debug output saved to file.
    pub async fn post_json_raw_debug(
        &self,
        uri: &str,
        body: Value,
        test_name: Option<&str>,
    ) -> (StatusCode, String) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();

        if let Some(name) = test_name {
            save_debug_response(self.spec.name, name, status, &body_str);
        }

        (status, body_str)
    }

    /// GET request to the app and return status + JSON body.
    pub async fn get_json(&self, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
            panic!(
                "Failed to parse response as JSON: {e}\nstatus: {status}\nbody: {}",
                String::from_utf8_lossy(&body_bytes)
            )
        });
        (status, json)
    }

    /// POST JSON to the app and return status + headers + JSON body.
    /// Use this when you need to verify response headers (e.g., cost tracking).
    pub async fn post_json_with_headers(
        &self,
        uri: &str,
        body: Value,
    ) -> (StatusCode, http::HeaderMap, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
            panic!(
                "Failed to parse response as JSON: {e}\nstatus: {status}\nbody: {}",
                String::from_utf8_lossy(&body_bytes)
            )
        });
        (status, headers, json)
    }

    /// Validate a response against the OpenAPI schema if enabled for this provider.
    /// This should be called after the manual validator functions to catch schema drift.
    ///
    /// # Panics
    /// Panics if schema validation is enabled and the response doesn't match the schema.
    #[cfg(feature = "response-validation")]
    pub fn validate_schema(&self, schema_id: schema::SchemaId, body: &Value) {
        if !self.spec.validate_against_schema {
            return;
        }

        if let Err(e) = schema::validate_response(schema_id, body) {
            panic!(
                "Schema validation failed for {} ({}): {}\nResponse: {}",
                self.spec.name,
                schema_id.schema_name(),
                e,
                serde_json::to_string_pretty(body).unwrap_or_default()
            );
        }
    }

    /// Validate streaming chunks against the OpenAPI schema if enabled for this provider.
    /// This should be called after parsing SSE chunks to catch schema drift in streaming responses.
    ///
    /// # Panics
    /// Panics if schema validation is enabled and any chunk doesn't match the schema.
    #[cfg(feature = "response-validation")]
    pub fn validate_streaming_schema(&self, chunks: &[Value]) {
        if !self.spec.validate_against_schema {
            return;
        }

        if let Err(e) = schema::validate_streaming_chunks(chunks) {
            panic!(
                "Streaming schema validation failed for {}: {}",
                self.spec.name, e
            );
        }
    }

    /// Validate Responses API streaming chunks against OpenAPI schemas if enabled.
    /// Uses discriminator-based validation based on each chunk's `type` field.
    ///
    /// # Panics
    /// Panics if schema validation is enabled and any chunk doesn't match its schema.
    #[cfg(feature = "response-validation")]
    pub fn validate_responses_streaming_schema(&self, chunks: &[Value]) {
        if !self.spec.validate_against_schema {
            return;
        }

        if let Err(e) = schema::validate_responses_streaming_chunks(chunks) {
            panic!(
                "Responses streaming schema validation failed for {}: {}",
                self.spec.name, e
            );
        }
    }
}

/// Create a test application with the given provider spec and mock server.
async fn create_test_app(spec: &ProviderTestSpec, mock_server: &MockServer) -> axum::Router {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

    // Provider-specific configuration
    let provider_config = if spec.provider_type == "anthropic" {
        // Anthropic provider uses different config structure
        format!(
            r#"
[providers.mock-provider]
type = "anthropic"
base_url = "{}"
api_key = "test-api-key"
timeout_secs = 30
default_max_tokens = 4096

# Disable retries for predictable test behavior
[providers.mock-provider.retry]
enabled = false

# Disable circuit breaker for predictable test behavior
[providers.mock-provider.circuit_breaker]
enabled = false

# Claude Sonnet 4 pricing: $3/1M input, $15/1M output
# (in microcents: 3_000_000, 15_000_000)
[providers.mock-provider.models.claude-sonnet-4-20250514]
input_per_1m_tokens = 3000000
output_per_1m_tokens = 15000000
"#,
            mock_server.uri()
        )
    } else if spec.provider_type == "bedrock" {
        // Bedrock provider uses AWS-style configuration
        // We use static credentials (wiremock won't validate signatures)
        format!(
            r#"
[providers.mock-provider]
type = "bedrock"
region = "us-east-1"
converse_base_url = "{}"
timeout_secs = 30

# Static credentials for testing (signatures won't be validated by wiremock)
[providers.mock-provider.credentials]
type = "static"
access_key_id = "AKIAIOSFODNN7EXAMPLE"
secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

# Disable retries for predictable test behavior
[providers.mock-provider.retry]
enabled = false

# Disable circuit breaker for predictable test behavior
[providers.mock-provider.circuit_breaker]
enabled = false

# Claude on Bedrock pricing: $3/1M input, $15/1M output
[providers.mock-provider.models."anthropic.claude-sonnet-4-20250514-v1:0"]
input_per_1m_tokens = 3000000
output_per_1m_tokens = 15000000
"#,
            mock_server.uri()
        )
    } else if spec.provider_type == "gemini" {
        // Gemini Developer API uses API key mode with base_url override.
        // Fixture paths are /{model}:generateContent, so base_url is just the mock server.
        format!(
            r#"
[providers.mock-provider]
type = "gemini"
api_key = "test-api-key"
base_url = "{}"
timeout_secs = 30

# Disable retries for predictable test behavior
[providers.mock-provider.retry]
enabled = false

# Disable circuit breaker for predictable test behavior
[providers.mock-provider.circuit_breaker]
enabled = false

# Gemini 2.0 Flash pricing: $0.10/1M input, $0.40/1M output (approx)
# (in microcents: 100_000, 400_000)
[providers.mock-provider.models.gemini-2-0-flash]
input_per_1m_tokens = 100000
output_per_1m_tokens = 400000
"#,
            mock_server.uri()
        )
    } else {
        // OpenAI-compatible providers (open_ai, openrouter, etc.)
        format!(
            r#"
[providers.mock-provider]
type = "{}"
base_url = "{}"
api_key = "test-api-key"
timeout_secs = 30
supports_tools = true
supports_vision = true

# Disable retries for predictable test behavior
[providers.mock-provider.retry]
enabled = false

# Disable circuit breaker for predictable test behavior
[providers.mock-provider.circuit_breaker]
enabled = false

# Pricing for cost calculation tests
# gpt-4o-mini: $0.15/1M input, $0.60/1M output (in microcents: 150_000, 600_000)
[providers.mock-provider.models.gpt-4o-mini]
input_per_1m_tokens = 150000
output_per_1m_tokens = 600000

# o3-mini: $1.10/1M input, $4.40/1M output, reasoning same as output
# (in microcents: 1_100_000, 4_400_000)
[providers.mock-provider.models.o3-mini]
input_per_1m_tokens = 1100000
output_per_1m_tokens = 4400000
reasoning_per_1m_tokens = 4400000

# text-embedding-3-small: $0.02/1M tokens (in microcents: 20_000)
[providers.mock-provider.models.text-embedding-3-small]
input_per_1m_tokens = 20000
output_per_1m_tokens = 0

# gpt-3.5-turbo-instruct: $1.50/1M input, $2.00/1M output
[providers.mock-provider.models.gpt-3.5-turbo-instruct]
input_per_1m_tokens = 1500000
output_per_1m_tokens = 2000000
"#,
            spec.provider_type,
            mock_server.uri()
        )
    };

    #[cfg(feature = "sso")]
    let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
    #[cfg(not(feature = "sso"))]
    let session_section = "";

    let extra_config = spec.extra_config;
    let config_str = format!(
        r#"
[database]
type = "sqlite"
path = "file:provider_e2e_test_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "mock-provider"
{provider_config}
{extra_config}
"#
    );

    let config = GatewayConfig::parse(&config_str).expect("Failed to parse test config");
    let state = crate::AppState::new(config.clone())
        .await
        .expect("Failed to create AppState");
    crate::build_app(&config, state)
}

// =============================================================================
// Chat Completions Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_chat_completion_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.chat_completion_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_chat_completion(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_schema(schema::SchemaId::ChatCompletion, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_chat_completion_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.chat_completion_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}],
                "stream": true
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let chunks = validators::assert_streaming_chat_completion(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_streaming_schema(&chunks);
    assert!(chunks.len() > 1, "Should have multiple streaming chunks");
}

// =============================================================================
// Responses API Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_debug(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "Hello"
            }),
            Some("test_responses_success"),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_responses_api(&body);
    // TODO: Response schema has complex anyOf/allOf that fails to compile with jsonschema crate
    // harness.validate_schema(schema::SchemaId::Response, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw_debug(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "Hello",
                "stream": true
            }),
            Some("test_responses_streaming"),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let chunks = validators::assert_streaming_responses(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_responses_streaming_schema(&chunks);
    assert!(chunks.len() > 1, "Should have multiple streaming events");
}

// =============================================================================
// Embeddings Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_embedding_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.embedding_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/embeddings",
            json!({
                "model": "text-embedding-3-small",
                "input": "The quick brown fox"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_embeddings(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_schema(schema::SchemaId::Embedding, &body);
}

// =============================================================================
// Completions API Tests (Legacy)
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_completion_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.completion_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/completions",
            json!({
                "model": "gpt-3.5-turbo-instruct",
                "prompt": "Say this is a test"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_completion(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_schema(schema::SchemaId::Completion, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_completion_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.completion_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw(
            "/api/v1/completions",
            json!({
                "model": "gpt-3.5-turbo-instruct",
                "prompt": "Say this is a test",
                "stream": true
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(body.contains("data:"), "Response should contain SSE data");
    assert!(body.contains("[DONE]"), "Response should contain [DONE]");
}

// =============================================================================
// Tool Calling Tests (Chat Completions)
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_tool_call_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.tool_call_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "What's the weather in San Francisco?"}],
                "tools": [weather_tool()]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_tool_calls(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_schema(schema::SchemaId::ChatCompletion, &body);
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_tool_call_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.tool_call_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw_debug(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "What's the weather in San Francisco?"}],
                "tools": [weather_tool()],
                "stream": true
            }),
            Some("test_tool_call_streaming"),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let chunks = validators::assert_streaming_chat_completion(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_streaming_schema(&chunks);
    // At least one chunk should contain tool_calls
    assert!(
        chunks
            .iter()
            .any(|c| !c["choices"][0]["delta"]["tool_calls"].is_null()),
        "At least one chunk should contain tool_calls"
    );
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_tool_call_parallel(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.tool_call_parallel else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_debug(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "What's the weather in San Francisco and New York?"}],
                "tools": [weather_tool()]
            }),
            Some("test_tool_call_parallel"),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_tool_calls(&body);
    #[cfg(feature = "response-validation")]
    harness.validate_schema(schema::SchemaId::ChatCompletion, &body);

    let tool_calls = body["choices"][0]["message"]["tool_calls"]
        .as_array()
        .expect("tool_calls should be array");
    assert!(
        tool_calls.len() >= 2,
        "Expected at least 2 parallel tool calls, got {}",
        tool_calls.len()
    );
}

// =============================================================================
// Reasoning Model Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_reasoning_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.reasoning_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "o3-mini",
                "messages": [{"role": "user", "content": "What is 2+2?"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "chat.completion");
    assert!(body["choices"][0]["message"]["content"].is_string());

    // Verify reasoning tokens are present (only for providers that expose them)
    if spec.supports_reasoning_tokens {
        let usage = &body["usage"];
        let completion_details = &usage["completion_tokens_details"];
        let reasoning_tokens = completion_details["reasoning_tokens"].as_u64().unwrap_or(0);
        assert!(
            reasoning_tokens > 0,
            "Expected reasoning_tokens > 0, got {}",
            reasoning_tokens
        );
    }
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_reasoning_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.reasoning_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw(
            "/api/v1/chat/completions",
            json!({
                "model": "o3-mini",
                "messages": [{"role": "user", "content": "What is 2+2?"}],
                "stream": true
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(body.contains("data:"), "Response should contain SSE data");
    assert!(body.contains("[DONE]"), "Response should contain [DONE]");
}

// =============================================================================
// Vision Tests
// =============================================================================

// A tiny 1x1 red PNG image encoded in base64
const TINY_RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_vision_base64_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.vision_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "What is in this image?"},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/png;base64,{}", TINY_RED_PNG_BASE64)
                            }
                        }
                    ]
                }]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "chat.completion");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .expect("vision response should have textual content");
    assert!(
        !content.trim().is_empty(),
        "vision response content must be non-empty for {}",
        spec.name
    );
    assert!(
        body["choices"][0]["finish_reason"].is_string(),
        "vision response should have finish_reason"
    );

    // Vision requests typically have high prompt token counts due to image encoding
    if spec.min_vision_prompt_tokens > 0 {
        let prompt_tokens = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        assert!(
            prompt_tokens > spec.min_vision_prompt_tokens,
            "Expected prompt_tokens > {} for vision, got {}",
            spec.min_vision_prompt_tokens,
            prompt_tokens
        );
    }
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_vision_url_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.vision_url_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "What is in this image?"},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": "https://example.com/image.png"
                            }
                        }
                    ]
                }]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "chat.completion");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .expect("vision response should have textual content");
    assert!(
        !content.trim().is_empty(),
        "vision response content must be non-empty for {}",
        spec.name
    );
    assert!(
        body["choices"][0]["finish_reason"].is_string(),
        "vision response should have finish_reason"
    );
}

// =============================================================================
// Responses API - Tool Calling Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_tool_call_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_tool_call_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "What's the weather in San Francisco?",
                "tools": [responses_weather_tool()]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    validators::assert_responses_function_calls(&body);
    // TODO: Response schema has complex anyOf/allOf that fails to compile with jsonschema crate
    // harness.validate_schema(schema::SchemaId::Response, &body);

    // Verify it's the expected function
    let function_calls: Vec<_> = body["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["type"] == "function_call")
        .collect();
    assert_eq!(function_calls[0]["name"], "get_weather");
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_tool_call_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_tool_call_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "What's the weather in San Francisco?",
                "tools": [responses_weather_tool()],
                "stream": true
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let chunks = validators::assert_streaming_responses(&body);
    assert!(!chunks.is_empty(), "Should have streaming events");
}

// =============================================================================
// Responses API - Reasoning Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_reasoning_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_reasoning_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": "o3-mini",
                "input": "What is 2+2?",
                "reasoning": {"effort": "low"}
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "response");
    assert!(body["output"].is_array());

    // Verify reasoning tokens are present (only for providers that expose them)
    if spec.supports_reasoning_tokens {
        let usage = &body["usage"];
        let output_details = &usage["output_tokens_details"];
        let reasoning_tokens = output_details["reasoning_tokens"].as_u64().unwrap_or(0);
        assert!(
            reasoning_tokens > 0,
            "Expected reasoning_tokens > 0, got {}",
            reasoning_tokens
        );
    }
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_reasoning_streaming(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_reasoning_streaming else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_raw(
            "/api/v1/responses",
            json!({
                "model": "o3-mini",
                "input": "What is 2+2?",
                "reasoning": {"effort": "low"},
                "stream": true
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(body.contains("data:"), "Response should contain SSE data");
    assert!(body.contains("[DONE]"), "Response should contain [DONE]");
}

// =============================================================================
// Responses API - Vision Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_vision_base64_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_vision_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": [{
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "What is in this image?"},
                        {
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": format!("data:image/png;base64,{}", TINY_RED_PNG_BASE64)
                        }
                    ]
                }]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "response");
    assert!(body["output"].is_array());
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_vision_url_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_vision_url_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": [{
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "What is in this image?"},
                        {
                            "type": "input_image",
                            "detail": "low",
                            "image_url": "https://example.com/image.png"
                        }
                    ]
                }]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "response");
    assert!(body["output"].is_array());
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_rate_limit_error(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.rate_limit else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "Expected 429 for {}",
        spec.name
    );
    validators::assert_error(&body);
    // TODO: Error schema requires 'param' field which our errors don't include
    // harness.validate_schema(schema::SchemaId::Error, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_server_error(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.server_error else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "Expected 500 for {}",
        spec.name
    );
    validators::assert_error(&body);
    // TODO: Error schema requires 'param' field which our errors don't include
    // harness.validate_schema(schema::SchemaId::Error, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_bad_request_error(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.bad_request else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    let expected_status = StatusCode::from_u16(spec.expected_bad_request_status)
        .expect("Invalid expected_bad_request_status");
    assert_eq!(
        status, expected_status,
        "Expected {} for {}",
        spec.expected_bad_request_status, spec.name
    );
    validators::assert_error(&body);
    // TODO: Error schema requires 'param' field which our errors don't include
    // harness.validate_schema(schema::SchemaId::Error, &body);
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[cfg_attr(feature = "provider-bedrock", case::bedrock(&BEDROCK_SPEC))]
#[cfg_attr(feature = "provider-vertex", case::gemini(&GEMINI_SPEC))]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_unauthorized_error(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.unauthorized else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json_debug(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            Some("test_unauthorized_error"),
        )
        .await;

    let expected_status = StatusCode::from_u16(spec.expected_unauthorized_status)
        .expect("Invalid expected_unauthorized_status");
    assert_eq!(
        status, expected_status,
        "Expected {} for {}",
        spec.expected_unauthorized_status, spec.name
    );
    validators::assert_error(&body);
    // TODO: Error schema requires 'param' field which our errors don't include
    // harness.validate_schema(schema::SchemaId::Error, &body);
}

// =============================================================================
// Provider-Specific Tests
// =============================================================================

/// Test that OpenRouter responses include the cost field in usage
#[tokio::test]
async fn test_openrouter_cost_in_usage() {
    let spec = &OPENROUTER_SPEC;
    let Some(fixture_id) = spec.fixtures.chat_completion_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK);

    // OpenRouter includes cost in usage
    let cost = &body["usage"]["cost"];
    assert!(
        cost.is_number(),
        "OpenRouter response should include cost in usage, got: {:?}",
        body["usage"]
    );
}

// =============================================================================
// Additional Tool Calling Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_tool_call_with_result(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.tool_call_with_result else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Send a request with tool call history and tool result
    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [
                    {"role": "user", "content": "What's the weather in San Francisco?"},
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_abc123",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"San Francisco\",\"unit\":\"celsius\"}"
                            }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_abc123",
                        "content": "{\"temperature\": 18, \"condition\": \"partly cloudy\"}"
                    }
                ],
                "tools": [weather_tool()]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert!(body["choices"][0]["message"]["content"].is_string());
}

// =============================================================================
// Additional Responses API Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_tool_call_parallel(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_tool_call_parallel else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "What's the weather in Tokyo and London?",
                "parallel_tool_calls": true,
                "tools": [responses_weather_tool()]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "response");

    let output = body["output"].as_array().unwrap();
    // Filter to only function_call items (Anthropic may include a message item first)
    let function_calls: Vec<_> = output
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect();

    // Models may return more tool calls than strictly necessary
    assert!(
        function_calls.len() >= 2,
        "Should have at least two parallel tool calls, got {}",
        function_calls.len()
    );
    // Verify all function calls are for get_weather
    for call in &function_calls {
        assert_eq!(call["name"], "get_weather");
    }
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::anthropic(&ANTHROPIC_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_tool_call_with_result(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_tool_call_with_result else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": [
                    {"type": "message", "role": "user", "content": "What's the weather in San Francisco?"},
                    {
                        "type": "function_call",
                        "call_id": "call_responses_recorded_001",
                        "name": "get_weather",
                        "arguments": "{\"location\":\"San Francisco\"}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_responses_recorded_001",
                        "output": "{\"temperature\": 18, \"condition\": \"partly cloudy\"}"
                    }
                ],
                "tools": [responses_weather_tool()]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["object"], "response");

    let output = body["output"].as_array().unwrap();
    assert!(!output.is_empty(), "Should have output");

    // Find the message output (model's response after processing tool result)
    let message_output: Vec<_> = output.iter().filter(|o| o["type"] == "message").collect();
    assert!(
        !message_output.is_empty(),
        "Should have a message response after tool result"
    );
}

// =============================================================================
// Developer Message Role Tests (Reasoning Models)
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_developer_message_role(#[case] spec: &'static ProviderTestSpec) {
    // Developer role is used with reasoning models - reuse reasoning fixture
    let Some(fixture_id) = spec.fixtures.reasoning_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Test that developer messages are handled correctly
    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "o3-mini",
                "messages": [
                    {"role": "developer", "content": "You are a helpful math tutor. Always show your work."},
                    {"role": "user", "content": "What is the 15th prime number?"}
                ],
                "max_completion_tokens": 1000
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

// =============================================================================
// Models List Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_models_list(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.models_list else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness.get_json("/api/v1/models").await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(body["data"].is_array());

    let models = body["data"].as_array().unwrap();
    assert!(!models.is_empty(), "Should have models");
    // Each model should have an id
    assert!(models.iter().all(|m| m["id"].is_string()));
}

// =============================================================================
// Cost Calculation and Usage Headers Tests
// =============================================================================

/// Helper to get header value as i64
fn get_header_i64(headers: &http::HeaderMap, name: &str) -> Option<i64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

/// Test that X-Input-Tokens and X-Output-Tokens headers are set correctly
/// based on the usage data in the fixture response.
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_usage_token_headers(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.chat_completion_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Get expected values from response body
    let expected_input = body["usage"]["prompt_tokens"].as_i64().unwrap();
    let expected_output = body["usage"]["completion_tokens"].as_i64().unwrap();

    // Verify headers match the usage data
    let input_tokens = get_header_i64(&headers, "X-Input-Tokens");
    let output_tokens = get_header_i64(&headers, "X-Output-Tokens");

    assert_eq!(
        input_tokens,
        Some(expected_input),
        "X-Input-Tokens header should match prompt_tokens for {}",
        spec.name
    );
    assert_eq!(
        output_tokens,
        Some(expected_output),
        "X-Output-Tokens header should match completion_tokens for {}",
        spec.name
    );
}

/// Test that X-Reasoning-Tokens header is set for reasoning model responses
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_reasoning_tokens_header(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.reasoning_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/chat/completions",
            json!({
                "model": "o3-mini",
                "messages": [{"role": "user", "content": "What is 2+2?"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Get expected reasoning tokens from response body
    let expected_reasoning = body["usage"]["completion_tokens_details"]["reasoning_tokens"]
        .as_i64()
        .unwrap_or(0);

    // Verify reasoning tokens header is set when > 0
    let reasoning_tokens = get_header_i64(&headers, "X-Reasoning-Tokens");

    assert!(
        expected_reasoning > 0,
        "Fixture should have reasoning_tokens > 0"
    );
    assert_eq!(
        reasoning_tokens,
        Some(expected_reasoning),
        "X-Reasoning-Tokens header should match fixture for {}",
        spec.name
    );
}

/// Test that X-Cost-Microcents header is set when pricing is configured.
/// The test verifies cost calculation based on token counts and configured pricing.
/// Note: Ollama is excluded since local models have no pricing.
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_cost_calculation_header(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.chat_completion_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Get token counts from response
    let input_tokens = body["usage"]["prompt_tokens"].as_i64().unwrap();
    let output_tokens = body["usage"]["completion_tokens"].as_i64().unwrap();

    // Verify cost header is set
    let cost_microcents = get_header_i64(&headers, "X-Cost-Microcents");
    assert!(
        cost_microcents.is_some(),
        "X-Cost-Microcents header should be set for {}",
        spec.name
    );

    // Calculate expected cost based on gpt-4o-mini pricing:
    // $0.15/1M input = 150_000 microcents/1M
    // $0.60/1M output = 600_000 microcents/1M
    // Cost = (input * 150_000 + output * 600_000) / 1_000_000
    let expected_cost = (input_tokens * 150_000 + output_tokens * 600_000) / 1_000_000;

    assert_eq!(
        cost_microcents,
        Some(expected_cost),
        "X-Cost-Microcents should match calculated cost for {} (input={}, output={})",
        spec.name,
        input_tokens,
        output_tokens
    );
}

/// Test that X-Finish-Reason header is set correctly
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_finish_reason_header(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.chat_completion_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/chat/completions",
            json!({
                "model": spec.default_model,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Get expected finish reason from response body
    let expected_reason = body["choices"][0]["finish_reason"].as_str().unwrap();

    // Verify header is set
    let finish_reason = headers.get("X-Finish-Reason").and_then(|v| v.to_str().ok());

    assert_eq!(
        finish_reason,
        Some(expected_reason),
        "X-Finish-Reason header should match response for {}",
        spec.name
    );
}

/// Test cost calculation for reasoning models.
/// Note: In OpenAI's API, reasoning_tokens are INCLUDED in completion_tokens,
/// so we don't double-count them. The reasoning_per_1m_tokens pricing field
/// is only used if a provider bills reasoning tokens at a different rate.
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_reasoning_model_cost_calculation(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.reasoning_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/chat/completions",
            json!({
                "model": "o3-mini",
                "messages": [{"role": "user", "content": "What is 2+2?"}]
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Get token counts from response
    let input_tokens = body["usage"]["prompt_tokens"].as_i64().unwrap();
    let output_tokens = body["usage"]["completion_tokens"].as_i64().unwrap();
    let reasoning_tokens = body["usage"]["completion_tokens_details"]["reasoning_tokens"]
        .as_i64()
        .unwrap_or(0);

    // Verify reasoning tokens are present and included in output tokens
    assert!(
        reasoning_tokens > 0,
        "Fixture should have reasoning_tokens > 0"
    );
    assert!(
        output_tokens > reasoning_tokens,
        "completion_tokens ({}) should include reasoning_tokens ({})",
        output_tokens,
        reasoning_tokens
    );

    // Verify cost header is set
    let cost_microcents = get_header_i64(&headers, "X-Cost-Microcents");
    assert!(
        cost_microcents.is_some(),
        "X-Cost-Microcents header should be set for reasoning model"
    );

    // Calculate expected cost based on o3-mini pricing:
    // $1.10/1M input = 1_100_000 microcents/1M
    // $4.40/1M output = 4_400_000 microcents/1M
    // Note: reasoning_tokens are already included in completion_tokens (output_tokens)
    let expected_cost = (input_tokens * 1_100_000 + output_tokens * 4_400_000) / 1_000_000;

    assert_eq!(
        cost_microcents,
        Some(expected_cost),
        "X-Cost-Microcents should be calculated from input and output tokens for {} \
        (input={}, output={} which includes reasoning={})",
        spec.name,
        input_tokens,
        output_tokens,
        reasoning_tokens
    );
}

/// Test usage headers for Responses API
#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[case::openrouter(&OPENROUTER_SPEC)]
#[case::ollama(&OLLAMA_SPEC)]
#[tokio::test]
async fn test_responses_api_usage_headers(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.responses_success else {
        return;
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, headers, body) = harness
        .post_json_with_headers(
            "/api/v1/responses",
            json!({
                "model": spec.get_responses_model(),
                "input": "Hello"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Responses API uses input_tokens and output_tokens in usage
    let expected_input = body["usage"]["input_tokens"].as_i64().unwrap();
    let expected_output = body["usage"]["output_tokens"].as_i64().unwrap();

    // Verify headers match the usage data
    let input_tokens = get_header_i64(&headers, "X-Input-Tokens");
    let output_tokens = get_header_i64(&headers, "X-Output-Tokens");

    assert_eq!(
        input_tokens,
        Some(expected_input),
        "X-Input-Tokens header should match input_tokens for {}",
        spec.name
    );
    assert_eq!(
        output_tokens,
        Some(expected_output),
        "X-Output-Tokens header should match output_tokens for {}",
        spec.name
    );

    // Verify cost is calculated
    let cost_microcents = get_header_i64(&headers, "X-Cost-Microcents");
    assert!(
        cost_microcents.is_some(),
        "X-Cost-Microcents header should be set for Responses API"
    );
}

// =============================================================================
// Circuit Breaker and Retry Tests
// =============================================================================

use wiremock::Mock;

use crate::providers::test_utils::{SequentialResponder, success_response_from_fixture};

/// Test harness for circuit breaker and retry tests.
/// Unlike E2ETestHarness, this enables circuit breaker and retry.
pub struct ResilienceTestHarness {
    pub app: axum::Router,
    pub mock_server: MockServer,
}

impl ResilienceTestHarness {
    /// Create a new test harness with circuit breaker and retry enabled.
    ///
    /// # Arguments
    /// * `failure_threshold` - Number of failures before circuit opens
    /// * `open_timeout_secs` - Seconds before transitioning to half-open
    /// * `success_threshold` - Successes needed in half-open to close
    /// * `max_retries` - Maximum retry attempts
    /// * `initial_delay_ms` - Initial retry delay in milliseconds
    pub async fn new(
        failure_threshold: u32,
        open_timeout_secs: u64,
        success_threshold: u32,
        max_retries: u32,
        initial_delay_ms: u64,
    ) -> Self {
        let mock_server = MockServer::start().await;
        let app = create_resilience_test_app(
            &mock_server,
            failure_threshold,
            open_timeout_secs,
            success_threshold,
            max_retries,
            initial_delay_ms,
        )
        .await;
        Self { app, mock_server }
    }

    /// POST JSON to the app and return status + JSON body.
    pub async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|e| {
            panic!(
                "Failed to parse response as JSON: {e}\nstatus: {status}\nbody: {}",
                String::from_utf8_lossy(&body_bytes)
            )
        });
        (status, json)
    }
}

/// Create a test app with circuit breaker and retry enabled.
async fn create_resilience_test_app(
    mock_server: &MockServer,
    failure_threshold: u32,
    open_timeout_secs: u64,
    success_threshold: u32,
    max_retries: u32,
    initial_delay_ms: u64,
) -> axum::Router {
    static COUNTER: AtomicU64 = AtomicU64::new(1000);
    let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

    #[cfg(feature = "sso")]
    let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
    #[cfg(not(feature = "sso"))]
    let session_section = "";

    let mock_uri = mock_server.uri();
    let config_str = format!(
        r#"
[database]
type = "sqlite"
path = "file:resilience_test_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "mock-provider"

[providers.mock-provider]
type = "open_ai"
base_url = "{mock_uri}"
api_key = "test-api-key"
timeout_secs = 30
supports_tools = true

# Circuit breaker configuration
[providers.mock-provider.circuit_breaker]
enabled = true
failure_threshold = {failure_threshold}
open_timeout_secs = {open_timeout_secs}
success_threshold = {success_threshold}
failure_status_codes = [500, 502, 503, 504]

# Retry configuration
[providers.mock-provider.retry]
enabled = true
max_retries = {max_retries}
initial_delay_ms = {initial_delay_ms}
max_delay_ms = 1000
backoff_multiplier = 2.0
jitter = 0.0
retryable_status_codes = [429, 500, 502, 503, 504]
"#
    );

    let config = GatewayConfig::parse(&config_str).expect("Failed to parse test config");
    let state = crate::AppState::new(config.clone())
        .await
        .expect("Failed to create AppState");
    crate::build_app(&config, state)
}

/// Test that retry succeeds after intermittent failures.
/// The server fails twice, then succeeds on the third attempt.
#[tokio::test]
async fn test_retry_succeeds_after_failures() {
    // Configure with max_retries=3 (4 total attempts), no circuit breaker interference
    let harness = ResilienceTestHarness::new(
        10, // High threshold so circuit breaker doesn't trip
        60, 1, 3,  // max_retries
        10, // Short delay for fast tests
    )
    .await;

    // Load a success fixture to get a valid response
    let success_fixture = load_fixture(FixtureId::OpenAiChatCompletionSuccess);
    let success_response = success_response_from_fixture(&success_fixture);

    // Create a responder that fails twice, then succeeds
    let responder = SequentialResponder::fail_then_succeed(2, success_response);

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    // Should succeed after retries
    assert_eq!(
        status,
        StatusCode::OK,
        "Request should succeed after retries"
    );
    assert_eq!(body["object"], "chat.completion");

    // Verify the server was called 3 times (2 failures + 1 success)
    assert_eq!(
        responder.call_count(),
        3,
        "Server should have been called 3 times (2 failures + 1 success)"
    );
}

/// Test that retry exhaustion returns an error.
/// The server always fails, and after max_retries the client gives up.
#[tokio::test]
async fn test_retry_exhaustion_returns_error() {
    // Configure with max_retries=2 (3 total attempts), no circuit breaker
    let harness = ResilienceTestHarness::new(
        10, // High threshold
        60, 1, 2,  // max_retries
        10, // Short delay
    )
    .await;

    // Create a responder that always fails
    let responder = SequentialResponder::always_fail();

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    let (status, body) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    // Should return 500 after exhausting retries
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request should fail after exhausting retries"
    );
    validators::assert_error(&body);

    // Verify the server was called max_retries + 1 times
    assert_eq!(
        responder.call_count(),
        3,
        "Server should have been called 3 times (initial + 2 retries)"
    );
}

/// Test that circuit breaker opens after threshold failures.
/// After `failure_threshold` consecutive failures, the circuit opens and rejects requests immediately.
#[tokio::test]
async fn test_circuit_breaker_opens_after_threshold() {
    // Configure with failure_threshold=2, no retries (to isolate circuit breaker behavior)
    let harness = ResilienceTestHarness::new(
        2,  // failure_threshold
        60, // Long timeout so circuit stays open
        1, 0, // No retries
        10,
    )
    .await;

    // Create a responder that always fails
    let responder = SequentialResponder::always_fail();

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    // First failure
    let (status1, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status1, StatusCode::INTERNAL_SERVER_ERROR);

    // Second failure - this trips the circuit
    let (status2, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status2, StatusCode::INTERNAL_SERVER_ERROR);

    // Third request - circuit should be open, rejecting immediately
    let (status3, body3) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(
        status3,
        StatusCode::SERVICE_UNAVAILABLE,
        "Circuit breaker should reject requests when open"
    );
    assert!(
        body3["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Circuit breaker"),
        "Error message should mention circuit breaker"
    );

    // Server should only have been called twice (before circuit opened)
    assert_eq!(
        responder.call_count(),
        2,
        "Server should only be called twice before circuit opens"
    );
}

/// Test that circuit breaker transitions from open to half-open after timeout.
#[tokio::test]
async fn test_circuit_breaker_half_open_transition() {
    // Configure with short timeout for fast test
    let harness = ResilienceTestHarness::new(
        1, // failure_threshold=1 (open after 1 failure)
        1, // open_timeout_secs=1 (transition to half-open after 1 second)
        1, // success_threshold=1 (close after 1 success)
        0, // No retries
        10,
    )
    .await;

    // Load success fixture for recovery
    let success_fixture = load_fixture(FixtureId::OpenAiChatCompletionSuccess);
    let success_response = success_response_from_fixture(&success_fixture);

    // Create a responder that fails once, then succeeds
    let responder = SequentialResponder::fail_then_succeed(1, success_response);

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    // First request fails and opens the circuit
    let (status1, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status1, StatusCode::INTERNAL_SERVER_ERROR);

    // Second request - circuit is open, should be rejected
    let (status2, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(
        status2,
        StatusCode::SERVICE_UNAVAILABLE,
        "Circuit should be open"
    );

    // Wait for timeout to elapse (circuit transitions to half-open)
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // Third request - circuit is half-open, request goes through
    // The responder now returns success, so circuit should close
    let (status3, body3) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

    assert_eq!(
        status3,
        StatusCode::OK,
        "Half-open circuit should allow probe request"
    );
    assert_eq!(body3["object"], "chat.completion");

    // Fourth request - circuit should be closed now
    let (status4, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(
        status4,
        StatusCode::OK,
        "Circuit should be closed after successful probe"
    );

    // Server should have been called 3 times total:
    // 1. Initial failure (opens circuit)
    // 2. Probe in half-open (success, closes circuit)
    // 3. Normal request after close
    assert_eq!(responder.call_count(), 3);
}

/// Test that circuit breaker reopens if probe fails in half-open state.
#[tokio::test]
async fn test_circuit_breaker_reopens_on_half_open_failure() {
    let harness = ResilienceTestHarness::new(
        1, // failure_threshold=1
        1, // open_timeout_secs=1
        1, // success_threshold=1
        0, // No retries
        10,
    )
    .await;

    // Create a responder that always fails
    let responder = SequentialResponder::always_fail();

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    // First request fails and opens the circuit
    let (status1, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status1, StatusCode::INTERNAL_SERVER_ERROR);

    // Wait for timeout to elapse
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // Probe request in half-open - fails, circuit reopens
    let (status2, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(
        status2,
        StatusCode::INTERNAL_SERVER_ERROR,
        "Probe should fail"
    );

    // Next request - circuit is open again
    let (status3, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(
        status3,
        StatusCode::SERVICE_UNAVAILABLE,
        "Circuit should be open again after failed probe"
    );

    // Server was called twice (initial failure + probe failure)
    assert_eq!(responder.call_count(), 2);
}

/// Test that retry and circuit breaker work together correctly.
/// Circuit breaker records one failure per request (after all retries exhaust).
#[tokio::test]
async fn test_retry_with_circuit_breaker_integration() {
    // Configure: 2 retries (3 attempts total per request), circuit opens after 2 request failures
    let harness = ResilienceTestHarness::new(
        2, // failure_threshold (2 failed requests)
        60, 1, 2,  // max_retries (3 HTTP attempts per request)
        10, // Short delay
    )
    .await;

    // Create a responder that always fails
    let responder = SequentialResponder::always_fail();

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(responder.clone())
        .mount(&harness.mock_server)
        .await;

    // First request: fails after 3 HTTP attempts (initial + 2 retries)
    // Circuit breaker records 1 failure
    let (status1, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status1, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        responder.call_count(),
        3,
        "First request should have 3 HTTP attempts"
    );

    // Second request: fails after 3 more HTTP attempts
    // Circuit breaker records 2nd failure -> circuit opens
    let (status2, _) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status2, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        responder.call_count(),
        6,
        "Two requests = 6 total HTTP attempts"
    );

    // Third request: circuit is open, rejected immediately without hitting server
    let (status3, body3) = harness
        .post_json(
            "/api/v1/chat/completions",
            json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;
    assert_eq!(status3, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body3["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Circuit breaker")
    );

    // Server call count unchanged (circuit prevented request)
    assert_eq!(responder.call_count(), 6);
}

// =============================================================================
// Image Generation Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_image_generation_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.image_generation_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let (status, body) = harness
        .post_json(
            "/api/v1/images/generations",
            json!({
                "model": "dall-e-3",
                "prompt": "A cute baby sea otter",
                "n": 1,
                "size": "1024x1024"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);

    // Validate response structure
    assert!(
        body.get("created").is_some(),
        "Response should have 'created' field"
    );
    assert!(
        body.get("data").is_some(),
        "Response should have 'data' field"
    );

    let data = body["data"].as_array().expect("data should be an array");
    assert!(!data.is_empty(), "data array should not be empty");

    // Each image should have either url or b64_json
    for image in data {
        assert!(
            image.get("url").is_some() || image.get("b64_json").is_some(),
            "Image should have url or b64_json"
        );
    }
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_image_edit_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.image_edit_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Image edit requires multipart form data
    // We'll test that the endpoint accepts the request and returns proper response
    let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
    let body = format!(
        "--{boundary}\r\n\
        Content-Disposition: form-data; name=\"image\"; filename=\"test.png\"\r\n\
        Content-Type: image/png\r\n\r\n\
        {}\r\n\
        --{boundary}\r\n\
        Content-Disposition: form-data; name=\"prompt\"\r\n\r\n\
        A cute baby sea otter wearing a beret\r\n\
        --{boundary}\r\n\
        Content-Disposition: form-data; name=\"model\"\r\n\r\n\
        dall-e-2\r\n\
        --{boundary}--\r\n",
        TINY_RED_PNG_BASE64, // Using the tiny PNG from vision tests
        boundary = boundary
    );

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/images/edits")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap();

    let response = harness.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(
        json.get("created").is_some(),
        "Response should have 'created' field"
    );
    assert!(
        json.get("data").is_some(),
        "Response should have 'data' field"
    );
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_image_variation_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.image_variation_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Image variation requires multipart form data
    let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
    let body = format!(
        "--{boundary}\r\n\
        Content-Disposition: form-data; name=\"image\"; filename=\"test.png\"\r\n\
        Content-Type: image/png\r\n\r\n\
        {}\r\n\
        --{boundary}\r\n\
        Content-Disposition: form-data; name=\"model\"\r\n\r\n\
        dall-e-2\r\n\
        --{boundary}--\r\n",
        TINY_RED_PNG_BASE64,
        boundary = boundary
    );

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/images/variations")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap();

    let response = harness.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(
        json.get("created").is_some(),
        "Response should have 'created' field"
    );
    assert!(
        json.get("data").is_some(),
        "Response should have 'data' field"
    );
}

// =============================================================================
// Audio Tests
// =============================================================================

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_audio_speech_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.audio_speech_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/audio/speech")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "model": "tts-1",
                "input": "Hello, this is a test.",
                "voice": "alloy"
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = harness.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    assert!(
        content_type.starts_with("audio/"),
        "Expected audio content type, got: {}",
        content_type
    );

    // Verify we got non-trivial audio bytes back (not just headers).
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        body_bytes.len() > 32,
        "Audio response too small ({} bytes) for {}",
        body_bytes.len(),
        spec.name
    );
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_audio_transcription_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.audio_transcription_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Audio transcription requires multipart form data with an audio file
    let boundary = "----WebKitFormBoundaryAudio";
    // Minimal fake audio bytes - just enough to pass multipart parsing
    let fake_audio: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00";

    // Build multipart body manually with binary audio data
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(fake_audio);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(b"whisper-1\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/audio/transcriptions")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap();

    let response = harness.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .expect("Transcription response should have a string 'text' field");
    assert!(
        !text.trim().is_empty(),
        "Transcription text must be non-empty for {}",
        spec.name
    );
}

#[rstest]
#[case::openai(&OPENAI_SPEC)]
#[tokio::test]
async fn test_audio_translation_success(#[case] spec: &'static ProviderTestSpec) {
    let Some(fixture_id) = spec.fixtures.audio_translation_success else {
        return; // Provider doesn't support this endpoint
    };

    let harness = E2ETestHarness::new(spec).await;
    harness.mount_fixture(fixture_id, 1).await;

    // Audio translation requires multipart form data with an audio file
    let boundary = "----WebKitFormBoundaryAudioTranslate";
    // Minimal fake audio bytes - just enough to pass multipart parsing
    let fake_audio: &[u8] = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00";

    // Build multipart body manually with binary audio data
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(fake_audio);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(b"whisper-1\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/audio/translations")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(Body::from(body))
        .unwrap();

    let response = harness.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::OK, "Expected OK for {}", spec.name);
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .expect("Translation response should have a string 'text' field");
    assert!(
        !text.trim().is_empty(),
        "Translation text must be non-empty for {}",
        spec.name
    );
}
