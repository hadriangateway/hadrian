//! Test provider implementation with configurable failure modes.
//!
//! This module provides a mock provider for testing the gateway without external dependencies.
//! It supports various failure modes for testing fallback behavior, circuit breakers, and error handling.

use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    response::Response,
};
use http::{StatusCode, header::CONTENT_TYPE};
use serde_json::json;

use crate::{
    api_types::{
        CreateChatCompletionPayload, CreateCompletionPayload, CreateEmbeddingPayload,
        CreateResponsesPayload,
        audio::{CreateSpeechRequest, CreateTranscriptionRequest, CreateTranslationRequest},
        images::{
            CreateImageEditRequest, CreateImageRequest, CreateImageVariationRequest, Image,
            ImagesResponse,
        },
    },
    config::TestFailureMode,
    providers::{ModelInfo, ModelsResponse, Provider, ProviderError},
};

/// A test provider that returns generic responses without making real API calls.
/// Useful for testing the gateway API without external dependencies.
///
/// Supports configurable failure modes via `TestFailureMode`:
/// - `None`: Normal operation (default)
/// - `HttpError`: Return a specific HTTP status code
/// - `ConnectionError`: Simulate a network connection error
/// - `Timeout`: Wait for a duration before timing out
/// - `FailAfterN`: Succeed N times, then start failing
pub struct TestProvider {
    model_name: String,
    failure_mode: TestFailureMode,
    /// Request counter for FailAfterN mode
    request_count: AtomicU32,
}

impl TestProvider {
    /// Create a new test provider with the specified model name.
    /// Uses default failure mode (None - normal operation).
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            failure_mode: TestFailureMode::None,
            request_count: AtomicU32::new(0),
        }
    }

    /// Create a new test provider with the specified model name and failure mode.
    #[cfg(test)]
    pub fn with_failure_mode(model_name: impl Into<String>, failure_mode: TestFailureMode) -> Self {
        Self {
            model_name: model_name.into(),
            failure_mode,
            request_count: AtomicU32::new(0),
        }
    }

    /// Create a test provider from configuration.
    pub fn from_config(config: &crate::config::TestProviderConfig) -> Self {
        Self {
            model_name: config.model_name.clone(),
            failure_mode: config.failure_mode.clone(),
            request_count: AtomicU32::new(0),
        }
    }

    /// Check the failure mode and apply it.
    /// Returns `Ok(Some(response))` if an HTTP error response should be returned,
    /// `Ok(None)` if the request should proceed normally,
    /// or `Err` for connection/timeout errors.
    async fn apply_failure_mode(&self) -> Result<Option<Response>, ProviderError> {
        match &self.failure_mode {
            TestFailureMode::None => Ok(None),

            TestFailureMode::HttpError {
                status_code,
                message,
            } => {
                let status =
                    StatusCode::from_u16(*status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let msg = message
                    .clone()
                    .unwrap_or_else(|| format!("Test provider error: {}", status_code));

                Ok(Some(
                    Response::builder()
                        .status(status)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({
                                "error": {
                                    "message": msg,
                                    "type": "test_error",
                                    "code": status_code
                                }
                            })
                            .to_string(),
                        ))
                        .unwrap_or_else(|_| {
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Body::empty())
                                .unwrap()
                        }),
                ))
            }

            TestFailureMode::ConnectionError { message } => Err(ProviderError::Internal(format!(
                "Connection error: {}",
                message
            ))),

            TestFailureMode::Timeout { delay_ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                Err(ProviderError::Internal("Request timed out".to_string()))
            }

            TestFailureMode::FailAfterN {
                success_count,
                failure_status,
            } => {
                let count = self.request_count.fetch_add(1, Ordering::SeqCst);
                if count >= *success_count {
                    let status = StatusCode::from_u16(*failure_status)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    Ok(Some(
                        Response::builder()
                            .status(status)
                            .header(CONTENT_TYPE, "application/json")
                            .body(Body::from(
                                json!({
                                    "error": {
                                        "message": format!("FailAfterN triggered (request {} of {})", count + 1, success_count),
                                        "type": "test_error",
                                        "code": failure_status
                                    }
                                })
                                .to_string(),
                            ))
                            .unwrap_or_else(|_| {
                                Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Body::empty())
                                    .unwrap()
                            }),
                    ))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

fn generate_id() -> String {
    format!("test-{}", uuid::Uuid::new_v4())
}

fn current_timestamp() -> i64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as i64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}

fn build_json_response(body: serde_json::Value) -> Result<Response, ProviderError> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))?)
}

fn build_stream_response(chunks: Vec<String>) -> Result<Response, ProviderError> {
    let stream_body = chunks.join("");
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from(stream_body))?)
}

/// Check if the model name is a magic error model that should trigger a specific HTTP error.
/// Magic model names allow tests to trigger specific error responses without config changes.
///
/// Returns `Some(Response)` if the model is a magic error model, `None` otherwise.
fn check_magic_error_model(model: &str) -> Option<Result<Response, ProviderError>> {
    // Extract the model suffix after "test/" if present
    let model_suffix = model.strip_prefix("test/").unwrap_or(model);

    match model_suffix {
        "error-500" => Some(build_error_response(500, "Internal Server Error")),
        "error-503" => Some(build_error_response(503, "Service Unavailable")),
        "error-429" => Some(build_error_response(429, "Too Many Requests")),
        "error-502" => Some(build_error_response(502, "Bad Gateway")),
        "error-504" => Some(build_error_response(504, "Gateway Timeout")),
        _ => None,
    }
}

/// Build an error response with the specified status code and message.
fn build_error_response(status_code: u16, message: &str) -> Result<Response, ProviderError> {
    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    Ok(Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "error": {
                    "message": message,
                    "type": "test_error",
                    "code": status_code
                }
            })
            .to_string(),
        ))?)
}

/// Generate word-based embeddings for semantic search testing.
///
/// This creates embeddings where texts sharing words have positive cosine similarity,
/// enabling vector search to work with test embeddings. Each word activates a
/// subset of dimensions based on its hash, creating a sparse-like representation.
fn generate_word_based_embedding(text: &str, dims: usize) -> Vec<f64> {
    use std::hash::{Hash, Hasher};

    // Normalize and tokenize: lowercase, split on non-alphanumeric, filter short words
    let text_lower = text.to_lowercase();
    let words: Vec<&str> = text_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .collect();

    if words.is_empty() {
        // Return a zero vector for empty input
        return vec![0.0; dims];
    }

    // Accumulate word contributions
    let mut embedding = vec![0.0; dims];

    // Each word activates ~10% of dimensions with positive values
    let active_dims = (dims / 10).max(8);

    for word in &words {
        // Hash the word to get a seed
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        word.hash(&mut hasher);
        let word_hash = hasher.finish();

        // Activate a subset of dimensions for this word
        for k in 0..active_dims {
            // Determine which dimension to activate
            let dim_idx =
                ((word_hash.wrapping_mul(k as u64 + 1).wrapping_add(k as u64)) as usize) % dims;
            // Add a positive contribution (words always contribute positively)
            embedding[dim_idx] += 1.0;
        }
    }

    // Normalize to unit length for cosine similarity
    let magnitude: f64 = embedding.iter().map(|x| x * x).sum::<f64>().sqrt();
    if magnitude > 0.0 {
        for val in &mut embedding {
            *val /= magnitude;
        }
    }

    embedding
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Provider for TestProvider {
    fn default_health_check_model(&self) -> Option<&str> {
        Some("test-model")
    }

    async fn create_chat_completion(
        &self,
        _client: &reqwest::Client,
        payload: CreateChatCompletionPayload,
    ) -> Result<Response, ProviderError> {
        let model = payload
            .model
            .as_deref()
            .unwrap_or(&self.model_name)
            .to_string();

        // Check for magic error models (test/error-500, test/error-503, etc.)
        if let Some(error_response) = check_magic_error_model(&model) {
            return error_response;
        }

        // Check failure mode - may return error response, ProviderError, or proceed normally
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        if payload.stream {
            let id = generate_id();
            let chunks = vec![
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": current_timestamp(),
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": "" },
                            "finish_reason": null
                        }]
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": current_timestamp(),
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": { "content": "This is a test response from the test provider." },
                            "finish_reason": null
                        }]
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": current_timestamp(),
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 10,
                            "completion_tokens": 10,
                            "total_tokens": 20
                        }
                    })
                ),
                "data: [DONE]\n\n".to_string(),
            ];
            build_stream_response(chunks)
        } else {
            build_json_response(json!({
                "id": generate_id(),
                "object": "chat.completion",
                "created": current_timestamp(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "This is a test response from the test provider."
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 10,
                    "total_tokens": 20
                }
            }))
        }
    }

    async fn create_responses(
        &self,
        _client: &reqwest::Client,
        payload: CreateResponsesPayload,
    ) -> Result<Response, ProviderError> {
        let model = payload
            .model
            .as_deref()
            .unwrap_or(&self.model_name)
            .to_string();

        // Check for magic error models (test/error-500, test/error-503, etc.)
        if let Some(error_response) = check_magic_error_model(&model) {
            return error_response;
        }

        // Check failure mode - may return error response, ProviderError, or proceed normally
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        if payload.stream {
            let response_id = generate_id();
            let message_id = generate_id();
            let timestamp = current_timestamp();

            let chunks = vec![
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.created",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "created_at": timestamp,
                            "model": model,
                            "status": "in_progress",
                            "output": []
                        }
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {
                            "type": "message",
                            "id": message_id,
                            "role": "assistant",
                            "content": []
                        }
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.content_part.added",
                        "item_id": message_id,
                        "output_index": 0,
                        "content_index": 0,
                        "part": {
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                            "logprobs": []
                        }
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.output_text.delta",
                        "item_id": message_id,
                        "output_index": 0,
                        "content_index": 0,
                        "delta": "This is a test response from the test provider."
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": message_id,
                        "output_index": 0,
                        "content_index": 0,
                        "text": "This is a test response from the test provider."
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {
                            "type": "message",
                            "id": message_id,
                            "role": "assistant",
                            "status": "completed",
                            "content": [{
                                "type": "output_text",
                                "text": "This is a test response from the test provider.",
                                "annotations": [],
                                "logprobs": []
                            }]
                        }
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "created_at": timestamp,
                            "model": model,
                            "status": "completed",
                            "output": [{
                                "type": "message",
                                "id": message_id,
                                "role": "assistant",
                                "status": "completed",
                                "content": [{
                                    "type": "output_text",
                                    "text": "This is a test response from the test provider.",
                                    "annotations": [],
                                    "logprobs": []
                                }]
                            }],
                            "usage": {
                                "input_tokens": 10,
                                "input_tokens_details": { "cached_tokens": 0 },
                                "output_tokens": 10,
                                "output_tokens_details": { "reasoning_tokens": 0 },
                                "total_tokens": 20
                            }
                        }
                    })
                ),
            ];
            build_stream_response(chunks)
        } else {
            let response_id = generate_id();
            let message_id = generate_id();

            build_json_response(json!({
                "id": response_id,
                "object": "response",
                "created_at": current_timestamp(),
                "model": model,
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": message_id,
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": "This is a test response from the test provider.",
                        "annotations": []
                    }]
                }],
                "output_text": "This is a test response from the test provider.",
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens": 10,
                    "output_tokens_details": { "reasoning_tokens": 0 },
                    "total_tokens": 20
                }
            }))
        }
    }

    async fn create_completion(
        &self,
        _client: &reqwest::Client,
        payload: CreateCompletionPayload,
    ) -> Result<Response, ProviderError> {
        // Check failure mode - may return error response, ProviderError, or proceed normally
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        let model = payload
            .model
            .as_deref()
            .unwrap_or(&self.model_name)
            .to_string();

        if payload.stream {
            let id = generate_id();
            let chunks = vec![
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": id,
                        "object": "text_completion",
                        "created": current_timestamp(),
                        "model": model,
                        "choices": [{
                            "text": "This is a test completion from the test provider.",
                            "index": 0,
                            "logprobs": null,
                            "finish_reason": null
                        }]
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": id,
                        "object": "text_completion",
                        "created": current_timestamp(),
                        "model": model,
                        "choices": [{
                            "text": "",
                            "index": 0,
                            "logprobs": null,
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 5,
                            "completion_tokens": 10,
                            "total_tokens": 15
                        }
                    })
                ),
                "data: [DONE]\n\n".to_string(),
            ];
            build_stream_response(chunks)
        } else {
            build_json_response(json!({
                "id": generate_id(),
                "object": "text_completion",
                "created": current_timestamp(),
                "model": model,
                "choices": [{
                    "text": "This is a test completion from the test provider.",
                    "index": 0,
                    "logprobs": null,
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 5,
                    "completion_tokens": 10,
                    "total_tokens": 15
                }
            }))
        }
    }

    async fn create_embedding(
        &self,
        _client: &reqwest::Client,
        payload: CreateEmbeddingPayload,
    ) -> Result<Response, ProviderError> {
        // Check failure mode - may return error response, ProviderError, or proceed normally
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        // Use requested dimensions or default to 1536 (OpenAI's ada-002 default)
        let dims = payload.dimensions.unwrap_or(1536) as usize;

        // Collect input texts. Batch (`TextArray`) inputs produce one
        // embedding per element — mirroring real embedding APIs — so
        // callers like Hadrian-side tool search can embed a catalog in
        // one request.
        let texts: Vec<String> = match &payload.input {
            crate::api_types::embeddings::EmbeddingInput::Text(t) => vec![t.clone()],
            crate::api_types::embeddings::EmbeddingInput::TextArray(arr) => arr.clone(),
            crate::api_types::embeddings::EmbeddingInput::Tokens(_) => vec![String::new()],
            crate::api_types::embeddings::EmbeddingInput::TokenArrays(_) => vec![String::new()],
            crate::api_types::embeddings::EmbeddingInput::Multimodal(_) => vec![String::new()],
        };

        // Generate word-based embeddings so texts sharing words have positive cosine similarity.
        // This enables semantic search testing without a real embedding provider.
        let data: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                json!({
                    "object": "embedding",
                    "embedding": generate_word_based_embedding(text, dims),
                    "index": i
                })
            })
            .collect();

        build_json_response(json!({
            "object": "list",
            "data": data,
            "model": payload.model,
            "usage": {
                "prompt_tokens": 8,
                "total_tokens": 8
            }
        }))
    }

    async fn list_models(
        &self,
        _client: &reqwest::Client,
    ) -> Result<ModelsResponse, ProviderError> {
        // Apply failure mode for testing health checks and error handling
        match &self.failure_mode {
            TestFailureMode::None => {}
            TestFailureMode::ConnectionError { message } => {
                return Err(ProviderError::Internal(format!(
                    "Connection error: {}",
                    message
                )));
            }
            TestFailureMode::Timeout { delay_ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                return Err(ProviderError::Internal("Request timed out".to_string()));
            }
            TestFailureMode::HttpError { status_code, .. }
            | TestFailureMode::FailAfterN {
                failure_status: status_code,
                ..
            } => {
                // For list_models, HTTP errors become ProviderError::Internal
                // since we can't return an HTTP response from this method
                return Err(ProviderError::Internal(format!(
                    "HTTP error: {}",
                    status_code
                )));
            }
        }

        Ok(ModelsResponse {
            data: vec![
                ModelInfo {
                    id: "test-model".to_string(),
                    extra: json!({
                        "object": "model",
                        "created": current_timestamp(),
                        "owned_by": "test-provider"
                    }),
                },
                ModelInfo {
                    id: "test-model-large".to_string(),
                    extra: json!({
                        "object": "model",
                        "created": current_timestamp(),
                        "owned_by": "test-provider"
                    }),
                },
            ],
        })
    }

    async fn create_image(
        &self,
        _client: &reqwest::Client,
        payload: CreateImageRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        // Check failure mode
        if let Some(_error_response) = self.apply_failure_mode().await? {
            return Err(ProviderError::Internal("Test failure mode active".into()));
        }

        let n = payload.n.unwrap_or(1) as usize;
        let data: Vec<Image> = (0..n)
            .map(|i| Image {
                url: Some(format!(
                    "https://test-provider.example.com/images/generated_{}.png",
                    i
                )),
                b64_json: None,
                revised_prompt: Some(format!("Test revised prompt for: {}", payload.prompt)),
            })
            .collect();

        Ok(ImagesResponse {
            created: current_timestamp(),
            data: Some(data),
            background: None,
            output_format: Some("png".to_string()),
            size: Some("1024x1024".to_string()),
            quality: Some("standard".to_string()),
            usage: None,
        })
    }

    async fn create_image_edit(
        &self,
        _client: &reqwest::Client,
        _image: Bytes,
        _mask: Option<Bytes>,
        request: CreateImageEditRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        // Check failure mode
        if let Some(_error_response) = self.apply_failure_mode().await? {
            return Err(ProviderError::Internal("Test failure mode active".into()));
        }

        let n = request.n.unwrap_or(1) as usize;
        let data: Vec<Image> = (0..n)
            .map(|i| Image {
                url: Some(format!(
                    "https://test-provider.example.com/images/edited_{}.png",
                    i
                )),
                b64_json: None,
                revised_prompt: None,
            })
            .collect();

        Ok(ImagesResponse {
            created: current_timestamp(),
            data: Some(data),
            background: None,
            output_format: Some("png".to_string()),
            size: Some("1024x1024".to_string()),
            quality: None,
            usage: None,
        })
    }

    async fn create_image_variation(
        &self,
        _client: &reqwest::Client,
        _image: Bytes,
        request: CreateImageVariationRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        // Check failure mode
        if let Some(_error_response) = self.apply_failure_mode().await? {
            return Err(ProviderError::Internal("Test failure mode active".into()));
        }

        let n = request.n.unwrap_or(1) as usize;
        let data: Vec<Image> = (0..n)
            .map(|i| Image {
                url: Some(format!(
                    "https://test-provider.example.com/images/variation_{}.png",
                    i
                )),
                b64_json: None,
                revised_prompt: None,
            })
            .collect();

        Ok(ImagesResponse {
            created: current_timestamp(),
            data: Some(data),
            background: None,
            output_format: Some("png".to_string()),
            size: Some("1024x1024".to_string()),
            quality: None,
            usage: None,
        })
    }

    async fn create_speech(
        &self,
        _client: &reqwest::Client,
        payload: CreateSpeechRequest,
    ) -> Result<Response, ProviderError> {
        // Check failure mode
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        // Generate a minimal valid MP3 file (silent audio)
        // This is a valid MP3 frame header + minimal data for testing
        let mock_audio: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, // MP3 frame header (MPEG1 Layer3, 128kbps, 44100Hz)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Padding
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];

        // Determine content type based on requested format
        let content_type = match payload.response_format {
            Some(crate::api_types::audio::SpeechResponseFormat::Mp3) | None => "audio/mpeg",
            Some(crate::api_types::audio::SpeechResponseFormat::Opus) => "audio/opus",
            Some(crate::api_types::audio::SpeechResponseFormat::Aac) => "audio/aac",
            Some(crate::api_types::audio::SpeechResponseFormat::Flac) => "audio/flac",
            Some(crate::api_types::audio::SpeechResponseFormat::Wav) => "audio/wav",
            Some(crate::api_types::audio::SpeechResponseFormat::Pcm) => "audio/pcm",
        };

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(mock_audio))?)
    }

    async fn create_transcription(
        &self,
        _client: &reqwest::Client,
        _file: Bytes,
        _filename: String,
        request: CreateTranscriptionRequest,
    ) -> Result<Response, ProviderError> {
        // Check failure mode
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        // Return response based on requested format
        match request.response_format {
            Some(crate::api_types::audio::AudioResponseFormat::Text) => Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(Body::from(
                    "This is a test transcription from the test provider.",
                ))?),
            Some(crate::api_types::audio::AudioResponseFormat::Srt) => {
                let srt = "1\n00:00:00,000 --> 00:00:03,000\nThis is a test transcription from the test provider.\n";
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(Body::from(srt))?)
            }
            Some(crate::api_types::audio::AudioResponseFormat::Vtt) => {
                let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:03.000\nThis is a test transcription from the test provider.\n";
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/vtt")
                    .body(Body::from(vtt))?)
            }
            Some(crate::api_types::audio::AudioResponseFormat::VerboseJson) => {
                build_json_response(json!({
                    "task": "transcribe",
                    "language": "english",
                    "duration": 3.0,
                    "text": "This is a test transcription from the test provider.",
                    "words": [
                        {"word": "This", "start": 0.0, "end": 0.2},
                        {"word": "is", "start": 0.2, "end": 0.3},
                        {"word": "a", "start": 0.3, "end": 0.4},
                        {"word": "test", "start": 0.4, "end": 0.6},
                        {"word": "transcription", "start": 0.6, "end": 1.2},
                        {"word": "from", "start": 1.2, "end": 1.4},
                        {"word": "the", "start": 1.4, "end": 1.5},
                        {"word": "test", "start": 1.5, "end": 1.7},
                        {"word": "provider", "start": 1.7, "end": 2.2}
                    ],
                    "segments": [{
                        "id": 0,
                        "seek": 0,
                        "start": 0.0,
                        "end": 3.0,
                        "text": "This is a test transcription from the test provider.",
                        "tokens": [50364, 1212, 307, 257, 1500, 1107],
                        "temperature": 0.0,
                        "avg_logprob": -0.25,
                        "compression_ratio": 1.1,
                        "no_speech_prob": 0.01
                    }]
                }))
            }
            Some(crate::api_types::audio::AudioResponseFormat::DiarizedJson) => {
                build_json_response(json!({
                    "task": "transcribe",
                    "duration": 3.0,
                    "text": "This is a test transcription from the test provider.",
                    "segments": [{
                        "type": "transcript.text.segment",
                        "id": "seg_001",
                        "start": 0.0,
                        "end": 3.0,
                        "text": "This is a test transcription from the test provider.",
                        "speaker": "speaker_1"
                    }]
                }))
            }
            // Default to JSON format
            Some(crate::api_types::audio::AudioResponseFormat::Json) | None => {
                build_json_response(json!({
                    "text": "This is a test transcription from the test provider."
                }))
            }
        }
    }

    async fn create_translation(
        &self,
        _client: &reqwest::Client,
        _file: Bytes,
        _filename: String,
        request: CreateTranslationRequest,
    ) -> Result<Response, ProviderError> {
        // Check failure mode
        if let Some(error_response) = self.apply_failure_mode().await? {
            return Ok(error_response);
        }

        // Return response based on requested format
        match request.response_format {
            Some(crate::api_types::audio::AudioResponseFormat::Text) => Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain")
                .body(Body::from(
                    "This is a test translation to English from the test provider.",
                ))?),
            Some(crate::api_types::audio::AudioResponseFormat::Srt) => {
                let srt = "1\n00:00:00,000 --> 00:00:03,000\nThis is a test translation to English from the test provider.\n";
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/plain")
                    .body(Body::from(srt))?)
            }
            Some(crate::api_types::audio::AudioResponseFormat::Vtt) => {
                let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:03.000\nThis is a test translation to English from the test provider.\n";
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "text/vtt")
                    .body(Body::from(vtt))?)
            }
            Some(crate::api_types::audio::AudioResponseFormat::VerboseJson) => {
                build_json_response(json!({
                    "language": "english",
                    "duration": 3.0,
                    "text": "This is a test translation to English from the test provider.",
                    "segments": [{
                        "id": 0,
                        "seek": 0,
                        "start": 0.0,
                        "end": 3.0,
                        "text": "This is a test translation to English from the test provider.",
                        "tokens": [50364, 1212, 307, 257, 1500, 1107],
                        "temperature": 0.0,
                        "avg_logprob": -0.25,
                        "compression_ratio": 1.1,
                        "no_speech_prob": 0.01
                    }]
                }))
            }
            // DiarizedJson doesn't apply to translations, fall through to JSON
            Some(crate::api_types::audio::AudioResponseFormat::DiarizedJson)
            | Some(crate::api_types::audio::AudioResponseFormat::Json)
            | None => build_json_response(json!({
                "text": "This is a test translation to English from the test provider."
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{Message, MessageContent};

    fn make_chat_payload(stream: bool) -> CreateChatCompletionPayload {
        CreateChatCompletionPayload {
            messages: vec![Message::User {
                content: MessageContent::Text("Hello".to_string()),
                name: None,
            }],
            model: Some("test-model".to_string()),
            stream,
            models: None,
            frequency_penalty: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            max_completion_tokens: None,
            max_tokens: None,
            metadata: None,
            presence_penalty: None,
            reasoning: None,
            response_format: None,
            seed: None,
            stop: None,
            stream_options: None,
            temperature: None,
            tool_choice: None,
            tools: None,
            top_p: None,
            user: None,
            sovereignty_requirements: None,
        }
    }

    #[tokio::test]
    async fn test_chat_completion_non_streaming() {
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();

        let response = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_chat_completion_streaming() {
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();

        let response = provider
            .create_chat_completion(&client, make_chat_payload(true))
            .await;
        assert!(response.is_ok());

        let response = response.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_list_models() {
        let provider = TestProvider::new("test-model");
        let client = reqwest::Client::new();

        let models = provider.list_models(&client).await;
        assert!(models.is_ok());

        let models = models.unwrap();
        assert_eq!(models.data.len(), 2);
        assert_eq!(models.data[0].id, "test-model");
    }

    #[tokio::test]
    async fn test_http_error_failure_mode() {
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::HttpError {
                status_code: 503,
                message: Some("Service Unavailable".to_string()),
            },
        );
        let client = reqwest::Client::new();

        let response = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;

        // Should return Ok with error status code
        assert!(response.is_ok());
        let response = response.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_connection_error_failure_mode() {
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::ConnectionError {
                message: "Connection refused".to_string(),
            },
        );
        let client = reqwest::Client::new();

        let response = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;

        // Should return ProviderError
        assert!(response.is_err());
        let err = response.unwrap_err();
        assert!(err.to_string().contains("Connection error"));
    }

    #[tokio::test]
    async fn test_fail_after_n_failure_mode() {
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::FailAfterN {
                success_count: 2,
                failure_status: 500,
            },
        );
        let client = reqwest::Client::new();

        // First 2 requests should succeed
        let response1 = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        assert!(response1.is_ok());
        assert_eq!(response1.unwrap().status(), StatusCode::OK);

        let response2 = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        assert!(response2.is_ok());
        assert_eq!(response2.unwrap().status(), StatusCode::OK);

        // Third request should fail with 500
        let response3 = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        assert!(response3.is_ok());
        assert_eq!(
            response3.unwrap().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // Fourth request should also fail
        let response4 = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        assert!(response4.is_ok());
        assert_eq!(
            response4.unwrap().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn test_timeout_failure_mode() {
        let provider = TestProvider::with_failure_mode(
            "test-model",
            TestFailureMode::Timeout { delay_ms: 10 }, // Short delay for test
        );
        let client = reqwest::Client::new();

        let start = std::time::Instant::now();
        let response = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;
        let elapsed = start.elapsed();

        // Should return ProviderError after delay
        assert!(response.is_err());
        assert!(elapsed.as_millis() >= 10);
        let err = response.unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_from_config() {
        let config = crate::config::TestProviderConfig {
            model_name: "my-model".to_string(),
            failure_mode: TestFailureMode::HttpError {
                status_code: 429,
                message: Some("Rate limited".to_string()),
            },
            timeout_secs: 60,
            allowed_models: vec![],
            model_aliases: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            retry: Default::default(),
            circuit_breaker: Default::default(),
            fallback_providers: vec![],
            model_fallbacks: std::collections::HashMap::new(),
            health_check: Default::default(),
            catalog_provider: None,
            sovereignty: None,
        };

        let provider = TestProvider::from_config(&config);
        let client = reqwest::Client::new();

        let response = provider
            .create_chat_completion(&client, make_chat_payload(false))
            .await;

        assert!(response.is_ok());
        assert_eq!(response.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_word_based_embedding_similarity() {
        // Helper to compute cosine similarity
        fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
            let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            let mag_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
            let mag_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
            if mag_a == 0.0 || mag_b == 0.0 {
                0.0
            } else {
                dot / (mag_a * mag_b)
            }
        }

        let dims = 256;

        // Document about machine learning
        let doc = "Machine learning is a subset of artificial intelligence that enables computers to learn from data";
        let doc_embedding = generate_word_based_embedding(doc, dims);

        // Query with shared words
        let query = "what is machine learning?";
        let query_embedding = generate_word_based_embedding(query, dims);

        // Unrelated text
        let unrelated = "The weather today is sunny and warm";
        let unrelated_embedding = generate_word_based_embedding(unrelated, dims);

        let sim_related = cosine_similarity(&doc_embedding, &query_embedding);
        let sim_unrelated = cosine_similarity(&doc_embedding, &unrelated_embedding);

        // Texts sharing words should have positive similarity
        assert!(
            sim_related > 0.0,
            "Related texts should have positive similarity, got {}",
            sim_related
        );

        // Related texts should be more similar than unrelated
        assert!(
            sim_related > sim_unrelated,
            "Related texts ({}) should be more similar than unrelated ({})",
            sim_related,
            sim_unrelated
        );

        // Same text should have similarity of 1.0
        let same_sim = cosine_similarity(&doc_embedding, &doc_embedding);
        assert!(
            (same_sim - 1.0).abs() < 0.0001,
            "Same text should have similarity ~1.0, got {}",
            same_sim
        );
    }

    #[test]
    fn test_word_based_embedding_deterministic() {
        let text = "hello world test";
        let dims = 128;

        let emb1 = generate_word_based_embedding(text, dims);
        let emb2 = generate_word_based_embedding(text, dims);

        assert_eq!(emb1, emb2, "Same input should produce same embedding");
    }

    #[test]
    fn test_word_based_embedding_case_insensitive() {
        let text1 = "Machine Learning";
        let text2 = "machine learning";
        let dims = 128;

        let emb1 = generate_word_based_embedding(text1, dims);
        let emb2 = generate_word_based_embedding(text2, dims);

        assert_eq!(emb1, emb2, "Embeddings should be case-insensitive");
    }
}
