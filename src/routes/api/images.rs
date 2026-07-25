#[cfg(feature = "server")]
use axum::extract::Multipart;
use axum::{
    Extension, Json,
    body::Bytes,
    extract::State,
    response::{IntoResponse, Response},
};
use axum_valid::Valid;
use http::StatusCode;

use super::{ApiError, check_sovereignty, image_quality_to_string, image_size_to_string};
#[cfg(feature = "provider-azure")]
use crate::providers::azure_openai;
use crate::{
    AppState, api_types,
    auth::AuthenticatedRequest,
    authz::RequestContext,
    config::ProviderConfig,
    middleware::AuthzContext,
    providers::{Provider, open_ai, test},
    routing::{resolver, route_model_extended},
};

// ============================================================================
// Image Generation Endpoints
// ============================================================================

/// Create image from text prompt
///
/// POST /v1/images/generations
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/images/generations",
    tag = "Images",
    request_body = api_types::CreateImageRequest,
    responses(
        (status = 200, description = "Image generated successfully", body = api_types::ImagesResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.images.generations",
    skip(state, auth, authz, payload),
    fields(model = %payload.model.as_deref().unwrap_or("dall-e-2"))
)]
pub async fn api_v1_images_generations(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::CreateImageRequest>>,
) -> Result<Response, ApiError> {
    // Route the model to a provider
    let model = payload.model.clone();
    let routed = route_model_extended(model.as_deref(), &state.config.providers)?;

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
        let model_to_check = model.as_deref().unwrap_or(&model_name);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
        // Build request context with image-specific fields
        let mut request_ctx = RequestContext::new().with_image_count(payload.n.unwrap_or(1) as u32);

        if let Some(ref size) = payload.size {
            request_ctx = request_ctx.with_image_size(image_size_to_string(size));
        }
        if let Some(ref quality) = payload.quality {
            request_ctx = request_ctx.with_image_quality(image_quality_to_string(quality));
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
                model.as_deref().or(Some(&model_name)),
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
    let _sovereignty_reqs = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Replace model with resolved name (strip provider prefix like "openai/dall-e-3" → "dall-e-3")
    let mut payload = payload;
    payload.model = Some(model_name.clone());

    // Strip parameters unsupported by this model's family (e.g. response_format for gpt-image)
    let model_family = provider_config
        .get_model_config(&model_name)
        .and_then(|mc| mc.family.as_deref());
    payload.normalize_for_family(model_family);

    // Capture size/quality for pricing before payload is consumed
    let pricing_size = payload.size.as_ref().map(image_size_to_string);
    let pricing_quality = payload.quality.as_ref().map(image_quality_to_string);

    // Execute the image generation request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image(&state.http_client, payload)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image(&state.http_client, payload)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_image(&state.http_client, payload)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support image generation",
            ));
        }
    };

    let images_response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Image generation failed: {}", e),
        )
    })?;

    // Count images and compute cost. The usage row is written by the API
    // middleware from the response headers below; the handler only prices
    // the request (org-scoped so admin pricing rows apply).
    let image_count = images_response.data.as_ref().map(|d| d.len()).unwrap_or(0) as i64;
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_images(image_count, pricing_size, pricing_quality),
    );

    // Build response with cost headers
    let mut response = Json(&images_response).into_response();

    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = image_count.to_string().parse() {
        response.headers_mut().insert("X-Image-Count", value);
    }
    if let Ok(value) = provider_name.parse() {
        response.headers_mut().insert("X-Provider", value);
    }
    if let Ok(source_val) = provider_source.parse() {
        response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(value) = model_name.parse() {
        response.headers_mut().insert("X-Model", value);
    }
    response.headers_mut().insert(
        "X-Pricing-Source",
        http::HeaderValue::from_static(pricing_source.as_str()),
    );

    Ok(response)
}

#[cfg(feature = "server")]
/// Edit image with text instructions
///
/// POST /v1/images/edits
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/images/edits",
    tag = "Images",
    request_body(content_type = "multipart/form-data", content = api_types::CreateImageEditRequest),
    responses(
        (status = 200, description = "Image edited successfully", body = api_types::ImagesResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.images.edits", skip(state, auth, authz, multipart))]
pub async fn api_v1_images_edits(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    // Parse multipart form data
    let mut image_data: Option<Bytes> = None;
    let mut mask_data: Option<Bytes> = None;
    let mut prompt: Option<String> = None;
    let mut model: Option<String> = None;
    let mut n: Option<i32> = None;
    let mut size: Option<api_types::images::ImageSize> = None;
    let mut response_format: Option<api_types::images::ImageResponseFormat> = None;
    let mut user: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "multipart_error",
            format!("Failed to read multipart field: {}", e),
        )
    })? {
        let field_name = field.name().unwrap_or_default().to_string();

        match field_name.as_str() {
            "image" => {
                image_data = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "image_read_error",
                        format!("Failed to read image: {}", e),
                    )
                })?);
            }
            "mask" => {
                mask_data = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "mask_read_error",
                        format!("Failed to read mask: {}", e),
                    )
                })?);
            }
            "prompt" => {
                prompt = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "prompt_read_error",
                        format!("Failed to read prompt: {}", e),
                    )
                })?);
            }
            "model" => {
                model = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "model_read_error",
                        format!("Failed to read model: {}", e),
                    )
                })?);
            }
            "n" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "n_read_error",
                        format!("Failed to read n: {}", e),
                    )
                })?;
                n = Some(value.parse().map_err(|_| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_n", "Invalid value for n")
                })?);
            }
            "size" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "size_read_error",
                        format!("Failed to read size: {}", e),
                    )
                })?;
                size = Some(
                    serde_json::from_str(&format!("\"{}\"", value)).map_err(|_| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_size",
                            format!("Invalid size: {}", value),
                        )
                    })?,
                );
            }
            "response_format" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "response_format_read_error",
                        format!("Failed to read response_format: {}", e),
                    )
                })?;
                response_format = Some(serde_json::from_str(&format!("\"{}\"", value)).map_err(
                    |_| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_response_format",
                            format!("Invalid response_format: {}", value),
                        )
                    },
                )?);
            }
            "user" => {
                user = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "user_read_error",
                        format!("Failed to read user: {}", e),
                    )
                })?);
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    // Validate required fields
    let image_data = image_data.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_image",
            "Missing required field: image",
        )
    })?;
    let prompt = prompt.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_prompt",
            "Missing required field: prompt",
        )
    })?;

    // Capture size for pricing before it's moved into the request
    let pricing_size = size.as_ref().map(image_size_to_string);

    // Build the request
    let request = api_types::CreateImageEditRequest {
        prompt,
        model: model.clone(),
        n,
        size,
        response_format,
        output_format: None,
        output_compression: None,
        background: None,
        quality: None,
        stream: None,
        partial_images: None,
        user,
    };

    // Route the model to a provider
    let routed = route_model_extended(model.as_deref(), &state.config.providers)?;

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

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
        // Build request context with image-specific fields
        let mut request_ctx = RequestContext::new().with_image_count(request.n.unwrap_or(1) as u32);

        if let Some(ref size) = request.size {
            request_ctx = request_ctx.with_image_size(image_size_to_string(size));
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
                model.as_deref().or(Some(&model_name)),
                Some(request_ctx),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    // Replace model with resolved name (strip provider prefix)
    let mut request = request;
    request.model = Some(model_name.clone());

    // Strip parameters unsupported by this model's family (e.g. response_format for gpt-image)
    let model_family = provider_config
        .get_model_config(&model_name)
        .and_then(|mc| mc.family.as_deref());
    request.normalize_for_family(model_family);

    // Execute the image edit request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image_edit(&state.http_client, image_data, mask_data, request)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image_edit(&state.http_client, image_data, mask_data, request)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_image_edit(&state.http_client, image_data, mask_data, request)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support image editing",
            ));
        }
    };

    let images_response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Image editing failed: {}", e),
        )
    })?;

    // Count images and compute cost; the API middleware writes the usage
    // row from the response headers below.
    let image_count = images_response.data.as_ref().map(|d| d.len()).unwrap_or(0) as i64;
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_images(
            image_count,
            pricing_size,
            None, // edits don't have a quality parameter
        ),
    );

    // Build response with cost headers
    let mut response = Json(&images_response).into_response();

    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = image_count.to_string().parse() {
        response.headers_mut().insert("X-Image-Count", value);
    }
    if let Ok(value) = provider_name.parse() {
        response.headers_mut().insert("X-Provider", value);
    }
    if let Ok(source_val) = provider_source.parse() {
        response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(value) = model_name.parse() {
        response.headers_mut().insert("X-Model", value);
    }
    response.headers_mut().insert(
        "X-Pricing-Source",
        http::HeaderValue::from_static(pricing_source.as_str()),
    );

    Ok(response)
}

#[cfg(feature = "server")]
/// Create image variations
///
/// POST /v1/images/variations
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/images/variations",
    tag = "Images",
    request_body(content_type = "multipart/form-data", content = api_types::CreateImageVariationRequest),
    responses(
        (status = 200, description = "Image variations created successfully", body = api_types::ImagesResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.images.variations", skip(state, auth, authz, multipart))]
pub async fn api_v1_images_variations(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    // Parse multipart form data
    let mut image_data: Option<Bytes> = None;
    let mut model: Option<String> = None;
    let mut n: Option<i32> = None;
    let mut size: Option<api_types::images::ImageSize> = None;
    let mut response_format: Option<api_types::images::ImageResponseFormat> = None;
    let mut user: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "multipart_error",
            format!("Failed to read multipart field: {}", e),
        )
    })? {
        let field_name = field.name().unwrap_or_default().to_string();

        match field_name.as_str() {
            "image" => {
                image_data = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "image_read_error",
                        format!("Failed to read image: {}", e),
                    )
                })?);
            }
            "model" => {
                model = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "model_read_error",
                        format!("Failed to read model: {}", e),
                    )
                })?);
            }
            "n" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "n_read_error",
                        format!("Failed to read n: {}", e),
                    )
                })?;
                n = Some(value.parse().map_err(|_| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_n", "Invalid value for n")
                })?);
            }
            "size" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "size_read_error",
                        format!("Failed to read size: {}", e),
                    )
                })?;
                size = Some(
                    serde_json::from_str(&format!("\"{}\"", value)).map_err(|_| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_size",
                            format!("Invalid size: {}", value),
                        )
                    })?,
                );
            }
            "response_format" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "response_format_read_error",
                        format!("Failed to read response_format: {}", e),
                    )
                })?;
                response_format = Some(serde_json::from_str(&format!("\"{}\"", value)).map_err(
                    |_| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_response_format",
                            format!("Invalid response_format: {}", value),
                        )
                    },
                )?);
            }
            "user" => {
                user = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "user_read_error",
                        format!("Failed to read user: {}", e),
                    )
                })?);
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    // Validate required fields
    let image_data = image_data.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_image",
            "Missing required field: image",
        )
    })?;

    // Capture size for pricing before it's moved into the request
    let pricing_size = size.as_ref().map(image_size_to_string);

    // Build the request
    let request = api_types::CreateImageVariationRequest {
        model: model.clone(),
        n,
        size,
        response_format,
        user,
    };

    // Route the model to a provider
    let routed = route_model_extended(model.as_deref(), &state.config.providers)?;

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

    // Check authorization if authz context is available and API RBAC is enabled
    if let Some(Extension(ref authz)) = authz {
        // Build request context with image-specific fields
        let mut request_ctx = RequestContext::new().with_image_count(request.n.unwrap_or(1) as u32);

        if let Some(ref size) = request.size {
            request_ctx = request_ctx.with_image_size(image_size_to_string(size));
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
                model.as_deref().or(Some(&model_name)),
                Some(request_ctx),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }

    // Replace model with resolved name (strip provider prefix)
    let mut request = request;
    request.model = Some(model_name.clone());

    // Execute the image variation request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image_variation(&state.http_client, image_data, request)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_image_variation(&state.http_client, image_data, request)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_image_variation(&state.http_client, image_data, request)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support image variations",
            ));
        }
    };

    let images_response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Image variation failed: {}", e),
        )
    })?;

    // Count images and compute cost; the API middleware writes the usage
    // row from the response headers below.
    let image_count = images_response.data.as_ref().map(|d| d.len()).unwrap_or(0) as i64;
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_images(
            image_count,
            pricing_size,
            None, // variations don't have a quality parameter
        ),
    );

    // Build response with cost headers
    let mut response = Json(&images_response).into_response();

    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = image_count.to_string().parse() {
        response.headers_mut().insert("X-Image-Count", value);
    }
    if let Ok(value) = provider_name.parse() {
        response.headers_mut().insert("X-Provider", value);
    }
    if let Ok(source_val) = provider_source.parse() {
        response
            .headers_mut()
            .insert("X-Provider-Source", source_val);
    }
    if let Ok(value) = model_name.parse() {
        response.headers_mut().insert("X-Model", value);
    }
    response.headers_mut().insert(
        "X-Pricing-Source",
        http::HeaderValue::from_static(pricing_source.as_str()),
    );

    Ok(response)
}
