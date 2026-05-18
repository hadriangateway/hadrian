//! Consolidated execution logic for API endpoints.
//!
//! This module provides a unified framework for executing requests across different
//! API endpoints (chat completions, responses, completions, embeddings) with shared
//! functionality like fallback support, metrics, and tracing.

use axum::response::Response;

use super::ApiError;
#[cfg(feature = "provider-azure")]
use crate::providers::azure_openai;
#[cfg(feature = "provider-bedrock")]
use crate::providers::bedrock;
#[cfg(feature = "provider-vertex")]
use crate::providers::vertex;
use crate::{
    AppState, api_types,
    config::{ProviderConfig, SovereigntyMetadata, SovereigntyRequirements},
    observability::metrics,
    providers::{
        FallbackDecision, Provider, ProviderError, anthropic, build_fallback_chain,
        classify_provider_error, open_ai, should_fallback_on_response_status, test,
    },
    services::{
        container_session::MNT_DATA,
        preprocess_file_search_tools, preprocess_web_search_tools,
        shell_tool::{
            ShellExecutionLocation, ShellNetworkSummary, ShellToolHint, preprocess_shell_tools,
            resolve_shell_environment,
        },
    },
};

/// Whether OpenAI's native `shell` tool spec should be left intact
/// (vs rewritten to a function tool). True in both passthrough modes —
/// `passthrough_openai` (OpenAI's hosted container executes) and
/// `client_passthrough` (the API client executes). In both cases the
/// model should emit native `shell_call` items the OpenAI SDK can
/// recognize; Hadrian-hosted runtimes always rewrite so the executor
/// can intercept function-call output.
fn keep_openai_native_shell(state: &AppState) -> bool {
    state.config.features.shell.keeps_openai_native_shell()
}

/// Build the description hint the model sees on the function-mode shell
/// tool. Drawn from the runtime mode, per-request shell environment
/// overrides intersected with operator caps, and the containers config.
///
/// Re-resolves the per-request env even though `chat.rs` already did so
/// at admission time — preprocessing happens deep in the per-provider
/// dispatch and re-derivation is cheap. Errors fall back to defaults
/// silently because the request would have been rejected with a 400 at
/// admission if the env was actually invalid.
fn build_shell_tool_hint(
    state: &AppState,
    payload: &api_types::CreateResponsesPayload,
) -> ShellToolHint {
    let location = match state.config.features.shell {
        crate::config::ShellRuntimeConfig::ClientPassthrough => ShellExecutionLocation::ApiClient,
        _ => ShellExecutionLocation::HadrianSandbox,
    };

    let containers = &state.config.features.containers;
    let shell_limits = &state.config.features.server_tools.shell_limits;

    let request_env = payload
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find_map(|t| t.as_shell()))
        .and_then(|s| s.environment.as_ref());
    let resolved =
        resolve_shell_environment(request_env, shell_limits, &state.config.features.containers)
            .ok();

    let mem_limit_mb = resolved
        .as_ref()
        .and_then(|r| r.mem_limit_bytes)
        .map(|b| b / (1024 * 1024))
        .or_else(|| shell_limits.default_mem_limit_mb.map(u64::from));

    let network_summary = match resolved.as_ref() {
        Some(r) if !r.egress_policy.allow_hosts.is_empty() => {
            ShellNetworkSummary::Allowlist(r.egress_policy.allow_hosts.clone())
        }
        _ => ShellNetworkSummary::Unknown,
    };

    ShellToolHint {
        location,
        workdir: MNT_DATA,
        container_persistence: containers.enabled
            && matches!(location, ShellExecutionLocation::HadrianSandbox),
        network_summary,
        mem_limit_mb,
        command_timeout_secs: shell_limits.command_timeout_secs,
        ..ShellToolHint::default()
    }
}

// ============================================================================
// Unified Execution Result
// ============================================================================

/// Result of executing an API request, including provider metadata.
///
/// This unified struct replaces the separate `ChatCompletionResult`, `ResponsesResult`,
/// `CompletionResult`, and `EmbeddingResult` structs.
pub struct ExecutionResult {
    /// The HTTP response from the provider
    pub response: Response,
    /// Name of the provider that served the request
    pub provider_name: String,
    /// Name of the model that was used
    pub model_name: String,
}

// ============================================================================
// API Payload Trait
// ============================================================================

/// Trait for API payloads that can be executed against providers.
///
/// This trait abstracts the common operations needed for routing and execution:
/// - Getting and setting the model name
/// - Checking if streaming is enabled
pub trait ApiPayload: Clone + Send + Sync + 'static {
    /// Get the model name from the payload, if set.
    #[allow(dead_code)] // Required trait method for payload inspection
    fn model(&self) -> Option<&str>;

    /// Set the model name on the payload.
    fn set_model(&mut self, model: String);

    /// Check if streaming is enabled for this request.
    /// Returns `false` for payload types that don't support streaming (e.g., embeddings).
    #[allow(dead_code)] // Required trait method for streaming detection
    fn is_streaming(&self) -> bool {
        false
    }
}

// Implement ApiPayload for each payload type

impl ApiPayload for api_types::CreateChatCompletionPayload {
    fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn set_model(&mut self, model: String) {
        self.model = Some(model);
    }

    fn is_streaming(&self) -> bool {
        self.stream
    }
}

impl ApiPayload for api_types::CreateResponsesPayload {
    fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn set_model(&mut self, model: String) {
        self.model = Some(model);
    }

    fn is_streaming(&self) -> bool {
        self.stream
    }
}

impl ApiPayload for api_types::CreateCompletionPayload {
    fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn set_model(&mut self, model: String) {
        self.model = Some(model);
    }

    fn is_streaming(&self) -> bool {
        self.stream
    }
}

impl ApiPayload for api_types::CreateEmbeddingPayload {
    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    // Embeddings don't support streaming, so we use the default (false)
}

// ============================================================================
// Provider Executor Trait
// ============================================================================

/// Trait for executing API requests against providers.
///
/// This trait is implemented for marker types that represent each API operation,
/// allowing us to dispatch to the correct provider method generically.
///
/// On native targets, futures must be `Send` for use with multi-threaded runtimes.
/// On WASM, `Send` is not required (single-threaded).
pub trait ProviderExecutor: Send + Sync + 'static {
    /// The payload type for this operation.
    type Payload: ApiPayload;

    /// Execute the request against the given provider.
    #[cfg(not(target_arch = "wasm32"))]
    fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> impl std::future::Future<Output = Result<Response, ProviderError>> + Send;

    /// Execute the request against the given provider.
    #[cfg(target_arch = "wasm32")]
    fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> impl std::future::Future<Output = Result<Response, ProviderError>>;

    /// Name of the operation for logging/tracing.
    fn operation_name() -> &'static str;
}

// ============================================================================
// Executor Implementations
// ============================================================================

/// Marker type for chat completion operations.
pub struct ChatCompletionExecutor;

impl ProviderExecutor for ChatCompletionExecutor {
    type Payload = api_types::CreateChatCompletionPayload;

    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        match provider_config {
            ProviderConfig::OpenAi(config) => {
                open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
            ProviderConfig::Anthropic(config) => {
                // Get image fetch config from features configuration
                let image_fetch_config = state.config.features.image_fetching.to_runtime_config();
                anthropic::AnthropicProvider::from_config_with_registry_and_image_config(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                    image_fetch_config,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(config) => {
                azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(config) => {
                // Get image fetch config from features configuration
                let image_fetch_config = state.config.features.image_fetching.to_runtime_config();
                bedrock::BedrockProvider::from_config_with_registry_and_image_config(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                    image_fetch_config,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(config) => {
                // Get image fetch config from features configuration
                let image_fetch_config = state.config.features.image_fetching.to_runtime_config();
                vertex::VertexProvider::from_config_with_registry_and_image_config(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                    image_fetch_config,
                )
                .create_chat_completion(&state.http_client, payload)
                .await
            }
            ProviderConfig::Test(config) => {
                test::TestProvider::from_config(config)
                    .create_chat_completion(&state.http_client, payload)
                    .await
            }
        }
    }

    fn operation_name() -> &'static str {
        "chat_completion"
    }
}

/// Marker type for responses API operations.
pub struct ResponsesExecutor;

impl ProviderExecutor for ResponsesExecutor {
    type Payload = api_types::CreateResponsesPayload;

    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        // Shell tool preprocessing rules:
        // - OpenAI / Azure OpenAI: leave native `shell` tool intact when
        //   either passthrough mode is configured (so the model emits
        //   native `shell_call` items the OpenAI SDK / hosted container
        //   can consume); otherwise rewrite to function tool so the
        //   Hadrian-hosted executor can intercept.
        // - Anthropic / Bedrock / Vertex: always rewrite, since these
        //   providers have no native shell tool. Under `client_passthrough`
        //   the rewrite still happens; the model emits `function_call`
        //   items with `name="shell"` that pass through to the client.
        let openai_keep_native_shell = keep_openai_native_shell(state);
        // Build the hint once per provider attempt; rewrites are idempotent
        // and the hint depends only on request payload + operator config.
        let shell_hint = build_shell_tool_hint(state, &payload);
        match provider_config {
            ProviderConfig::OpenAi(config) => {
                let mut payload = payload;
                preprocess_file_search_tools(&mut payload);
                preprocess_web_search_tools(&mut payload);
                if !openai_keep_native_shell {
                    preprocess_shell_tools(&mut payload, &shell_hint);
                }

                open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses(&state.http_client, payload)
                .await
            }
            ProviderConfig::Anthropic(config) => {
                let mut payload = payload;
                preprocess_web_search_tools(&mut payload);
                preprocess_shell_tools(&mut payload, &shell_hint);
                anthropic::AnthropicProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(config) => {
                let mut payload = payload;
                preprocess_file_search_tools(&mut payload);
                preprocess_web_search_tools(&mut payload);
                if !openai_keep_native_shell {
                    preprocess_shell_tools(&mut payload, &shell_hint);
                }

                azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(config) => {
                let mut payload = payload;
                preprocess_file_search_tools(&mut payload);
                preprocess_web_search_tools(&mut payload);
                preprocess_shell_tools(&mut payload, &shell_hint);

                bedrock::BedrockProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(config) => {
                let mut payload = payload;
                preprocess_web_search_tools(&mut payload);
                preprocess_shell_tools(&mut payload, &shell_hint);
                vertex::VertexProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses(&state.http_client, payload)
                .await
            }
            ProviderConfig::Test(config) => {
                let mut payload = payload;
                preprocess_file_search_tools(&mut payload);
                preprocess_web_search_tools(&mut payload);
                preprocess_shell_tools(&mut payload, &shell_hint);

                test::TestProvider::from_config(config)
                    .create_responses(&state.http_client, payload)
                    .await
            }
        }
    }

    fn operation_name() -> &'static str {
        "responses"
    }
}

/// Marker type for the standalone `/v1/responses/compact` endpoint.
///
/// Forwards to providers that implement
/// [`Provider::create_responses_compact`]. Non-OpenAI providers return
/// `Unsupported` from the trait default, which surfaces as HTTP 501
/// to the caller with `error_code = "not_supported"`.
pub struct CompactExecutor;

impl ProviderExecutor for CompactExecutor {
    type Payload = api_types::CreateResponsesPayload;

    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        match provider_config {
            ProviderConfig::OpenAi(config) => {
                open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses_compact(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(config) => {
                azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_responses_compact(&state.http_client, payload)
                .await
            }
            // Every other provider falls through to the trait default
            // (Unsupported → 501). Listing them explicitly keeps the
            // match exhaustive so adding a future provider forces a
            // conscious choice here.
            ProviderConfig::Anthropic(_) => Err(ProviderError::Unsupported(
                "compaction is only supported by OpenAI-compatible providers".to_string(),
            )),
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(_) => Err(ProviderError::Unsupported(
                "compaction is only supported by OpenAI-compatible providers".to_string(),
            )),
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(_) => Err(ProviderError::Unsupported(
                "compaction is only supported by OpenAI-compatible providers".to_string(),
            )),
            ProviderConfig::Test(_) => Err(ProviderError::Unsupported(
                "compaction is only supported by OpenAI-compatible providers".to_string(),
            )),
        }
    }

    fn operation_name() -> &'static str {
        "responses_compact"
    }
}

/// Marker type for completion (legacy) operations.
pub struct CompletionExecutor;

impl ProviderExecutor for CompletionExecutor {
    type Payload = api_types::CreateCompletionPayload;

    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        match provider_config {
            ProviderConfig::OpenAi(config) => {
                open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_completion(&state.http_client, payload)
                .await
            }
            ProviderConfig::Anthropic(config) => {
                anthropic::AnthropicProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(config) => {
                azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(config) => {
                bedrock::BedrockProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_completion(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(config) => {
                vertex::VertexProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_completion(&state.http_client, payload)
                .await
            }
            ProviderConfig::Test(config) => {
                test::TestProvider::from_config(config)
                    .create_completion(&state.http_client, payload)
                    .await
            }
        }
    }

    fn operation_name() -> &'static str {
        "completion"
    }
}

/// Marker type for embedding operations.
pub struct EmbeddingExecutor;

impl ProviderExecutor for EmbeddingExecutor {
    type Payload = api_types::CreateEmbeddingPayload;

    async fn execute(
        state: &AppState,
        provider_name: &str,
        provider_config: &ProviderConfig,
        payload: Self::Payload,
    ) -> Result<Response, ProviderError> {
        match provider_config {
            ProviderConfig::OpenAi(config) => {
                open_ai::OpenAICompatibleProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_embedding(&state.http_client, payload)
                .await
            }
            ProviderConfig::Anthropic(config) => {
                anthropic::AnthropicProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_embedding(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-azure")]
            ProviderConfig::AzureOpenAi(config) => {
                azure_openai::AzureOpenAIProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_embedding(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-bedrock")]
            ProviderConfig::Bedrock(config) => {
                bedrock::BedrockProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_embedding(&state.http_client, payload)
                .await
            }
            #[cfg(feature = "provider-vertex")]
            ProviderConfig::Vertex(config) => {
                vertex::VertexProvider::from_config_with_registry(
                    config,
                    provider_name,
                    &state.circuit_breakers,
                )
                .create_embedding(&state.http_client, payload)
                .await
            }
            ProviderConfig::Test(config) => {
                test::TestProvider::from_config(config)
                    .create_embedding(&state.http_client, payload)
                    .await
            }
        }
    }

    fn operation_name() -> &'static str {
        "embedding"
    }
}

// ============================================================================
// Generic Fallback Executor
// ============================================================================

/// Execute an API request with fallback support.
///
/// This function provides a unified fallback mechanism for all API endpoints.
/// It tries the primary provider first, then falls back to configured alternatives
/// on retryable errors (5xx, timeout, circuit breaker open).
///
/// # Type Parameters
///
/// * `E` - The executor type that determines which provider method to call
///
/// # Arguments
///
/// * `state` - Application state containing providers and configuration
/// * `primary_provider_name` - Name of the primary provider to try first
/// * `primary_provider_config` - Configuration for the primary provider
/// * `primary_model_name` - Model name to use
/// * `payload` - The API request payload
///
/// # Returns
///
/// An `ExecutionResult` containing the response and provider metadata, or an `ApiError`.
#[tracing::instrument(
    skip(state, primary_provider_config, payload),
    fields(
        operation = %E::operation_name(),
        primary_provider = %primary_provider_name,
        primary_model = %primary_model_name,
        fallback_used = tracing::field::Empty,
        final_provider = tracing::field::Empty,
        final_model = tracing::field::Empty,
    )
)]
pub async fn execute_with_fallback<E: ProviderExecutor>(
    state: &AppState,
    primary_provider_name: String,
    primary_provider_config: ProviderConfig,
    primary_model_name: String,
    payload: E::Payload,
    sovereignty_requirements: Option<&SovereigntyRequirements>,
) -> Result<ExecutionResult, ApiError> {
    // Build fallback chain
    let fallback_chain = build_fallback_chain(
        &primary_provider_name,
        &primary_model_name,
        &state.config.providers,
    );

    // Track which provider we last tried (for metrics)
    let mut last_provider = primary_provider_name.clone();
    let mut last_model = primary_model_name.clone();

    // Hold a template clone for the fallback chain only when needed; the
    // primary call takes the original payload by value to avoid one clone in
    // the common no-fallback path.
    let payload_for_fallbacks = if fallback_chain.is_empty() {
        None
    } else {
        Some(payload.clone())
    };
    let mut current_payload = payload;
    current_payload.set_model(primary_model_name.clone());

    // Store the last response for chain exhaustion case
    let mut last_response: Option<Response> = None;

    match E::execute(
        state,
        &primary_provider_name,
        &primary_provider_config,
        current_payload,
    )
    .await
    {
        Ok(response) => {
            // Check if response status should trigger fallback (5xx errors)
            let status = response.status();
            if should_fallback_on_response_status(status) && !fallback_chain.is_empty() {
                tracing::info!(
                    provider = %primary_provider_name,
                    model = %primary_model_name,
                    status = %status,
                    fallback_count = fallback_chain.len(),
                    "Primary provider returned error status, trying fallbacks"
                );
                last_response = Some(response);
            } else {
                // Success or non-retryable error - return immediately
                tracing::Span::current().record("fallback_used", false);
                tracing::Span::current().record("final_provider", &primary_provider_name);
                tracing::Span::current().record("final_model", &primary_model_name);

                return Ok(ExecutionResult {
                    response,
                    provider_name: primary_provider_name,
                    model_name: primary_model_name,
                });
            }
        }
        Err(err) => {
            // Check if we should retry with fallback
            let decision = classify_provider_error(&err);
            if decision == FallbackDecision::NoRetry || fallback_chain.is_empty() {
                return Err(provider_error_to_api_error(err));
            }

            tracing::info!(
                provider = %primary_provider_name,
                model = %primary_model_name,
                error = %err,
                fallback_count = fallback_chain.len(),
                "Primary provider failed, trying fallbacks"
            );
        }
    }

    // Try each fallback in order. `payload_for_fallbacks` is `Some` whenever
    // `fallback_chain` is non-empty (which is the only case we reach this loop
    // with work to do), so unwrapping is safe.
    let payload_template = payload_for_fallbacks
        .expect("payload_for_fallbacks is Some when fallback_chain is non-empty");
    let mut last_error: Option<ProviderError> = None;

    for (idx, fallback) in fallback_chain.iter().enumerate() {
        let attempt = idx + 1;

        // Get the fallback provider config
        let Some(fallback_config) = state.config.providers.get(&fallback.provider_name) else {
            tracing::warn!(
                provider = %fallback.provider_name,
                "Fallback provider not found, skipping"
            );
            continue;
        };

        // Re-check the circuit breaker right before we call this fallback.
        // The chain was built once up front, but a provider may have tripped
        // its breaker since then (often *because of* the failures that drove
        // us into the fallback path). Skip provider+model combos whose breaker
        // is open so we don't waste a hop poking a known-down upstream.
        if let Some(breaker) = state.circuit_breakers.get(&fallback.provider_name)
            && let Err(cb_err) = breaker.check()
        {
            tracing::info!(
                provider = %fallback.provider_name,
                model = %fallback.model_name,
                error = %cb_err,
                "Skipping fallback: circuit breaker is open"
            );
            continue;
        }

        // Check sovereignty requirements for fallback provider/model
        if let Some(reqs) = sovereignty_requirements {
            let model_config = fallback_config.get_model_config(&fallback.model_name);
            let provider_sov = fallback_config.sovereignty();
            let model_sov = model_config.and_then(|mc| mc.sovereignty.as_ref());
            let resolved = SovereigntyMetadata::merge(provider_sov, model_sov).unwrap_or_default();
            let open_weights = model_config
                .and_then(|mc| mc.open_weights)
                .or_else(|| {
                    let catalog_provider_id = crate::catalog::resolve_catalog_provider_id(
                        fallback_config.provider_type_name(),
                        fallback_config.base_url(),
                        fallback_config.catalog_provider(),
                    )?;
                    state
                        .model_catalog
                        .lookup(&catalog_provider_id, &fallback.model_name)
                        .map(|e| e.open_weights)
                })
                .unwrap_or(false);

            if let Err(reason) = reqs.check(&resolved, open_weights) {
                tracing::debug!(
                    provider = %fallback.provider_name,
                    model = %fallback.model_name,
                    reason = %reason,
                    "Fallback provider skipped due to sovereignty requirements"
                );
                continue;
            }
        }

        // Update payload with fallback model
        let mut fallback_payload = payload_template.clone();
        fallback_payload.set_model(fallback.model_name.clone());

        tracing::debug!(
            provider = %fallback.provider_name,
            model = %fallback.model_name,
            attempt = attempt,
            "Trying fallback provider"
        );

        match E::execute(
            state,
            &fallback.provider_name,
            fallback_config,
            fallback_payload,
        )
        .await
        {
            Ok(response) => {
                // Check if response status should trigger fallback to next provider
                let status = response.status();
                if should_fallback_on_response_status(status) {
                    tracing::warn!(
                        provider = %fallback.provider_name,
                        model = %fallback.model_name,
                        status = %status,
                        attempt = attempt,
                        "Fallback provider returned error status"
                    );

                    // Record fallback failure metrics
                    metrics::record_fallback_attempt(
                        &last_provider,
                        &fallback.provider_name,
                        &last_model,
                        &fallback.model_name,
                        false,
                        attempt,
                    );

                    // Update tracking for next attempt
                    last_provider = fallback.provider_name.clone();
                    last_model = fallback.model_name.clone();
                    last_response = Some(response);
                    continue;
                }

                tracing::info!(
                    provider = %fallback.provider_name,
                    model = %fallback.model_name,
                    attempt = attempt,
                    "Fallback provider succeeded"
                );

                // Record fallback success metrics
                metrics::record_fallback_attempt(
                    &last_provider,
                    &fallback.provider_name,
                    &last_model,
                    &fallback.model_name,
                    true,
                    attempt,
                );

                // Update span with final result
                tracing::Span::current().record("fallback_used", true);
                tracing::Span::current().record("final_provider", &fallback.provider_name);
                tracing::Span::current().record("final_model", &fallback.model_name);

                return Ok(ExecutionResult {
                    response,
                    provider_name: fallback.provider_name.clone(),
                    model_name: fallback.model_name.clone(),
                });
            }
            Err(err) => {
                let decision = classify_provider_error(&err);
                tracing::warn!(
                    provider = %fallback.provider_name,
                    model = %fallback.model_name,
                    error = %err,
                    decision = ?decision,
                    "Fallback provider failed"
                );

                // Record fallback failure metrics
                metrics::record_fallback_attempt(
                    &last_provider,
                    &fallback.provider_name,
                    &last_model,
                    &fallback.model_name,
                    false,
                    attempt,
                );

                // Update tracking for next attempt
                last_provider = fallback.provider_name.clone();
                last_model = fallback.model_name.clone();

                // If error is not retryable, return immediately
                if decision == FallbackDecision::NoRetry {
                    return Err(provider_error_to_api_error(err));
                }

                last_error = Some(err);
            }
        }
    }

    // All fallbacks exhausted - record metric
    metrics::record_fallback_exhausted(
        &primary_provider_name,
        &primary_model_name,
        fallback_chain.len(),
    );

    tracing::error!(
        primary_provider = %primary_provider_name,
        primary_model = %primary_model_name,
        chain_length = fallback_chain.len(),
        "All fallback providers exhausted"
    );

    // If we have a last response (from HTTP error), return it
    // Otherwise return the last ProviderError
    if let Some(response) = last_response {
        tracing::Span::current().record("fallback_used", true);
        tracing::Span::current().record("final_provider", &last_provider);
        tracing::Span::current().record("final_model", &last_model);

        return Ok(ExecutionResult {
            response,
            provider_name: last_provider,
            model_name: last_model,
        });
    }

    Err(provider_error_to_api_error(last_error.unwrap_or_else(
        || ProviderError::Internal("All fallbacks exhausted".to_string()),
    )))
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert a provider error to an API error. The full error string is logged
/// for operator debugging (it can contain internal URLs/paths from upstream
/// SDKs) while only a generic message is returned to the client.
/// `CircuitBreakerOpen` is exposed verbatim because its display string is a
/// curated message we control (provider name + retry-at hint).
pub fn provider_error_to_api_error(e: ProviderError) -> ApiError {
    use http::StatusCode;

    let (status, code, public_message) = match &e {
        ProviderError::Request(_) => (
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "Upstream provider request failed".to_string(),
        ),
        ProviderError::ResponseBuilder(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_builder_error",
            "Failed to build response".to_string(),
        ),
        ProviderError::Internal(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Internal provider error".to_string(),
        ),
        ProviderError::Unsupported(msg) => {
            (StatusCode::NOT_IMPLEMENTED, "not_supported", msg.clone())
        }
        ProviderError::CircuitBreakerOpen(cb) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "circuit_breaker_open",
            cb.to_string(),
        ),
    };

    tracing::error!(error_code = %code, error = %e, "Provider error converted to API error");

    ApiError::new(status, code, public_message)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::StatusCode;

    use super::*;
    use crate::{
        api_types::{Message, MessageContent},
        config::{GatewayConfig, ProvidersConfig},
        events::EventBus,
        providers::CircuitBreakerRegistry,
    };

    /// Create a minimal AppState for testing with the given providers config.
    fn create_test_state(providers: ProvidersConfig) -> AppState {
        let mut config = GatewayConfig::parse("").expect("Empty config should parse");
        config.providers = providers;
        let config = Arc::new(config);

        AppState {
            http_client: reqwest::Client::new(),
            config: config.clone(),
            db: None,
            services: None,
            cache: None,
            secrets: None,
            dlq: None,
            pricing: Arc::new(crate::pricing::PricingConfig::default()),
            circuit_breakers: CircuitBreakerRegistry::new(),
            provider_health: crate::jobs::ProviderHealthStateRegistry::new(),
            task_tracker: tokio_util::task::TaskTracker::new(),
            usage_drain: {
                let tracker = tokio_util::task::TaskTracker::new();
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
            event_bus: Arc::new(EventBus::new()),
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

    /// Create a simple chat completion payload for testing.
    fn make_chat_payload(model: &str) -> api_types::CreateChatCompletionPayload {
        api_types::CreateChatCompletionPayload {
            messages: vec![Message::User {
                content: MessageContent::Text("Hello".to_string()),
                name: None,
            }],
            model: Some(model.to_string()),
            stream: false,
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

    /// Parse a TOML providers config.
    fn parse_providers(toml: &str) -> ProvidersConfig {
        toml::from_str(toml).expect("Failed to parse providers config")
    }

    // =========================================================================
    // Test: Fallback on HTTP 5xx errors
    // =========================================================================

    #[tokio::test]
    async fn test_fallback_on_http_5xx_error() {
        // Primary provider returns 503, backup should succeed
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503, message = "Service Unavailable" }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
            # No failure mode = success
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok(), "Should succeed via fallback");
        let result = result.unwrap();
        assert_eq!(result.provider_name, "backup", "Should use backup provider");
        assert_eq!(result.response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_fallback_on_http_500_error() {
        // Primary provider returns 500, backup should succeed
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok(), "Should succeed via fallback");
        assert_eq!(result.unwrap().provider_name, "backup");
    }

    #[tokio::test]
    async fn test_fallback_on_http_502_error() {
        // Primary provider returns 502 Bad Gateway
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 502 }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok(), "Should succeed via fallback");
        assert_eq!(result.unwrap().provider_name, "backup");
    }

    // =========================================================================
    // Test: Fallback on connection errors
    // =========================================================================

    #[tokio::test]
    async fn test_fallback_on_connection_error() {
        // Primary provider has connection error, backup should succeed
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "connection_error", message = "Connection refused" }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        // Connection errors return ProviderError::Internal which has NoRetry decision
        // This is intentional - the TestProvider returns Internal error for connection errors
        // In real scenarios, reqwest errors would be classified differently
        // For now, verify the error is handled appropriately
        assert!(
            result.is_err(),
            "Connection error from TestProvider returns Internal error (non-retryable)"
        );
    }

    // =========================================================================
    // Test: No fallback on 4xx errors
    // =========================================================================

    #[tokio::test]
    async fn test_no_fallback_on_400_bad_request() {
        // 400 Bad Request should NOT trigger fallback
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 400, message = "Bad Request" }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        // 4xx errors from TestProvider come back as successful HTTP responses with error status
        // The execute_with_fallback checks ProviderError classification, not response status
        // TestProvider returns Ok(Response) with status 400, which is considered success at provider level
        assert!(
            result.is_ok(),
            "400 response is returned as-is (not a ProviderError)"
        );
        let result = result.unwrap();
        assert_eq!(
            result.provider_name, "primary",
            "Should use primary (no fallback for 400)"
        );
        assert_eq!(result.response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_no_fallback_on_401_unauthorized() {
        // 401 Unauthorized should NOT trigger fallback
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 401 }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "primary");
        assert_eq!(result.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_no_fallback_on_429_rate_limit() {
        // 429 Too Many Requests should NOT trigger fallback
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 429 }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "primary");
        assert_eq!(result.response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    // =========================================================================
    // Test: Fallback chain exhaustion
    // =========================================================================

    #[tokio::test]
    async fn test_fallback_chain_exhaustion() {
        // All providers fail with 5xx
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
            fallback_providers = ["backup1", "backup2"]

            [backup1]
            type = "test"
            failure_mode = { type = "http_error", status_code = 502 }

            [backup2]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503 }
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        // All providers return 5xx, but these are Ok(Response) with error status
        // The fallback logic looks at ProviderError, not response status
        // So the last provider's response is returned
        assert!(result.is_ok());
        // The test providers return HTTP error responses, not ProviderErrors
        // So we get the response from the last fallback
        let result = result.unwrap();
        assert_eq!(result.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_no_fallback_configured() {
        // Primary fails but no fallbacks configured
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
            # No fallback_providers
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        // 500 is returned as HTTP response (not ProviderError)
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "primary");
        assert_eq!(result.response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // =========================================================================
    // Test: Model-level fallbacks
    // =========================================================================

    #[tokio::test]
    async fn test_model_fallback_same_provider() {
        // Model-specific fallback to a different model on the same provider
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503 }

            [primary.model_fallbacks]
            "gpt-4" = [{ model = "gpt-3.5-turbo" }]

            [backup]
            type = "test"
            # This provider succeeds
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "gpt-4".to_string(),
            make_chat_payload("gpt-4"),
            None,
        )
        .await;

        // Primary fails for gpt-4, model fallback to gpt-3.5-turbo on same provider
        // But that also fails (same failure_mode applies to all models)
        // No provider fallback configured, so we get the error response
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().response.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn test_model_fallback_different_provider() {
        // Model-specific fallback to a different provider
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503 }

            [primary.model_fallbacks]
            "gpt-4" = [{ provider = "backup", model = "claude-3" }]

            [backup]
            type = "test"
            # This provider succeeds
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "gpt-4".to_string(),
            make_chat_payload("gpt-4"),
            None,
        )
        .await;

        // Primary fails for gpt-4, model fallback sends to backup provider
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "backup");
        assert_eq!(result.model_name, "claude-3");
        assert_eq!(result.response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_model_fallbacks_tried_before_provider_fallbacks() {
        // Verify model fallbacks are tried before provider-level fallbacks
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503 }
            fallback_providers = ["provider_fallback"]

            [primary.model_fallbacks]
            "gpt-4" = [{ provider = "model_fallback", model = "alt-model" }]

            [model_fallback]
            type = "test"
            # This provider succeeds

            [provider_fallback]
            type = "test"
            # This should not be reached
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "gpt-4".to_string(),
            make_chat_payload("gpt-4"),
            None,
        )
        .await;

        // Model fallback should be tried first and succeed
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(
            result.provider_name, "model_fallback",
            "Model fallback should be used before provider fallback"
        );
    }

    // =========================================================================
    // Test: Primary provider success (no fallback needed)
    // =========================================================================

    #[tokio::test]
    async fn test_primary_success_no_fallback() {
        // Primary succeeds, no fallback should be attempted
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            # No failure mode = success
            fallback_providers = ["backup"]

            [backup]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "primary");
        assert_eq!(result.response.status(), StatusCode::OK);
    }

    // =========================================================================
    // Test: Fallback chain with multiple levels
    // =========================================================================

    #[tokio::test]
    async fn test_fallback_chain_second_fallback_succeeds() {
        // Primary fails, first fallback fails, second fallback succeeds
        let providers = parse_providers(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
            fallback_providers = ["backup1", "backup2"]

            [backup1]
            type = "test"
            failure_mode = { type = "http_error", status_code = 502 }

            [backup2]
            type = "test"
            # This one succeeds
        "#,
        );

        let state = create_test_state(providers.clone());
        let primary_config = providers.get("primary").unwrap().clone();

        let result = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            "primary".to_string(),
            primary_config,
            "test-model".to_string(),
            make_chat_payload("test-model"),
            None,
        )
        .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.provider_name, "backup2");
        assert_eq!(result.response.status(), StatusCode::OK);
    }
}
