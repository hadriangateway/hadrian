use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{body::Body, response::Response};
use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::multipart::{Form, Part};
use serde_json::Value;

use crate::{
    api_types::{
        CreateChatCompletionPayload, CreateCompletionPayload, CreateEmbeddingPayload,
        CreateImageRequest, CreateResponsesPayload, CreateSpeechRequest,
        CreateTranscriptionRequest, CreateTranslationRequest,
        audio::AudioResponseFormat,
        images::{CreateImageEditRequest, CreateImageVariationRequest, ImagesResponse},
    },
    config::{CircuitBreakerConfig, OpenAiProviderConfig, RetryConfig},
    providers,
    providers::{
        CircuitBreakerRegistry, ModelsResponse, Provider, ProviderError,
        circuit_breaker::CircuitBreaker, retry::with_circuit_breaker_and_retry,
    },
};

/// Normalize a chat completion response to ensure all required fields per OpenAI spec.
///
/// Some OpenAI-compatible providers (e.g., Ollama) don't include all required fields.
/// This function ensures:
/// - Each choice has `logprobs` (null if not present)
/// - Each message has `refusal` (null if not present)
fn normalize_chat_completion_response(mut response: Value) -> Value {
    if let Some(choices) = response.get_mut("choices").and_then(|v| v.as_array_mut()) {
        for choice in choices {
            // Ensure logprobs exists (required, can be null)
            if choice.get("logprobs").is_none() {
                choice["logprobs"] = Value::Null;
            }

            // Ensure message.refusal exists (required, can be null)
            if let Some(message) = choice.get_mut("message")
                && message.get("refusal").is_none()
            {
                message["refusal"] = Value::Null;
            }
        }
    }
    response
}

pub struct OpenAICompatibleProvider {
    api_key: Option<String>,
    base_url: String,
    headers: HashMap<String, String>,
    timeout: Duration,
    retry: RetryConfig,
    circuit_breaker_config: CircuitBreakerConfig,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
}

impl OpenAICompatibleProvider {
    /// Create a provider from configuration with a shared circuit breaker.
    pub fn from_config_with_registry(
        config: &OpenAiProviderConfig,
        provider_name: &str,
        registry: &CircuitBreakerRegistry,
    ) -> Self {
        let circuit_breaker = registry.get_or_create(provider_name, &config.circuit_breaker);
        let base_url = config.base_url.trim_end_matches('/').to_string();

        let mut headers = config.headers.clone();

        // OpenRouter app attribution: send Hadrian metadata by default unless
        // the user has explicitly set these headers (opt-out by overriding).
        if base_url.contains("openrouter.ai") {
            headers
                .entry("HTTP-Referer".to_string())
                .or_insert_with(|| "https://hadriangateway.com".to_string());
            headers
                .entry("X-OpenRouter-Title".to_string())
                .or_insert_with(|| "Hadrian Gateway".to_string());
            headers
                .entry("X-OpenRouter-Categories".to_string())
                .or_insert_with(|| "general-chat".to_string());
        }

        Self {
            api_key: config.api_key.clone(),
            base_url,
            headers,
            timeout: Duration::from_secs(config.timeout_secs),
            retry: config.retry.clone(),
            circuit_breaker_config: config.circuit_breaker.clone(),
            circuit_breaker,
        }
    }

    /// Build a request with common auth headers and timeout.
    fn build_request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = if let Some(api_key) = &self.api_key {
            request.header(AUTHORIZATION, format!("Bearer {}", api_key))
        } else {
            request
        };

        let request = self
            .headers
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .fold(request, |req, (key, value)| {
                req.header(key.as_str(), value.as_str())
            });

        request.timeout(self.timeout)
    }

    /// Build a multipart request with common auth headers and timeout.
    fn build_multipart_request(
        &self,
        client: &reqwest::Client,
        url: &str,
        form: Form,
    ) -> reqwest::RequestBuilder {
        let request = client.post(url).multipart(form);

        let request = if let Some(api_key) = &self.api_key {
            request.header(AUTHORIZATION, format!("Bearer {}", api_key))
        } else {
            request
        };

        let request = self
            .headers
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .fold(request, |req, (key, value)| {
                req.header(key.as_str(), value.as_str())
            });

        request.timeout(self.timeout)
    }

    /// Check response status and extract OpenAI error message on failure.
    ///
    /// OpenAI returns errors as `{"error": {"message": "...", "type": "...", "code": "..."}}`.
    /// Without this check, `response.json::<T>()` fails with an unhelpful
    /// "error decoding response body" when the status is non-2xx.
    async fn check_response(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ProviderError> {
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("(empty body)"));

        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(body);

        Err(ProviderError::Internal(format!(
            "OpenAI API error ({status}): {message}"
        )))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Provider for OpenAICompatibleProvider {
    fn default_health_check_model(&self) -> Option<&str> {
        Some("gpt-4o-mini")
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "chat_completion",
            model = %payload.model.as_deref().unwrap_or("default"),
            stream = payload.stream
        )
    )]
    async fn create_chat_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateChatCompletionPayload,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let stream = payload.stream;

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "chat_completion",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        // For non-streaming responses, normalize to ensure OpenAI spec compliance
        // Some providers (e.g., Ollama) don't include all required fields
        if !stream {
            let status = response.status();
            if status.is_success() {
                let body_bytes = response.bytes().await?;
                if let Ok(mut json) = serde_json::from_slice::<Value>(&body_bytes) {
                    json = normalize_chat_completion_response(json);
                    // Safe to unwrap: we just parsed valid JSON, re-serializing won't fail
                    let normalized_body =
                        serde_json::to_vec(&json).unwrap_or_else(|_| body_bytes.to_vec());
                    return Ok(Response::builder()
                        .status(status)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(normalized_body))?);
                }
                // If JSON parsing fails, return original body
                return Ok(Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body_bytes))?);
            }
        }

        providers::build_response(response, stream).await
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "responses",
            model = %payload.model.as_deref().unwrap_or("default"),
            stream = payload.stream
        )
    )]
    async fn create_responses(
        &self,
        client: &reqwest::Client,
        payload: CreateResponsesPayload,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let stream = payload.stream;

        // Drop gateway-managed fields (store, background, models, provider,
        // plugins, sovereignty_requirements, skills) that Hadrian consumes
        // itself and the upstream must never see. See the method docs for why.
        let mut payload = payload;
        payload.strip_gateway_fields();

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "responses",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        providers::build_response(response, stream).await
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "responses_compact",
            model = %payload.model,
        )
    )]
    async fn create_responses_compact(
        &self,
        client: &reqwest::Client,
        payload: crate::api_types::CompactRequest,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/responses/compact", self.base_url);
        let stream = payload.stream;
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "responses_compact",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        providers::build_response(response, stream).await
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "completion",
            model = %payload.model.as_deref().unwrap_or("default"),
            stream = payload.stream
        )
    )]
    async fn create_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateCompletionPayload,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/completions", self.base_url);
        let stream = payload.stream;

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "completion",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        providers::build_response(response, stream).await
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "embedding",
            model = %payload.model
        )
    )]
    async fn create_embedding(
        &self,
        client: &reqwest::Client,
        payload: CreateEmbeddingPayload,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/embeddings", self.base_url);

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_embedding(),
            "openai",
            "embedding",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        // Embeddings don't support streaming
        providers::build_response(response, false).await
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "openai", operation = "list_models")
    )]
    async fn list_models(&self, client: &reqwest::Client) -> Result<ModelsResponse, ProviderError> {
        let url = format!("{}/models", self.base_url);

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_read_only(),
            "openai",
            "list_models",
            || async { self.build_request(client.get(&url)).send().await },
        )
        .await?;

        let response = Self::check_response(response).await?;
        let models: ModelsResponse = response.json().await?;
        Ok(models)
    }

    // =========================================================================
    // Image generation methods
    // =========================================================================

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "create_image",
            model = %payload.model.as_deref().unwrap_or("dall-e-2")
        )
    )]
    async fn create_image(
        &self,
        client: &reqwest::Client,
        payload: CreateImageRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        let url = format!("{}/images/generations", self.base_url);

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "create_image",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        let images: ImagesResponse = response.json().await?;
        Ok(images)
    }

    #[tracing::instrument(
        skip(self, client, image, mask, request),
        fields(
            provider = "openai",
            operation = "create_image_edit",
            model = %request.model.as_deref().unwrap_or("dall-e-2"),
            image_size = image.len()
        )
    )]
    async fn create_image_edit(
        &self,
        client: &reqwest::Client,
        image: Bytes,
        mask: Option<Bytes>,
        request: CreateImageEditRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        let url = format!("{}/images/edits", self.base_url);

        // Pre-serialize enum values before retry loop to avoid repeated serialization
        let prompt = request.prompt.clone();
        let model = request.model.clone();
        let n = request.n.map(|v| v.to_string());
        let size = request.size.and_then(|s| {
            serde_json::to_string(&s)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let response_format = request.response_format.and_then(|rf| {
            serde_json::to_string(&rf)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let user = request.user.clone();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "create_image_edit",
            || {
                // Build form fresh for each retry attempt (Form is consumed on send)
                let mut form = Form::new()
                    .part("image", Part::bytes(image.to_vec()).file_name("image.png"))
                    .text("prompt", prompt.clone());

                if let Some(ref mask_bytes) = mask {
                    form = form.part(
                        "mask",
                        Part::bytes(mask_bytes.to_vec()).file_name("mask.png"),
                    );
                }
                if let Some(ref m) = model {
                    form = form.text("model", m.clone());
                }
                if let Some(ref n_val) = n {
                    form = form.text("n", n_val.clone());
                }
                if let Some(ref s) = size {
                    form = form.text("size", s.clone());
                }
                if let Some(ref rf) = response_format {
                    form = form.text("response_format", rf.clone());
                }
                if let Some(ref u) = user {
                    form = form.text("user", u.clone());
                }

                let url = url.clone();
                async move {
                    self.build_multipart_request(client, &url, form)
                        .send()
                        .await
                }
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        let images: ImagesResponse = response.json().await?;
        Ok(images)
    }

    #[tracing::instrument(
        skip(self, client, image, request),
        fields(
            provider = "openai",
            operation = "create_image_variation",
            model = %request.model.as_deref().unwrap_or("dall-e-2"),
            image_size = image.len()
        )
    )]
    async fn create_image_variation(
        &self,
        client: &reqwest::Client,
        image: Bytes,
        request: CreateImageVariationRequest,
    ) -> Result<ImagesResponse, ProviderError> {
        let url = format!("{}/images/variations", self.base_url);

        // Pre-serialize enum values before retry loop to avoid repeated serialization
        let model = request.model.clone();
        let n = request.n.map(|v| v.to_string());
        let size = request.size.and_then(|s| {
            serde_json::to_string(&s)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let response_format = request.response_format.and_then(|rf| {
            serde_json::to_string(&rf)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let user = request.user.clone();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "create_image_variation",
            || {
                // Build form fresh for each retry attempt (Form is consumed on send)
                let mut form =
                    Form::new().part("image", Part::bytes(image.to_vec()).file_name("image.png"));

                if let Some(ref m) = model {
                    form = form.text("model", m.clone());
                }
                if let Some(ref n_val) = n {
                    form = form.text("n", n_val.clone());
                }
                if let Some(ref s) = size {
                    form = form.text("size", s.clone());
                }
                if let Some(ref rf) = response_format {
                    form = form.text("response_format", rf.clone());
                }
                if let Some(ref u) = user {
                    form = form.text("user", u.clone());
                }

                let url = url.clone();
                async move {
                    self.build_multipart_request(client, &url, form)
                        .send()
                        .await
                }
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        let images: ImagesResponse = response.json().await?;
        Ok(images)
    }

    // =========================================================================
    // Audio methods
    // =========================================================================

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "create_speech",
            model = %payload.model,
            voice = ?payload.voice
        )
    )]
    async fn create_speech(
        &self,
        client: &reqwest::Client,
        payload: CreateSpeechRequest,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/audio/speech", self.base_url);

        // Pre-serialize before retry loop to avoid repeated serialization
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "create_speech",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        // Return audio bytes directly (not JSON)
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("audio/mpeg")
            .to_string();
        let bytes = response.bytes().await?;

        Ok(Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(bytes))?)
    }

    #[tracing::instrument(
        skip(self, client, file, request),
        fields(
            provider = "openai",
            operation = "create_transcription",
            model = %request.model,
            file_size = file.len()
        )
    )]
    async fn create_transcription(
        &self,
        client: &reqwest::Client,
        file: Bytes,
        filename: String,
        request: CreateTranscriptionRequest,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/audio/transcriptions", self.base_url);

        // Pre-serialize values before retry loop to avoid repeated serialization
        let model = request.model.clone();
        let language = request.language.clone();
        let prompt = request.prompt.clone();
        let response_format = request.response_format.and_then(|rf| {
            serde_json::to_string(&rf)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let temperature = request.temperature.map(|t| t.to_string());
        let granularities: Option<Vec<String>> =
            request.timestamp_granularities.as_ref().map(|grans| {
                grans
                    .iter()
                    .filter_map(|g| {
                        serde_json::to_string(g)
                            .ok()
                            .map(|v| v.trim_matches('"').to_string())
                    })
                    .collect()
            });

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "create_transcription",
            || {
                // Build form fresh for each retry attempt (Form is consumed on send)
                let mut form = Form::new()
                    .part(
                        "file",
                        Part::bytes(file.to_vec()).file_name(filename.clone()),
                    )
                    .text("model", model.clone());

                if let Some(ref lang) = language {
                    form = form.text("language", lang.clone());
                }
                if let Some(ref p) = prompt {
                    form = form.text("prompt", p.clone());
                }
                if let Some(ref rf) = response_format {
                    form = form.text("response_format", rf.clone());
                }
                if let Some(ref temp) = temperature {
                    form = form.text("temperature", temp.clone());
                }
                if let Some(ref grans) = granularities {
                    for g in grans {
                        form = form.text("timestamp_granularities[]", g.clone());
                    }
                }

                let url = url.clone();
                async move {
                    self.build_multipart_request(client, &url, form)
                        .send()
                        .await
                }
            },
        )
        .await?;

        // Response format determines content type
        let is_json = request
            .response_format
            .map(|f| {
                matches!(
                    f,
                    AudioResponseFormat::Json
                        | AudioResponseFormat::VerboseJson
                        | AudioResponseFormat::DiarizedJson
                )
            })
            .unwrap_or(true); // Default is JSON

        let status = response.status();
        let bytes = response.bytes().await?;

        let content_type = if is_json {
            "application/json"
        } else {
            "text/plain"
        };

        Ok(Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(bytes))?)
    }

    #[tracing::instrument(
        skip(self, client, file, request),
        fields(
            provider = "openai",
            operation = "create_translation",
            model = %request.model,
            file_size = file.len()
        )
    )]
    async fn create_translation(
        &self,
        client: &reqwest::Client,
        file: Bytes,
        filename: String,
        request: CreateTranslationRequest,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}/audio/translations", self.base_url);

        // Pre-serialize values before retry loop to avoid repeated serialization
        let model = request.model.clone();
        let prompt = request.prompt.clone();
        let response_format = request.response_format.and_then(|rf| {
            serde_json::to_string(&rf)
                .ok()
                .map(|v| v.trim_matches('"').to_string())
        });
        let temperature = request.temperature.map(|t| t.to_string());

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "create_translation",
            || {
                // Build form fresh for each retry attempt (Form is consumed on send)
                let mut form = Form::new()
                    .part(
                        "file",
                        Part::bytes(file.to_vec()).file_name(filename.clone()),
                    )
                    .text("model", model.clone());

                if let Some(ref p) = prompt {
                    form = form.text("prompt", p.clone());
                }
                if let Some(ref rf) = response_format {
                    form = form.text("response_format", rf.clone());
                }
                if let Some(ref temp) = temperature {
                    form = form.text("temperature", temp.clone());
                }

                let url = url.clone();
                async move {
                    self.build_multipart_request(client, &url, form)
                        .send()
                        .await
                }
            },
        )
        .await?;

        // Response format determines content type
        let is_json = request
            .response_format
            .map(|f| {
                matches!(
                    f,
                    AudioResponseFormat::Json | AudioResponseFormat::VerboseJson
                )
            })
            .unwrap_or(true); // Default is JSON

        let status = response.status();
        let bytes = response.bytes().await?;

        let content_type = if is_json {
            "application/json"
        } else {
            "text/plain"
        };

        Ok(Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(bytes))?)
    }

    // =========================================================================
    // Video generation methods
    // =========================================================================

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "openai",
            operation = "create_video",
            model = %payload.model.as_deref().unwrap_or("sora-2")
        )
    )]
    async fn create_video(
        &self,
        client: &reqwest::Client,
        payload: crate::api_types::CreateVideoRequest,
    ) -> Result<crate::api_types::Video, ProviderError> {
        let url = format!("{}/videos", self.base_url);
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "create_video",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "openai", operation = "get_video")
    )]
    async fn get_video(
        &self,
        client: &reqwest::Client,
        video_id: &str,
    ) -> Result<crate::api_types::Video, ProviderError> {
        let url = format!("{}/videos/{}", self.base_url, video_id);
        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_read_only(),
            "openai",
            "get_video",
            || async { self.build_request(client.get(&url)).send().await },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "openai", operation = "delete_video")
    )]
    async fn delete_video(
        &self,
        client: &reqwest::Client,
        video_id: &str,
    ) -> Result<crate::api_types::VideoDeleteResponse, ProviderError> {
        let url = format!("{}/videos/{}", self.base_url, video_id);
        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "openai",
            "delete_video",
            || async { self.build_request(client.delete(&url)).send().await },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "openai", operation = "get_video_content")
    )]
    async fn get_video_content(
        &self,
        client: &reqwest::Client,
        video_id: &str,
        variant: Option<crate::api_types::VideoVariant>,
    ) -> Result<Response, ProviderError> {
        let url = match variant {
            Some(v) => format!(
                "{}/videos/{}/content?variant={}",
                self.base_url,
                video_id,
                v.as_str()
            ),
            None => format!("{}/videos/{}/content", self.base_url, video_id),
        };

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_read_only(),
            "openai",
            "get_video_content",
            || async { self.build_request(client.get(&url)).send().await },
        )
        .await?;

        let response = Self::check_response(response).await?;

        // Return raw asset bytes, passing the provider's content-type through.
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("video/mp4")
            .to_string();
        let bytes = response.bytes().await?;

        Ok(Response::builder()
            .status(status)
            .header(CONTENT_TYPE, content_type)
            .body(Body::from(bytes))?)
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(provider = "openai", operation = "remix_video")
    )]
    async fn remix_video(
        &self,
        client: &reqwest::Client,
        video_id: &str,
        payload: crate::api_types::RemixVideoRequest,
    ) -> Result<crate::api_types::Video, ProviderError> {
        let url = format!("{}/videos/{}/remix", self.base_url, video_id);
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "remix_video",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(provider = "openai", operation = "edit_video")
    )]
    async fn edit_video(
        &self,
        client: &reqwest::Client,
        payload: crate::api_types::VideoEditRequest,
    ) -> Result<crate::api_types::Video, ProviderError> {
        let url = format!("{}/videos/edits", self.base_url);
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "edit_video",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(provider = "openai", operation = "extend_video")
    )]
    async fn extend_video(
        &self,
        client: &reqwest::Client,
        payload: crate::api_types::VideoExtensionRequest,
    ) -> Result<crate::api_types::Video, ProviderError> {
        let url = format!("{}/videos/extensions", self.base_url);
        let body = serde_json::to_vec(&payload).unwrap_or_default();

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "extend_video",
            || async {
                self.build_request(client.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send()
                    .await
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client, name, video, filename),
        fields(provider = "openai", operation = "create_character", video_size = video.len())
    )]
    async fn create_character(
        &self,
        client: &reqwest::Client,
        name: String,
        video: Bytes,
        filename: String,
    ) -> Result<crate::api_types::Character, ProviderError> {
        let url = format!("{}/videos/characters", self.base_url);

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_image_generation(),
            "openai",
            "create_character",
            || {
                // Build form fresh for each retry attempt (Form is consumed on send)
                let form = Form::new().text("name", name.clone()).part(
                    "video",
                    Part::bytes(video.to_vec()).file_name(filename.clone()),
                );
                let url = url.clone();
                async move {
                    self.build_multipart_request(client, &url, form)
                        .send()
                        .await
                }
            },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "openai", operation = "get_character")
    )]
    async fn get_character(
        &self,
        client: &reqwest::Client,
        character_id: &str,
    ) -> Result<crate::api_types::Character, ProviderError> {
        let url = format!("{}/videos/characters/{}", self.base_url, character_id);
        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry.for_read_only(),
            "openai",
            "get_character",
            || async { self.build_request(client.get(&url)).send().await },
        )
        .await?;

        let response = Self::check_response(response).await?;
        Ok(response.json().await?)
    }
}
