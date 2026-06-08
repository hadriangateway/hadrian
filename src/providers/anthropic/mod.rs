//! Anthropic Claude API provider.
//!
//! This provider implements the Anthropic Messages API and converts
//! OpenAI-compatible requests to Anthropic format.

mod convert;
mod stream;
mod types;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::response::Response;
use convert::{
    convert_anthropic_to_responses_response, convert_chat_completion_reasoning_config,
    convert_messages, convert_reasoning_config, convert_response,
    convert_responses_input_to_messages, convert_responses_tool_choice, convert_responses_tools,
    convert_stop, convert_tool_choice, convert_tools,
};
use serde::Deserialize;
use stream::{AnthropicToOpenAIStream, AnthropicToResponsesStream};
use types::{AnthropicMetadata, AnthropicRequest, AnthropicResponse};

use crate::{
    api_types::{
        CreateChatCompletionPayload, CreateCompletionPayload, CreateEmbeddingPayload,
        CreateResponsesPayload,
    },
    config::{AnthropicProviderConfig, CircuitBreakerConfig, RetryConfig, StreamingBufferConfig},
    providers::{
        CircuitBreakerRegistry, ModelInfo, ModelsResponse, Provider, ProviderError,
        circuit_breaker::CircuitBreaker,
        error::AnthropicErrorParser,
        image::{ImageFetchConfig, preprocess_messages_for_images},
        response::{error_response, json_response, streaming_response},
        retry::with_circuit_breaker_and_retry,
    },
};

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default max tokens if not specified.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Tokens reserved for the visible answer on top of the thinking budget when
/// extended thinking is enabled, so the reply still has room. Matches the
/// buffer qwen-code adopted for the same Anthropic constraint.
const THINKING_OUTPUT_MARGIN: u32 = 8000;

/// Anthropic requires `max_tokens > thinking.budget_tokens`, and the budget is
/// thinking-only — the visible answer needs headroom on top. `max_tokens` and
/// the thinking budget are derived independently (the budget from
/// `reasoning.effort`, `max_tokens` from the request/config/default), so a
/// common case like effort `medium` (budget 16000) against the 4096 default
/// produces `budget_tokens > max_tokens` and a 400. Raise `max_tokens` so it
/// always clears the budget plus [`THINKING_OUTPUT_MARGIN`].
///
/// Only ever RAISES `max_tokens`, so a generous client-supplied value, the
/// adaptive path, and interleaved thinking (where the budget may legitimately
/// exceed output) are all left untouched.
///
/// NOTE: the result is intentionally not clamped to the model's catalog
/// `limit.output` — the provider has no catalog handle, the computed ceiling
/// (≤ 32000 + margin) stays under every current Anthropic output cap, and
/// Anthropic validates client-specified extremes itself. Revisit if budgets grow.
fn max_tokens_with_thinking_headroom(
    max_tokens: u32,
    thinking: &Option<types::AnthropicThinkingConfig>,
) -> u32 {
    match thinking {
        Some(types::AnthropicThinkingConfig::Enabled { budget_tokens }) => {
            let raised = max_tokens.max(budget_tokens.saturating_add(THINKING_OUTPUT_MARGIN));
            if raised != max_tokens {
                tracing::debug!(
                    original_max_tokens = max_tokens,
                    adjusted_max_tokens = raised,
                    budget_tokens = *budget_tokens,
                    "Raised max_tokens to clear Anthropic thinking budget plus reply headroom"
                );
            }
            raised
        }
        // Disabled / Adaptive / None: no fixed budget to reconcile against.
        _ => max_tokens,
    }
}

/// Compute the `anthropic-beta` header value based on model and thinking config.
///
/// When thinking is enabled on models that match an entry in
/// `interleaved_thinking_models` (substring match), include the
/// `interleaved-thinking-2025-05-14` beta flag. Some Anthropic models reject
/// this header, so the allowlist is configurable.
fn compute_beta_header(
    model: &str,
    thinking: &Option<types::AnthropicThinkingConfig>,
    interleaved_thinking_models: &[String],
) -> Option<String> {
    let thinking_enabled = matches!(
        thinking,
        Some(types::AnthropicThinkingConfig::Enabled { .. })
            | Some(types::AnthropicThinkingConfig::Adaptive)
    );
    if thinking_enabled
        && interleaved_thinking_models
            .iter()
            .any(|pat| !pat.is_empty() && model.contains(pat.as_str()))
    {
        Some("interleaved-thinking-2025-05-14".to_string())
    } else {
        None
    }
}

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    default_model: Option<String>,
    default_max_tokens: Option<u32>,
    timeout: Duration,
    retry: RetryConfig,
    circuit_breaker_config: CircuitBreakerConfig,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
    streaming_buffer: StreamingBufferConfig,
    image_fetch_config: ImageFetchConfig,
    interleaved_thinking_models: Vec<String>,
}

impl AnthropicProvider {
    /// Create a provider from configuration with a shared circuit breaker.
    pub fn from_config_with_registry(
        config: &AnthropicProviderConfig,
        provider_name: &str,
        registry: &CircuitBreakerRegistry,
    ) -> Self {
        Self::from_config_with_registry_and_image_config(
            config,
            provider_name,
            registry,
            ImageFetchConfig::default(),
        )
    }

    /// Create a provider from configuration with a shared circuit breaker and custom image fetch config.
    pub fn from_config_with_registry_and_image_config(
        config: &AnthropicProviderConfig,
        provider_name: &str,
        registry: &CircuitBreakerRegistry,
        image_fetch_config: ImageFetchConfig,
    ) -> Self {
        let circuit_breaker = registry.get_or_create(provider_name, &config.circuit_breaker);

        // Anthropic supports HTTPS image URLs natively, so don't waste cycles
        // re-encoding them as base64 data URLs in the preprocess step.
        let mut image_fetch_config = image_fetch_config;
        image_fetch_config.pass_through_https = true;

        Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            default_model: config.default_model.clone(),
            default_max_tokens: config.default_max_tokens,
            timeout: Duration::from_secs(config.timeout_secs),
            retry: config.retry.clone(),
            circuit_breaker_config: config.circuit_breaker.clone(),
            circuit_breaker,
            streaming_buffer: config.streaming_buffer.clone(),
            image_fetch_config,
            interleaved_thinking_models: config.interleaved_thinking_models.clone(),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Provider for AnthropicProvider {
    fn default_health_check_model(&self) -> Option<&str> {
        Some("claude-haiku-4-5-20251001")
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "anthropic",
            operation = "chat_completion",
            model = %payload.model.as_deref().or(self.default_model.as_deref()).unwrap_or("claude-sonnet-4-20250514"),
            stream = payload.stream
        )
    )]
    async fn create_chat_completion(
        &self,
        client: &reqwest::Client,
        payload: CreateChatCompletionPayload,
    ) -> Result<Response, ProviderError> {
        let model = payload
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

        let max_tokens = payload
            .max_tokens
            .map(|v| v as u32)
            .or(self.default_max_tokens)
            .unwrap_or(DEFAULT_MAX_TOKENS);

        // Preprocess messages to convert HTTP image URLs to data URLs
        // Anthropic only supports base64 images, so we fetch HTTP URLs and convert them
        let mut messages_to_convert = payload.messages;
        preprocess_messages_for_images(
            client,
            &mut messages_to_convert,
            Some(&self.image_fetch_config),
        )
        .await;

        let (system, messages) = convert_messages(messages_to_convert);
        let stream = payload.stream;

        // Convert tools and tool_choice
        let tools = convert_tools(payload.tools);
        let tool_choice = if tools.is_some() {
            convert_tool_choice(payload.tool_choice)
        } else {
            None
        };

        // Convert reasoning config to thinking config (model-aware for adaptive thinking)
        let (thinking, output_config) =
            convert_chat_completion_reasoning_config(payload.reasoning.as_ref(), &model);

        // Ensure max_tokens clears the thinking budget (+ reply headroom).
        let max_tokens = max_tokens_with_thinking_headroom(max_tokens, &thinking);

        // Note: When thinking is enabled, temperature must be 1.0 per Anthropic API requirements
        let temperature = if thinking.is_some() {
            None // Anthropic requires temperature=1 when thinking is enabled, so we don't send it
        } else {
            payload.temperature
        };

        // Build metadata if user is provided
        let metadata = payload.user.map(|user_id| AnthropicMetadata {
            user_id: Some(user_id),
        });

        let anthropic_request = AnthropicRequest {
            model,
            messages,
            max_tokens,
            system,
            temperature,
            top_p: payload.top_p,
            top_k: None, // Not supported in chat completions payload
            stop_sequences: convert_stop(payload.stop),
            stream,
            tools,
            tool_choice,
            thinking,
            output_config,
            metadata,
        };

        // Pre-serialize request body before retry loop to avoid repeated serialization
        let beta_header = compute_beta_header(
            &anthropic_request.model,
            &anthropic_request.thinking,
            &self.interleaved_thinking_models,
        );
        let body = serde_json::to_vec(&anthropic_request).unwrap_or_default();

        let url = format!("{}/v1/messages", self.base_url);
        let api_key = self.api_key.clone();
        let timeout = self.timeout;

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "anthropic",
            "chat_completion",
            || async {
                let mut req = client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .timeout(timeout);
                if let Some(beta) = &beta_header {
                    req = req.header("anthropic-beta", beta.as_str());
                }
                req.body(body.clone()).send().await
            },
        )
        .await?;

        let status = response.status();
        if !status.is_success() {
            return error_response::<AnthropicErrorParser>(response).await;
        }

        if stream {
            // Transform Anthropic SSE events to OpenAI-compatible format
            use futures_util::StreamExt;

            let byte_stream =
                response
                    .bytes_stream()
                    .map(|result| -> Result<bytes::Bytes, std::io::Error> {
                        result.map_err(std::io::Error::other)
                    });
            let transformed_stream =
                AnthropicToOpenAIStream::new(byte_stream, &self.streaming_buffer);

            #[cfg(not(target_arch = "wasm32"))]
            {
                streaming_response(status, transformed_stream)
            }
            #[cfg(target_arch = "wasm32")]
            {
                streaming_response(status, crate::compat::AssertSendStream(transformed_stream))
            }
        } else {
            let anthropic_response: AnthropicResponse = response.json().await?;
            let openai_response = convert_response(anthropic_response);
            json_response(status, &openai_response)
        }
    }

    #[tracing::instrument(
        skip(self, client, payload),
        fields(
            provider = "anthropic",
            operation = "responses",
            model = %payload.model.as_deref().or(self.default_model.as_deref()).unwrap_or("claude-sonnet-4-20250514"),
            stream = payload.stream
        )
    )]
    async fn create_responses(
        &self,
        client: &reqwest::Client,
        payload: CreateResponsesPayload,
    ) -> Result<Response, ProviderError> {
        let model = payload
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

        let max_tokens = payload
            .max_output_tokens
            .map(|v| v as u32)
            .or(self.default_max_tokens)
            .unwrap_or(DEFAULT_MAX_TOKENS);

        let echo_fields = payload.echo_fields_json();

        // Convert Responses API input to Anthropic messages format
        let (system, messages) =
            convert_responses_input_to_messages(payload.input, payload.instructions.clone());

        // Convert tools and tool_choice
        let tools = convert_responses_tools(payload.tools);
        let tool_choice = if tools.is_some() {
            convert_responses_tool_choice(payload.tool_choice)
        } else {
            None
        };

        // Convert reasoning config to thinking config (model-aware for adaptive thinking)
        let (thinking, output_config) =
            convert_reasoning_config(payload.reasoning.as_ref(), &model);

        // Ensure max_tokens clears the thinking budget (+ reply headroom).
        let max_tokens = max_tokens_with_thinking_headroom(max_tokens, &thinking);

        // Note: When thinking is enabled, temperature must be 1.0 per Anthropic API requirements
        let temperature = if thinking.is_some() {
            None // Anthropic requires temperature=1 when thinking is enabled, so we don't send it
        } else {
            payload.temperature
        };

        let stream = payload.stream;

        // Build metadata if user is provided
        let metadata = payload.user.clone().map(|user_id| AnthropicMetadata {
            user_id: Some(user_id),
        });

        let anthropic_request = AnthropicRequest {
            model,
            messages,
            max_tokens,
            system,
            temperature,
            top_p: payload.top_p,
            top_k: None,
            stop_sequences: None, // Responses API doesn't have stop sequences
            stream,
            tools,
            tool_choice,
            thinking,
            output_config,
            metadata,
        };

        // Pre-serialize request body before retry loop to avoid repeated serialization
        let beta_header = compute_beta_header(
            &anthropic_request.model,
            &anthropic_request.thinking,
            &self.interleaved_thinking_models,
        );
        let body = serde_json::to_vec(&anthropic_request).unwrap_or_default();

        let url = format!("{}/v1/messages", self.base_url);
        let api_key = self.api_key.clone();
        let timeout = self.timeout;

        let response = with_circuit_breaker_and_retry(
            self.circuit_breaker.as_deref(),
            &self.circuit_breaker_config,
            &self.retry,
            "anthropic",
            "responses",
            || async {
                let mut req = client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header("content-type", "application/json")
                    .timeout(timeout);
                if let Some(beta) = &beta_header {
                    req = req.header("anthropic-beta", beta.as_str());
                }
                req.body(body.clone()).send().await
            },
        )
        .await?;

        let status = response.status();
        if !status.is_success() {
            return error_response::<AnthropicErrorParser>(response).await;
        }

        if stream {
            // Transform Anthropic SSE events to OpenAI Responses API format
            use futures_util::StreamExt;

            let byte_stream =
                response
                    .bytes_stream()
                    .map(|result| -> Result<bytes::Bytes, std::io::Error> {
                        result.map_err(std::io::Error::other)
                    });
            let transformed_stream =
                AnthropicToResponsesStream::new(byte_stream, &self.streaming_buffer, echo_fields);

            #[cfg(not(target_arch = "wasm32"))]
            {
                streaming_response(status, transformed_stream)
            }
            #[cfg(target_arch = "wasm32")]
            {
                streaming_response(status, crate::compat::AssertSendStream(transformed_stream))
            }
        } else {
            let anthropic_response: AnthropicResponse = response.json().await?;
            let responses_response = convert_anthropic_to_responses_response(
                anthropic_response,
                payload.reasoning.as_ref(),
                payload.user,
            );
            json_response(status, &responses_response.to_json_with_echo(echo_fields))
        }
    }

    async fn create_completion(
        &self,
        _client: &reqwest::Client,
        _payload: CreateCompletionPayload,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "Anthropic does not support legacy completions API".to_string(),
        ))
    }

    async fn create_embedding(
        &self,
        _client: &reqwest::Client,
        _payload: CreateEmbeddingPayload,
    ) -> Result<Response, ProviderError> {
        Err(ProviderError::Internal(
            "Anthropic does not support embeddings API".to_string(),
        ))
    }

    #[tracing::instrument(
        skip(self, client),
        fields(provider = "anthropic", operation = "list_models")
    )]
    async fn list_models(&self, client: &reqwest::Client) -> Result<ModelsResponse, ProviderError> {
        #[derive(Deserialize)]
        struct Page {
            data: Vec<ModelInfo>,
            has_more: bool,
            last_id: Option<String>,
        }

        let mut all_models = Vec::new();
        let mut after_id: Option<String> = None;

        loop {
            let mut url = format!("{}/v1/models?limit=1000", self.base_url);
            if let Some(ref cursor) = after_id {
                url.push_str("&after_id=");
                url.push_str(cursor);
            }

            let api_key = self.api_key.clone();
            let timeout = self.timeout;

            let response = with_circuit_breaker_and_retry(
                self.circuit_breaker.as_deref(),
                &self.circuit_breaker_config,
                &self.retry.for_read_only(),
                "anthropic",
                "list_models",
                || async {
                    client
                        .get(&url)
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", ANTHROPIC_VERSION)
                        .timeout(timeout)
                        .send()
                        .await
                },
            )
            .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                tracing::warn!(
                    status = %status,
                    body = %body,
                    "Failed to list models from Anthropic API"
                );
                return Err(ProviderError::Internal(format!(
                    "Anthropic models API error: {status} - {body}"
                )));
            }

            let page: Page = response.json().await?;
            all_models.extend(page.data);

            if !page.has_more {
                break;
            }
            after_id = page.last_id;
        }

        Ok(ModelsResponse { data: all_models })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> Option<types::AnthropicThinkingConfig> {
        Some(types::AnthropicThinkingConfig::Adaptive)
    }

    fn enabled_budget(budget_tokens: u32) -> Option<types::AnthropicThinkingConfig> {
        Some(types::AnthropicThinkingConfig::Enabled { budget_tokens })
    }

    #[test]
    fn max_tokens_raised_above_thinking_budget() {
        // The logged failure: effort `medium` (budget 16000) with the 4096
        // default would 400. Now it clears the budget plus the reply margin.
        assert_eq!(
            max_tokens_with_thinking_headroom(DEFAULT_MAX_TOKENS, &enabled_budget(16000)),
            16000 + THINKING_OUTPUT_MARGIN,
        );
        // High effort (32000) likewise.
        assert_eq!(
            max_tokens_with_thinking_headroom(DEFAULT_MAX_TOKENS, &enabled_budget(32000)),
            32000 + THINKING_OUTPUT_MARGIN,
        );
    }

    #[test]
    fn max_tokens_only_ever_raised() {
        // A client-supplied ceiling already above budget + margin is preserved.
        assert_eq!(
            max_tokens_with_thinking_headroom(50000, &enabled_budget(16000)),
            50000,
        );
    }

    #[test]
    fn max_tokens_above_budget_still_gets_margin() {
        // Boundary case: max_tokens already exceeds budget_tokens (so it would
        // satisfy Anthropic's raw `max_tokens > budget_tokens` check) but sits
        // below budget + margin — it must still be raised to clear the margin.
        assert_eq!(
            max_tokens_with_thinking_headroom(20000, &enabled_budget(16000)),
            16000 + THINKING_OUTPUT_MARGIN,
        );
    }

    #[test]
    fn max_tokens_untouched_without_fixed_budget() {
        // Adaptive, disabled, and absent thinking have no budget to reconcile.
        assert_eq!(
            max_tokens_with_thinking_headroom(
                4096,
                &Some(types::AnthropicThinkingConfig::Adaptive)
            ),
            4096,
        );
        assert_eq!(
            max_tokens_with_thinking_headroom(
                4096,
                &Some(types::AnthropicThinkingConfig::Disabled)
            ),
            4096,
        );
        assert_eq!(max_tokens_with_thinking_headroom(4096, &None), 4096);
    }

    #[test]
    fn beta_header_set_for_allowed_model() {
        let allow = vec!["opus-4-6".to_string()];
        assert_eq!(
            compute_beta_header("claude-opus-4-6-20260101", &enabled(), &allow),
            Some("interleaved-thinking-2025-05-14".to_string())
        );
    }

    #[test]
    fn beta_header_skipped_for_unlisted_model() {
        let allow = vec!["opus-4-6".to_string()];
        assert_eq!(
            compute_beta_header("claude-sonnet-4-5-20250929", &enabled(), &allow),
            None
        );
    }

    #[test]
    fn beta_header_skipped_when_thinking_disabled() {
        let allow = vec!["opus-4-6".to_string()];
        assert_eq!(
            compute_beta_header("claude-opus-4-6-20260101", &None, &allow),
            None
        );
    }

    #[test]
    fn beta_header_disabled_with_empty_allowlist() {
        assert_eq!(
            compute_beta_header("claude-opus-4-6-20260101", &enabled(), &[]),
            None
        );
    }

    #[test]
    fn beta_header_ignores_empty_pattern() {
        let allow = vec![String::new()];
        assert_eq!(
            compute_beta_header("claude-opus-4-6", &enabled(), &allow),
            None
        );
    }
}
