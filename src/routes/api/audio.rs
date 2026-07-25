#[cfg(feature = "server")]
use axum::extract::Multipart;
use axum::{Extension, Json, body::Bytes, extract::State, response::Response};
use axum_valid::Valid;
use http::StatusCode;

use super::{ApiError, check_sovereignty, voice_to_string};
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
// Audio Endpoints
// ============================================================================

/// Generate speech from text
///
/// POST /v1/audio/speech
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/audio/speech",
    tag = "Audio",
    request_body = api_types::CreateSpeechRequest,
    responses(
        (status = 200, description = "Audio generated successfully", content_type = "audio/mpeg"),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.audio.speech",
    skip(state, auth, authz, payload),
    fields(model = %payload.model, voice = ?payload.voice)
)]
pub async fn api_v1_audio_speech(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::CreateSpeechRequest>>,
) -> Result<Response, ApiError> {
    // Count characters for usage tracking (before consuming payload)
    let character_count = payload.input.chars().count() as i64;

    // Route the model to a provider
    let model = Some(payload.model.clone());
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
        // Build request context with audio TTS-specific fields
        let request_ctx = RequestContext::new()
            .with_character_count(character_count as u64)
            .with_voice(voice_to_string(&payload.voice));

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

    // Replace model with resolved name (strip provider prefix)
    let mut payload = payload;
    payload.model = model_name.clone();

    // Execute the speech request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_speech(&state.http_client, payload)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_speech(&state.http_client, payload)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_speech(&state.http_client, payload)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support text-to-speech",
            ));
        }
    };

    let mut response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Speech generation failed: {}", e),
        )
    })?;

    // Compute TTS cost (character-based pricing); the API middleware
    // writes the usage row from the response headers below.
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_tts_characters(character_count),
    );

    // Add cost headers to audio response
    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = character_count.to_string().parse() {
        response.headers_mut().insert("X-Character-Count", value);
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
/// Transcribe audio to text
///
/// POST /v1/audio/transcriptions
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/audio/transcriptions",
    tag = "Audio",
    request_body(content_type = "multipart/form-data", content = api_types::CreateTranscriptionRequest),
    responses(
        (status = 200, description = "Audio transcribed successfully", body = api_types::audio::TranscriptionResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.audio.transcriptions", skip(state, auth, authz, multipart))]
pub async fn api_v1_audio_transcriptions(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    // Parse multipart form data
    let mut file_data: Option<Bytes> = None;
    let mut filename: Option<String> = None;
    let mut model: Option<String> = None;
    let mut language: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut response_format: Option<api_types::audio::AudioResponseFormat> = None;
    let mut temperature: Option<f32> = None;
    let mut timestamp_granularities: Option<Vec<api_types::audio::TimestampGranularity>> = None;
    let mut sovereignty_requirements: Option<crate::config::SovereigntyRequirements> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "multipart_error",
            format!("Failed to read multipart field: {}", e),
        )
    })? {
        let field_name = field.name().unwrap_or_default().to_string();

        match field_name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                file_data = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "file_read_error",
                        format!("Failed to read file: {}", e),
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
            "language" => {
                language = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "language_read_error",
                        format!("Failed to read language: {}", e),
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
            "temperature" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "temperature_read_error",
                        format!("Failed to read temperature: {}", e),
                    )
                })?;
                temperature = Some(value.parse().map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_temperature",
                        "Invalid value for temperature",
                    )
                })?);
            }
            "timestamp_granularities[]" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "timestamp_granularities_read_error",
                        format!("Failed to read timestamp_granularities: {}", e),
                    )
                })?;
                let granularity: api_types::audio::TimestampGranularity =
                    serde_json::from_str(&format!("\"{}\"", value)).map_err(|_| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "invalid_timestamp_granularity",
                            format!("Invalid timestamp_granularity: {}", value),
                        )
                    })?;
                timestamp_granularities
                    .get_or_insert_with(Vec::new)
                    .push(granularity);
            }
            "sovereignty_requirements" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "sovereignty_requirements_read_error",
                        format!("Failed to read sovereignty_requirements: {}", e),
                    )
                })?;
                sovereignty_requirements = Some(serde_json::from_str(&value).map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_sovereignty_requirements",
                        format!("Invalid sovereignty_requirements: {}", e),
                    )
                })?);
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    // Validate required fields
    let file_data = file_data.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_file",
            "Missing required field: file",
        )
    })?;
    let model = model.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_model",
            "Missing required field: model",
        )
    })?;
    let filename = filename.unwrap_or_else(|| "audio.mp3".to_string());

    // Estimate audio duration from file size for usage tracking
    // Average bitrate ~128 kbps = 16000 bytes/sec
    // This is approximate; actual duration may vary by codec
    let file_size = file_data.len();
    let estimated_seconds = (file_size as i64 / 16000).max(1);

    // Build the request
    let request = api_types::CreateTranscriptionRequest {
        model: model.clone(),
        language,
        prompt,
        response_format,
        temperature,
        timestamp_granularities,
        stream: None,
        include: None,
        chunking_strategy: None,
        known_speaker_names: None,
        known_speaker_references: None,
    };

    // Route the model to a provider
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
        // Build request context with audio transcription-specific fields
        let mut request_ctx = RequestContext::new();

        if let Some(ref lang) = request.language {
            request_ctx = request_ctx.with_language(lang.clone());
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
                Some(&model),
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
        sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Replace model with resolved name (strip provider prefix)
    let mut request = request;
    request.model = model_name.clone();

    // Execute the transcription request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_transcription(&state.http_client, file_data, filename, request)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_transcription(&state.http_client, file_data, filename, request)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_transcription(&state.http_client, file_data, filename, request)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support audio transcription",
            ));
        }
    };

    let mut response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Transcription failed: {}", e),
        )
    })?;

    // Compute transcription cost (per-second pricing); the API middleware
    // writes the usage row from the response headers below.
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_audio_seconds(estimated_seconds),
    );

    // Add cost headers to response
    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = estimated_seconds.to_string().parse() {
        response.headers_mut().insert("X-Audio-Seconds", value);
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
/// Translate audio to English text
///
/// POST /v1/audio/translations
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/audio/translations",
    tag = "Audio",
    request_body(content_type = "multipart/form-data", content = api_types::CreateTranslationRequest),
    responses(
        (status = 200, description = "Audio translated successfully", body = api_types::audio::TranslationResponse),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.audio.translations", skip(state, auth, authz, multipart))]
pub async fn api_v1_audio_translations(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    // Parse multipart form data
    let mut file_data: Option<Bytes> = None;
    let mut filename: Option<String> = None;
    let mut model: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut response_format: Option<api_types::audio::AudioResponseFormat> = None;
    let mut temperature: Option<f32> = None;
    let mut sovereignty_requirements: Option<crate::config::SovereigntyRequirements> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "multipart_error",
            format!("Failed to read multipart field: {}", e),
        )
    })? {
        let field_name = field.name().unwrap_or_default().to_string();

        match field_name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                file_data = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "file_read_error",
                        format!("Failed to read file: {}", e),
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
            "prompt" => {
                prompt = Some(field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "prompt_read_error",
                        format!("Failed to read prompt: {}", e),
                    )
                })?);
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
            "temperature" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "temperature_read_error",
                        format!("Failed to read temperature: {}", e),
                    )
                })?;
                temperature = Some(value.parse().map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_temperature",
                        "Invalid value for temperature",
                    )
                })?);
            }
            "sovereignty_requirements" => {
                let value = field.text().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "sovereignty_requirements_read_error",
                        format!("Failed to read sovereignty_requirements: {}", e),
                    )
                })?;
                sovereignty_requirements = Some(serde_json::from_str(&value).map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_sovereignty_requirements",
                        format!("Invalid sovereignty_requirements: {}", e),
                    )
                })?);
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    // Validate required fields
    let file_data = file_data.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_file",
            "Missing required field: file",
        )
    })?;
    let model = model.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_model",
            "Missing required field: model",
        )
    })?;
    let filename = filename.unwrap_or_else(|| "audio.mp3".to_string());

    // Estimate audio duration from file size for usage tracking
    // Average bitrate ~128 kbps = 16000 bytes/sec
    // This is approximate; actual duration may vary by codec
    let file_size = file_data.len();
    let estimated_seconds = (file_size as i64 / 16000).max(1);

    // Build the request
    let request = api_types::CreateTranslationRequest {
        model: model.clone(),
        prompt,
        response_format,
        temperature,
    };

    // Route the model to a provider
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
        // Build request context (translation has minimal context - just model)
        let request_ctx = RequestContext::new();

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
                Some(&model),
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
        sovereignty_requirements.as_ref(),
        &provider_config,
        &model_name,
        &state.model_catalog,
    )?;

    // Replace model with resolved name (strip provider prefix)
    let mut request = request;
    request.model = model_name.clone();

    // Execute the translation request
    let response = match provider_config {
        ProviderConfig::OpenAi(config) => {
            open_ai::OpenAICompatibleProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_translation(&state.http_client, file_data, filename, request)
            .await
        }
        #[cfg(feature = "provider-azure")]
        ProviderConfig::AzureOpenAi(config) => {
            azure_openai::AzureOpenAIProvider::from_config_with_registry(
                &config,
                &provider_name,
                &state.circuit_breakers,
            )
            .create_translation(&state.http_client, file_data, filename, request)
            .await
        }
        ProviderConfig::Test(config) => {
            test::TestProvider::new(&config.model_name)
                .create_translation(&state.http_client, file_data, filename, request)
                .await
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_provider",
                "This provider does not support audio translation",
            ));
        }
    };

    let mut response = response.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Translation failed: {}", e),
        )
    })?;

    // Compute translation cost (per-second pricing); the API middleware
    // writes the usage row from the response headers below.
    let principal = crate::models::RequestPrincipal::from_auth(auth.as_ref().map(|e| &e.0))
        .or_defaults(state.default_identity());

    let (cost_microcents, pricing_source) = crate::providers::compute_media_cost(
        &provider_name,
        &model_name,
        &state.pricing,
        principal.org_id,
        &crate::pricing::TokenUsage::for_audio_seconds(estimated_seconds),
    );

    // Add cost headers to response
    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        response.headers_mut().insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = estimated_seconds.to_string().parse() {
        response.headers_mut().insert("X-Audio-Seconds", value);
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
