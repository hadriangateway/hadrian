use std::time::Duration;

use axum::{Extension, Json, body::Body, extract::State, http::HeaderMap, response::Response};
use axum_valid::Valid;
use chrono::Utc;
use http::StatusCode;

use super::{
    ApiError, check_sovereignty, log_guardrails_evaluation, log_output_guardrails_evaluation,
    messages_contain_images, reasoning_effort_to_string, response_format_to_string,
    responses_reasoning_effort_to_string, should_bypass_cache,
};
use crate::{
    AppState, api_types,
    auth::AuthenticatedRequest,
    authz::RequestContext,
    cache::{CacheLookupResult, CacheTenantScope, SemanticLookupResult, StoreParams},
    middleware::{AuthzContext, ClientInfo, RequestId},
    models::UsageLogEntry,
    routes::execution::{
        ChatCompletionExecutor, CompactExecutor, CompletionExecutor, ExecutionResult,
        ProviderExecutor, ResponsesExecutor, execute_with_fallback,
    },
    routing::{resolver, route_model_extended, route_models_extended},
    services::response_persister::persist_non_streaming,
};

/// Cache status for tracking cache hits/misses in response headers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CacheStatus {
    /// No caching (streaming request, non-deterministic, etc.)
    None,
    /// Cache miss - request is cacheable but not found
    Miss,
}

/// Build a tenant scope from the optional API-key auth, used to key cache
/// entries so two tenants never share a response/embedding cache hit.
pub(super) fn tenant_scope_from_auth(
    auth: Option<&Extension<AuthenticatedRequest>>,
) -> CacheTenantScope {
    let api_key = auth.and_then(|a| a.api_key());
    CacheTenantScope {
        org_id: api_key.and_then(|k| k.org_id.map(|id| id.to_string())),
        project_id: api_key.and_then(|k| k.project_id.map(|id| id.to_string())),
        api_key_id: api_key.map(|k| k.key.id.to_string()),
        user_id: api_key.and_then(|k| match &k.key.owner {
            crate::models::ApiKeyOwner::User { user_id } => Some(user_id.to_string()),
            _ => None,
        }),
    }
}

/// Apply output guardrails to a non-streaming response.
///
/// Extracts assistant content from the response body, evaluates it against guardrails,
/// and applies the configured action (block, warn, redact, etc.).
///
/// Returns the (potentially modified) response and headers to add.
pub(super) async fn apply_output_guardrails(
    state: &AppState,
    response: Response,
    user_id: Option<String>,
    auth: Option<&Extension<AuthenticatedRequest>>,
    ip_address: Option<String>,
    user_agent: Option<String>,
) -> Result<(Response, Vec<(&'static str, String)>), ApiError> {
    let output_guardrails = state.output_guardrails.as_ref().unwrap();

    // Read the response body
    let (parts, body) = response.into_parts();
    let body_bytes =
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for output guardrails");
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "response_read_error",
                    "Failed to read response for guardrails evaluation",
                ));
            }
        };

    // Extract assistant content from the response
    let assistant_content = crate::guardrails::extract_assistant_content_from_response(&body_bytes);

    // If no content to evaluate, return the original response
    if assistant_content.is_empty() {
        let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
        return Ok((response, Vec::new()));
    }

    // Evaluate the content
    let result = output_guardrails
        .evaluate_response(&assistant_content, None, user_id.as_deref())
        .await;

    match result {
        Ok(guardrails_result) => {
            let headers = guardrails_result.to_headers();

            // Log audit event for output guardrails evaluation
            log_output_guardrails_evaluation(
                state,
                auth,
                output_guardrails.provider_name(),
                &guardrails_result,
                None,
                ip_address,
                user_agent,
            );

            // Check if content should be blocked
            if guardrails_result.is_blocked() {
                let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                    crate::guardrails::ContentSource::LlmOutput,
                    "Response blocked by output guardrails",
                    guardrails_result.violations().to_vec(),
                );
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "guardrails_output_blocked",
                    error.to_string(),
                ));
            }

            // Check if content should be redacted
            if let Some(modified_content) = guardrails_result.modified_content() {
                // Rebuild the response with the modified content
                let modified_body = modify_response_content(&body_bytes, modified_content)
                    .unwrap_or_else(|| {
                        // If we can't modify the JSON, return the original
                        body_bytes.to_vec()
                    });
                let response = Response::from_parts(parts, Body::from(modified_body));
                return Ok((response, headers));
            }

            // Log warnings if any violations were found but allowed
            if !guardrails_result.response.violations.is_empty() {
                tracing::info!(
                    violations = ?guardrails_result.response.violations.len(),
                    "Output guardrails found violations but allowed response"
                );
            }

            // Return the original response with headers
            let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
            Ok((response, headers))
        }
        Err(e) => {
            // Guardrails evaluation failed
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::INTERNAL_SERVER_ERROR,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            Err(ApiError::new(status, e.error_code(), e.to_string()))
        }
    }
}

/// Returns true if the resolved provider can handle a shell-tool spec
/// forwarded verbatim (i.e. it implements OpenAI's hosted shell-tool
/// runtime). Used to gate `ShellRuntimeConfig::PassthroughOpenAI`.
fn provider_supports_passthrough_shell(provider: &crate::config::ProviderConfig) -> bool {
    use crate::config::ProviderConfig;
    matches!(provider, ProviderConfig::OpenAi(_)) || {
        #[cfg(feature = "provider-azure")]
        {
            matches!(provider, ProviderConfig::AzureOpenAi(_))
        }
        #[cfg(not(feature = "provider-azure"))]
        {
            false
        }
    }
}

/// Modifies the assistant content in a chat completion response JSON.
///
/// Returns the modified response body, or None if modification failed.
fn modify_response_content(body: &[u8], new_content: &str) -> Option<Vec<u8>> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;

    // Modify choices[0].message.content
    if let Some(choices) = json.get_mut("choices").and_then(|c| c.as_array_mut())
        && let Some(first_choice) = choices.first_mut()
        && let Some(message) = first_choice.get_mut("message")
    {
        message["content"] = serde_json::Value::String(new_content.to_string());
    }

    serde_json::to_vec(&json).ok()
}

/// Build a [`UsageLogEntry`] for streaming cost tracking.
///
/// When authenticated, attributes usage to the principal (user, org, project, etc.).
/// When anonymous (no auth configured), attributes to the default anonymous user/org
/// so that streaming requests are tracked the same way the middleware tracks
/// non-streaming anonymous requests.
pub(super) fn build_streaming_usage_entry(
    auth: &Option<Extension<AuthenticatedRequest>>,
    state: &AppState,
    model: &str,
    provider: &str,
    header_project_id: Option<uuid::Uuid>,
) -> Option<UsageLogEntry> {
    if let Some(Extension(auth)) = auth {
        let api_key = auth.api_key();
        Some(UsageLogEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: api_key.map(|k| k.key.id),
            user_id: auth.user_id(),
            org_id: api_key
                .and_then(|k| k.org_id)
                .or_else(|| auth.principal().org_id()),
            project_id: api_key.and_then(|k| k.project_id).or(header_project_id),
            team_id: api_key.and_then(|k| k.team_id),
            service_account_id: api_key.and_then(|k| k.service_account_id),
            model: model.to_string(),
            provider: provider.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents: None,
            http_referer: None,
            request_at: Utc::now(),
            streamed: true,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: None,
            pricing_source: crate::pricing::CostPricingSource::None,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
        })
    } else if state.default_user_id.is_some() || state.default_org_id.is_some() {
        // Anonymous mode: attribute to the default user/org so streaming usage
        // is tracked the same way middleware tracks non-streaming anonymous usage.
        Some(UsageLogEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: None,
            user_id: state.default_user_id,
            org_id: state.default_org_id,
            project_id: header_project_id,
            team_id: None,
            service_account_id: None,
            model: model.to_string(),
            provider: provider.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents: None,
            http_referer: None,
            request_at: Utc::now(),
            streamed: true,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: None,
            pricing_source: crate::pricing::CostPricingSource::None,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
        })
    } else {
        None
    }
}

/// Wraps a streaming response with guardrails filtering.
///
/// This function intercepts the SSE stream, extracts content, and evaluates
/// it against guardrails policies. The behavior depends on the configured mode:
/// - FinalOnly: Pass chunks through, evaluate complete response at end
/// - Buffered: Evaluate periodically during streaming
/// - PerChunk: Evaluate each chunk individually
pub fn wrap_streaming_with_guardrails(
    response: Response,
    output_guardrails: &crate::guardrails::OutputGuardrails,
    user_id: Option<String>,
    request_id: Option<String>,
) -> Response {
    use futures_util::StreamExt;

    // Check if this is a streaming response
    let is_streaming = response
        .headers()
        .get("Transfer-Encoding")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("chunked"))
        .unwrap_or(false);

    if !is_streaming {
        return response;
    }

    let (parts, body) = response.into_parts();

    // Convert body to byte stream
    let stream = body.into_data_stream().map(
        |result: Result<bytes::Bytes, axum::Error>| -> Result<bytes::Bytes, std::io::Error> {
            result.map_err(std::io::Error::other)
        },
    );

    // Create streaming guardrails config
    let config = crate::guardrails::StreamingGuardrailsConfig {
        mode: output_guardrails.streaming_mode(),
        request_id,
        user_id,
        retry_config: crate::guardrails::GuardrailsRetryConfig::default(),
        on_error: output_guardrails.on_error(),
    };

    // Wrap with guardrails filter stream
    let guardrails_stream = crate::guardrails::GuardrailsFilterStream::new(
        stream,
        output_guardrails.provider(),
        output_guardrails.action_executor(),
        config,
    );

    let new_body = axum::body::Body::from_stream(guardrails_stream);
    tracing::debug!("Streaming response wrapped with guardrails filter");

    Response::from_parts(parts, new_body)
}

/// Create a chat completion
///
/// Creates a model response for the given chat conversation. Supports both streaming and
/// non-streaming responses. The model can be specified using provider prefixes (e.g.,
/// `openai/gpt-4o`) or with dynamic routing for multi-tenant configurations.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/chat/completions",
    tag = "chat",
    request_body(
        content = api_types::CreateChatCompletionPayload,
        examples(
            ("Simple" = (
                summary = "Simple text completion",
                value = json!({
                    "model": "openai/gpt-4o",
                    "messages": [
                        {"role": "user", "content": "Hello, how are you?"}
                    ]
                })
            )),
            ("With system prompt" = (
                summary = "Completion with system prompt and parameters",
                value = json!({
                    "model": "anthropic/claude-sonnet-4-20250514",
                    "messages": [
                        {"role": "system", "content": "You are a helpful assistant."},
                        {"role": "user", "content": "Explain quantum computing in simple terms."}
                    ],
                    "max_tokens": 500,
                    "temperature": 0.7
                })
            )),
            ("Streaming" = (
                summary = "Streaming completion",
                value = json!({
                    "model": "openai/gpt-4o",
                    "messages": [
                        {"role": "user", "content": "Write a short poem about coding."}
                    ],
                    "stream": true
                })
            )),
            ("With tools" = (
                summary = "Completion with function calling",
                value = json!({
                    "model": "openai/gpt-4o",
                    "messages": [
                        {"role": "user", "content": "What's the weather in San Francisco?"}
                    ],
                    "tools": [{
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get the current weather for a location",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "location": {"type": "string", "description": "City name"}
                                },
                                "required": ["location"]
                            }
                        }
                    }],
                    "tool_choice": "auto"
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Chat completion response (streaming returns SSE events)",
            example = json!({
                "id": "chatcmpl-abc123",
                "object": "chat.completion",
                "created": 1733580800,
                "model": "gpt-4o-2024-08-06",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! I'm doing well, thank you for asking. How can I help you today?"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 18,
                    "total_tokens": 30
                }
            })
        ),
        (status = 400, description = "Bad request - invalid model, missing fields, or validation error",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "routing_error",
                    "message": "Model 'invalid-model' not found"
                }
            })
        ),
        (status = 401, description = "Unauthorized - invalid or missing API key",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "invalid_api_key",
                    "message": "Invalid API key provided"
                }
            })
        ),
        (status = 429, description = "Rate limit exceeded",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "rate_limit_exceeded",
                    "message": "Rate limit exceeded: 100 requests per minute",
                    "details": {
                        "limit": 100,
                        "window": "minute",
                        "retry_after_secs": 30
                    }
                }
            })
        ),
        (status = 502, description = "Provider error - upstream LLM provider returned an error",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "provider_error",
                    "message": "Upstream provider returned error: Service temporarily unavailable"
                }
            })
        ),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.chat_completions",
    skip(state, headers, auth, authz, request_id, client_info, payload),
    fields(
        model = %payload.model.as_deref().unwrap_or("default"),
        streaming = payload.stream,
    )
)]
pub async fn api_v1_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    request_id: Option<Extension<RequestId>>,
    client_info: Option<Extension<ClientInfo>>,
    Valid(Json(mut payload)): Valid<Json<api_types::CreateChatCompletionPayload>>,
) -> Result<Response, ApiError> {
    let (ci_ip, ci_ua) = client_info
        .map(|Extension(ci)| (ci.ip_address, ci.user_agent))
        .unwrap_or_default();

    // Route the model to a provider with dynamic support
    let model_clone = payload.model.clone();
    let is_streaming = payload.stream;
    let routed = route_model_extended(model_clone.as_deref(), &state.config.providers)?;

    // Resolve to concrete provider configuration
    let resolved = resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        auth.as_ref().map(|e| &e.0),
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "provider_resolution_error",
            format!("Failed to resolve provider: {}", e),
        )
    })?;
    let provider_source = resolved.source;
    let (provider_name, provider_config, model_name) = (
        resolved.provider_name,
        resolved.provider_config,
        resolved.model,
    );

    // Update the payload with the resolved model name (provider prefix stripped)
    payload.model = Some(model_name.clone());

    // Check model restrictions if API key auth is used
    // Use original model string (with provider prefix) for restriction check
    if let Some(Extension(ref auth)) = auth
        && let Some(api_key) = auth.api_key()
    {
        let model_to_check = model_clone.as_deref().unwrap_or(&model_name);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
        // Build request context from payload
        let mut request_ctx = RequestContext::new()
            .with_messages_count(payload.messages.len() as u64)
            .with_tools(payload.tools.is_some())
            .with_file_search(false) // file_search is only in Responses API
            .with_stream(payload.stream)
            .with_images(messages_contain_images(&payload.messages));

        // Add optional fields
        if let Some(max_tokens) = payload.max_tokens {
            request_ctx = request_ctx.with_max_tokens(max_tokens);
        }
        if let Some(ref reasoning) = payload.reasoning
            && let Some(ref effort) = reasoning.effort
        {
            request_ctx = request_ctx.with_reasoning_effort(reasoning_effort_to_string(effort));
        }
        if let Some(ref format) = payload.response_format {
            request_ctx = request_ctx.with_response_format(response_format_to_string(format));
        }
        if let Some(temp) = payload.temperature {
            request_ctx = request_ctx.with_temperature(temp);
        }

        // Get org_id and project_id from auth context
        // Try API key first, then fall back to identity's first org_id
        let org_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });

        // Check model access authorization
        // Use original model string (with provider prefix) for RBAC policy evaluation
        // so policies can match against user-facing model identifiers
        authz
            .require_api(
                "model",
                "use",
                model_clone.as_deref().or(Some(&model_name)),
                Some(request_ctx),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    // Check sovereignty requirements (API key + per-request)
    let sovereignty_reqs = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Check if input guardrails are configured and what mode they're in
    let use_concurrent_guardrails = state
        .input_guardrails
        .as_ref()
        .map(|g| g.is_concurrent())
        .unwrap_or(false);

    // Apply input guardrails in blocking mode (concurrent mode is handled later with the LLM call)
    let mut guardrails_headers: Vec<(&'static str, String)> = Vec::new();
    if let Some(ref input_guardrails) = state.input_guardrails
        && !use_concurrent_guardrails
    {
        // Blocking mode: evaluate guardrails before proceeding
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        let result = input_guardrails
            .evaluate_payload(&payload, None, user_id.as_deref())
            .await;

        match result {
            Ok(guardrails_result) => {
                // Collect headers for later (can't add to response yet)
                guardrails_headers = guardrails_result.to_headers();

                // Log audit event for guardrails evaluation
                log_guardrails_evaluation(
                    &state,
                    auth.as_ref(),
                    input_guardrails.provider_name(),
                    "input",
                    &guardrails_result,
                    None,
                    ci_ip.clone(),
                    ci_ua.clone(),
                );

                // Check if content should be blocked
                if guardrails_result.is_blocked() {
                    // Return the guardrails error (which implements IntoResponse)
                    let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                        crate::guardrails::ContentSource::UserInput,
                        "Content blocked by input guardrails",
                        guardrails_result.violations().to_vec(),
                    );
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "guardrails_blocked",
                        error.to_string(),
                    ));
                }

                // Log warnings if any violations were found but allowed
                if !guardrails_result.response.violations.is_empty() {
                    tracing::info!(
                        violations = ?guardrails_result.response.violations.len(),
                        "Input guardrails found violations but allowed request"
                    );
                }
            }
            Err(e) => {
                // Guardrails evaluation failed - the error handling is already done
                // by the evaluator based on on_error config, so this is a hard error
                let status = match e.error_code() {
                    "guardrails_blocked" => StatusCode::BAD_REQUEST,
                    "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                    "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                    "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                    "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                    _ => StatusCode::BAD_GATEWAY,
                };
                return Err(ApiError::new(status, e.error_code(), e.to_string()));
            }
        }
        // If concurrent mode, guardrails will be evaluated alongside the LLM call later
    }

    // Check if cache should be bypassed based on request headers
    let force_refresh = should_bypass_cache(&headers);

    // Track cache status for response headers
    let mut cache_status = CacheStatus::None;

    // Get cache key components for cache operations
    let key_components = state
        .config
        .features
        .response_caching
        .as_ref()
        .map(|c| &c.key_components);

    let cache_tenant = tenant_scope_from_auth(auth.as_ref());

    // Check semantic cache first (if available), then fall back to simple response cache
    if let Some(ref semantic_cache) = state.semantic_cache {
        let key_components = key_components.cloned().unwrap_or_default();
        match semantic_cache
            .lookup(
                &payload,
                &model_name,
                &key_components,
                &cache_tenant,
                force_refresh,
            )
            .await
        {
            SemanticLookupResult::ExactHit(cached) => {
                tracing::debug!(
                    model = %model_name,
                    provider = %cached.provider,
                    cached_at = cached.cached_at,
                    "Returning exact-match cached response (semantic cache)"
                );
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", &cached.content_type)
                    .header("X-Cache", "HIT")
                    .header("X-Cached-At", cached.cached_at.to_string())
                    .body(Body::from(cached.body))
                    .unwrap());
            }
            SemanticLookupResult::SemanticHit {
                response,
                similarity,
            } => {
                tracing::debug!(
                    model = %model_name,
                    provider = %response.provider,
                    cached_at = response.cached_at,
                    similarity = %similarity,
                    "Returning semantic-match cached response"
                );
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", &response.content_type)
                    .header("X-Cache", "SEMANTIC_HIT")
                    .header("X-Cache-Similarity", format!("{:.4}", similarity))
                    .header("X-Cached-At", response.cached_at.to_string())
                    .body(Body::from(response.body))
                    .unwrap());
            }
            SemanticLookupResult::Miss => {
                cache_status = CacheStatus::Miss;
            }
            SemanticLookupResult::Bypass => {
                // Request is not cacheable (streaming, non-deterministic, etc.)
            }
        }
    } else if let Some(ref response_cache) = state.response_cache {
        // Fall back to simple response cache if semantic cache is not configured
        match response_cache
            .lookup(&payload, &model_name, &cache_tenant, force_refresh)
            .await
        {
            CacheLookupResult::Hit(cached) => {
                tracing::debug!(
                    model = %model_name,
                    provider = %cached.provider,
                    cached_at = cached.cached_at,
                    "Returning cached response"
                );
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", &cached.content_type)
                    .header("X-Cache", "HIT")
                    .header("X-Cached-At", cached.cached_at.to_string())
                    .body(Body::from(cached.body))
                    .unwrap());
            }
            CacheLookupResult::Miss => {
                cache_status = CacheStatus::Miss;
            }
            CacheLookupResult::Bypass => {
                // Request is not cacheable (streaming, non-deterministic, etc.)
            }
        }
    }

    // Execute request with fallback support
    // In concurrent guardrails mode, we race the guardrails evaluation with the LLM call
    let (response, provider_name, model_name) = if use_concurrent_guardrails {
        // Concurrent mode: race guardrails with LLM
        let input_guardrails = state.input_guardrails.as_ref().unwrap();
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        // Create the guardrails evaluation future
        let guardrails_payload = payload.clone();
        let guardrails_user_id = user_id.clone();
        let guardrails_future = input_guardrails.evaluate_payload(
            &guardrails_payload,
            None,
            guardrails_user_id.as_deref(),
        );

        // Create the LLM call future
        let llm_state = state.clone();
        let llm_provider_name = provider_name.clone();
        let llm_provider_config = provider_config.clone();
        let llm_model_name = model_name.clone();
        let llm_payload = payload.clone();
        let llm_sovereignty_reqs = sovereignty_reqs.clone();
        let llm_future = async move {
            execute_with_fallback::<ChatCompletionExecutor>(
                &llm_state,
                llm_provider_name,
                llm_provider_config,
                llm_model_name,
                llm_payload,
                llm_sovereignty_reqs.as_ref(),
            )
            .await
        };

        // Run concurrent evaluation
        let outcome = crate::guardrails::run_concurrent_evaluation(
            input_guardrails,
            guardrails_future,
            llm_future,
        )
        .await
        .map_err(|e| {
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::BAD_REQUEST,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            ApiError::new(status, e.error_code(), e.to_string())
        })?;

        // Collect guardrails headers from concurrent evaluation
        guardrails_headers = outcome.to_headers();

        // Log audit event for guardrails evaluation (concurrent mode)
        if let Some(ref guardrails_result) = outcome.guardrails_result {
            log_guardrails_evaluation(
                &state,
                auth.as_ref(),
                input_guardrails.provider_name(),
                "input",
                guardrails_result,
                None,
                ci_ip.clone(),
                ci_ua.clone(),
            );
        }

        // Extract the LLM result
        // The llm_result is Option<ChatCompletionResult> since successful LLM results
        // are extracted from Result<ChatCompletionResult, ApiError>
        match outcome.llm_result {
            Some(result) => (result.response, result.provider_name, result.model_name),
            None => {
                // LLM didn't complete or failed (error was logged in run_concurrent_evaluation)
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "llm_error",
                    "LLM request failed during concurrent guardrails evaluation".to_string(),
                ));
            }
        }
    } else {
        // Blocking mode: execute LLM after guardrails
        let ExecutionResult {
            response,
            provider_name,
            model_name,
        } = execute_with_fallback::<ChatCompletionExecutor>(
            &state,
            provider_name,
            provider_config,
            model_name,
            payload.clone(),
            sovereignty_reqs.as_ref(),
        )
        .await?;
        (response, provider_name, model_name)
    };

    // Apply output guardrails if configured
    let (response, output_guardrails_headers) = if let Some(ref output_guardrails) =
        state.output_guardrails
        && response.status().is_success()
    {
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));
        let req_id = request_id.as_ref().map(|r| r.0.0.clone());

        if is_streaming {
            // Wrap streaming response with guardrails filter
            let wrapped =
                wrap_streaming_with_guardrails(response, output_guardrails, user_id, req_id);
            // Note: For streaming, headers are not added here since evaluation happens asynchronously
            (wrapped, Vec::new())
        } else {
            // Apply guardrails to non-streaming response
            apply_output_guardrails(&state, response, user_id, auth.as_ref(), ci_ip, ci_ua).await?
        }
    } else {
        (response, Vec::new())
    };

    // Cache the RAW response BEFORE cost injection (if applicable)
    // This ensures cached responses don't have stale pricing and cost $0 on replay
    let response = if cache_status == CacheStatus::Miss && response.status().is_success() {
        // Extract content-type and body for caching
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        // Read the body bytes for caching
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => {
                let body_vec = bytes.to_vec();

                // Store in semantic cache if available, otherwise in response cache
                if let Some(ref semantic_cache) = state.semantic_cache {
                    let cache = semantic_cache.clone();
                    let payload_clone = payload.clone();
                    let model_clone = model_name.clone();
                    let provider_clone = provider_name.clone();
                    let content_type_clone = content_type.clone();
                    let body_clone = body_vec.clone();
                    let key_components_clone = key_components.cloned().unwrap_or_default();
                    let ttl_secs = state
                        .config
                        .features
                        .response_caching
                        .as_ref()
                        .map(|c| c.ttl_secs)
                        .unwrap_or(3600);
                    let tenant_clone = cache_tenant.clone();

                    #[cfg(feature = "server")]
                    state.task_tracker.spawn(async move {
                        let params = StoreParams {
                            payload: &payload_clone,
                            model: &model_clone,
                            provider: &provider_clone,
                            tenant: &tenant_clone,
                            body: body_clone,
                            content_type: &content_type_clone,
                            key_components: &key_components_clone,
                            ttl: Duration::from_secs(ttl_secs),
                        };
                        if !cache.store(params).await {
                            tracing::debug!(
                                "Semantic cache store returned false (caching bypassed or disabled)"
                            );
                        }
                    });
                } else if let Some(ref response_cache) = state.response_cache {
                    let cache = response_cache.clone();
                    let payload_clone = payload.clone();
                    let model_clone = model_name.clone();
                    let provider_clone = provider_name.clone();
                    let content_type_clone = content_type;
                    let body_clone = body_vec.clone();
                    let tenant_clone = cache_tenant.clone();
                    #[cfg(feature = "server")]
                    state.task_tracker.spawn(async move {
                        cache
                            .store(
                                &payload_clone,
                                &model_clone,
                                &provider_clone,
                                &tenant_clone,
                                body_clone,
                                &content_type_clone,
                            )
                            .await;
                    });
                }

                // Rebuild response for cost injection
                Response::from_parts(parts, Body::from(body_vec))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for caching");
                // Return error - we've consumed the body
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Failed to process response"))
                    .unwrap());
            }
        }
    } else {
        response
    };

    // Create usage entry for streaming cost tracking
    let usage_entry = if is_streaming {
        build_streaming_usage_entry(&auth, &state, &model_name, &provider_name, {
            headers
                .get("X-Hadrian-Project")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
        })
    } else {
        None
    };

    // Inject cost calculation into the response
    let mut final_response =
        crate::providers::inject_cost_into_response(crate::providers::CostInjectionParams {
            response,
            provider: &provider_name,
            model: &model_name,
            pricing: &state.pricing,
            db: state.db.as_ref(),
            usage_entry,
            #[cfg(feature = "server")]
            task_tracker: Some(&state.task_tracker),
            #[cfg(feature = "server")]
            usage_drain: Some(&state.usage_drain),
            max_response_body_bytes: state.config.server.max_response_body_bytes,
            streaming_idle_timeout_secs: state.config.server.streaming_idle_timeout_secs,
            validation_config: &state.config.observability.response_validation,
            response_type: if is_streaming {
                crate::validation::ResponseType::ChatCompletionStream
            } else {
                crate::validation::ResponseType::ChatCompletion
            },
        })
        .await;

    // Add X-Cache: MISS header if this was a cache miss
    if cache_status == CacheStatus::Miss {
        final_response
            .headers_mut()
            .insert("X-Cache", "MISS".parse().unwrap());
    }

    // Add X-Provider and X-Model headers to identify which provider served the request
    // This is especially useful when fallback was used
    if let Ok(header_val) = provider_name.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider", header_val);
    }
    if let Ok(source_val) = provider_source.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(header_val) = model_name.parse() {
        final_response.headers_mut().insert("X-Model", header_val);
    }

    // Add input guardrails headers if any were collected
    for (key, value) in guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    // Add output guardrails headers if any were collected
    for (key, value) in output_guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    Ok(final_response)
}

/// Create a response
///
/// Creates a model response using the Responses API format.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/responses",
    tag = "chat",
    request_body = api_types::CreateResponsesPayload,
    responses(
        (status = 200, description = "Response object (streaming or non-streaming)"),
        (status = 400, description = "Bad request", body = crate::openapi::ErrorResponse),
        (status = 502, description = "Provider error", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.responses",
    skip(state, headers, auth, authz, request_id, client_info, payload),
    fields(
        model = %payload.model.as_deref().unwrap_or("default"),
        streaming = payload.stream,
    )
)]
pub async fn api_v1_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    request_id: Option<Extension<RequestId>>,
    client_info: Option<Extension<ClientInfo>>,
    Valid(Json(mut payload)): Valid<Json<api_types::CreateResponsesPayload>>,
) -> Result<Response, ApiError> {
    let (ci_ip, ci_ua) = client_info
        .map(|Extension(ci)| (ci.ip_address, ci.user_agent))
        .unwrap_or_default();

    // Route the model to a provider with dynamic support
    let model_clone = payload.model.clone();
    let models_clone = payload.models.clone();
    let is_streaming = payload.stream;
    let routed = route_models_extended(
        model_clone.as_deref(),
        models_clone.as_deref(),
        &state.config.providers,
    )?;

    // Resolve to concrete provider configuration
    let resolved = resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        auth.as_ref().map(|e| &e.0),
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "provider_resolution_error",
            format!("Failed to resolve provider: {}", e),
        )
    })?;
    let provider_source = resolved.source;
    let (provider_name, provider_config, model_name) = (
        resolved.provider_name,
        resolved.provider_config,
        resolved.model,
    );

    // Update the payload with the resolved model name (provider prefix stripped)
    payload.model = Some(model_name.clone());

    // Check model restrictions if API key auth is used
    // Use original model string (with provider prefix) for restriction check
    if let Some(Extension(ref auth)) = auth
        && let Some(api_key) = auth.api_key()
    {
        let model_to_check = model_clone.as_deref().unwrap_or(&model_name);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // Shell-tool passthrough requires an OpenAI-compatible upstream
    // (OpenAI's hosted runtime or Azure OpenAI). Reject early instead
    // of dropping the tool silently in a downstream provider's
    // convert.rs.
    let payload_has_shell = payload
        .tools
        .as_ref()
        .map(|t| t.iter().any(|tt| tt.is_shell()))
        .unwrap_or(false);
    if payload_has_shell
        && matches!(
            state.config.features.shell,
            crate::config::ShellRuntimeConfig::PassthroughOpenAI
        )
        && !provider_supports_passthrough_shell(&provider_config)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "passthrough_requires_openai_upstream",
            "shell-tool passthrough is configured but the resolved provider is not OpenAI-compatible",
        ));
    }

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
        // Check if file_search tool is present
        let has_file_search = payload
            .tools
            .as_ref()
            .map(|tools| tools.iter().any(|t| t.is_file_search()))
            .unwrap_or(false);

        // Build request context from payload
        let mut request_ctx = RequestContext::new()
            .with_tools(payload.tools.is_some())
            .with_file_search(has_file_search)
            .with_stream(payload.stream);

        // Add optional fields
        if let Some(max_tokens) = payload.max_output_tokens {
            request_ctx = request_ctx.with_max_tokens(max_tokens as u64);
        }
        if let Some(ref reasoning) = payload.reasoning
            && let Some(ref effort) = reasoning.effort
        {
            request_ctx =
                request_ctx.with_reasoning_effort(responses_reasoning_effort_to_string(effort));
        }
        if let Some(temp) = payload.temperature {
            request_ctx = request_ctx.with_temperature(temp);
        }

        // Get org_id and project_id from auth context
        // Try API key first, then fall back to identity's first org_id
        let org_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });

        // Check model access authorization
        // Use original model string (with provider prefix) for RBAC policy evaluation
        authz
            .require_api(
                "model",
                "use",
                model_clone.as_deref().or(Some(&model_name)),
                Some(request_ctx),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    // Check sovereignty requirements (API key + per-request)
    let sovereignty_reqs = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Check if cache should be bypassed based on request headers
    let force_refresh = should_bypass_cache(&headers);

    // Background mode: insert the row now and return queued JSON
    // immediately. The background worker (jobs/background_responses.rs)
    // claims the row asynchronously and runs the LLM in its own task,
    // recording events via the persister. Clients poll
    // GET /v1/responses/{id} for status or
    // GET /v1/responses/{id}/events?starting_after=N for the live event log.
    if payload.background == Some(true) {
        if let Some(ref store) = state.responses_store {
            let principal_org = auth
                .as_ref()
                .and_then(|a| a.api_key().and_then(|k| k.org_id))
                .or_else(|| auth.as_ref().and_then(|a| a.principal().org_id()))
                .or(state.default_org_id)
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::UNAUTHORIZED,
                        "org_required",
                        "Background responses require an authenticated org",
                    )
                })?;
            let owner = crate::services::responses_pipeline::derive_response_owner(
                &state,
                auth.as_ref().map(|e| &e.0),
            )
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "org_required",
                    "Background responses require an authenticated principal",
                )
            })?;
            let principal_user = auth
                .as_ref()
                .and_then(|a| a.user_id())
                .or(state.default_user_id);
            let principal_api_key = auth.as_ref().and_then(|a| a.api_key().map(|k| k.key.id));
            let principal_project = auth
                .as_ref()
                .and_then(|a| a.api_key().and_then(|k| k.project_id));
            let principal_service_account = auth
                .as_ref()
                .and_then(|a| a.api_key().and_then(|k| k.service_account_id));

            let resp_id = crate::services::ResponsesStore::new_response_id();
            let now = chrono::Utc::now();
            let new_row = crate::db::repos::NewResponse {
                id: resp_id.clone(),
                org_id: principal_org,
                owner_type: owner.owner_type(),
                owner_id: owner.owner_id(),
                project_id: principal_project,
                user_id: principal_user,
                api_key_id: principal_api_key,
                service_account_id: principal_service_account,
                status: crate::db::repos::ResponseStatus::Queued,
                background: true,
                model: model_name.clone(),
                provider: Some(provider_name.clone()),
                created_at: now,
                // Background mode persists the user's original payload
                // verbatim — `resolve_and_inject_skills` hasn't run on
                // this code path (it short-circuits earlier than the
                // skill-resolution block below), so `instructions`
                // does not contain inlined SKILL.md content. The
                // background worker resolves skills locally at
                // execute time so the row stays free of operator-
                // private skill content.
                request_payload: serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null),
                retention_expires_at: store.retention_expires_at(now),
            };
            store.create(new_row).await.map_err(|e| {
                tracing::error!(error = %e, "background dispatch failed");
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "background_dispatch_failed",
                    "Failed to enqueue background response",
                )
            })?;

            // Return the queued response envelope. The client uses
            // resp_id to poll for status / events.
            let queued = serde_json::json!({
                "id": resp_id,
                "object": "response",
                "status": "queued",
                "background": true,
                "model": model_name,
                "provider": provider_name,
                "created_at": now.timestamp(),
            });
            return Ok(Response::builder()
                .status(StatusCode::ACCEPTED)
                .header("Content-Type", "application/json")
                .body(Body::from(queued.to_string()))
                .unwrap());
        }
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "background_mode_requires_persistence",
            "background=true requires a configured database".to_string(),
        ));
    }

    // Resolve skills (Hadrian extension): fetch the bundles, mount
    // them in the sandbox via `mounted_skills`, and prepend each
    // SKILL.md to `payload.instructions` so the model knows the skill
    // is available. Background-mode requests skip this — they short
    // -circuit above and the worker resolves skills when it runs, so
    // the persisted `request_payload` keeps the user's original input
    // (not the rewritten instructions).
    let principal = crate::services::responses_pipeline::PipelinePrincipal::from_auth(
        &state,
        auth.as_ref().map(|e| &e.0),
    );
    // Snapshot the caller's original `instructions` before skill
    // resolution rewrites them with inlined SKILL.md content. The
    // foreground persistence block below restores this snapshot when
    // building `request_payload`, so retrieve echoes the user's
    // original input rather than leaking operator-private skill
    // content to anyone with GET access in the same org.
    let original_instructions = payload.instructions.clone();
    let mounted_skills = crate::services::responses_pipeline::resolve_and_inject_skills(
        &state,
        &mut payload,
        principal.org_id,
    )
    .await
    .map_err(|e| {
        let code = match &e {
            crate::services::responses_pipeline::SkillResolutionError::InvalidId(_)
            | crate::services::responses_pipeline::SkillResolutionError::NotFound(_)
            | crate::services::responses_pipeline::SkillResolutionError::MissingOrg => {
                "invalid_skill_reference"
            }
            crate::services::responses_pipeline::SkillResolutionError::NoService => {
                "skills_not_configured"
            }
            crate::services::responses_pipeline::SkillResolutionError::Db(_) => {
                "skill_lookup_failed"
            }
        };
        let status = if matches!(
            e,
            crate::services::responses_pipeline::SkillResolutionError::Db(_)
        ) {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::BAD_REQUEST
        };
        ApiError::new(status, code, e.to_string())
    })?;

    // Track cache status for response headers
    let mut cache_status = CacheStatus::None;

    let cache_tenant = tenant_scope_from_auth(auth.as_ref());

    // Check response cache (simple cache only for now - semantic cache not yet supported for responses)
    if let Some(ref response_cache) = state.response_cache {
        match response_cache
            .lookup_responses(&payload, &model_name, &cache_tenant, force_refresh)
            .await
        {
            CacheLookupResult::Hit(cached) => {
                tracing::debug!(
                    model = %model_name,
                    provider = %cached.provider,
                    cached_at = cached.cached_at,
                    "Returning cached response (responses API)"
                );
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", &cached.content_type)
                    .header("X-Cache", "HIT")
                    .header("X-Cached-At", cached.cached_at.to_string())
                    .header("X-Provider", &cached.provider)
                    .header("X-Model", &cached.model)
                    .body(Body::from(cached.body))
                    .unwrap());
            }
            CacheLookupResult::Miss => {
                cache_status = CacheStatus::Miss;
            }
            CacheLookupResult::Bypass => {
                // Request is not cacheable (streaming, non-deterministic, etc.)
            }
        }
    }

    // Check if input guardrails are configured and what mode they're in
    let use_concurrent_guardrails = state
        .input_guardrails
        .as_ref()
        .map(|g| g.is_concurrent())
        .unwrap_or(false);

    // Apply input guardrails in blocking mode (concurrent mode is handled later with the LLM call)
    let mut guardrails_headers: Vec<(&'static str, String)> = Vec::new();
    if let Some(ref input_guardrails) = state.input_guardrails
        && !use_concurrent_guardrails
    {
        // Blocking mode: evaluate guardrails before proceeding
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        let result = input_guardrails
            .evaluate_responses_payload(&payload, None, user_id.as_deref())
            .await;

        match result {
            Ok(guardrails_result) => {
                guardrails_headers = guardrails_result.to_headers();

                // Log audit event for guardrails evaluation
                log_guardrails_evaluation(
                    &state,
                    auth.as_ref(),
                    input_guardrails.provider_name(),
                    "input",
                    &guardrails_result,
                    None,
                    ci_ip.clone(),
                    ci_ua.clone(),
                );

                if guardrails_result.is_blocked() {
                    let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                        crate::guardrails::ContentSource::UserInput,
                        "Content blocked by input guardrails",
                        guardrails_result.violations().to_vec(),
                    );
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "guardrails_blocked",
                        error.to_string(),
                    ));
                }

                if !guardrails_result.response.violations.is_empty() {
                    tracing::info!(
                        violations = ?guardrails_result.response.violations.len(),
                        "Input guardrails found violations but allowed request"
                    );
                }
            }
            Err(e) => {
                let status = match e.error_code() {
                    "guardrails_blocked" => StatusCode::BAD_REQUEST,
                    "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                    "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                    "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                    "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                    _ => StatusCode::BAD_GATEWAY,
                };
                return Err(ApiError::new(status, e.error_code(), e.to_string()));
            }
        }
        // If concurrent mode, guardrails will be evaluated alongside the LLM call below
    }

    // Create a provider from config and make a request
    // In concurrent mode, we race guardrails with the LLM call
    // Clone provider_config early - we need it later for file_search callback
    let saved_provider_config = provider_config.clone();
    let (response, provider_name, model_name, provider_config) = if use_concurrent_guardrails {
        let input_guardrails = state.input_guardrails.as_ref().unwrap();
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        // Create guardrails evaluation future
        let guardrails_payload = payload.clone();
        let guardrails_user_id = user_id.clone();
        let guardrails_future = input_guardrails.evaluate_responses_payload(
            &guardrails_payload,
            None,
            guardrails_user_id.as_deref(),
        );

        // Create LLM call future with fallback support
        let llm_state = state.clone();
        let llm_provider_name = provider_name.clone();
        let llm_provider_config = provider_config.clone();
        let llm_model_name = model_name.clone();
        let llm_payload = payload.clone();
        let llm_sovereignty_reqs = sovereignty_reqs.clone();
        let llm_future = async move {
            execute_with_fallback::<ResponsesExecutor>(
                &llm_state,
                llm_provider_name,
                llm_provider_config,
                llm_model_name,
                llm_payload,
                llm_sovereignty_reqs.as_ref(),
            )
            .await
        };

        // Run concurrent evaluation
        let outcome = crate::guardrails::run_concurrent_evaluation(
            input_guardrails,
            guardrails_future,
            llm_future,
        )
        .await
        .map_err(|e| {
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::BAD_REQUEST,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            ApiError::new(status, e.error_code(), e.to_string())
        })?;

        // Collect guardrails headers
        guardrails_headers = outcome.to_headers();

        // Log audit event for guardrails evaluation (concurrent mode)
        if let Some(ref guardrails_result) = outcome.guardrails_result {
            log_guardrails_evaluation(
                &state,
                auth.as_ref(),
                input_guardrails.provider_name(),
                "input",
                guardrails_result,
                None,
                ci_ip.clone(),
                ci_ua.clone(),
            );
        }

        // Extract LLM result
        match outcome.llm_result {
            Some(result) => (
                result.response,
                result.provider_name,
                result.model_name,
                saved_provider_config,
            ),
            None => {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "llm_error",
                    "LLM request failed during concurrent guardrails evaluation".to_string(),
                ));
            }
        }
    } else {
        // Blocking mode: execute LLM with fallback support
        let ExecutionResult {
            response,
            provider_name,
            model_name,
        } = execute_with_fallback::<ResponsesExecutor>(
            &state,
            provider_name,
            provider_config,
            model_name,
            payload.clone(),
            sovereignty_reqs.as_ref(),
        )
        .await?;
        (response, provider_name, model_name, saved_provider_config)
    };

    // Apply output guardrails if configured
    let (final_response, output_guardrails_headers) = if let Some(ref _output_guardrails) =
        state.output_guardrails
        && response.status().is_success()
    {
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));
        let req_id = request_id.as_ref().map(|r| r.0.0.clone());

        if is_streaming {
            // Streaming guardrails are applied inside the shared
            // pipeline below alongside the tool runner + persister.
            // Suppress unused-var warnings by binding explicitly.
            let _ = (user_id, req_id);
            (response, Vec::new())
        } else {
            // Apply guardrails to non-streaming response. Non-streaming
            // needs ci_ip/ci_ua for audit logging, so it stays out of
            // the shared pipeline.
            apply_output_guardrails_responses(
                &state,
                response,
                user_id,
                auth.as_ref(),
                ci_ip,
                ci_ua,
            )
            .await?
        }
    } else {
        (response, Vec::new())
    };

    // Insert the persisted-response row up front (when store=true)
    // so the shared pipeline can attach the persister wrap. Principal
    // was already built up top for skill resolution.
    let persistence_handle: Option<crate::services::responses_pipeline::PersistenceHandle> = {
        let want_persist = state.responses_store.is_some()
            && payload.store != Some(false)
            && final_response.status().is_success();
        if !want_persist {
            None
        } else if let (Some(store), Some(row_org), Some(owner)) = (
            state.responses_store.as_ref(),
            principal.org_id,
            crate::services::responses_pipeline::derive_response_owner(
                &state,
                auth.as_ref().map(|e| &e.0),
            ),
        ) {
            let resp_id = crate::services::ResponsesStore::new_response_id();
            let now = chrono::Utc::now();
            let new_row = crate::db::repos::NewResponse {
                id: resp_id.clone(),
                org_id: row_org,
                owner_type: owner.owner_type(),
                owner_id: owner.owner_id(),
                project_id: principal.project_id,
                user_id: principal.user_id,
                api_key_id: principal.api_key_id,
                service_account_id: principal.service_account_id,
                status: crate::db::repos::ResponseStatus::InProgress,
                background: payload.background.unwrap_or(false),
                model: model_name.clone(),
                provider: Some(provider_name.clone()),
                created_at: now,
                // Persist the caller's original instructions, not the
                // skill-rewritten ones, so retrieve doesn't leak
                // operator-private SKILL.md content. The execution
                // pipeline keeps using `payload` with the rewritten
                // instructions — only the persisted snapshot is
                // restored.
                request_payload: {
                    let mut snapshot = payload.clone();
                    snapshot.instructions = original_instructions.clone();
                    serde_json::to_value(&snapshot).unwrap_or(serde_json::Value::Null)
                },
                retention_expires_at: store.retention_expires_at(now),
            };
            match store.create(new_row).await {
                Ok((record, cancel_rx)) => {
                    Some(crate::services::responses_pipeline::PersistenceHandle {
                        response_id: resp_id,
                        org_id: row_org,
                        initial_sequence_number: record.last_sequence_number,
                        cancel_rx,
                    })
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to insert response row; persistence skipped"
                    );
                    None
                }
            }
        } else {
            // Persistence requires an authenticated tenant. Without
            // an org we can't scope subsequent retrieve/cancel/delete
            // calls safely, so persistence is silently skipped — the
            // request still serves a response.
            if state.responses_store.is_some() {
                tracing::warn!(
                    "Response persistence skipped: no org on principal (anonymous/disabled auth)"
                );
            }
            None
        }
    };
    let persistence_id_and_org = persistence_handle
        .as_ref()
        .map(|h| (h.response_id.clone(), h.org_id));

    // Apply the shared pipeline: output guardrails + server-executed
    // tool loop + persister.
    let mut final_response = if is_streaming {
        let req_id_str = request_id.as_ref().map(|r| r.0.0.clone());
        crate::services::responses_pipeline::apply_streaming_pipeline(
            &state,
            &payload,
            provider_name.clone(),
            provider_config.clone(),
            model_name.clone(),
            principal,
            mounted_skills,
            req_id_str,
            final_response,
            persistence_handle,
        )
    } else {
        final_response
    };

    // Add input guardrails headers
    for (key, value) in guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    // Add output guardrails headers
    for (key, value) in output_guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    // Cache and/or persist the response (non-streaming only). The two
    // operations share a materialized body: read it once, hand the
    // bytes to whichever side wants them. Persistence is needed
    // regardless of cache outcome — without this branch a cache hit
    // or a cache-disabled deployment would leave the response row
    // stuck `in_progress` until retention pruned it.
    let needs_cache_store = cache_status == CacheStatus::Miss
        && final_response.status().is_success()
        && !is_streaming
        && state.response_cache.is_some();
    let needs_persist =
        !is_streaming && persistence_id_and_org.is_some() && state.responses_store.is_some();
    let final_response = if needs_cache_store || needs_persist {
        // Extract content-type and body once.
        let content_type = final_response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        let (parts, body) = final_response.into_parts();
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => {
                let body_vec = bytes.to_vec();

                // Store in response cache (semantic cache not yet supported for responses API)
                if needs_cache_store && let Some(ref response_cache) = state.response_cache {
                    let cache = response_cache.clone();
                    let payload_clone = payload.clone();
                    let model_clone = model_name.clone();
                    let provider_clone = provider_name.clone();
                    let content_type_clone = content_type;
                    let body_clone = body_vec.clone();
                    let tenant_clone = cache_tenant.clone();
                    #[cfg(feature = "server")]
                    state.task_tracker.spawn(async move {
                        cache
                            .store_responses(
                                &payload_clone,
                                &model_clone,
                                &provider_clone,
                                &tenant_clone,
                                body_clone,
                                &content_type_clone,
                            )
                            .await;
                    });
                }

                // Persist the non-streaming response now that the body
                // is materialized. Streaming responses are persisted by
                // `wrap_streaming_with_persistence` from inside its
                // spawned task as the final event arrives.
                if let (Some((resp_id, org_id)), Some(store)) = (
                    persistence_id_and_org.as_ref(),
                    state.responses_store.as_ref(),
                ) {
                    persist_non_streaming(
                        store,
                        resp_id,
                        *org_id,
                        &body_vec,
                        parts.status.as_u16(),
                    )
                    .await;
                }

                // Rebuild response
                Response::from_parts(parts, Body::from(body_vec))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for caching/persistence");
                // Return error - we've consumed the body
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Failed to process response"))
                    .unwrap());
            }
        }
    } else {
        final_response
    };

    // Create usage entry for streaming cost tracking
    let usage_entry = if is_streaming {
        build_streaming_usage_entry(&auth, &state, &model_name, &provider_name, {
            headers
                .get("X-Hadrian-Project")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
        })
    } else {
        None
    };

    // Inject cost calculation into the response
    let mut final_response =
        crate::providers::inject_cost_into_response(crate::providers::CostInjectionParams {
            response: final_response,
            provider: &provider_name,
            model: &model_name,
            pricing: &state.pricing,
            db: state.db.as_ref(),
            usage_entry,
            #[cfg(feature = "server")]
            task_tracker: Some(&state.task_tracker),
            #[cfg(feature = "server")]
            usage_drain: Some(&state.usage_drain),
            max_response_body_bytes: state.config.server.max_response_body_bytes,
            streaming_idle_timeout_secs: state.config.server.streaming_idle_timeout_secs,
            validation_config: &state.config.observability.response_validation,
            response_type: if is_streaming {
                crate::validation::ResponseType::ResponseStream
            } else {
                crate::validation::ResponseType::Response
            },
        })
        .await;

    // Add X-Cache: MISS header if this was a cache miss
    if cache_status == CacheStatus::Miss {
        final_response
            .headers_mut()
            .insert("X-Cache", "MISS".parse().unwrap());
    }

    // Add X-Provider and X-Model headers to identify which provider served the request
    // This is especially useful when fallback was used
    if let Ok(header_val) = provider_name.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider", header_val);
    }
    if let Ok(source_val) = provider_source.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(header_val) = model_name.parse() {
        final_response.headers_mut().insert("X-Model", header_val);
    }

    Ok(final_response)
}

/// Apply output guardrails to a non-streaming responses API response.
///
/// Similar to `apply_output_guardrails` but uses responses-specific content extraction.
async fn apply_output_guardrails_responses(
    state: &AppState,
    response: Response,
    user_id: Option<String>,
    auth: Option<&Extension<AuthenticatedRequest>>,
    ip_address: Option<String>,
    user_agent: Option<String>,
) -> Result<(Response, Vec<(&'static str, String)>), ApiError> {
    let output_guardrails = state.output_guardrails.as_ref().unwrap();

    // Read the response body
    let (parts, body) = response.into_parts();
    let body_bytes =
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for output guardrails");
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "response_read_error",
                    "Failed to read response for guardrails evaluation",
                ));
            }
        };

    // Extract content from the responses format
    let content = crate::guardrails::extract_text_from_responses_response(&body_bytes);

    // If no content to evaluate, return the original response
    if content.is_empty() {
        let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
        return Ok((response, Vec::new()));
    }

    // Evaluate the content
    let result = output_guardrails
        .evaluate_response(&content, None, user_id.as_deref())
        .await;

    match result {
        Ok(guardrails_result) => {
            let headers = guardrails_result.to_headers();

            // Log audit event for output guardrails evaluation
            log_output_guardrails_evaluation(
                state,
                auth,
                output_guardrails.provider_name(),
                &guardrails_result,
                None,
                ip_address,
                user_agent,
            );

            // Check if content should be blocked
            if guardrails_result.is_blocked() {
                let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                    crate::guardrails::ContentSource::LlmOutput,
                    "Response blocked by output guardrails",
                    guardrails_result.violations().to_vec(),
                );
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "guardrails_output_blocked",
                    error.to_string(),
                ));
            }

            // Check if content should be redacted
            if let Some(modified_content) = guardrails_result.modified_content() {
                // For responses API, rebuild with modified output_text
                let modified_body = modify_responses_content(&body_bytes, modified_content)
                    .unwrap_or_else(|| body_bytes.to_vec());
                let response = Response::from_parts(parts, Body::from(modified_body));
                return Ok((response, headers));
            }

            // Log warnings if any violations were found but allowed
            if !guardrails_result.response.violations.is_empty() {
                tracing::info!(
                    violations = ?guardrails_result.response.violations.len(),
                    "Output guardrails found violations but allowed response"
                );
            }

            // Return the original response with headers
            let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
            Ok((response, headers))
        }
        Err(e) => {
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::INTERNAL_SERVER_ERROR,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            Err(ApiError::new(status, e.error_code(), e.to_string()))
        }
    }
}

/// Compact a context window via the provider's standalone compact
/// endpoint.
///
/// Stateless passthrough: forwards `model` + `input` (and any other
/// fields the provider accepts) to the upstream `/responses/compact`
/// endpoint and streams the compacted window back. Only OpenAI and
/// Azure OpenAI implement this; routing to any other provider returns
/// 501 with `error_code = "not_supported"`.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/responses/compact",
    tag = "responses",
    request_body = api_types::CreateResponsesPayload,
    responses(
        (status = 200, description = "Compacted context window"),
        (status = 400, description = "Bad request", body = crate::openapi::ErrorResponse),
        (status = 501, description = "Provider does not support compaction", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.responses.compact",
    skip(state, auth, authz, payload),
    fields(model = %payload.model.as_deref().unwrap_or("default"))
)]
pub async fn api_v1_responses_compact(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(mut payload)): Valid<Json<api_types::CreateResponsesPayload>>,
) -> Result<Response, ApiError> {
    // Route + resolve the model the same way the main responses
    // handler does so per-org overrides and model-aliasing apply.
    let model_clone = payload.model.clone();
    let models_clone = payload.models.clone();
    let routed = route_models_extended(
        model_clone.as_deref(),
        models_clone.as_deref(),
        &state.config.providers,
    )?;

    let resolved = resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        auth.as_ref().map(|e| &e.0),
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "provider_resolution_error",
            format!("Failed to resolve provider: {e}"),
        )
    })?;
    let (provider_name, provider_config, model_name) = (
        resolved.provider_name,
        resolved.provider_config,
        resolved.model,
    );
    payload.model = Some(model_name.clone());

    // Per-API-key model restrictions (mirrors api_v1_responses).
    if let Some(Extension(ref auth)) = auth
        && let Some(api_key) = auth.api_key()
    {
        let model_to_check = model_clone.as_deref().unwrap_or(&model_name);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // RBAC: same `model:use` policy as the main responses endpoint —
    // compaction is a strict subset of `/responses` access.
    if let Some(Extension(ref authz)) = authz {
        let org_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.as_ref().and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });
        authz
            .require_api(
                "model",
                "use",
                model_clone.as_deref().or(Some(&model_name)),
                Some(RequestContext::new().with_stream(payload.stream)),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    // Compaction sends context through the model, so it has the same
    // data-sovereignty surface as the main responses endpoint. Apply
    // the same per-API-key + per-request residency check.
    let _ = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    CompactExecutor::execute(&state, &provider_name, &provider_config, payload)
        .await
        .map_err(|e| {
            let (status, code) = match &e {
                crate::providers::ProviderError::Unsupported(_) => {
                    (StatusCode::NOT_IMPLEMENTED, "not_supported")
                }
                _ => (StatusCode::BAD_GATEWAY, "provider_error"),
            };
            ApiError::new(status, code, e.to_string())
        })
}

/// Modifies the output_text in a responses API response JSON.
///
/// Returns the modified response body, or None if modification failed.
fn modify_responses_content(body: &[u8], new_content: &str) -> Option<Vec<u8>> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;

    // Modify output_text field
    json["output_text"] = serde_json::Value::String(new_content.to_string());

    // Also modify content in output[0].content if it's a message
    if let Some(output) = json.get_mut("output").and_then(|o| o.as_array_mut()) {
        for item in output {
            if item.get("type").and_then(|t| t.as_str()) == Some("message")
                && let Some(content) = item.get_mut("content").and_then(|c| c.as_array_mut())
            {
                for content_item in content {
                    if content_item.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                        content_item["text"] = serde_json::Value::String(new_content.to_string());
                    }
                }
            }
        }
    }

    serde_json::to_vec(&json).ok()
}

/// Create a text completion
///
/// Creates a completion for the provided prompt and parameters.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/completions",
    tag = "completions",
    request_body = api_types::CreateCompletionPayload,
    responses(
        (status = 200, description = "Completion response (streaming or non-streaming)"),
        (status = 400, description = "Bad request", body = crate::openapi::ErrorResponse),
        (status = 502, description = "Provider error", body = crate::openapi::ErrorResponse),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.completions",
    skip(state, headers, auth, request_id, client_info, payload),
    fields(
        model = %payload.model.as_deref().unwrap_or("default"),
        streaming = payload.stream,
    )
)]
pub async fn api_v1_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthenticatedRequest>>,
    request_id: Option<Extension<RequestId>>,
    client_info: Option<Extension<ClientInfo>>,
    Valid(Json(mut payload)): Valid<Json<api_types::CreateCompletionPayload>>,
) -> Result<Response, ApiError> {
    let (ci_ip, ci_ua) = client_info
        .map(|Extension(ci)| (ci.ip_address, ci.user_agent))
        .unwrap_or_default();

    // Route the model to a provider with dynamic support
    let model_clone = payload.model.clone();
    let models_clone = payload.models.clone();
    let is_streaming = payload.stream;
    let routed = route_models_extended(
        model_clone.as_deref(),
        models_clone.as_deref(),
        &state.config.providers,
    )?;

    // Resolve to concrete provider configuration
    let resolved = resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        auth.as_ref().map(|e| &e.0),
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "provider_resolution_error",
            format!("Failed to resolve provider: {}", e),
        )
    })?;
    let provider_source = resolved.source;
    let (provider_name, provider_config, model_name) = (
        resolved.provider_name,
        resolved.provider_config,
        resolved.model,
    );

    // Update the payload with the resolved model name (provider prefix stripped)
    payload.model = Some(model_name.clone());

    // Check model restrictions if API key auth is used
    // Use original model string (with provider prefix) for restriction check
    if let Some(Extension(ref auth)) = auth
        && let Some(api_key) = auth.api_key()
    {
        let model_to_check = model_clone.as_deref().unwrap_or(&model_name);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // Check sovereignty requirements (API key + per-request)
    let sovereignty_reqs = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Check if cache should be bypassed based on request headers
    let force_refresh = should_bypass_cache(&headers);

    // Track cache status for response headers
    let mut cache_status = CacheStatus::None;

    let cache_tenant = tenant_scope_from_auth(auth.as_ref());

    // Check response cache (simple cache only - semantic cache not yet supported for completions)
    if let Some(ref response_cache) = state.response_cache {
        match response_cache
            .lookup_completions(&payload, &model_name, &cache_tenant, force_refresh)
            .await
        {
            CacheLookupResult::Hit(cached) => {
                tracing::debug!(
                    model = %model_name,
                    provider = %cached.provider,
                    cached_at = cached.cached_at,
                    "Returning cached response (completions API)"
                );
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", &cached.content_type)
                    .header("X-Cache", "HIT")
                    .header("X-Cached-At", cached.cached_at.to_string())
                    .header("X-Provider", &cached.provider)
                    .header("X-Model", &cached.model)
                    .body(Body::from(cached.body))
                    .unwrap());
            }
            CacheLookupResult::Miss => {
                cache_status = CacheStatus::Miss;
            }
            CacheLookupResult::Bypass => {
                // Request is not cacheable (streaming, non-deterministic, etc.)
            }
        }
    }

    // Check if input guardrails are configured and what mode they're in
    let use_concurrent_guardrails = state
        .input_guardrails
        .as_ref()
        .map(|g| g.is_concurrent())
        .unwrap_or(false);

    // Apply input guardrails in blocking mode (concurrent mode is handled later with the LLM call)
    let mut guardrails_headers: Vec<(&'static str, String)> = Vec::new();
    if let Some(ref input_guardrails) = state.input_guardrails
        && !use_concurrent_guardrails
    {
        // Blocking mode: evaluate guardrails before proceeding
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        let result = input_guardrails
            .evaluate_completion_payload(&payload, None, user_id.as_deref())
            .await;

        match result {
            Ok(guardrails_result) => {
                guardrails_headers = guardrails_result.to_headers();

                // Log audit event for guardrails evaluation
                log_guardrails_evaluation(
                    &state,
                    auth.as_ref(),
                    input_guardrails.provider_name(),
                    "input",
                    &guardrails_result,
                    None,
                    ci_ip.clone(),
                    ci_ua.clone(),
                );

                if guardrails_result.is_blocked() {
                    let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                        crate::guardrails::ContentSource::UserInput,
                        "Content blocked by input guardrails",
                        guardrails_result.violations().to_vec(),
                    );
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "guardrails_blocked",
                        error.to_string(),
                    ));
                }

                if !guardrails_result.response.violations.is_empty() {
                    tracing::info!(
                        violations = ?guardrails_result.response.violations.len(),
                        "Input guardrails found violations but allowed request"
                    );
                }
            }
            Err(e) => {
                let status = match e.error_code() {
                    "guardrails_blocked" => StatusCode::BAD_REQUEST,
                    "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                    "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                    "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                    "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                    _ => StatusCode::BAD_GATEWAY,
                };
                return Err(ApiError::new(status, e.error_code(), e.to_string()));
            }
        }
        // If concurrent mode, guardrails will be evaluated alongside the LLM call below
    }

    // Create a provider from config and make a request
    // In concurrent mode, we race guardrails with the LLM call
    let (response, provider_name, model_name) = if use_concurrent_guardrails {
        // SAFETY: use_concurrent_guardrails is only true when input_guardrails is Some
        let input_guardrails = state.input_guardrails.as_ref().unwrap();
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));

        // Create guardrails evaluation future
        let guardrails_payload = payload.clone();
        let guardrails_user_id = user_id.clone();
        let guardrails_future = input_guardrails.evaluate_completion_payload(
            &guardrails_payload,
            None,
            guardrails_user_id.as_deref(),
        );

        // Create LLM call future with fallback support
        let llm_state = state.clone();
        let llm_provider_name = provider_name.clone();
        let llm_provider_config = provider_config.clone();
        let llm_model_name = model_name.clone();
        let llm_payload = payload.clone();
        let llm_sovereignty_reqs = sovereignty_reqs.clone();
        let llm_future = async move {
            execute_with_fallback::<CompletionExecutor>(
                &llm_state,
                llm_provider_name,
                llm_provider_config,
                llm_model_name,
                llm_payload,
                llm_sovereignty_reqs.as_ref(),
            )
            .await
        };

        // Run concurrent evaluation
        let outcome = crate::guardrails::run_concurrent_evaluation(
            input_guardrails,
            guardrails_future,
            llm_future,
        )
        .await
        .map_err(|e| {
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::BAD_REQUEST,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            ApiError::new(status, e.error_code(), e.to_string())
        })?;

        // Collect guardrails headers
        guardrails_headers = outcome.to_headers();

        // Log audit event for guardrails evaluation (concurrent mode)
        if let Some(ref guardrails_result) = outcome.guardrails_result {
            log_guardrails_evaluation(
                &state,
                auth.as_ref(),
                input_guardrails.provider_name(),
                "input",
                guardrails_result,
                None,
                ci_ip.clone(),
                ci_ua.clone(),
            );
        }

        // Extract LLM result
        match outcome.llm_result {
            Some(result) => (result.response, result.provider_name, result.model_name),
            None => {
                // LLM didn't complete or failed (error was logged in run_concurrent_evaluation)
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "llm_error",
                    "LLM request failed during concurrent guardrails evaluation".to_string(),
                ));
            }
        }
    } else {
        // Blocking mode: execute LLM with fallback support
        let ExecutionResult {
            response,
            provider_name,
            model_name,
        } = execute_with_fallback::<CompletionExecutor>(
            &state,
            provider_name,
            provider_config,
            model_name,
            payload.clone(),
            sovereignty_reqs.as_ref(),
        )
        .await?;
        (response, provider_name, model_name)
    };

    // Apply output guardrails if configured
    let (mut final_response, output_guardrails_headers) = if let Some(ref output_guardrails) =
        state.output_guardrails
        && response.status().is_success()
    {
        let user_id = auth
            .as_ref()
            .and_then(|a| a.api_key().map(|k| k.key.id.to_string()));
        let req_id = request_id.as_ref().map(|r| r.0.0.clone());

        if is_streaming {
            // Wrap streaming response with guardrails filter
            // Note: For completions, we reuse the same streaming filter
            let wrapped =
                wrap_streaming_with_guardrails(response, output_guardrails, user_id, req_id);
            (wrapped, Vec::new())
        } else {
            // Apply guardrails to non-streaming response
            apply_output_guardrails_completions(
                &state,
                response,
                user_id,
                auth.as_ref(),
                ci_ip,
                ci_ua,
            )
            .await?
        }
    } else {
        (response, Vec::new())
    };

    // Add input guardrails headers
    for (key, value) in guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    // Add output guardrails headers
    for (key, value) in output_guardrails_headers {
        if let Ok(header_val) = value.parse() {
            final_response.headers_mut().insert(key, header_val);
        }
    }

    // Cache successful responses (non-streaming only)
    let final_response = if cache_status == CacheStatus::Miss
        && final_response.status().is_success()
        && !is_streaming
    {
        // Extract content-type and body for caching
        let content_type = final_response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();

        // Read the body bytes for caching
        let (parts, body) = final_response.into_parts();
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => {
                let body_vec = bytes.to_vec();

                // Store in response cache
                if let Some(ref response_cache) = state.response_cache {
                    let cache = response_cache.clone();
                    let payload_clone = payload.clone();
                    let model_clone = model_name.clone();
                    let provider_clone = provider_name.clone();
                    let content_type_clone = content_type;
                    let body_clone = body_vec.clone();
                    let tenant_clone = cache_tenant.clone();
                    #[cfg(feature = "server")]
                    state.task_tracker.spawn(async move {
                        cache
                            .store_completions(
                                &payload_clone,
                                &model_clone,
                                &provider_clone,
                                &tenant_clone,
                                body_clone,
                                &content_type_clone,
                            )
                            .await;
                    });
                }

                // Rebuild response
                Response::from_parts(parts, Body::from(body_vec))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for caching");
                // Return error - we've consumed the body
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Failed to process response"))
                    .unwrap());
            }
        }
    } else {
        final_response
    };

    // Create usage entry for streaming cost tracking
    let usage_entry = if is_streaming {
        build_streaming_usage_entry(&auth, &state, &model_name, &provider_name, {
            headers
                .get("X-Hadrian-Project")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
        })
    } else {
        None
    };

    // Inject cost calculation into the response
    let mut final_response =
        crate::providers::inject_cost_into_response(crate::providers::CostInjectionParams {
            response: final_response,
            provider: &provider_name,
            model: &model_name,
            pricing: &state.pricing,
            db: state.db.as_ref(),
            usage_entry,
            #[cfg(feature = "server")]
            task_tracker: Some(&state.task_tracker),
            #[cfg(feature = "server")]
            usage_drain: Some(&state.usage_drain),
            max_response_body_bytes: state.config.server.max_response_body_bytes,
            streaming_idle_timeout_secs: state.config.server.streaming_idle_timeout_secs,
            validation_config: &state.config.observability.response_validation,
            response_type: if is_streaming {
                crate::validation::ResponseType::ChatCompletionStream // Legacy completions use same schema
            } else {
                crate::validation::ResponseType::Completion
            },
        })
        .await;

    // Add X-Cache: MISS header if this was a cache miss
    if cache_status == CacheStatus::Miss {
        final_response
            .headers_mut()
            .insert("X-Cache", "MISS".parse().unwrap());
    }

    // Add X-Provider and X-Model headers to identify which provider served the request
    // This is especially useful when fallback was used
    if let Ok(header_val) = provider_name.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider", header_val);
    }
    if let Ok(source_val) = provider_source.parse() {
        final_response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(header_val) = model_name.parse() {
        final_response.headers_mut().insert("X-Model", header_val);
    }

    Ok(final_response)
}

/// Apply output guardrails to a non-streaming completions API response.
///
/// Similar to `apply_output_guardrails` but uses completions-specific content extraction.
async fn apply_output_guardrails_completions(
    state: &AppState,
    response: Response,
    user_id: Option<String>,
    auth: Option<&Extension<AuthenticatedRequest>>,
    ip_address: Option<String>,
    user_agent: Option<String>,
) -> Result<(Response, Vec<(&'static str, String)>), ApiError> {
    let output_guardrails = state.output_guardrails.as_ref().unwrap();

    // Read the response body
    let (parts, body) = response.into_parts();
    let body_bytes =
        match axum::body::to_bytes(body, state.config.server.max_response_body_bytes).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read response body for output guardrails");
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "response_read_error",
                    "Failed to read response for guardrails evaluation",
                ));
            }
        };

    // Extract content from the completions format
    let content = crate::guardrails::extract_text_from_completion_response(&body_bytes);

    // If no content to evaluate, return the original response
    if content.is_empty() {
        let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
        return Ok((response, Vec::new()));
    }

    // Evaluate the content
    let result = output_guardrails
        .evaluate_response(&content, None, user_id.as_deref())
        .await;

    match result {
        Ok(guardrails_result) => {
            let headers = guardrails_result.to_headers();

            // Log audit event for output guardrails evaluation
            log_output_guardrails_evaluation(
                state,
                auth,
                output_guardrails.provider_name(),
                &guardrails_result,
                None,
                ip_address,
                user_agent,
            );

            // Check if content should be blocked
            if guardrails_result.is_blocked() {
                let error = crate::guardrails::GuardrailsError::blocked_with_violations(
                    crate::guardrails::ContentSource::LlmOutput,
                    "Response blocked by output guardrails",
                    guardrails_result.violations().to_vec(),
                );
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "guardrails_output_blocked",
                    error.to_string(),
                ));
            }

            // Check if content should be redacted
            if let Some(modified_content) = guardrails_result.modified_content() {
                // For completions API, rebuild with modified text
                let modified_body = modify_completion_content(&body_bytes, modified_content)
                    .unwrap_or_else(|| body_bytes.to_vec());
                let response = Response::from_parts(parts, Body::from(modified_body));
                return Ok((response, headers));
            }

            // Log warnings if any violations were found but allowed
            if !guardrails_result.response.violations.is_empty() {
                tracing::info!(
                    violations = ?guardrails_result.response.violations.len(),
                    "Output guardrails found violations but allowed response"
                );
            }

            // Return the original response with headers
            let response = Response::from_parts(parts, Body::from(body_bytes.to_vec()));
            Ok((response, headers))
        }
        Err(e) => {
            let status = match e.error_code() {
                "guardrails_blocked" => StatusCode::INTERNAL_SERVER_ERROR,
                "guardrails_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "guardrails_auth_error" => StatusCode::UNAUTHORIZED,
                "guardrails_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                "guardrails_config_error" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_GATEWAY,
            };
            Err(ApiError::new(status, e.error_code(), e.to_string()))
        }
    }
}

/// Modifies the text in a completions API response JSON.
///
/// Returns the modified response body, or None if modification failed.
fn modify_completion_content(body: &[u8], new_content: &str) -> Option<Vec<u8>> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;

    // Modify choices[].text
    if let Some(choices) = json.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices {
            choice["text"] = serde_json::Value::String(new_content.to_string());
        }
    }

    serde_json::to_vec(&json).ok()
}
