//! OpenAI-compatible Videos API endpoints.
//!
//! Video generation is asynchronous. `POST /v1/videos` proxies to the resolved
//! provider, returns the queued job, and persists a `video_id -> provider/owner`
//! mapping. The bare-id endpoints (`GET`/`DELETE`/`content`/`remix`) look the
//! mapping up (org-scoped) and proxy live to the originating provider —
//! "proxy-on-read", so status and bytes are always authoritative upstream.

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Multipart, Path, Query, State},
    response::{IntoResponse, Response},
};
use axum_valid::Valid;
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::{ApiError, check_sovereignty};
use crate::{
    AppState, api_types,
    auth::AuthenticatedRequest,
    authz::RequestContext,
    db::repos::{
        NewVideo, NewVideoCharacter, ResponseOwner, VideoListOrder, VideoPatch, VideoRecord,
    },
    middleware::AuthzContext,
    pricing::TokenUsage,
    providers::{MediaUsageParams, ProviderError, log_media_usage},
    routing::{resolver, route_model_extended},
    services::{
        VideoStore,
        responses_pipeline::{derive_response_owner, resolve_request_org},
    },
};

// ============================================================================
// Shared helpers
// ============================================================================

fn get_video_store(state: &AppState) -> Result<&std::sync::Arc<VideoStore>, ApiError> {
    state.video_store.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "feature_not_available",
            "Video endpoints require database support. Rebuild with --features minimal or higher.",
        )
    })
}

/// Calling principal resolved into the columns a `videos` row needs.
struct VideoPrincipal {
    org_id: Uuid,
    owner: ResponseOwner,
    user_id: Option<Uuid>,
    api_key_id: Option<Uuid>,
    project_id: Option<Uuid>,
    service_account_id: Option<Uuid>,
}

fn video_principal(
    state: &AppState,
    auth: Option<&AuthenticatedRequest>,
) -> Result<VideoPrincipal, ApiError> {
    let org_id = resolve_request_org(auth, state.default_org_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "org_required",
            "Video endpoints require an authenticated org",
        )
    })?;
    let owner = derive_response_owner(state, auth).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "org_required",
            "Video endpoints require an authenticated principal",
        )
    })?;
    Ok(VideoPrincipal {
        org_id,
        owner,
        user_id: auth.and_then(|a| a.user_id()).or(state.default_user_id),
        api_key_id: auth.and_then(|a| a.api_key().map(|k| k.key.id)),
        project_id: auth.and_then(|a| a.api_key().and_then(|k| k.project_id)),
        service_account_id: auth.and_then(|a| a.api_key().and_then(|k| k.service_account_id)),
    })
}

/// Route + resolve a model string to a concrete provider.
async fn resolve_provider(
    state: &AppState,
    auth: Option<&AuthenticatedRequest>,
    model: Option<&str>,
) -> Result<resolver::ResolvedProviderInfo, ApiError> {
    let routed = route_model_extended(model, &state.config.providers)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "routing_error", e.to_string()))?;
    resolver::resolve_to_provider(
        routed,
        state.db.as_ref(),
        state.cache.as_ref(),
        state.secrets.as_ref(),
        auth,
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "provider_resolution_error",
            format!("Failed to resolve provider: {e}"),
        )
    })
}

/// Re-resolve the provider that created a stored job, from its persisted
/// `provider`/`model`, so a bare-id request can proxy back to it.
async fn resolve_stored_provider(
    state: &AppState,
    auth: Option<&AuthenticatedRequest>,
    record: &VideoRecord,
) -> Result<resolver::ResolvedProviderInfo, ApiError> {
    let model_str = match &record.provider {
        Some(provider) => format!("{provider}/{}", record.model),
        None => record.model.clone(),
    };
    resolve_provider(state, auth, Some(&model_str)).await
}

fn build_provider(
    state: &AppState,
    resolved: &resolver::ResolvedProviderInfo,
) -> Result<std::sync::Arc<dyn crate::providers::Provider>, ApiError> {
    crate::init::create_provider_instance(
        &resolved.provider_config,
        &resolved.provider_name,
        &state.circuit_breakers,
    )
    .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "unsupported_provider", e))
}

fn provider_error(e: ProviderError) -> ApiError {
    match e {
        ProviderError::Unsupported(msg) => {
            ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", msg)
        }
        other => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            format!("Video request failed: {other}"),
        ),
    }
}

fn ts_to_dt(secs: Option<i64>) -> Option<DateTime<Utc>> {
    secs.and_then(|s| DateTime::from_timestamp(s, 0))
}

/// Build a new persistence row from a freshly-created job.
fn new_video_row(
    video: &api_types::Video,
    principal: &VideoPrincipal,
    model_name: &str,
    provider_name: &str,
    now: DateTime<Utc>,
    retention_expires_at: DateTime<Utc>,
) -> NewVideo {
    NewVideo {
        id: video.id.clone(),
        org_id: principal.org_id,
        owner_type: principal.owner.owner_type(),
        owner_id: principal.owner.owner_id(),
        project_id: principal.project_id,
        user_id: principal.user_id,
        api_key_id: principal.api_key_id,
        service_account_id: principal.service_account_id,
        status: video.status.as_str().to_string(),
        model: model_name.to_string(),
        provider: Some(provider_name.to_string()),
        prompt: video.prompt.clone(),
        size: video.size.clone(),
        seconds: video.seconds.clone(),
        progress: video.progress,
        remixed_from_video_id: video.remixed_from_video_id.clone(),
        created_at: now,
        completed_at: ts_to_dt(video.completed_at),
        expires_at: ts_to_dt(video.expires_at),
        error: video
            .error
            .as_ref()
            .and_then(|e| serde_json::to_value(e).ok()),
        snapshot: serde_json::to_value(video).unwrap_or(Value::Null),
        retention_expires_at,
    }
}

fn record_to_video(record: &VideoRecord) -> Result<api_types::Video, ApiError> {
    serde_json::from_value(record.snapshot.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_snapshot",
            format!("Failed to decode stored video: {e}"),
        )
    })
}

/// Attach the cost/observability headers used across the media endpoints.
fn with_video_headers(
    mut response: Response,
    cost_microcents: Option<i64>,
    seconds: i64,
    provider_name: &str,
    source: &str,
    model_name: &str,
) -> Response {
    let headers = response.headers_mut();
    if let Some(cost) = cost_microcents
        && let Ok(value) = cost.to_string().parse()
    {
        headers.insert("X-Cost-Microcents", value);
    }
    if let Ok(value) = seconds.to_string().parse() {
        headers.insert("X-Video-Seconds", value);
    }
    if let Ok(value) = provider_name.parse() {
        headers.insert("X-Provider", value);
    }
    if let Ok(value) = source.parse() {
        headers.insert("X-Provider-Source", value);
    }
    if let Ok(value) = model_name.parse() {
        headers.insert("X-Model", value);
    }
    response
}

/// Shared authorization + API-key model-restriction check for the
/// generation endpoints (create/remix/edit/extend/characters).
async fn authorize_model(
    auth: Option<&AuthenticatedRequest>,
    authz: Option<&AuthzContext>,
    requested_model: Option<&str>,
    resolved_model: &str,
) -> Result<(), ApiError> {
    if let Some(auth) = auth
        && let Some(api_key) = auth.api_key()
    {
        let model_to_check = requested_model.unwrap_or(resolved_model);
        api_key.check_model_allowed(model_to_check).map_err(|e| {
            ApiError::new(StatusCode::FORBIDDEN, "model_not_allowed", e.to_string())
        })?;
    }

    if let Some(authz) = authz {
        let org_id = auth.and_then(|a| {
            a.api_key()
                .and_then(|k| k.org_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.org_ids.first().cloned()))
        });
        let project_id = auth.and_then(|a| {
            a.api_key()
                .and_then(|k| k.project_id.map(|id| id.to_string()))
                .or_else(|| a.identity().and_then(|i| i.project_ids.first().cloned()))
        });
        authz
            .require_api(
                "model",
                "use",
                requested_model.or(Some(resolved_model)),
                Some(RequestContext::new()),
                org_id.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| {
                ApiError::new(StatusCode::FORBIDDEN, "authorization_denied", e.to_string())
            })?;
    }
    Ok(())
}

// ============================================================================
// Create
// ============================================================================

/// Create a video generation job
///
/// POST /v1/videos
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/videos",
    tag = "Videos",
    request_body = api_types::CreateVideoRequest,
    responses(
        (status = 200, description = "Video job created", body = api_types::Video),
        (status = 400, description = "Bad request"),
        (status = 501, description = "Database support required")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.videos.create",
    skip(state, auth, authz, payload),
    fields(model = %payload.model.as_deref().unwrap_or("sora-2"))
)]
pub async fn api_v1_videos_create(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::CreateVideoRequest>>,
) -> Result<Response, ApiError> {
    let auth_ref = auth.as_ref().map(|e| &e.0);
    let authz_ref = authz.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;

    let requested_model = payload.model.clone();
    let resolved = resolve_provider(&state, auth_ref, requested_model.as_deref()).await?;
    authorize_model(
        auth_ref,
        authz_ref,
        requested_model.as_deref(),
        &resolved.model,
    )
    .await?;
    let _sovereignty = check_sovereignty(
        auth.as_ref(),
        payload.sovereignty_requirements.as_ref(),
        &resolved.provider_config,
        &resolved.model,
        &state.model_catalog,
    )?;

    let seconds = payload.seconds.map(|s| s.as_i64()).unwrap_or(4);

    // Strip the gateway-only fields and pin the resolved model before
    // forwarding upstream.
    let mut payload = payload;
    payload.model = Some(resolved.model.clone());
    payload.sovereignty_requirements = None;

    let provider = build_provider(&state, &resolved)?;
    let video = provider
        .create_video(&state.http_client, payload)
        .await
        .map_err(provider_error)?;

    persist_and_respond(&state, store, auth_ref, &resolved, video, seconds).await
}

/// Shared tail for the generation endpoints: persist the new job, log
/// per-second usage, and return the job with cost headers.
async fn persist_and_respond(
    state: &AppState,
    store: &std::sync::Arc<VideoStore>,
    auth: Option<&AuthenticatedRequest>,
    resolved: &resolver::ResolvedProviderInfo,
    video: api_types::Video,
    seconds: i64,
) -> Result<Response, ApiError> {
    let principal = video_principal(state, auth)?;
    let now = Utc::now();
    let row = new_video_row(
        &video,
        &principal,
        &resolved.model,
        &resolved.provider_name,
        now,
        store.retention_expires_at(now),
    );
    store.create(row).await.map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persist_error",
            format!("Failed to persist video job: {e}"),
        )
    })?;

    let (cost_microcents, _) = log_media_usage(MediaUsageParams {
        provider: &resolved.provider_name,
        model: &resolved.model,
        pricing: &state.pricing,
        db: state.db.as_ref(),
        api_key_id: principal.api_key_id,
        #[cfg(feature = "server")]
        task_tracker: &state.task_tracker,
        usage: TokenUsage::for_video_seconds(seconds),
    })
    .await;

    let response = Json(&video).into_response();
    Ok(with_video_headers(
        response,
        cost_microcents,
        seconds,
        &resolved.provider_name,
        resolved.source,
        &resolved.model,
    ))
}

// ============================================================================
// Retrieve / List
// ============================================================================

/// Retrieve a video job (refreshes status from the provider)
///
/// GET /v1/videos/{video_id}
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/videos/{video_id}",
    tag = "Videos",
    params(("video_id" = String, Path, description = "Video job id")),
    responses(
        (status = 200, description = "Video job", body = api_types::Video),
        (status = 404, description = "Not found")
    ),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.retrieve", skip(state, auth))]
pub async fn api_v1_videos_retrieve(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    Path(video_id): Path<String>,
) -> Result<Response, ApiError> {
    let auth = auth.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth)?;

    let record = store
        .get(&video_id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&video_id))?;

    // Proxy-on-read: fetch fresh status from the originating provider and
    // refresh the snapshot. Fall back to the stored snapshot if the
    // provider is unreachable so reads stay resilient.
    let resolved = resolve_stored_provider(&state, auth, &record).await;
    match resolved {
        Ok(resolved) => {
            let provider = build_provider(&state, &resolved)?;
            match provider.get_video(&state.http_client, &video_id).await {
                Ok(fresh) => {
                    let patch = VideoPatch {
                        status: fresh.status.as_str().to_string(),
                        progress: fresh.progress,
                        completed_at: ts_to_dt(fresh.completed_at),
                        expires_at: ts_to_dt(fresh.expires_at),
                        error: fresh
                            .error
                            .as_ref()
                            .and_then(|e| serde_json::to_value(e).ok()),
                        snapshot: serde_json::to_value(&fresh).unwrap_or(Value::Null),
                    };
                    if let Err(e) = store.refresh(&video_id, principal.org_id, patch).await {
                        tracing::warn!(error = %e, video_id, "failed to refresh video snapshot");
                    }
                    Ok(Json(&fresh).into_response())
                }
                Err(e) => {
                    tracing::warn!(error = %e, video_id, "upstream get_video failed; serving snapshot");
                    Ok(Json(&record_to_video(&record)?).into_response())
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, video_id, "provider re-resolve failed; serving snapshot");
            Ok(Json(&record_to_video(&record)?).into_response())
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListVideosQuery {
    /// Pagination cursor: a video id to list items after.
    pub after: Option<String>,
    /// Max items to return (default 20, max 100).
    pub limit: Option<i64>,
    /// Sort order: `asc` or `desc` (default `desc`).
    pub order: Option<String>,
}

/// List video jobs
///
/// GET /v1/videos
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/videos",
    tag = "Videos",
    responses((status = 200, description = "List of video jobs", body = api_types::VideoListResponse)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.list", skip(state, auth))]
pub async fn api_v1_videos_list(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    Query(query): Query<ListVideosQuery>,
) -> Result<Json<api_types::VideoListResponse>, ApiError> {
    let auth = auth.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth)?;

    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let order = match query.order.as_deref() {
        Some("asc") => VideoListOrder::Asc,
        _ => VideoListOrder::Desc,
    };

    let (records, has_more) = store
        .list(
            principal.owner.owner_type(),
            principal.owner.owner_id(),
            principal.org_id,
            query.after,
            limit,
            order,
        )
        .await
        .map_err(db_error)?;

    let data = records
        .iter()
        .map(record_to_video)
        .collect::<Result<Vec<_>, _>>()?;
    let first_id = data.first().map(|v| v.id.clone());
    let last_id = data.last().map(|v| v.id.clone());

    Ok(Json(api_types::VideoListResponse {
        object: "list".to_string(),
        data,
        first_id,
        last_id,
        has_more,
    }))
}

// ============================================================================
// Delete
// ============================================================================

/// Delete a video job
///
/// DELETE /v1/videos/{video_id}
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/api/v1/videos/{video_id}",
    tag = "Videos",
    params(("video_id" = String, Path, description = "Video job id")),
    responses((status = 200, description = "Deleted", body = api_types::VideoDeleteResponse)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.delete", skip(state, auth))]
pub async fn api_v1_videos_delete(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    Path(video_id): Path<String>,
) -> Result<Json<api_types::VideoDeleteResponse>, ApiError> {
    let auth = auth.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth)?;

    let record = store
        .get(&video_id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&video_id))?;

    // Best-effort upstream delete; a failure there shouldn't strand the
    // local mapping row, which we always remove.
    if let Ok(resolved) = resolve_stored_provider(&state, auth, &record).await {
        let provider = build_provider(&state, &resolved)?;
        if let Err(e) = provider.delete_video(&state.http_client, &video_id).await {
            tracing::warn!(error = %e, video_id, "upstream delete_video failed");
        }
    }

    let deleted = store
        .delete(&video_id, principal.org_id)
        .await
        .map_err(db_error)?;
    Ok(Json(api_types::VideoDeleteResponse::new(video_id, deleted)))
}

// ============================================================================
// Content download
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ContentQuery {
    /// Asset variant: `video` (default), `thumbnail`, or `spritesheet`.
    pub variant: Option<api_types::VideoVariant>,
}

/// Download the rendered asset for a video job
///
/// GET /v1/videos/{video_id}/content
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/videos/{video_id}/content",
    tag = "Videos",
    params(("video_id" = String, Path, description = "Video job id")),
    responses((status = 200, description = "Rendered asset bytes")),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.content", skip(state, auth))]
pub async fn api_v1_videos_content(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    Path(video_id): Path<String>,
    Query(query): Query<ContentQuery>,
) -> Result<Response, ApiError> {
    let auth = auth.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth)?;

    let record = store
        .get(&video_id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&video_id))?;

    let resolved = resolve_stored_provider(&state, auth, &record).await?;
    let provider = build_provider(&state, &resolved)?;
    provider
        .get_video_content(&state.http_client, &video_id, query.variant)
        .await
        .map_err(provider_error)
}

// ============================================================================
// Remix / Edit / Extend
// ============================================================================

/// Remix a video into a new job
///
/// POST /v1/videos/{video_id}/remix
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/videos/{video_id}/remix",
    tag = "Videos",
    params(("video_id" = String, Path, description = "Source video id")),
    request_body = api_types::RemixVideoRequest,
    responses((status = 200, description = "New video job", body = api_types::Video)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.remix", skip(state, auth, authz, payload))]
pub async fn api_v1_videos_remix(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Path(video_id): Path<String>,
    Valid(Json(payload)): Valid<Json<api_types::RemixVideoRequest>>,
) -> Result<Response, ApiError> {
    let auth_ref = auth.as_ref().map(|e| &e.0);
    let authz_ref = authz.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth_ref)?;

    let record = store
        .get(&video_id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&video_id))?;

    let resolved = resolve_stored_provider(&state, auth_ref, &record).await?;
    authorize_model(auth_ref, authz_ref, None, &resolved.model).await?;
    // Remix launches new upstream generation, so it must pass the same
    // sovereignty gate as create (API-key requirements; no per-request field).
    let _sovereignty = check_sovereignty(
        auth.as_ref(),
        None,
        &resolved.provider_config,
        &resolved.model,
        &state.model_catalog,
    )?;

    let provider = build_provider(&state, &resolved)?;
    let video = provider
        .remix_video(&state.http_client, &video_id, payload)
        .await
        .map_err(provider_error)?;

    let seconds = record
        .seconds
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    persist_and_respond(&state, store, auth_ref, &resolved, video, seconds).await
}

/// Edit a video into a new job
///
/// POST /v1/videos/edits
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/videos/edits",
    tag = "Videos",
    request_body = api_types::VideoEditRequest,
    responses((status = 200, description = "New video job", body = api_types::Video)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.edits", skip(state, auth, authz, payload))]
pub async fn api_v1_videos_edits(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::VideoEditRequest>>,
) -> Result<Response, ApiError> {
    let auth_ref = auth.as_ref().map(|e| &e.0);
    let authz_ref = authz.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth_ref)?;

    let record = store
        .get(&payload.video.id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&payload.video.id))?;

    let resolved = resolve_stored_provider(&state, auth_ref, &record).await?;
    authorize_model(auth_ref, authz_ref, None, &resolved.model).await?;
    // Edit launches new upstream generation, so it must pass the same
    // sovereignty gate as create (API-key requirements; no per-request field).
    let _sovereignty = check_sovereignty(
        auth.as_ref(),
        None,
        &resolved.provider_config,
        &resolved.model,
        &state.model_catalog,
    )?;

    let provider = build_provider(&state, &resolved)?;
    let video = provider
        .edit_video(&state.http_client, payload)
        .await
        .map_err(provider_error)?;

    let seconds = record
        .seconds
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    persist_and_respond(&state, store, auth_ref, &resolved, video, seconds).await
}

/// Extend a video into a new job
///
/// POST /v1/videos/extensions
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/videos/extensions",
    tag = "Videos",
    request_body = api_types::VideoExtensionRequest,
    responses((status = 200, description = "New video job", body = api_types::Video)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.extensions", skip(state, auth, authz, payload))]
pub async fn api_v1_videos_extensions(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    Valid(Json(payload)): Valid<Json<api_types::VideoExtensionRequest>>,
) -> Result<Response, ApiError> {
    let auth_ref = auth.as_ref().map(|e| &e.0);
    let authz_ref = authz.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth_ref)?;

    let record = store
        .get(&payload.video.id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&payload.video.id))?;

    let resolved = resolve_stored_provider(&state, auth_ref, &record).await?;
    authorize_model(auth_ref, authz_ref, None, &resolved.model).await?;
    // Extension launches new upstream generation, so it must pass the same
    // sovereignty gate as create (API-key requirements; no per-request field).
    let _sovereignty = check_sovereignty(
        auth.as_ref(),
        None,
        &resolved.provider_config,
        &resolved.model,
        &state.model_catalog,
    )?;

    let seconds = payload.seconds.as_i64();
    let provider = build_provider(&state, &resolved)?;
    let video = provider
        .extend_video(&state.http_client, payload)
        .await
        .map_err(provider_error)?;

    persist_and_respond(&state, store, auth_ref, &resolved, video, seconds).await
}

// ============================================================================
// Characters
// ============================================================================

/// Create a character from a reference video
///
/// POST /v1/videos/characters (multipart/form-data: name, video, [model])
#[cfg(feature = "server")]
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/api/v1/videos/characters",
    tag = "Videos",
    request_body(content_type = "multipart/form-data", content = api_types::videos::CreateCharacterRequest),
    responses((status = 200, description = "Character", body = api_types::Character)),
    security(("api_key" = []))
))]
#[tracing::instrument(
    name = "api.videos.characters.create",
    skip(state, auth, authz, multipart)
)]
pub async fn api_v1_videos_characters_create(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    authz: Option<Extension<AuthzContext>>,
    mut multipart: Multipart,
) -> Result<Json<api_types::Character>, ApiError> {
    let auth_ref = auth.as_ref().map(|e| &e.0);
    let authz_ref = authz.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;

    let mut name: Option<String> = None;
    let mut model: Option<String> = None;
    let mut video_bytes: Option<Bytes> = None;
    let mut filename: String = "video.mp4".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "multipart_error",
            format!("Failed to read multipart field: {e}"),
        )
    })? {
        match field.name().unwrap_or_default() {
            "name" => name = Some(read_text(field, "name").await?),
            "model" => model = Some(read_text(field, "model").await?),
            "video" => {
                if let Some(fname) = field.file_name() {
                    filename = fname.to_string();
                }
                video_bytes = Some(field.bytes().await.map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "video_read_error",
                        format!("Failed to read video: {e}"),
                    )
                })?);
            }
            _ => {}
        }
    }

    let name = name.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_name",
            "Missing required field: name",
        )
    })?;
    let video_bytes = video_bytes.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "missing_video",
            "Missing required field: video",
        )
    })?;

    // Characters carry no model field upstream; Hadrian uses an optional
    // `model` form field (default `sora-2`) purely to pick the provider.
    let requested_model = model.clone().or_else(|| Some("sora-2".to_string()));
    let resolved = resolve_provider(&state, auth_ref, requested_model.as_deref()).await?;
    authorize_model(
        auth_ref,
        authz_ref,
        requested_model.as_deref(),
        &resolved.model,
    )
    .await?;
    // Character creation starts provider-side work, so it must pass the same
    // sovereignty gate as create (API-key requirements; no per-request field).
    let _sovereignty = check_sovereignty(
        auth.as_ref(),
        None,
        &resolved.provider_config,
        &resolved.model,
        &state.model_catalog,
    )?;

    let provider = build_provider(&state, &resolved)?;
    let character = provider
        .create_character(&state.http_client, name, video_bytes, filename)
        .await
        .map_err(provider_error)?;

    let principal = video_principal(&state, auth_ref)?;
    let now = Utc::now();
    store
        .create_character(NewVideoCharacter {
            id: character.id.clone(),
            org_id: principal.org_id,
            owner_type: principal.owner.owner_type(),
            owner_id: principal.owner.owner_id(),
            project_id: principal.project_id,
            user_id: principal.user_id,
            api_key_id: principal.api_key_id,
            service_account_id: principal.service_account_id,
            provider: Some(resolved.provider_name.clone()),
            model: Some(resolved.model.clone()),
            name: character.name.clone(),
            snapshot: serde_json::to_value(&character).unwrap_or(Value::Null),
            created_at: now,
        })
        .await
        .map_err(db_error)?;

    Ok(Json(character))
}

/// Retrieve a character
///
/// GET /v1/videos/characters/{character_id}
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/api/v1/videos/characters/{character_id}",
    tag = "Videos",
    params(("character_id" = String, Path, description = "Character id")),
    responses((status = 200, description = "Character", body = api_types::Character)),
    security(("api_key" = []))
))]
#[tracing::instrument(name = "api.videos.characters.retrieve", skip(state, auth))]
pub async fn api_v1_videos_characters_retrieve(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedRequest>>,
    Path(character_id): Path<String>,
) -> Result<Json<api_types::Character>, ApiError> {
    let auth = auth.as_ref().map(|e| &e.0);
    let store = get_video_store(&state)?;
    let principal = video_principal(&state, auth)?;

    let record = store
        .get_character(&character_id, principal.org_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found(&character_id))?;

    // Re-resolve from the stored provider/model and proxy a fresh lookup;
    // fall back to the stored snapshot on failure.
    let model_str = match (&record.provider, &record.model) {
        (Some(p), Some(m)) => Some(format!("{p}/{m}")),
        _ => record.model.clone(),
    };
    if let Some(model_str) = model_str
        && let Ok(resolved) = resolve_provider(&state, auth, Some(&model_str)).await
        && let Ok(provider) = build_provider(&state, &resolved)
        && let Ok(character) = provider
            .get_character(&state.http_client, &character_id)
            .await
    {
        return Ok(Json(character));
    }

    let character: api_types::Character =
        serde_json::from_value(record.snapshot.clone()).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "corrupt_snapshot",
                format!("Failed to decode stored character: {e}"),
            )
        })?;
    Ok(Json(character))
}

// ============================================================================
// Small helpers
// ============================================================================

fn db_error(e: crate::db::DbError) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "database_error",
        format!("Database error: {e}"),
    )
}

fn not_found(id: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("Video '{id}' not found"),
    )
}

#[cfg(feature = "server")]
async fn read_text(
    field: axum::extract::multipart::Field<'_>,
    name: &str,
) -> Result<String, ApiError> {
    field.text().await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "field_read_error",
            format!("Failed to read field '{name}': {e}"),
        )
    })
}
