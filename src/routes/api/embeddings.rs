use axum::{Extension, Json, body::Body, extract::State, http::HeaderMap, response::Response};
use axum_valid::Valid;
use http::StatusCode;

use super::{ApiError, CacheStatus, check_sovereignty, should_bypass_cache};
use crate::{
    AppState, api_types,
    auth::AuthenticatedRequest,
    cache::CacheLookupResult,
    middleware::AuthzContext,
    routes::execution::{EmbeddingExecutor, ExecutionResult, execute_with_fallback},
    routing::{resolver, route_model_extended},
};

/// Create embeddings
///
/// Creates an embedding vector representing the input text. Embeddings are useful for
/// semantic search, clustering, classification, and similarity comparisons.
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/embeddings",
    tag = "embeddings",
    request_body(
        content = api_types::CreateEmbeddingPayload,
        examples(
            ("Single text" = (
                summary = "Embed a single text string",
                value = json!({
                    "model": "openai/text-embedding-3-small",
                    "input": "Hello world"
                })
            )),
            ("Multiple texts" = (
                summary = "Embed multiple texts in one request",
                value = json!({
                    "model": "openai/text-embedding-3-large",
                    "input": [
                        "First document to embed",
                        "Second document to embed",
                        "Third document to embed"
                    ],
                    "dimensions": 1024
                })
            ))
        )
    ),
    responses(
        (status = 200, description = "Embedding vectors for the input text(s)",
            example = json!({
                "object": "list",
                "data": [{
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.0023064255, -0.009327292, 0.015797347]
                }],
                "model": "text-embedding-3-small",
                "usage": {
                    "prompt_tokens": 2,
                    "total_tokens": 2
                }
            })
        ),
        (status = 400, description = "Bad request - invalid model or input",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "routing_error",
                    "message": "Model 'invalid-embedding-model' not found"
                }
            })
        ),
        (status = 502, description = "Provider error",
            body = crate::openapi::ErrorResponse,
            example = json!({
                "error": {
                    "code": "provider_error",
                    "message": "Upstream provider returned error"
                }
            })
        ),
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.embeddings",
    skip(state, headers, auth, authz, payload),
    fields(model = %payload.model)
)]
pub async fn api_v1_embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::CreateEmbeddingPayload>>,
) -> Result<Response, ApiError> {
    // Route the model to a provider with dynamic support
    let model = payload.model.clone();
    let routed = route_model_extended(Some(&model), &state.config.providers)?;

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

    // Check model restrictions if API key auth is used
    // Use original model string (with provider prefix) for restriction check
    if let Some(Extension(ref auth)) = auth
        && let Some(api_key) = auth.api_key()
    {
        api_key.check_model_allowed(&model).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
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

        // Check model access authorization (embeddings have no special request context)
        // Use original model string (with provider prefix) for RBAC policy evaluation
        authz
            .require_api(
                "model",
                "use",
                Some(&model),
                None, // No request context needed for embeddings
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

    // Track cache status for response headers
    let mut cache_status = CacheStatus::None;

    let cache_tenant = super::chat::tenant_scope_from_auth(auth.as_ref());

    // Check response cache (embeddings are fully deterministic - excellent for caching)
    if let Some(ref response_cache) = state.response_cache {
        match response_cache
            .lookup_embeddings(&payload, &model_name, &cache_tenant, force_refresh)
            .await
        {
            CacheLookupResult::Hit(cached) => {
                tracing::debug!(
                    model = %model_name,
                    provider = %cached.provider,
                    cached_at = cached.cached_at,
                    "Returning cached response (embeddings API)"
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
                // Caching disabled
            }
        }
    }

    // Execute embedding with fallback support
    let ExecutionResult {
        response,
        provider_name,
        model_name,
    } = execute_with_fallback::<EmbeddingExecutor>(
        &state,
        provider_name,
        provider_config,
        model_name,
        payload.clone(),
        sovereignty_reqs.as_ref(),
    )
    .await?;

    // Cache successful responses
    let final_response = if cache_status == CacheStatus::Miss && response.status().is_success() {
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
                            .store_embeddings(
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
        response
    };

    // Inject cost calculation into the response
    // Note: Embeddings don't stream, so no usage_entry or streaming_idle_timeout needed
    let mut final_response =
        crate::providers::inject_cost_into_response(crate::providers::CostInjectionParams {
            response: final_response,
            provider: &provider_name,
            model: &model_name,
            pricing: &state.pricing,
            db: state.db.as_ref(),
            usage_entry: None,
            pricing_org_id: crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
                .or_defaults(state.default_identity())
                .org_id,
            #[cfg(feature = "server")]
            task_tracker: Some(&state.task_tracker),
            #[cfg(feature = "server")]
            usage_drain: Some(&state.usage_drain),
            #[cfg(feature = "server")]
            settlement_cell: None, // Non-streaming: middleware settles from headers
            estimated_input_tokens: 0,
            max_response_body_bytes: state.config.server.max_response_body_bytes,
            streaming_idle_timeout_secs: 0, // Embeddings don't stream
            validation_config: &state.config.observability.response_validation,
            response_type: crate::validation::ResponseType::Embedding,
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
