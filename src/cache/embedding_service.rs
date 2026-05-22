//! Embedding service for semantic caching.
//!
//! Generates embeddings for request messages using a configured LLM provider.
//! These embeddings are used for semantic similarity lookups in the cache.

use std::time::Instant;

use reqwest::Client;
use thiserror::Error;

use crate::{
    api_types::{
        CreateChatCompletionPayload, CreateEmbeddingPayload, Message, MessageContent,
        chat_completion::ContentPart,
        embeddings::{CreateEmbeddingResponse, EmbeddingInput, EmbeddingVector},
    },
    config::{EmbeddingConfig, ProviderConfig},
    observability::metrics::record_embedding_generation,
    providers::{CircuitBreakerRegistry, Provider, ProviderError},
};

/// Errors that can occur during embedding generation.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("Failed to parse embedding response: {0}")]
    ParseError(String),

    #[error("No embeddings returned from provider")]
    EmptyResponse,

    #[error("Provider '{0}' not configured")]
    ProviderNotConfigured(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

/// Service for generating embeddings from chat completion requests.
///
/// Uses a configured provider to generate embeddings that can be used
/// for semantic similarity matching in the response cache.
pub struct EmbeddingService {
    provider: Box<dyn Provider>,
    provider_name: String,
    model: String,
    dimensions: usize,
    http_client: Client,
}

impl EmbeddingService {
    /// Create a new embedding service from configuration.
    ///
    /// # Arguments
    /// * `config` - Embedding configuration specifying provider, model, and dimensions
    /// * `provider_config` - The provider configuration for the embedding provider
    /// * `circuit_breakers` - Registry for circuit breakers
    /// * `http_client` - HTTP client for making requests
    ///
    /// # Returns
    /// A configured embedding service or an error if the provider is not supported.
    pub fn new(
        config: &EmbeddingConfig,
        provider_config: &ProviderConfig,
        circuit_breakers: &CircuitBreakerRegistry,
        http_client: Client,
    ) -> Result<Self, EmbeddingError> {
        let provider: Box<dyn Provider> = match provider_config {
            ProviderConfig::OpenAi(cfg) => Box::new(
                crate::providers::open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    cfg,
                    &config.provider,
                    circuit_breakers,
                ),
            ),
            ProviderConfig::Anthropic(cfg) => Box::new(
                crate::providers::anthropic::AnthropicProvider::from_config_with_registry(
                    cfg,
                    &config.provider,
                    circuit_breakers,
                ),
            ),
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(cfg) => Box::new(
                crate::providers::azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    cfg,
                    &config.provider,
                    circuit_breakers,
                ),
            ),
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(cfg) => Box::new(
                crate::providers::vertex::VertexProvider::from_config_with_registry(
                    cfg,
                    &config.provider,
                    circuit_breakers,
                ),
            ),
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(cfg) => Box::new(
                crate::providers::bedrock::BedrockProvider::from_config_with_registry(
                    cfg,
                    &config.provider,
                    circuit_breakers,
                ),
            ),
            ProviderConfig::Test(cfg) => {
                Box::new(crate::providers::test::TestProvider::from_config(cfg))
            }
        };

        Ok(Self {
            provider,
            provider_name: config.provider.clone(),
            model: config.model.clone(),
            dimensions: config.dimensions,
            http_client,
        })
    }

    /// Generate an embedding for a chat completion request.
    ///
    /// Converts the messages to text and generates an embedding using the configured
    /// provider. The resulting embedding can be used for semantic similarity matching.
    ///
    /// # Arguments
    /// * `payload` - The chat completion request to generate an embedding for
    ///
    /// # Returns
    /// A vector of floats representing the embedding, or an error.
    pub async fn embed_request(
        &self,
        payload: &CreateChatCompletionPayload,
    ) -> Result<Vec<f64>, EmbeddingError> {
        // Convert the request to a text representation
        let text = self.normalize_request_to_text(payload);

        // Create embedding request
        let embedding_payload = CreateEmbeddingPayload {
            input: EmbeddingInput::Text(text),
            model: self.model.clone(),
            encoding_format: None,
            dimensions: Some(self.dimensions as i64),
            user: None,
            provider: None,
            input_type: None,
            sovereignty_requirements: None,
        };

        // Start timing
        let start = Instant::now();

        // Call the provider
        let response = self
            .provider
            .create_embedding(&self.http_client, embedding_payload)
            .await;

        let duration_secs = start.elapsed().as_secs_f64();

        match response {
            Ok(resp) => {
                // Parse the response and extract metrics
                match self.parse_embedding_response_with_usage(resp).await {
                    Ok((embedding, token_count)) => {
                        record_embedding_generation(
                            &self.provider_name,
                            &self.model,
                            "success",
                            duration_secs,
                            token_count,
                            1, // batch_size: single request
                        );
                        Ok(embedding)
                    }
                    Err(e) => {
                        record_embedding_generation(
                            &self.provider_name,
                            &self.model,
                            "error",
                            duration_secs,
                            None,
                            1,
                        );
                        Err(e)
                    }
                }
            }
            Err(e) => {
                record_embedding_generation(
                    &self.provider_name,
                    &self.model,
                    "error",
                    duration_secs,
                    None,
                    1,
                );
                Err(e.into())
            }
        }
    }

    /// Generate an embedding for arbitrary text.
    ///
    /// # Arguments
    /// * `text` - The text to generate an embedding for
    ///
    /// # Returns
    /// A vector of floats representing the embedding, or an error.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f64>, EmbeddingError> {
        let embedding_payload = CreateEmbeddingPayload {
            input: EmbeddingInput::Text(text.to_string()),
            model: self.model.clone(),
            encoding_format: None,
            dimensions: Some(self.dimensions as i64),
            user: None,
            provider: None,
            input_type: None,
            sovereignty_requirements: None,
        };

        // Start timing
        let start = Instant::now();

        let response = self
            .provider
            .create_embedding(&self.http_client, embedding_payload)
            .await;

        let duration_secs = start.elapsed().as_secs_f64();

        match response {
            Ok(resp) => {
                // Parse the response and extract metrics
                match self.parse_embedding_response_with_usage(resp).await {
                    Ok((embedding, token_count)) => {
                        record_embedding_generation(
                            &self.provider_name,
                            &self.model,
                            "success",
                            duration_secs,
                            token_count,
                            1, // batch_size: single text
                        );
                        Ok(embedding)
                    }
                    Err(e) => {
                        record_embedding_generation(
                            &self.provider_name,
                            &self.model,
                            "error",
                            duration_secs,
                            None,
                            1,
                        );
                        Err(e)
                    }
                }
            }
            Err(e) => {
                record_embedding_generation(
                    &self.provider_name,
                    &self.model,
                    "error",
                    duration_secs,
                    None,
                    1,
                );
                Err(e.into())
            }
        }
    }

    /// Generate embeddings for a batch of texts in a single request.
    ///
    /// Returns one vector per input, in the same order as `texts` (the
    /// provider's `index` field is honored when present). Used by
    /// Hadrian-side tool search to embed a deferred MCP catalog in one
    /// round-trip. Returns an empty `Vec` for empty input without
    /// contacting the provider.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let embedding_payload = CreateEmbeddingPayload {
            input: EmbeddingInput::TextArray(texts.to_vec()),
            model: self.model.clone(),
            encoding_format: None,
            dimensions: Some(self.dimensions as i64),
            user: None,
            provider: None,
            input_type: None,
            sovereignty_requirements: None,
        };

        let start = Instant::now();
        let response = self
            .provider
            .create_embedding(&self.http_client, embedding_payload)
            .await;
        let duration_secs = start.elapsed().as_secs_f64();

        let status_label = |ok: bool| if ok { "success" } else { "error" };
        let resp = match response {
            Ok(resp) => resp,
            Err(e) => {
                record_embedding_generation(
                    &self.provider_name,
                    &self.model,
                    status_label(false),
                    duration_secs,
                    None,
                    texts.len() as u32,
                );
                return Err(e.into());
            }
        };

        match self
            .parse_embedding_batch_with_usage(resp, texts.len())
            .await
        {
            Ok((embeddings, token_count)) => {
                record_embedding_generation(
                    &self.provider_name,
                    &self.model,
                    status_label(true),
                    duration_secs,
                    token_count,
                    texts.len() as u32,
                );
                Ok(embeddings)
            }
            Err(e) => {
                record_embedding_generation(
                    &self.provider_name,
                    &self.model,
                    status_label(false),
                    duration_secs,
                    None,
                    texts.len() as u32,
                );
                Err(e)
            }
        }
    }

    /// Parse a batch embedding response, returning vectors ordered by the
    /// provider's `index` field (falling back to response order).
    async fn parse_embedding_batch_with_usage(
        &self,
        response: axum::response::Response,
        expected: usize,
    ) -> Result<(Vec<Vec<f64>>, Option<u32>), EmbeddingError> {
        if !response.status().is_success() {
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            return Err(EmbeddingError::ParseError(format!(
                "Provider returned error status {status}: {body_str}"
            )));
        }

        let body = axum::body::to_bytes(response.into_body(), 50 * 1024 * 1024)
            .await
            .map_err(|e| {
                EmbeddingError::ParseError(format!("Failed to read response body: {e}"))
            })?;
        let parsed: CreateEmbeddingResponse = serde_json::from_slice(&body)
            .map_err(|e| EmbeddingError::ParseError(format!("Failed to parse response: {e}")))?;

        let token_count = parsed.usage.as_ref().map(|u| u.total_tokens as u32);

        if parsed.data.is_empty() {
            return Err(EmbeddingError::EmptyResponse);
        }

        // Sort by `index` so the output order matches the input order even
        // if the provider returns them out of order.
        let mut data = parsed.data;
        data.sort_by(|a, b| {
            a.index
                .unwrap_or(0.0)
                .partial_cmp(&b.index.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let embeddings: Vec<Vec<f64>> = data
            .into_iter()
            .map(|d| decode_embedding_vector(d.embedding))
            .collect::<Result<_, _>>()?;

        if embeddings.len() != expected {
            return Err(EmbeddingError::ParseError(format!(
                "expected {expected} embeddings, provider returned {}",
                embeddings.len()
            )));
        }

        Ok((embeddings, token_count))
    }

    /// Convert a chat completion request to a normalized text representation.
    ///
    /// This creates a consistent text format for embedding that captures
    /// the semantic content of the request.
    fn normalize_request_to_text(&self, payload: &CreateChatCompletionPayload) -> String {
        let mut parts = Vec::new();

        for message in &payload.messages {
            let (role, content_str) = match message {
                Message::System { content, .. } => ("system", message_content_to_string(content)),
                Message::Developer { content, .. } => {
                    ("developer", message_content_to_string(content))
                }
                Message::User { content, .. } => ("user", message_content_to_string(content)),
                Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } => {
                    let mut text = content
                        .as_ref()
                        .map(message_content_to_string)
                        .unwrap_or_default();
                    if let Some(calls) = tool_calls {
                        for call in calls {
                            text.push_str(&format!(" [tool:{}]", call.function.name));
                        }
                    }
                    ("assistant", text)
                }
                Message::Tool { content, .. } => ("tool", message_content_to_string(content)),
            };

            if !content_str.is_empty() {
                parts.push(format!("{}: {}", role, content_str));
            }
        }

        parts.join("\n")
    }

    /// Parse an embedding response from the provider and extract usage information.
    ///
    /// Returns the embedding vector and optionally the token count from the response.
    async fn parse_embedding_response_with_usage(
        &self,
        response: axum::response::Response,
    ) -> Result<(Vec<f64>, Option<u32>), EmbeddingError> {
        // Check status
        if !response.status().is_success() {
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);
            return Err(EmbeddingError::ParseError(format!(
                "Provider returned error status {}: {}",
                status, body_str
            )));
        }

        // Parse response body
        let body = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .map_err(|e| {
                EmbeddingError::ParseError(format!("Failed to read response body: {}", e))
            })?;

        let parsed: CreateEmbeddingResponse = serde_json::from_slice(&body)
            .map_err(|e| EmbeddingError::ParseError(format!("Failed to parse response: {}", e)))?;

        // Extract token count from usage if available
        let token_count = parsed.usage.as_ref().map(|u| u.total_tokens as u32);

        // Extract the first embedding
        let embedding_data = parsed
            .data
            .into_iter()
            .next()
            .ok_or(EmbeddingError::EmptyResponse)?;

        let embedding = decode_embedding_vector(embedding_data.embedding)?;

        Ok((embedding, token_count))
    }

    /// Get the configured embedding dimensions.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Get the configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get the provider name.
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
}

/// Decode a provider embedding vector (float array or base64-packed
/// little-endian f32) into `Vec<f64>`.
fn decode_embedding_vector(vector: EmbeddingVector) -> Result<Vec<f64>, EmbeddingError> {
    match vector {
        EmbeddingVector::Float(vec) => Ok(vec),
        EmbeddingVector::Base64(b64) => {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)
                .map_err(|e| EmbeddingError::ParseError(format!("Invalid base64: {e}")))?;
            if bytes.len() % 4 != 0 {
                return Err(EmbeddingError::ParseError(
                    "Invalid base64 embedding length".to_string(),
                ));
            }
            Ok(bytes
                .chunks(4)
                .map(|chunk| {
                    let arr: [u8; 4] = chunk.try_into().unwrap();
                    f32::from_le_bytes(arr) as f64
                })
                .collect())
        }
    }
}

/// Convert MessageContent to a plain string.
fn message_content_to_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => {
            // Extract text from content parts
            parts
                .iter()
                .filter_map(|part| {
                    if let ContentPart::Text { text, .. } = part {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

impl std::fmt::Debug for EmbeddingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingService")
            .field("provider_name", &self.provider_name)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_payload(messages: Vec<Message>) -> CreateChatCompletionPayload {
        CreateChatCompletionPayload {
            messages,
            model: Some("gpt-4".to_string()),
            models: None,
            temperature: Some(0.0),
            seed: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            frequency_penalty: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            max_completion_tokens: None,
            max_tokens: None,
            metadata: None,
            presence_penalty: None,
            reasoning: None,
            stop: None,
            stream: false,
            stream_options: None,
            top_p: None,
            user: None,
            sovereignty_requirements: None,
        }
    }

    fn test_embedding_service() -> EmbeddingService {
        let cfg = EmbeddingConfig {
            provider: "test".to_string(),
            model: "test-embed".to_string(),
            dimensions: 64,
        };
        let test_cfg: crate::config::TestProviderConfig =
            toml::from_str("").expect("default test provider config");
        let provider_cfg = ProviderConfig::Test(test_cfg);
        EmbeddingService::new(
            &cfg,
            &provider_cfg,
            &CircuitBreakerRegistry::new(),
            Client::new(),
        )
        .expect("test embedding service")
    }

    #[tokio::test]
    async fn embed_batch_returns_one_vector_per_input_in_order() {
        let svc = test_embedding_service();
        let texts = vec![
            "search jira issues".to_string(),
            "create confluence page".to_string(),
            "list github pull requests".to_string(),
        ];
        let vecs = svc.embed_batch(&texts).await.expect("embeds");
        assert_eq!(vecs.len(), 3);
        for v in &vecs {
            assert_eq!(v.len(), 64);
        }
        // Distinct inputs with distinct words should not all be identical.
        assert_ne!(vecs[0], vecs[1]);
        // embed_batch of one input matches embed_text for the same text.
        let single = svc
            .embed_batch(&["search jira issues".to_string()])
            .await
            .unwrap();
        let direct = svc.embed_text("search jira issues").await.unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0], direct);
    }

    #[tokio::test]
    async fn embed_batch_empty_input_is_noop() {
        let svc = test_embedding_service();
        let vecs = svc.embed_batch(&[]).await.expect("ok");
        assert!(vecs.is_empty());
    }

    #[test]
    fn test_normalize_simple_messages() {
        let messages = vec![
            Message::System {
                content: MessageContent::Text("You are a helpful assistant.".to_string()),
                name: None,
            },
            Message::User {
                content: MessageContent::Text("What is 2+2?".to_string()),
                name: None,
            },
        ];

        let payload = create_test_payload(messages);

        // Test the normalization logic directly
        let mut parts = Vec::new();
        for message in &payload.messages {
            let (role, content) = match message {
                Message::System { content, .. } => ("system", message_content_to_string(content)),
                Message::User { content, .. } => ("user", message_content_to_string(content)),
                _ => continue,
            };
            if !content.is_empty() {
                parts.push(format!("{}: {}", role, content));
            }
        }
        let normalized = parts.join("\n");

        assert_eq!(
            normalized,
            "system: You are a helpful assistant.\nuser: What is 2+2?"
        );
    }

    #[test]
    fn test_normalize_multimodal_messages() {
        let messages = vec![Message::User {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Describe this image:".to_string(),
                    cache_control: None,
                },
                ContentPart::ImageUrl {
                    image_url: crate::api_types::chat_completion::ImageUrl {
                        url: "https://example.com/image.png".to_string(),
                        detail: None,
                    },
                    cache_control: None,
                },
                ContentPart::Text {
                    text: "in detail".to_string(),
                    cache_control: None,
                },
            ]),
            name: None,
        }];

        let payload = create_test_payload(messages);

        // Test text extraction from content parts
        let mut text_parts = Vec::new();
        for message in &payload.messages {
            if let Message::User {
                content: MessageContent::Parts(parts),
                ..
            } = message
            {
                for part in parts {
                    if let ContentPart::Text { text, .. } = part {
                        text_parts.push(text.clone());
                    }
                }
            }
        }
        let text = text_parts.join(" ");

        assert_eq!(text, "Describe this image: in detail");
    }

    #[test]
    fn test_normalize_empty_messages() {
        let messages = vec![];
        let payload = create_test_payload(messages);

        let mut parts = Vec::new();
        for message in &payload.messages {
            let (role, content) = match message {
                Message::System { content, .. } => ("system", message_content_to_string(content)),
                _ => continue,
            };
            if !content.is_empty() {
                parts.push(format!("{}: {}", role, content));
            }
        }
        let normalized = parts.join("\n");

        assert!(normalized.is_empty());
    }

    #[test]
    fn test_message_content_to_string() {
        // Test text content
        let text_content = MessageContent::Text("Hello world".to_string());
        assert_eq!(message_content_to_string(&text_content), "Hello world");

        // Test parts content
        let parts_content = MessageContent::Parts(vec![
            ContentPart::Text {
                text: "First".to_string(),
                cache_control: None,
            },
            ContentPart::ImageUrl {
                image_url: crate::api_types::chat_completion::ImageUrl {
                    url: "https://example.com/image.png".to_string(),
                    detail: None,
                },
                cache_control: None,
            },
            ContentPart::Text {
                text: "Second".to_string(),
                cache_control: None,
            },
        ]);
        assert_eq!(message_content_to_string(&parts_content), "First Second");
    }
}
