#[cfg(any(feature = "server", feature = "wasm"))]
use axum::Router;
#[cfg(feature = "server")]
use axum::middleware::from_fn_with_state;
#[cfg(feature = "server")]
use axum::routing::{delete, get, post};
use axum::{
    Extension, Json,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use http::StatusCode;
use serde::Deserialize;
#[cfg(feature = "server")]
use tower::ServiceBuilder;
use uuid::Uuid;

#[cfg(feature = "wasm")]
use crate::compat::wasm_routing::{delete, get, post};
use crate::{
    AppState, api_types,
    auth::AuthenticatedRequest,
    config::{ProviderConfig, SovereigntyMetadata, SovereigntyRequirements},
    db::DbError,
    models::{VectorStore, VectorStoreOwnerType},
    routing::RoutingError,
    services::{FilesServiceError, Services},
};

mod audio;
pub(crate) mod chat;
#[cfg(feature = "server")]
pub mod containers;
mod embeddings;
mod files;
mod images;
mod models;
#[cfg(feature = "server")]
pub mod responses_lookup;
#[cfg(feature = "server")]
pub mod skills;
pub(crate) mod tools;
mod vector_stores;

// Re-export all public items from submodules
pub use audio::*;
pub use chat::*;
pub use embeddings::*;
pub use files::*;
pub use images::*;
pub use models::*;
pub use tools::*;
pub use vector_stores::*;

/// Check if cache should be bypassed based on request headers.
///
/// Respects:
/// - `Cache-Control: no-cache` or `Cache-Control: no-store`
/// - `X-Cache-Force-Refresh: true`
fn should_bypass_cache(headers: &HeaderMap) -> bool {
    // Check Cache-Control header
    if let Some(cache_control) = headers.get("Cache-Control")
        && let Ok(value) = cache_control.to_str()
        && (value.contains("no-cache") || value.contains("no-store"))
    {
        return true;
    }

    // Check X-Cache-Force-Refresh header
    if let Some(force_refresh) = headers.get("X-Cache-Force-Refresh")
        && let Ok(value) = force_refresh.to_str()
        && (value.eq_ignore_ascii_case("true") || value == "1")
    {
        return true;
    }

    false
}

/// Enforce sovereignty requirements against the resolved provider/model.
///
/// Merges API-key requirements and per-request requirements, then checks the
/// merged result against the resolved sovereignty metadata. Returns the merged
/// requirements on success (for use in fallback chain filtering), or an `ApiError`
/// on violation.
fn check_sovereignty(
    auth: Option<&Extension<AuthenticatedRequest>>,
    per_request: Option<&SovereigntyRequirements>,
    provider_config: &ProviderConfig,
    model_name: &str,
    catalog: &crate::catalog::ModelCatalogRegistry,
) -> Result<Option<SovereigntyRequirements>, ApiError> {
    let key_reqs = auth
        .and_then(|Extension(a)| a.api_key())
        .and_then(|k| k.sovereignty_requirements());
    let merged = SovereigntyRequirements::merge(key_reqs, per_request);
    let Some(reqs) = merged else {
        return Ok(None);
    };

    let model_config = provider_config.get_model_config(model_name);
    let provider_sov = provider_config.sovereignty();
    let model_sov = model_config.and_then(|mc| mc.sovereignty.as_ref());
    let resolved = SovereigntyMetadata::merge(provider_sov, model_sov).unwrap_or_default();

    // Open weights: config overrides catalog (matching /v1/models response)
    let open_weights = model_config
        .and_then(|mc| mc.open_weights)
        .or_else(|| {
            let catalog_provider_id = crate::catalog::resolve_catalog_provider_id(
                provider_config.provider_type_name(),
                provider_config.base_url(),
                provider_config.catalog_provider(),
            )?;
            catalog
                .lookup(&catalog_provider_id, model_name)
                .map(|e| e.open_weights)
        })
        .unwrap_or(false);

    reqs.check(&resolved, open_weights).map_err(|reason| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "sovereignty_violation",
            format!("Request blocked by sovereignty requirements: {reason}"),
        )
    })?;

    Ok(Some(reqs))
}

/// Check if any messages contain image content (multimodal).
fn messages_contain_images(messages: &[api_types::Message]) -> bool {
    use api_types::{
        Message,
        chat_completion::{ContentPart, MessageContent},
    };
    messages.iter().any(|msg| {
        let content = match msg {
            Message::System { content, .. } => Some(content),
            Message::User { content, .. } => Some(content),
            Message::Assistant { content, .. } => content.as_ref(),
            Message::Tool { content, .. } => Some(content),
            Message::Developer { content, .. } => Some(content),
        };
        content.is_some_and(|c| match c {
            MessageContent::Text(_) => false,
            MessageContent::Parts(parts) => parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
        })
    })
}

/// Convert ResponseFormat enum to string for CEL policies.
fn response_format_to_string(format: &api_types::chat_completion::ResponseFormat) -> &'static str {
    use api_types::chat_completion::ResponseFormat;
    match format {
        ResponseFormat::Text => "text",
        ResponseFormat::JsonObject => "json_object",
        ResponseFormat::JsonSchema { .. } => "json_schema",
        ResponseFormat::Grammar { .. } => "grammar",
        ResponseFormat::Python => "python",
    }
}

/// Convert ReasoningEffort enum to string for CEL policies.
fn reasoning_effort_to_string(effort: &api_types::ReasoningEffort) -> &'static str {
    use api_types::ReasoningEffort;
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Convert ResponsesReasoningEffort enum to string for CEL policies.
fn responses_reasoning_effort_to_string(
    effort: &api_types::ResponsesReasoningEffort,
) -> &'static str {
    use api_types::ResponsesReasoningEffort;
    match effort {
        ResponsesReasoningEffort::None => "none",
        ResponsesReasoningEffort::Minimal => "minimal",
        ResponsesReasoningEffort::Low => "low",
        ResponsesReasoningEffort::Medium => "medium",
        ResponsesReasoningEffort::High => "high",
    }
}

/// Convert ImageSize enum to string for CEL policies.
fn image_size_to_string(size: &api_types::ImageSize) -> &'static str {
    use api_types::ImageSize;
    match size {
        ImageSize::Auto => "auto",
        ImageSize::Size256 => "256x256",
        ImageSize::Size512 => "512x512",
        ImageSize::Size1024 => "1024x1024",
        ImageSize::Size1536x1024 => "1536x1024",
        ImageSize::Size1024x1536 => "1024x1536",
        ImageSize::Size1792x1024 => "1792x1024",
        ImageSize::Size1024x1792 => "1024x1792",
    }
}

/// Convert ImageQuality enum to string for CEL policies.
fn image_quality_to_string(quality: &api_types::ImageQuality) -> &'static str {
    use api_types::ImageQuality;
    match quality {
        ImageQuality::Standard => "standard",
        ImageQuality::Hd => "hd",
        ImageQuality::Low => "low",
        ImageQuality::Medium => "medium",
        ImageQuality::High => "high",
        ImageQuality::Auto => "auto",
    }
}

/// Convert Voice enum to string for CEL policies.
fn voice_to_string(voice: &api_types::Voice) -> &'static str {
    use api_types::Voice;
    match voice {
        Voice::Alloy => "alloy",
        Voice::Ash => "ash",
        Voice::Ballad => "ballad",
        Voice::Coral => "coral",
        Voice::Echo => "echo",
        Voice::Fable => "fable",
        Voice::Nova => "nova",
        Voice::Onyx => "onyx",
        Voice::Sage => "sage",
        Voice::Shimmer => "shimmer",
        Voice::Verse => "verse",
        Voice::Marin => "marin",
        Voice::Cedar => "cedar",
    }
}

/// Error response for API requests.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    /// Create a new API error
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = crate::openapi::ErrorResponse::new(self.code, self.message);
        (self.status, Json(body)).into_response()
    }
}

impl From<RoutingError> for ApiError {
    fn from(err: RoutingError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "routing_error", err.to_string())
    }
}

impl From<DbError> for ApiError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "not_found", "Resource not found")
            }
            DbError::Conflict(msg) => Self::new(StatusCode::CONFLICT, "conflict", msg),
            DbError::Validation(msg) => Self::new(StatusCode::BAD_REQUEST, "validation_error", msg),
            DbError::NotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "database_required",
                "Database not configured",
            ),
            _ => {
                tracing::error!(error = %err, "Database error");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "An internal database error occurred",
                )
            }
        }
    }
}

impl From<FilesServiceError> for ApiError {
    fn from(err: FilesServiceError) -> Self {
        match err {
            FilesServiceError::Database(db_err) => db_err.into(),
            FilesServiceError::Storage(storage_err) => {
                tracing::error!(error = %storage_err, "File storage error");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    "An internal storage error occurred",
                )
            }
            FilesServiceError::NotFound(id) => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("File '{}' not found", id),
            ),
        }
    }
}

/// Sort order for list queries.
///
/// OpenAI-compatible sort order parameter for paginated list endpoints.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    /// Ascending order (oldest first)
    Asc,
    /// Descending order (newest first)
    #[default]
    Desc,
}

impl From<SortOrder> for crate::db::repos::SortOrder {
    fn from(order: SortOrder) -> Self {
        match order {
            SortOrder::Asc => crate::db::repos::SortOrder::Asc,
            SortOrder::Desc => crate::db::repos::SortOrder::Desc,
        }
    }
}

/// Check if the authenticated request has access to a resource based on its owner.
///
/// This function enforces ownership-based access control for vector stores and files:
/// - User-owned resources: caller must be the owner user
/// - Organization-owned resources: caller must belong to the organization
/// - Project-owned resources: caller must belong to the project
///
/// Returns `Ok(())` if access is allowed, or an `ApiError` with status 403 if denied.
fn check_resource_access(
    auth: &AuthenticatedRequest,
    owner_type: VectorStoreOwnerType,
    owner_id: Uuid,
) -> Result<(), ApiError> {
    let allowed = match owner_type {
        VectorStoreOwnerType::User => auth.user_id() == Some(owner_id),
        VectorStoreOwnerType::Organization => {
            // Check identity org membership or API key org ownership
            auth.identity()
                .map(|i| i.org_ids.contains(&owner_id.to_string()))
                .unwrap_or(false)
                || auth
                    .api_key()
                    .and_then(|k| k.org_id)
                    .map(|id| id == owner_id)
                    .unwrap_or(false)
        }
        VectorStoreOwnerType::Team => {
            // Check identity team membership or API key team ownership
            auth.identity()
                .map(|i| i.team_ids.contains(&owner_id.to_string()))
                .unwrap_or(false)
                || auth
                    .api_key()
                    .and_then(|k| k.team_id)
                    .map(|id| id == owner_id)
                    .unwrap_or(false)
        }
        VectorStoreOwnerType::Project => {
            // Check identity project membership or API key project ownership
            auth.identity()
                .map(|i| i.project_ids.contains(&owner_id.to_string()))
                .unwrap_or(false)
                || auth
                    .api_key()
                    .and_then(|k| k.project_id)
                    .map(|id| id == owner_id)
                    .unwrap_or(false)
        }
    };

    if allowed {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "access_denied",
            "You do not have permission to access this resource",
        ))
    }
}

/// Check resource access with optional authentication.
/// When auth is None (e.g., auth.mode.type = "none"), access is allowed.
fn check_resource_access_optional(
    auth: Option<&AuthenticatedRequest>,
    owner_type: VectorStoreOwnerType,
    owner_id: Uuid,
) -> Result<(), ApiError> {
    match auth {
        Some(auth) => check_resource_access(auth, owner_type, owner_id),
        None => Ok(()), // No auth configured, allow access
    }
}

/// User's identity memberships: (user_id, org_ids, team_ids, project_ids)
type IdentityMemberships = (Option<Uuid>, Vec<Uuid>, Vec<Uuid>, Vec<Uuid>);

/// Extract identity memberships from authentication context.
///
/// Returns the user ID and lists of organization, team, and project IDs
/// that the authenticated user has access to. This is used to filter
/// resources like vector stores to only show those the user can access.
///
/// Returns an error if no authentication is provided (required for accessible listing).
fn extract_identity_memberships(
    auth: Option<&AuthenticatedRequest>,
) -> Result<IdentityMemberships, ApiError> {
    let auth = auth.ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Authentication is required to list accessible vector stores. Provide owner_type and owner_id to list specific collections without authentication.",
        )
    })?;

    let mut user_id: Option<Uuid> = None;
    let mut org_ids: Vec<Uuid> = Vec::new();
    let mut team_ids: Vec<Uuid> = Vec::new();
    let mut project_ids: Vec<Uuid> = Vec::new();

    // Extract from API key if present
    if let Some(api_key) = auth.api_key() {
        if let Some(uid) = api_key.user_id {
            user_id = Some(uid);
        }
        if let Some(org_id) = api_key.org_id {
            org_ids.push(org_id);
        }
        if let Some(team_id) = api_key.team_id {
            team_ids.push(team_id);
        }
        if let Some(project_id) = api_key.project_id {
            project_ids.push(project_id);
        }
    }

    // Extract from identity if present (OIDC claims)
    if let Some(identity) = auth.identity() {
        if let Some(uid) = identity.user_id {
            user_id = Some(uid);
        }
        // Parse string IDs to UUIDs
        for org_id_str in &identity.org_ids {
            if let Ok(org_id) = org_id_str.parse::<Uuid>()
                && !org_ids.contains(&org_id)
            {
                org_ids.push(org_id);
            }
        }
        for team_id_str in &identity.team_ids {
            if let Ok(team_id) = team_id_str.parse::<Uuid>()
                && !team_ids.contains(&team_id)
            {
                team_ids.push(team_id);
            }
        }
        for project_id_str in &identity.project_ids {
            if let Ok(project_id) = project_id_str.parse::<Uuid>()
                && !project_ids.contains(&project_id)
            {
                project_ids.push(project_id);
            }
        }
    }

    Ok((user_id, org_ids, team_ids, project_ids))
}

/// Validate that the vector store's embedding configuration matches the configured embedding service.
///
/// Collections are created with a specific embedding model and dimensions. When adding files,
/// the embeddings must be generated with the same model to ensure search quality. This function
/// validates that the gateway's configured embedding service matches the vector store's settings.
///
/// Returns an error if:
/// - File search service is not configured (no embedding service available)
/// - The embedding model doesn't match
/// - The embedding dimensions don't match
fn validate_embedding_model_compatibility(
    state: &AppState,
    vector_store: &VectorStore,
) -> Result<(), ApiError> {
    let file_search_service = state.file_search_service.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "embedding_service_unavailable",
            "File search service is not configured. Cannot process files for vector stores.",
        )
    })?;

    let embedding_service = file_search_service.embedding_service();
    let configured_model = embedding_service.model();
    let configured_dimensions = embedding_service.dimensions();

    // Check model compatibility
    if vector_store.embedding_model != configured_model {
        tracing::warn!(
            vector_store_id = %vector_store.id,
            collection_model = %vector_store.embedding_model,
            configured_model = %configured_model,
            "Embedding model mismatch: vector store was created with a different model"
        );
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "embedding_model_mismatch",
            format!(
                "Vector store '{}' uses embedding model '{}', but the gateway is configured with '{}'. \
                Files must be processed with the same embedding model used when the vector store was created. \
                Either reconfigure the gateway to use '{}' or create a new vector store with model '{}'.",
                vector_store.name,
                vector_store.embedding_model,
                configured_model,
                vector_store.embedding_model,
                configured_model
            ),
        ));
    }

    // Check dimensions compatibility
    if vector_store.embedding_dimensions != configured_dimensions as i32 {
        tracing::warn!(
            vector_store_id = %vector_store.id,
            collection_dimensions = vector_store.embedding_dimensions,
            configured_dimensions = configured_dimensions,
            "Embedding dimensions mismatch: vector store was created with different dimensions"
        );
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "embedding_dimensions_mismatch",
            format!(
                "Vector store '{}' uses {} embedding dimensions, but the gateway is configured with {}. \
                Files must be processed with the same embedding dimensions used when the vector store was created.",
                vector_store.name, vector_store.embedding_dimensions, configured_dimensions
            ),
        ));
    }

    Ok(())
}

// ============================================================================
// Guardrails Audit Logging Helpers
// ============================================================================

/// Logs a guardrails evaluation event to the audit log.
///
/// This function spawns a background task to log the event, ensuring
/// request latency is not impacted by audit logging.
#[allow(clippy::too_many_arguments)]
fn log_guardrails_evaluation(
    state: &AppState,
    auth: Option<&Extension<AuthenticatedRequest>>,
    provider: &str,
    stage: &str,
    result: &crate::guardrails::InputGuardrailsResult,
    request_id: Option<&str>,
    ip_address: Option<String>,
    user_agent: Option<String>,
) {
    // Get the audit config
    let Some(guardrails_config) = &state.config.features.guardrails else {
        return;
    };
    let audit_config = &guardrails_config.audit;

    // Check if we should log this evaluation
    if !audit_config.enabled {
        return;
    }

    // Only log if there are violations or if log_all_evaluations is true
    let has_violations = !result.response.violations.is_empty();
    if !has_violations && !audit_config.log_all_evaluations {
        return;
    }

    let Some(db) = &state.db else { return };

    // Determine what action was taken
    let (action_type, should_log) = match &result.action {
        crate::guardrails::ResolvedAction::Allow => ("allow", audit_config.log_all_evaluations),
        crate::guardrails::ResolvedAction::Block { .. } => ("block", audit_config.log_blocked),
        crate::guardrails::ResolvedAction::Warn { .. } => ("warn", audit_config.log_violations),
        crate::guardrails::ResolvedAction::Log { .. } => ("log", audit_config.log_violations),
        crate::guardrails::ResolvedAction::Redact { .. } => ("redact", audit_config.log_redacted),
    };

    if !should_log {
        return;
    }

    let db = db.clone();
    let api_key_id = auth.and_then(|a| a.0.api_key().map(|k| k.key.id));
    let org_id = auth.and_then(|a| a.0.api_key().and_then(|k| k.org_id));
    let project_id = auth.and_then(|a| a.0.api_key().and_then(|k| k.project_id));
    let provider = provider.to_string();
    let stage = stage.to_string();
    let request_id = request_id.map(String::from);
    let passed = result.response.passed;
    let latency_ms = result.response.latency_ms;
    let action_type = action_type.to_string();
    let violations: Vec<serde_json::Value> = result
        .response
        .violations
        .iter()
        .map(|v| {
            serde_json::json!({
                "category": v.category.to_string(),
                "severity": v.severity.to_string(),
                "confidence": v.confidence,
                "message": v.message,
            })
        })
        .collect();

    #[cfg(feature = "server")]
    state.task_tracker.spawn(async move {
        let result = db
            .audit_logs()
            .create(crate::models::CreateAuditLog {
                actor_type: api_key_id
                    .map(|_| crate::models::AuditActorType::ApiKey)
                    .unwrap_or(crate::models::AuditActorType::System),
                actor_id: api_key_id,
                action: format!("guardrails.{}", action_type),
                resource_type: "guardrails".to_string(),
                resource_id: api_key_id.unwrap_or(uuid::Uuid::nil()),
                org_id,
                project_id,
                details: serde_json::json!({
                    "provider": provider,
                    "stage": stage,
                    "passed": passed,
                    "latency_ms": latency_ms,
                    "action": action_type,
                    "violations": violations,
                    "request_id": request_id,
                }),
                ip_address,
                user_agent,
            })
            .await;

        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                provider = %provider,
                stage = %stage,
                "Failed to log guardrails audit event"
            );
        }
    });
}

/// Logs an output guardrails evaluation event to the audit log.
fn log_output_guardrails_evaluation(
    state: &AppState,
    auth: Option<&Extension<AuthenticatedRequest>>,
    provider: &str,
    result: &crate::guardrails::OutputGuardrailsResult,
    request_id: Option<&str>,
    ip_address: Option<String>,
    user_agent: Option<String>,
) {
    // Get the audit config
    let Some(guardrails_config) = &state.config.features.guardrails else {
        return;
    };
    let audit_config = &guardrails_config.audit;

    // Check if we should log this evaluation
    if !audit_config.enabled {
        return;
    }

    // Only log if there are violations or if log_all_evaluations is true
    let has_violations = !result.response.violations.is_empty();
    if !has_violations && !audit_config.log_all_evaluations {
        return;
    }

    let Some(db) = &state.db else { return };

    // Determine what action was taken
    let (action_type, should_log) = match &result.action {
        crate::guardrails::ResolvedAction::Allow => ("allow", audit_config.log_all_evaluations),
        crate::guardrails::ResolvedAction::Block { .. } => ("block", audit_config.log_blocked),
        crate::guardrails::ResolvedAction::Warn { .. } => ("warn", audit_config.log_violations),
        crate::guardrails::ResolvedAction::Log { .. } => ("log", audit_config.log_violations),
        crate::guardrails::ResolvedAction::Redact { .. } => ("redact", audit_config.log_redacted),
    };

    if !should_log {
        return;
    }

    let db = db.clone();
    let api_key_id = auth.and_then(|a| a.0.api_key().map(|k| k.key.id));
    let org_id = auth.and_then(|a| a.0.api_key().and_then(|k| k.org_id));
    let project_id = auth.and_then(|a| a.0.api_key().and_then(|k| k.project_id));
    let provider = provider.to_string();
    let request_id = request_id.map(String::from);
    let passed = result.response.passed;
    let latency_ms = result.response.latency_ms;
    let action_type = action_type.to_string();

    // For redacted content, include hashes instead of actual content
    let content_hashes = if let crate::guardrails::ResolvedAction::Redact {
        original_content,
        modified_content,
        ..
    } = &result.action
    {
        Some(serde_json::json!({
            "original_content_hash": crate::guardrails::audit::hash_content(original_content),
            "redacted_content_hash": crate::guardrails::audit::hash_content(modified_content),
        }))
    } else {
        None
    };

    let violations: Vec<serde_json::Value> = result
        .response
        .violations
        .iter()
        .map(|v| {
            serde_json::json!({
                "category": v.category.to_string(),
                "severity": v.severity.to_string(),
                "confidence": v.confidence,
                "message": v.message,
            })
        })
        .collect();

    #[cfg(feature = "server")]
    state.task_tracker.spawn(async move {
        let mut details = serde_json::json!({
            "provider": provider,
            "stage": "output",
            "passed": passed,
            "latency_ms": latency_ms,
            "action": action_type,
            "violations": violations,
            "request_id": request_id,
        });

        // Add content hashes if this was a redaction
        if let Some(hashes) = content_hashes
            && let Some(obj) = details.as_object_mut()
        {
            obj.insert("content_hashes".to_string(), hashes);
        }

        let result = db
            .audit_logs()
            .create(crate::models::CreateAuditLog {
                actor_type: api_key_id
                    .map(|_| crate::models::AuditActorType::ApiKey)
                    .unwrap_or(crate::models::AuditActorType::System),
                actor_id: api_key_id,
                action: format!("guardrails.{}", action_type),
                resource_type: "guardrails".to_string(),
                resource_id: api_key_id.unwrap_or(uuid::Uuid::nil()),
                org_id,
                project_id,
                details,
                ip_address,
                user_agent,
            })
            .await;

        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                provider = %provider,
                "Failed to log output guardrails audit event"
            );
        }
    });
}

// ============================================================================
// Files API (OpenAI-compatible)
// ============================================================================

/// Get services from app state for Files/Vector Stores APIs
fn get_services(state: &AppState) -> Result<&Services, ApiError> {
    state.services.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "feature_not_available",
            "This endpoint requires database support. Rebuild with --features database-sqlite or --features database-postgres.",
        )
    })
}

/// Per-route body size limits (audio uploads, file uploads).
///
/// Pulled from `[server]` config and threaded through router composition so
/// individual routes can opt into a higher cap than the global
/// `RequestBodyLimitLayer` would otherwise impose.
#[cfg(any(feature = "server", feature = "wasm"))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ApiBodyLimits {
    pub audio: usize,
    pub files: usize,
    pub skills: usize,
}

#[cfg(any(feature = "server", feature = "wasm"))]
impl Default for ApiBodyLimits {
    fn default() -> Self {
        // Generous WASM-side defaults; the server overrides from config.
        Self {
            audio: 100 * 1024 * 1024,
            files: 512 * 1024 * 1024,
            skills: 64 * 1024 * 1024,
        }
    }
}

/// Route definitions for the OpenAI-compatible API.
///
/// Shared between server and WASM builds. The server wraps these with auth/rate-limit
/// middleware in [`get_api_routes`]; the WASM build uses them directly.
#[cfg(any(feature = "server", feature = "wasm"))]
pub(crate) fn api_v1_routes(limits: ApiBodyLimits) -> Router<AppState> {
    use axum::extract::DefaultBodyLimit;
    let router = Router::new()
        .route("/v1/chat/completions", post(api_v1_chat_completions))
        .route("/v1/responses", post(api_v1_responses))
        .route("/v1/completions", post(api_v1_completions))
        .route("/v1/embeddings", post(api_v1_embeddings))
        .route("/v1/models", get(api_v1_models))
        // Images API (OpenAI-compatible)
        .route("/v1/images/generations", post(api_v1_images_generations))
        // Tools API (Hadrian extension)
        .route("/v1/tools/web-search", post(web_search))
        .route("/v1/tools/web-fetch", post(web_fetch));
    // Responses persistence + containers endpoints depend on the DB-backed
    // ResponsesStore / ContainersService, which are server-only (no WASM).
    #[cfg(feature = "server")]
    let router = router
        .route("/v1/responses/compact", post(api_v1_responses_compact))
        .route(
            "/v1/responses/{response_id}",
            get(responses_lookup::api_v1_responses_get)
                .delete(responses_lookup::api_v1_responses_delete),
        )
        .route(
            "/v1/responses/{response_id}/cancel",
            post(responses_lookup::api_v1_responses_cancel),
        )
        .route(
            "/v1/containers",
            post(containers::api_v1_containers_create).get(containers::api_v1_containers_list),
        )
        .route(
            "/v1/containers/{container_id}",
            get(containers::api_v1_containers_get).delete(containers::api_v1_containers_delete),
        )
        .route(
            "/v1/containers/{container_id}/files",
            get(containers::api_v1_containers_list_files)
                .post(containers::api_v1_containers_file_upload),
        )
        .route(
            "/v1/containers/{container_id}/files/{file_id}",
            get(containers::api_v1_containers_file_get)
                .delete(containers::api_v1_containers_file_delete),
        )
        .route(
            "/v1/containers/{container_id}/files/{file_id}/content",
            get(containers::api_v1_containers_file_content),
        )
        .route("/v1/images/edits", post(api_v1_images_edits))
        .route("/v1/images/variations", post(api_v1_images_variations));
    let router = router
        // Audio API (OpenAI-compatible). speech is text-only (small payload), so
        // it stays on the global limit; transcription/translation receive raw
        // audio uploads and get the larger per-route cap below.
        .route("/v1/audio/speech", post(api_v1_audio_speech));
    #[cfg(feature = "server")]
    let router = router
        .route(
            "/v1/audio/transcriptions",
            post(api_v1_audio_transcriptions).layer(DefaultBodyLimit::max(limits.audio)),
        )
        .route(
            "/v1/audio/translations",
            post(api_v1_audio_translations).layer(DefaultBodyLimit::max(limits.audio)),
        );
    // Files API (OpenAI-compatible). Uploads need the largest cap; list/get
    // are unaffected.
    #[cfg(feature = "server")]
    let router = router.route(
        "/v1/files",
        post(api_v1_files_upload)
            .layer(DefaultBodyLimit::max(limits.files))
            .merge(get(api_v1_files_list)),
    );
    #[cfg(not(feature = "server"))]
    let router = router.route("/v1/files", get(api_v1_files_list));
    // Skills API (OpenAI-compatible). Server-only: uploads parse multipart/zip
    // and downloads emit zip. Create/version-create get the larger body cap.
    #[cfg(feature = "server")]
    let router = router
        .route(
            "/v1/skills",
            post(skills::api_v1_skills_create)
                .layer(DefaultBodyLimit::max(limits.skills))
                .merge(get(skills::api_v1_skills_list)),
        )
        .route(
            "/v1/skills/{skill_id}",
            get(skills::api_v1_skills_get)
                .merge(post(skills::api_v1_skills_set_default))
                .merge(delete(skills::api_v1_skills_delete)),
        )
        .route(
            "/v1/skills/{skill_id}/content",
            get(skills::api_v1_skills_get_content),
        )
        .route(
            "/v1/skills/{skill_id}/versions",
            post(skills::api_v1_skills_create_version)
                .layer(DefaultBodyLimit::max(limits.skills))
                .merge(get(skills::api_v1_skills_list_versions)),
        )
        .route(
            "/v1/skills/{skill_id}/versions/{version}",
            get(skills::api_v1_skills_get_version)
                .merge(delete(skills::api_v1_skills_delete_version)),
        )
        .route(
            "/v1/skills/{skill_id}/versions/{version}/content",
            get(skills::api_v1_skills_get_version_content),
        );
    router
        .route(
            "/v1/files/{file_id}",
            get(api_v1_files_get).merge(delete(api_v1_files_delete)),
        )
        .route("/v1/files/{file_id}/content", get(api_v1_files_get_content))
        // Vector Stores API (OpenAI-compatible)
        .route(
            "/v1/vector_stores",
            post(api_v1_vector_stores_create).merge(get(api_v1_vector_stores_list)),
        )
        .route(
            "/v1/vector_stores/{vector_store_id}",
            get(api_v1_vector_stores_get)
                .merge(post(api_v1_vector_stores_modify))
                .merge(delete(api_v1_vector_stores_delete)),
        )
        .route(
            "/v1/vector_stores/{vector_store_id}/files",
            post(api_v1_vector_stores_create_file).merge(get(api_v1_vector_stores_list_files)),
        )
        .route(
            "/v1/vector_stores/{vector_store_id}/files/{file_id}",
            get(api_v1_vector_stores_get_file).merge(delete(api_v1_vector_stores_delete_file)),
        )
        // Hadrian extension: chunk inspection (not in OpenAI API)
        .route(
            "/v1/vector_stores/{vector_store_id}/files/{file_id}/chunks",
            get(api_v1_vector_stores_list_file_chunks),
        )
        // Search endpoint (OpenAI-compatible, but schema has Hadrian extensions)
        .route(
            "/v1/vector_stores/{vector_store_id}/search",
            post(api_v1_vector_stores_search),
        )
        // File batches
        .route(
            "/v1/vector_stores/{vector_store_id}/file_batches",
            post(api_v1_vector_stores_create_file_batch),
        )
        .route(
            "/v1/vector_stores/{vector_store_id}/file_batches/{batch_id}",
            get(api_v1_vector_stores_get_file_batch)
                .merge(delete(api_v1_vector_stores_cancel_file_batch)),
        )
        .route(
            "/v1/vector_stores/{vector_store_id}/file_batches/{batch_id}/files",
            get(api_v1_vector_stores_list_batch_files),
        )
}

/// Server-only: wraps [`api_v1_routes`] with auth, rate-limit, and authz middleware.
#[cfg(feature = "server")]
pub fn get_api_routes(state: AppState) -> Router<AppState> {
    let limits = ApiBodyLimits {
        audio: state.config.server.audio_body_limit_bytes,
        files: state.config.server.files_body_limit_bytes,
        skills: state.config.server.skills_body_limit_bytes,
    };
    api_v1_routes(limits)
        // Apply middleware layers in order (ServiceBuilder runs top-to-bottom):
        // 1. Rate limiting - reject requests early before auth overhead
        // 2. Auth, budget, usage - authenticates and sets AuthenticatedRequest
        // 3. Authorization - policy checks (needs AuthenticatedRequest from step 2)
        .route_layer(
            ServiceBuilder::new()
                .layer(from_fn_with_state(
                    state.clone(),
                    crate::middleware::rate_limit_middleware,
                ))
                .layer(from_fn_with_state(
                    state.clone(),
                    crate::middleware::api_middleware,
                ))
                .layer(from_fn_with_state(
                    state,
                    crate::middleware::api_authz_middleware,
                )),
        )
}

#[cfg(all(test, feature = "database-sqlite"))]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    // ============================================================================
    // Test Infrastructure
    // ============================================================================

    /// Create a test application with an in-memory database and test provider
    async fn test_app() -> axum::Router {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

        #[cfg(feature = "sso")]
        let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
        #[cfg(not(feature = "sso"))]
        let session_section = "";

        let config_str = format!(
            r#"
[database]
type = "sqlite"
path = "file:api_test_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "test"

[providers.test]
type = "test"
model_name = "test-model"

[providers.secondary-test]
type = "test"
model_name = "secondary-model"
"#
        );

        let config =
            crate::config::GatewayConfig::parse(&config_str).expect("Failed to parse test config");
        let state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");
        crate::build_app(&config, state)
    }

    /// Helper to make a JSON POST request
    async fn post_json(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        post_json_with_headers(app, uri, body, vec![]).await
    }

    /// Helper to make a JSON POST request with custom headers
    async fn post_json_with_headers(
        app: &axum::Router,
        uri: &str,
        body: Value,
        headers: Vec<(&str, &str)>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");

        for (key, value) in headers {
            builder = builder.header(key, value);
        }

        let request = builder
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    /// Helper to make a JSON POST request and return raw body (for streaming)
    async fn post_json_raw(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, String) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    /// Helper to make a GET request
    async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    /// Helper to make a GET request and return raw bytes with headers
    async fn get_raw(
        app: &axum::Router,
        uri: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, body.to_vec())
    }

    /// Helper to make a DELETE request
    async fn delete_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    // ============================================================================
    // Chat Completions - Deep Response Validation
    // ============================================================================

    #[tokio::test]
    async fn test_chat_completions_response_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        // Validate response structure thoroughly
        assert_eq!(body["object"], "chat.completion");
        assert!(body["id"].as_str().unwrap().starts_with("test-"));
        assert!(body["created"].is_number());

        // Validate choices array
        let choices = body["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);

        let choice = &choices[0];
        assert_eq!(choice["index"], 0);
        assert_eq!(choice["finish_reason"], "stop");

        // Validate message content matches test provider output
        let message = &choice["message"];
        assert_eq!(message["role"], "assistant");
        assert_eq!(
            message["content"],
            "This is a test response from the test provider."
        );

        // Validate usage statistics
        let usage = &body["usage"];
        assert_eq!(usage["prompt_tokens"], 10);
        assert_eq!(usage["completion_tokens"], 10);
        assert_eq!(usage["total_tokens"], 20);
    }

    #[tokio::test]
    async fn test_chat_completions_streaming_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}],
                "stream": true
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        // Validate SSE format
        assert!(body.starts_with("data:"), "Should start with 'data:'");
        assert!(body.ends_with("[DONE]\n\n"), "Should end with [DONE]");

        // Parse and validate individual chunks
        let chunks: Vec<&str> = body.split("data: ").filter(|s| !s.is_empty()).collect();
        assert!(chunks.len() >= 3, "Should have at least 3 chunks");

        // First chunk should have role
        let first_chunk: Value = serde_json::from_str(chunks[0].trim()).unwrap();
        assert_eq!(first_chunk["object"], "chat.completion.chunk");
        assert_eq!(first_chunk["choices"][0]["delta"]["role"], "assistant");

        // Second chunk should have content
        let second_chunk: Value = serde_json::from_str(chunks[1].trim()).unwrap();
        assert_eq!(
            second_chunk["choices"][0]["delta"]["content"],
            "This is a test response from the test provider."
        );

        // Third chunk should have finish_reason and usage
        let third_chunk: Value = serde_json::from_str(chunks[2].trim()).unwrap();
        assert_eq!(third_chunk["choices"][0]["finish_reason"], "stop");
        assert_eq!(third_chunk["usage"]["total_tokens"], 20);
    }

    #[tokio::test]
    async fn test_chat_completions_model_passthrough() {
        let app = test_app().await;

        // The model name should be passed through to the response
        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/custom-model-name",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        // Test provider uses the model name from the payload
        assert_eq!(body["model"], "custom-model-name");
    }

    #[tokio::test]
    async fn test_chat_completions_default_provider() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "any-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
        // Model should be the unprefixed model name
        assert_eq!(body["model"], "any-model");
    }

    #[tokio::test]
    async fn test_chat_completions_specific_provider() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "secondary-test/my-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["model"], "my-model");
    }

    // ============================================================================
    // Chat Completions - Error Cases
    // ============================================================================

    #[tokio::test]
    async fn test_chat_completions_missing_model_error() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["code"].is_string());
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("No model")
        );
    }

    #[tokio::test]
    async fn test_chat_completions_unknown_provider_error() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "nonexistent-provider/model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("not found"),
            "Error should mention provider not found: {}",
            message
        );
    }

    #[tokio::test]
    async fn test_chat_completions_missing_messages_validation() {
        let app = test_app().await;

        let (status, _body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model"
            }),
        )
        .await;

        // Missing messages field should fail validation (422)
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_chat_completions_empty_messages_array() {
        let app = test_app().await;

        let (status, _body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": []
            }),
        )
        .await;

        // Empty messages array fails validation (400 Bad Request)
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ============================================================================
    // Chat Completions - Edge Cases
    // ============================================================================

    #[tokio::test]
    async fn test_chat_completions_unicode_content() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [
                    {"role": "user", "content": "Hello 你好 مرحبا 🎉 émojis and ümläuts"}
                ]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_chat_completions_very_long_content() {
        let app = test_app().await;

        // Create a message with 10KB of content
        let long_content = "x".repeat(10 * 1024);

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": long_content}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_chat_completions_special_characters() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [
                    {"role": "user", "content": "Test with \"quotes\", 'apostrophes', \n newlines, \t tabs, and \\backslashes\\"}
                ]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_chat_completions_multiple_messages() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [
                    {"role": "system", "content": "You are a helpful assistant"},
                    {"role": "user", "content": "First message"},
                    {"role": "assistant", "content": "First response"},
                    {"role": "user", "content": "Second message"}
                ]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_chat_completions_with_optional_parameters() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}],
                "temperature": 0.7,
                "max_tokens": 100,
                "top_p": 0.9,
                "frequency_penalty": 0.5,
                "presence_penalty": 0.5,
                "stop": ["\n"],
                "user": "test-user-123"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    // ============================================================================
    // Responses API - Deep Validation
    // ============================================================================

    #[tokio::test]
    async fn test_responses_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/responses",
            json!({
                "model": "test/test-model",
                "input": "Hello"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "response");
        // The request is stored (store defaults to true), so Hadrian — the
        // system of record — returns its own persisted `resp_…` id (what GET and
        // `previous_response_id` chaining resolve against), not the upstream
        // test provider's `test-…` id.
        let id = body["id"].as_str().unwrap();
        assert!(id.starts_with("resp_"), "unexpected response id: {id}");
        assert_eq!(body["status"], "completed");

        // Validate output structure
        let output = body["output"].as_array().unwrap();
        assert!(!output.is_empty());

        let first_output = &output[0];
        assert_eq!(first_output["type"], "message");
        assert_eq!(first_output["role"], "assistant");

        // Validate usage
        let usage = &body["usage"];
        assert_eq!(usage["input_tokens"], 10);
        assert_eq!(usage["output_tokens"], 10);
        assert_eq!(usage["total_tokens"], 20);
    }

    #[tokio::test]
    async fn test_responses_streaming_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/responses",
            json!({
                "model": "test/test-model",
                "input": "Hello",
                "stream": true
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("response.created"));
        assert!(body.contains("response.completed"));
        assert!(body.contains("This is a test response"));
    }

    #[tokio::test]
    async fn test_responses_with_models_array() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/responses",
            json!({
                "models": ["test/test-model"],
                "input": "Hello"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "response");
    }

    // ============================================================================
    // Completions API - Deep Validation
    // ============================================================================

    #[tokio::test]
    async fn test_completions_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/completions",
            json!({
                "model": "test/test-model",
                "prompt": "Once upon a time"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "text_completion");

        // Validate choices
        let choices = body["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["index"], 0);
        assert_eq!(choices[0]["finish_reason"], "stop");
        assert!(
            choices[0]["text"]
                .as_str()
                .unwrap()
                .contains("test completion")
        );

        // Validate usage
        assert_eq!(body["usage"]["prompt_tokens"], 5);
        assert_eq!(body["usage"]["completion_tokens"], 10);
        assert_eq!(body["usage"]["total_tokens"], 15);
    }

    #[tokio::test]
    async fn test_completions_streaming_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/completions",
            json!({
                "model": "test/test-model",
                "prompt": "Once upon a time",
                "stream": true
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("text_completion"));
        assert!(body.contains("test completion"));
        assert!(body.contains("[DONE]"));
    }

    // ============================================================================
    // Embeddings API - Deep Validation
    // ============================================================================

    #[tokio::test]
    async fn test_embeddings_content_validation() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/embeddings",
            json!({
                "model": "test/test-model",
                "input": "Hello world"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);

        let embedding_obj = &data[0];
        assert_eq!(embedding_obj["object"], "embedding");
        assert_eq!(embedding_obj["index"], 0);

        // Validate embedding vector
        let embedding = embedding_obj["embedding"].as_array().unwrap();
        assert_eq!(embedding.len(), 1536, "Should have 1536 dimensions");

        // Validate usage
        assert_eq!(body["usage"]["prompt_tokens"], 8);
        assert_eq!(body["usage"]["total_tokens"], 8);
    }

    #[tokio::test]
    async fn test_embeddings_array_input() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/embeddings",
            json!({
                "model": "test/test-model",
                "input": ["Hello", "World"]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
    }

    #[tokio::test]
    async fn test_embeddings_missing_input_error() {
        let app = test_app().await;

        let (status, _body) = post_json(
            &app,
            "/api/v1/embeddings",
            json!({
                "model": "test/test-model"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ============================================================================
    // Models API - Deep Validation
    // ============================================================================

    #[tokio::test]
    async fn test_list_models_content_validation() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/api/v1/models").await;

        assert_eq!(status, StatusCode::OK);
        let models = body["data"].as_array().unwrap();

        // Should have 4 models total (2 per test provider)
        assert_eq!(models.len(), 4);

        // Validate model structure
        for model in models {
            let id = model["id"].as_str().unwrap();
            assert!(id.contains('/'), "Model ID should be provider-prefixed");
            assert!(model["object"].is_string() || model["object"].is_null());
        }

        // Check for specific provider prefixes
        let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert!(ids.iter().any(|id| id.starts_with("test/")));
        assert!(ids.iter().any(|id| id.starts_with("secondary-test/")));
    }

    // ============================================================================
    // Dynamic Provider Routing Tests
    // ============================================================================

    #[tokio::test]
    async fn test_dynamic_provider_org_scope_not_found() {
        let app = test_app().await;

        // Try to use a dynamic provider that doesn't exist
        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": ":org/nonexistent-org/my-provider/gpt-4",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        // Should fail because org doesn't exist
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("not found") || message.contains("Organization"),
            "Should indicate org/provider not found: {}",
            message
        );
    }

    #[tokio::test]
    async fn test_dynamic_provider_invalid_scope_format() {
        let app = test_app().await;

        // Invalid scope format - missing components
        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": ":org/incomplete",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("Missing") || message.contains("component"),
            "Should indicate missing components: {}",
            message
        );
    }

    // ============================================================================
    // Authenticated Request Tests
    // ============================================================================
    //
    // Note: The API middleware allows anonymous requests by default - auth is optional.
    // These tests verify that authenticated requests work correctly, not that auth is enforced.

    #[tokio::test]
    async fn test_chat_completions_with_valid_api_key() {
        let app = test_app().await;

        // First create an org
        let (status, org) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({
                "slug": "test-org-auth",
                "name": "Test Org for Auth"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let org_id = org["id"].as_str().unwrap();

        // Create an API key for the org (correct format from admin tests)
        let (status, api_key_response) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({
                "name": "test-key",
                "owner": {"type": "organization", "org_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let api_key = api_key_response["key"].as_str().unwrap();

        // Make authenticated request using Authorization header
        let (status, body) = post_json_with_headers(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            vec![("Authorization", &format!("Bearer {}", api_key))],
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_chat_completions_with_x_api_key_header() {
        let app = test_app().await;

        // Create org and API key
        let (_, org) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "test-org-x-api", "name": "Test"}),
        )
        .await;
        let org_id = org["id"].as_str().unwrap();

        let (_, api_key_response) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "x-api-key-test", "owner": {"type": "organization", "org_id": org_id}}),
        )
        .await;
        let api_key = api_key_response["key"].as_str().unwrap();

        // Make request using X-API-Key header
        let (status, body) = post_json_with_headers(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            vec![("X-API-Key", api_key)],
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_request_with_invalid_api_key_format() {
        let app = test_app().await;

        // Providing an invalid API key returns 401 — the gateway does not
        // fall through to anonymous access when credentials are present but invalid
        let (status, body) = post_json_with_headers(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            vec![(
                "Authorization",
                "Bearer malformed-key-without-proper-prefix",
            )],
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["type"], "authentication_error");
    }

    #[tokio::test]
    async fn test_anonymous_request_allowed_by_default() {
        let app = test_app().await;

        // Request without any auth headers
        let (status, body) = post_json(
            &app,
            "/api/v1/chat/completions",
            json!({
                "model": "test/test-model",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
        )
        .await;

        // Anonymous requests are allowed by default
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
    }

    // ============================================================================
    // Error Handling Tests
    // ============================================================================

    #[tokio::test]
    async fn test_invalid_json_body() {
        let app = test_app().await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from("not valid json"))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert!(
            response.status() == StatusCode::BAD_REQUEST
                || response.status() == StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn test_empty_body() {
        let app = test_app().await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert!(
            response.status() == StatusCode::BAD_REQUEST
                || response.status() == StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn test_wrong_content_type() {
        let app = test_app().await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/chat/completions")
            .header("content-type", "text/plain")
            .body(Body::from(
                r#"{"model": "test/test-model", "messages": []}"#,
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        // Should fail due to wrong content type
        assert!(
            response.status() == StatusCode::BAD_REQUEST
                || response.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE
                || response.status() == StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn test_method_not_allowed() {
        let app = test_app().await;

        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/chat/completions")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ============================================================================
    // Unit Tests for ApiError
    // ============================================================================

    #[test]
    fn test_api_error_new() {
        use super::ApiError;

        let error = ApiError::new(StatusCode::BAD_REQUEST, "test_code", "Test message");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "test_code");
        assert_eq!(error.message, "Test message");
    }

    #[test]
    fn test_api_error_into_response() {
        use axum::response::IntoResponse;

        use super::ApiError;

        let error = ApiError::new(StatusCode::NOT_FOUND, "not_found", "Resource not found");
        let response = error.into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_routing_error_to_api_error() {
        use super::ApiError;
        use crate::routing::RoutingError;

        let test_cases = vec![
            (RoutingError::NoModel, "No model specified"),
            (
                RoutingError::ProviderNotFound("test".to_string()),
                "not found",
            ),
            (RoutingError::NoDefaultProvider, "No default provider"),
        ];

        for (routing_error, expected_msg_part) in test_cases {
            let api_error: ApiError = routing_error.into();
            assert_eq!(api_error.status, StatusCode::BAD_REQUEST);
            assert_eq!(api_error.code, "routing_error");
            assert!(
                api_error.message.contains(expected_msg_part),
                "Expected '{}' to contain '{}'",
                api_error.message,
                expected_msg_part
            );
        }
    }

    #[test]
    fn test_provider_error_to_api_error() {
        use crate::{providers::ProviderError, routes::execution::provider_error_to_api_error};

        let internal_error = ProviderError::Internal("test error".to_string());
        let api_error = provider_error_to_api_error(internal_error);
        assert_eq!(api_error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(api_error.code, "internal_error");
    }

    // ============================================================================
    // Unit Tests for check_resource_access
    // ============================================================================

    /// Helper to create an AuthenticatedRequest from an Identity for testing
    fn test_auth_from_identity(
        user_id: Option<uuid::Uuid>,
        org_ids: Vec<String>,
        project_ids: Vec<String>,
    ) -> crate::auth::AuthenticatedRequest {
        use crate::auth::{AuthenticatedRequest, Identity, IdentityKind};

        let identity = Identity {
            external_id: "test-external-id".to_string(),
            email: None,
            name: None,
            user_id,
            roles: vec![],
            idp_groups: vec![],
            org_ids,
            team_ids: vec![],
            project_ids,
        };
        AuthenticatedRequest::new(IdentityKind::Identity(identity))
    }

    #[test]
    fn test_check_resource_access_user_owner_allowed() {
        use super::VectorStoreOwnerType;

        let user_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(Some(user_id), vec![], vec![]);

        let result = super::check_resource_access(&auth, VectorStoreOwnerType::User, user_id);
        assert!(
            result.is_ok(),
            "User should have access to their own resources"
        );
    }

    #[test]
    fn test_check_resource_access_user_owner_denied() {
        use super::VectorStoreOwnerType;

        let user_a_id = uuid::Uuid::new_v4();
        let user_b_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(Some(user_a_id), vec![], vec![]);

        let result = super::check_resource_access(&auth, VectorStoreOwnerType::User, user_b_id);
        assert!(
            result.is_err(),
            "User A should NOT have access to User B's resources"
        );

        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.code, "access_denied");
    }

    #[test]
    fn test_check_resource_access_org_member_allowed() {
        use super::VectorStoreOwnerType;

        let org_id = uuid::Uuid::new_v4();
        let auth =
            test_auth_from_identity(Some(uuid::Uuid::new_v4()), vec![org_id.to_string()], vec![]);

        let result =
            super::check_resource_access(&auth, VectorStoreOwnerType::Organization, org_id);
        assert!(
            result.is_ok(),
            "Org member should have access to org resources"
        );
    }

    #[test]
    fn test_check_resource_access_org_nonmember_denied() {
        use super::VectorStoreOwnerType;

        let org_a_id = uuid::Uuid::new_v4();
        let org_b_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(
            Some(uuid::Uuid::new_v4()),
            vec![org_a_id.to_string()],
            vec![],
        );

        let result =
            super::check_resource_access(&auth, VectorStoreOwnerType::Organization, org_b_id);
        assert!(
            result.is_err(),
            "Non-member should NOT have access to org resources"
        );

        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_check_resource_access_project_member_allowed() {
        use super::VectorStoreOwnerType;

        let project_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(
            Some(uuid::Uuid::new_v4()),
            vec![],
            vec![project_id.to_string()],
        );

        let result = super::check_resource_access(&auth, VectorStoreOwnerType::Project, project_id);
        assert!(
            result.is_ok(),
            "Project member should have access to project resources"
        );
    }

    #[test]
    fn test_check_resource_access_project_nonmember_denied() {
        use super::VectorStoreOwnerType;

        let project_a_id = uuid::Uuid::new_v4();
        let project_b_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(
            Some(uuid::Uuid::new_v4()),
            vec![],
            vec![project_a_id.to_string()],
        );

        let result =
            super::check_resource_access(&auth, VectorStoreOwnerType::Project, project_b_id);
        assert!(
            result.is_err(),
            "Non-member should NOT have access to project resources"
        );

        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_check_resource_access_optional_allows_when_no_auth() {
        use super::VectorStoreOwnerType;

        let owner_id = uuid::Uuid::new_v4();

        // When auth is None (no authentication configured), access should be allowed
        let result =
            super::check_resource_access_optional(None, VectorStoreOwnerType::User, owner_id);
        assert!(result.is_ok(), "Should allow access when auth is disabled");
    }

    #[test]
    fn test_check_resource_access_optional_delegates_when_auth_present() {
        use super::VectorStoreOwnerType;

        let user_a_id = uuid::Uuid::new_v4();
        let user_b_id = uuid::Uuid::new_v4();
        let auth = test_auth_from_identity(Some(user_a_id), vec![], vec![]);

        // Should deny when user tries to access another user's resource
        let result = super::check_resource_access_optional(
            Some(&auth),
            VectorStoreOwnerType::User,
            user_b_id,
        );
        assert!(
            result.is_err(),
            "Should deny access to another user's resources"
        );
    }
    fn create_file_upload_multipart(
        file_content: &[u8],
        filename: &str,
        owner_type: &str,
        owner_id: &str,
        purpose: Option<&str>,
    ) -> (String, Vec<u8>) {
        let boundary = "----FileUploadBoundary12345";
        let mut body = Vec::new();

        // File field
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                filename
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(file_content);
        body.extend_from_slice(b"\r\n");

        // owner_type field
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_type\"\r\n\r\n");
        body.extend_from_slice(owner_type.as_bytes());
        body.extend_from_slice(b"\r\n");

        // owner_id field
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_id\"\r\n\r\n");
        body.extend_from_slice(owner_id.as_bytes());
        body.extend_from_slice(b"\r\n");

        // Optional purpose field
        if let Some(p) = purpose {
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(b"Content-Disposition: form-data; name=\"purpose\"\r\n\r\n");
            body.extend_from_slice(p.as_bytes());
            body.extend_from_slice(b"\r\n");
        }

        // End boundary
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let content_type = format!("multipart/form-data; boundary={}", boundary);
        (content_type, body)
    }

    /// Helper to create an organization and return its ID (for file upload tests)
    async fn create_org_for_files(app: &axum::Router, slug: &str) -> String {
        let (status, org) = post_json(
            app,
            "/admin/v1/organizations",
            json!({"slug": slug, "name": format!("Org {}", slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        org["id"].as_str().unwrap().to_string()
    }

    /// Helper to create a user and return its ID (for file upload tests)
    async fn create_user_for_files(app: &axum::Router, external_id: &str) -> String {
        let (status, user) = post_json(
            app,
            "/admin/v1/users",
            json!({
                "external_id": external_id,
                "email": format!("{}@example.com", external_id),
                "name": format!("Test User {}", external_id)
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        user["id"].as_str().unwrap().to_string()
    }

    /// Helper to create a team and return its ID (for file upload tests)
    async fn create_team_for_files(app: &axum::Router, org_slug: &str, slug: &str) -> String {
        let (status, team) = post_json(
            app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({
                "slug": slug,
                "name": format!("Team {}", slug)
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        team["id"].as_str().unwrap().to_string()
    }

    /// Helper to create a project and return its ID (for file upload tests)
    async fn create_project_for_files(app: &axum::Router, org_slug: &str, slug: &str) -> String {
        let (status, project) = post_json(
            app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({
                "slug": slug,
                "name": format!("Project {}", slug)
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        project["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_file_upload_basic() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-upload-basic-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Hello, this is test file content.",
            "test-document.txt",
            "user",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "file");
        assert!(json["id"].as_str().unwrap().starts_with("file-"));
        assert_eq!(json["filename"], "test-document.txt");
        assert_eq!(json["purpose"], "assistants"); // Default purpose
        assert_eq!(json["bytes"], 33); // Length of test content
        assert!(json["created_at"].is_string()); // DateTime<Utc> serializes as ISO 8601 string
    }

    #[tokio::test]
    async fn test_file_upload_with_purpose_batch() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-batch-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Batch file content",
            "batch-input.jsonl",
            "user",
            &owner_id,
            Some("batch"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["purpose"], "batch");
    }

    #[tokio::test]
    async fn test_file_upload_with_purpose_fine_tune() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-finetune-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Fine-tuning training data",
            "training-data.jsonl",
            "user",
            &owner_id,
            Some("fine-tune"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        // Note: FilePurpose::FineTune serializes as "fine_tune" due to serde rename_all = "snake_case"
        assert_eq!(json["purpose"], "fine_tune");
    }

    #[tokio::test]
    async fn test_file_upload_with_purpose_vision() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-vision-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"\x89PNG\r\n\x1a\nimage data here",
            "image.png",
            "user",
            &owner_id,
            Some("vision"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["purpose"], "vision");
    }

    #[tokio::test]
    async fn test_file_upload_owner_type_organization() {
        let app = test_app().await;
        let owner_id = create_org_for_files(&app, "file-org-owner").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Organization file",
            "org-doc.pdf",
            "organization",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "file");
    }

    #[tokio::test]
    async fn test_file_upload_owner_type_project() {
        let app = test_app().await;
        let org_slug = "file-project-org";
        let _org_id = create_org_for_files(&app, org_slug).await;
        let owner_id = create_project_for_files(&app, org_slug, "file-project-owner").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Project file",
            "project-doc.md",
            "project",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "file");
    }

    #[tokio::test]
    async fn test_file_upload_owner_type_team() {
        let app = test_app().await;
        let org_slug = "file-team-org";
        let _org_id = create_org_for_files(&app, org_slug).await;
        let owner_id = create_team_for_files(&app, org_slug, "file-team-owner").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Team shared file",
            "team-notes.txt",
            "team",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["object"], "file");
    }

    #[tokio::test]
    async fn test_file_upload_unicode_filename() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-unicode-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"Unicode content test",
            "日本語ドキュメント_文档_документ.txt",
            "user",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["filename"], "日本語ドキュメント_文档_документ.txt");
    }

    #[tokio::test]
    async fn test_file_upload_missing_file_field() {
        let app = test_app().await;
        let owner_id = uuid::Uuid::new_v4().to_string();
        let boundary = "----FileUploadBoundary12345";
        let mut body = Vec::new();

        // Only owner_type and owner_id, no file field
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_type\"\r\n\r\n");
        body.extend_from_slice(b"user\r\n");
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_id\"\r\n\r\n");
        body.extend_from_slice(owner_id.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let content_type = format!("multipart/form-data; boundary={}", boundary);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "missing_file");
    }

    #[tokio::test]
    async fn test_file_upload_missing_owner_type() {
        let app = test_app().await;
        let owner_id = uuid::Uuid::new_v4().to_string();
        let boundary = "----FileUploadBoundary12345";
        let mut body = Vec::new();

        // File and owner_id, but no owner_type
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        body.extend_from_slice(b"Test content\r\n");
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_id\"\r\n\r\n");
        body.extend_from_slice(owner_id.as_bytes());
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let content_type = format!("multipart/form-data; boundary={}", boundary);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "missing_owner_type");
    }

    #[tokio::test]
    async fn test_file_upload_missing_owner_id() {
        let app = test_app().await;
        let boundary = "----FileUploadBoundary12345";
        let mut body = Vec::new();

        // File and owner_type, but no owner_id
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        body.extend_from_slice(b"Test content\r\n");
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner_type\"\r\n\r\n");
        body.extend_from_slice(b"user\r\n");
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let content_type = format!("multipart/form-data; boundary={}", boundary);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "missing_owner_id");
    }

    #[tokio::test]
    async fn test_file_upload_invalid_owner_type() {
        let app = test_app().await;
        let owner_id = uuid::Uuid::new_v4().to_string();
        let (content_type, body) = create_file_upload_multipart(
            b"Test content",
            "test.txt",
            "invalid_type",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_owner_type");
    }

    #[tokio::test]
    async fn test_file_upload_invalid_owner_id() {
        let app = test_app().await;
        let (content_type, body) = create_file_upload_multipart(
            b"Test content",
            "test.txt",
            "user",
            "not-a-valid-uuid",
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_owner_id");
    }

    #[tokio::test]
    async fn test_file_upload_invalid_purpose() {
        let app = test_app().await;
        let owner_id = uuid::Uuid::new_v4().to_string();
        let (content_type, body) = create_file_upload_multipart(
            b"Test content",
            "test.txt",
            "user",
            &owner_id,
            Some("invalid_purpose"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_purpose");
    }

    #[tokio::test]
    async fn test_file_upload_owner_not_found() {
        let app = test_app().await;
        // Use a valid UUID format but for a non-existent user
        let owner_id = uuid::Uuid::new_v4().to_string();
        let (content_type, body) =
            create_file_upload_multipart(b"Test content", "test.txt", "user", &owner_id, None);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "owner_not_found");
    }

    #[tokio::test]
    async fn test_file_upload_empty_file() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-empty-user").await;
        let (content_type, body) =
            create_file_upload_multipart(b"", "empty.txt", "user", &owner_id, None);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        // Empty files should be allowed
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["bytes"], 0);
    }

    #[tokio::test]
    async fn test_file_upload_binary_content() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-binary-user").await;
        // Binary content with various byte values including null bytes
        // Use .png extension since binary files with .bin are not allowed for assistants purpose
        let binary_content: Vec<u8> = (0..=255).collect();
        let (content_type, body) =
            create_file_upload_multipart(&binary_content, "binary.png", "user", &owner_id, None);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["bytes"], 256);
        assert_eq!(json["filename"], "binary.png");
    }

    /// Create a test application with a custom max file size limit
    async fn test_app_with_file_size_limit(max_file_size_mb: u64) -> axum::Router {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

        #[cfg(feature = "sso")]
        let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
        #[cfg(not(feature = "sso"))]
        let session_section = "";

        let config_str = format!(
            r#"
[database]
type = "sqlite"
path = "file:api_test_file_limit_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "test"

[providers.test]
type = "test"
model_name = "test-model"

[features.file_processing]
max_file_size_mb = {max_file_size_mb}
"#
        );

        let config =
            crate::config::GatewayConfig::parse(&config_str).expect("Failed to parse test config");
        let state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");
        crate::build_app(&config, state)
    }

    /// Create a test application with file_search_service configured.
    ///
    /// This enables testing endpoints that require the file search service,
    /// such as the vector store file addition endpoint which validates
    /// embedding model compatibility.
    async fn test_app_with_file_search() -> axum::Router {
        let (app, _db) = test_app_with_file_search_and_db().await;
        app
    }

    /// Create a test application with file_search_service configured, returning
    /// both the app router and the database for direct manipulation in tests.
    async fn test_app_with_file_search_and_db() -> (axum::Router, std::sync::Arc<crate::db::DbPool>)
    {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

        #[cfg(feature = "sso")]
        let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
        #[cfg(not(feature = "sso"))]
        let session_section = "";

        let config_str = format!(
            r#"
[database]
type = "sqlite"
path = "file:api_test_file_search_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "test"

[providers.test]
type = "test"
model_name = "test-model"
"#
        );

        let config =
            crate::config::GatewayConfig::parse(&config_str).expect("Failed to parse test config");
        let mut state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");

        // Create EmbeddingService using the test provider
        // Use the default embedding model name that collections are created with
        let embedding_config = crate::config::EmbeddingConfig {
            provider: "test".to_string(),
            model: "text-embedding-3-small".to_string(), // Default vector store model
            dimensions: 1536,                            // Default vector store dimensions
        };

        let provider_config = config.providers.get("test").expect("test provider config");
        let embedding_service = std::sync::Arc::new(
            crate::cache::EmbeddingService::new(
                &embedding_config,
                provider_config,
                &state.circuit_breakers,
                state.http_client.clone(),
            )
            .expect("Failed to create embedding service"),
        );

        // Create TestVectorStore with matching dimensions
        let vector_store: std::sync::Arc<dyn crate::cache::vector_store::VectorBackend> =
            std::sync::Arc::new(crate::cache::vector_store::TestVectorStore::new(1536));

        let db = state.db.clone().expect("db should be configured");

        // Create FileSearchService
        let file_search_service = crate::services::FileSearchService::new(
            db.clone(),
            embedding_service,
            vector_store,
            None, // No reranker needed for tests
            crate::services::FileSearchServiceConfig {
                default_max_results: 10,
                default_threshold: 0.7,
                retry: crate::config::RetryConfig::default(),
                circuit_breaker: crate::config::CircuitBreakerConfig::default(),
                rerank: crate::config::RerankConfig::default(),
            },
        );

        state.file_search_service = Some(std::sync::Arc::new(file_search_service));

        (crate::build_app(&config, state), db)
    }

    /// Create a test application with MockableTestVectorStore for testing search results.
    ///
    /// Returns the app router, database, and a handle to set mock search results.
    async fn test_app_with_mockable_file_search() -> (
        axum::Router,
        std::sync::Arc<crate::db::DbPool>,
        std::sync::Arc<std::sync::Mutex<Vec<crate::cache::vector_store::ChunkSearchResult>>>,
    ) {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let db_id = COUNTER.fetch_add(1, Ordering::SeqCst);

        #[cfg(feature = "sso")]
        let session_section = r#"
[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
"#;
        #[cfg(not(feature = "sso"))]
        let session_section = "";

        let config_str = format!(
            r#"
[database]
type = "sqlite"
path = "file:api_test_mockable_fs_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers]
default_provider = "test"

[providers.test]
type = "test"
model_name = "test-model"
"#
        );

        let config =
            crate::config::GatewayConfig::parse(&config_str).expect("Failed to parse test config");
        let mut state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");

        // Create EmbeddingService using the test provider
        let embedding_config = crate::config::EmbeddingConfig {
            provider: "test".to_string(),
            model: "text-embedding-3-small".to_string(),
            dimensions: 1536,
        };

        let provider_config = config.providers.get("test").expect("test provider config");
        let embedding_service = std::sync::Arc::new(
            crate::cache::EmbeddingService::new(
                &embedding_config,
                provider_config,
                &state.circuit_breakers,
                state.http_client.clone(),
            )
            .expect("Failed to create embedding service"),
        );

        // Create MockableTestVectorStore with matching dimensions
        let mockable_store = crate::cache::vector_store::MockableTestVectorStore::new(1536);
        let mock_results_handle = mockable_store.mock_results_handle();
        let vector_store: std::sync::Arc<dyn crate::cache::vector_store::VectorBackend> =
            std::sync::Arc::new(mockable_store);

        let db = state.db.clone().expect("db should be configured");

        // Create FileSearchService
        let file_search_service = crate::services::FileSearchService::new(
            db.clone(),
            embedding_service,
            vector_store,
            None,
            crate::services::FileSearchServiceConfig {
                default_max_results: 10,
                default_threshold: 0.7,
                retry: crate::config::RetryConfig::default(),
                circuit_breaker: crate::config::CircuitBreakerConfig::default(),
                rerank: crate::config::RerankConfig::default(),
            },
        );

        state.file_search_service = Some(std::sync::Arc::new(file_search_service));

        (crate::build_app(&config, state), db, mock_results_handle)
    }

    #[tokio::test]
    async fn test_file_upload_file_size_limit_exceeded() {
        // Create app with 0 MB limit (any non-empty file will be rejected)
        let app = test_app_with_file_size_limit(0).await;
        let owner_id = create_user_for_files(&app, "file-size-limit-user").await;

        // Try to upload a small file (should be rejected since limit is 0)
        let (content_type, body) = create_file_upload_multipart(
            b"This file content exceeds the configured limit",
            "too-large.txt",
            "user",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json["error"]["code"], "file_too_large");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("exceeds maximum allowed size")
        );
    }

    #[tokio::test]
    async fn test_file_upload_file_size_within_limit() {
        // Create app with 1 MB limit
        let app = test_app_with_file_size_limit(1).await;
        let owner_id = create_user_for_files(&app, "file-size-ok-user").await;

        // Upload a small file (should succeed since it's under 1 MB)
        let (content_type, body) = create_file_upload_multipart(
            b"This file is small enough",
            "small-file.txt",
            "user",
            &owner_id,
            None,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["filename"], "small-file.txt");
    }

    #[tokio::test]
    async fn test_file_upload_invalid_file_type_fine_tune_txt() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-finetune-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"This should fail - not jsonl",
            "training-data.txt", // Should be .jsonl for fine-tune
            "user",
            &owner_id,
            Some("fine-tune"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_file_type");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("fine-tune")
        );
    }

    #[tokio::test]
    async fn test_file_upload_invalid_file_type_batch_pdf() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-batch-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"This should fail - not jsonl",
            "batch-requests.pdf", // Should be .jsonl for batch
            "user",
            &owner_id,
            Some("batch"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_file_type");
        assert!(json["error"]["message"].as_str().unwrap().contains("batch"));
    }

    #[tokio::test]
    async fn test_file_upload_invalid_file_type_vision_txt() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-vision-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"This should fail - not an image",
            "document.txt", // Should be image for vision
            "user",
            &owner_id,
            Some("vision"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_file_type");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("vision")
        );
    }

    #[tokio::test]
    async fn test_file_upload_invalid_file_type_assistants_exe() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-assistants-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"\x4D\x5A\x90\x00", // PE header bytes
            "malware.exe",       // Executable files not allowed
            "user",
            &owner_id,
            None, // Default is assistants
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_file_type");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("assistants")
        );
    }

    #[tokio::test]
    async fn test_file_upload_invalid_file_type_no_extension() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-noext-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"No extension file",
            "README", // No extension
            "user",
            &owner_id,
            Some("fine-tune"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_file_type");
        // Message should indicate no extension
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("(no extension)")
        );
    }

    #[tokio::test]
    async fn test_file_upload_valid_file_type_assistants_pdf() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-valid-pdf-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"%PDF-1.4 fake pdf content",
            "document.pdf",
            "user",
            &owner_id,
            None, // Default is assistants
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["filename"], "document.pdf");
    }

    #[tokio::test]
    async fn test_file_upload_valid_file_type_vision_jpeg() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-valid-jpeg-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"\xFF\xD8\xFF\xE0", // JPEG magic bytes
            "photo.jpeg",
            "user",
            &owner_id,
            Some("vision"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["filename"], "photo.jpeg");
        assert_eq!(json["purpose"], "vision");
    }

    #[tokio::test]
    async fn test_file_upload_valid_file_type_assistants_code() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-type-valid-code-user").await;
        let (content_type, body) = create_file_upload_multipart(
            b"fn main() { println!(\"Hello\"); }",
            "main.rs",
            "user",
            &owner_id,
            None, // Default is assistants
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["filename"], "main.rs");
    }

    // ============================================================================
    // Vector Store API Tests
    // ============================================================================

    /// Helper to create an organization and return its ID
    async fn create_org_for_vector_store(app: &axum::Router, slug: &str) -> String {
        let (status, org) = post_json(
            app,
            "/admin/v1/organizations",
            json!({"slug": slug, "name": format!("Org {}", slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        org["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_vector_store_create_basic() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-create-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Test Vector Store"
            }),
        )
        .await;

        if status != StatusCode::CREATED {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::CREATED);

        // Validate response structure
        assert!(body["id"].as_str().unwrap().starts_with("vs_"));
        assert_eq!(body["object"], "vector_store");
        assert_eq!(body["name"], "Test Vector Store");
        assert_eq!(body["owner_type"], "organization");
        assert_eq!(body["owner_id"], org_id);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["embedding_model"], "text-embedding-3-small");
        assert_eq!(body["embedding_dimensions"], 1536);
        assert_eq!(body["usage_bytes"], 0);
        assert!(body["created_at"].is_string());
        assert!(body["updated_at"].is_string());

        // File counts should be zero initially
        assert_eq!(body["file_counts"]["in_progress"], 0);
        assert_eq!(body["file_counts"]["completed"], 0);
        assert_eq!(body["file_counts"]["failed"], 0);
        assert_eq!(body["file_counts"]["cancelled"], 0);
        assert_eq!(body["file_counts"]["total"], 0);
    }

    #[tokio::test]
    async fn test_vector_store_create_with_description() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-desc-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Described Store",
                "description": "A test vector store with a description"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "Described Store");
        assert_eq!(
            body["description"],
            "A test vector store with a description"
        );
    }

    #[tokio::test]
    async fn test_vector_store_create_with_metadata() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-meta-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Metadata Store",
                "metadata": {
                    "env": "test",
                    "version": "1.0"
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["metadata"]["env"], "test");
        assert_eq!(body["metadata"]["version"], "1.0");
    }

    #[tokio::test]
    async fn test_vector_store_create_with_custom_embedding() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-embed-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Custom Embedding Store",
                "embedding_model": "text-embedding-ada-002",
                "embedding_dimensions": 1024
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["embedding_model"], "text-embedding-ada-002");
        assert_eq!(body["embedding_dimensions"], 1024);
    }

    #[tokio::test]
    async fn test_vector_store_create_auto_generated_name() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-autoname-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        // Name should be auto-generated (not null/empty)
        assert!(body["name"].is_string());
        assert!(!body["name"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_vector_store_create_owner_not_found() {
        let app = test_app().await;
        let fake_org_id = uuid::Uuid::new_v4().to_string();

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": fake_org_id},
                "name": "Orphan Store"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_create_invalid_owner_type() {
        let app = test_app().await;

        let (status, _body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "invalid_type", "organization_id": uuid::Uuid::new_v4().to_string()},
                "name": "Invalid Owner Store"
            }),
        )
        .await;

        // Should fail validation (422) or bad request (400)
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "Expected 422 or 400, got {}",
            status
        );
    }

    #[tokio::test]
    async fn test_vector_store_create_missing_owner() {
        let app = test_app().await;

        let (status, _body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "name": "No Owner Store"
            }),
        )
        .await;

        // Missing required field should fail validation
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_vector_store_create_with_expires_after() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-expires-org").await;

        let (status, body) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Expiring Store",
                "expires_after": {
                    "anchor": "last_active_at",
                    "days": 7
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["expires_after"]["anchor"], "last_active_at");
        assert_eq!(body["expires_after"]["days"], 7);
    }

    #[tokio::test]
    async fn test_vector_store_list_empty() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-list-empty-org").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores?owner_type=organization&owner_id={}",
                org_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_vector_store_list_with_stores() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-list-stores-org").await;

        // Create two vector stores
        let (status, _) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Store One"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Store Two"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // List should return both
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores?owner_type=organization&owner_id={}",
                org_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_vector_store_get_by_id() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-get-org").await;

        // Create a vector store
        let (status, created) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Get Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let store_id = created["id"].as_str().unwrap();

        // Get by ID
        let (status, body) = get_json(&app, &format!("/api/v1/vector_stores/{}", store_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], store_id);
        assert_eq!(body["name"], "Get Test Store");
    }

    #[tokio::test]
    async fn test_vector_store_get_not_found() {
        let app = test_app().await;
        let fake_id = format!("vs_{}", uuid::Uuid::new_v4());

        let (status, body) = get_json(&app, &format!("/api/v1/vector_stores/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_modify() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-modify-org").await;

        // Create a vector store
        let (status, created) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Original Name"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let store_id = created["id"].as_str().unwrap();

        // Modify it
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}", store_id),
            json!({
                "name": "Updated Name",
                "description": "New description"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "Updated Name");
        assert_eq!(body["description"], "New description");
    }

    #[tokio::test]
    async fn test_vector_store_modify_not_found() {
        let app = test_app().await;
        let fake_id = format!("vs_{}", uuid::Uuid::new_v4());

        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}", fake_id),
            json!({"name": "New Name"}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_delete() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-delete-org").await;

        // Create a vector store
        let (status, created) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "To Be Deleted"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let store_id = created["id"].as_str().unwrap();

        // Delete it
        let (status, body) =
            delete_json(&app, &format!("/api/v1/vector_stores/{}", store_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], store_id);
        assert_eq!(body["object"], "vector_store.deleted");
        assert_eq!(body["deleted"], true);

        // Verify it's gone
        let (status, _) = get_json(&app, &format!("/api/v1/vector_stores/{}", store_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_vector_store_delete_not_found() {
        let app = test_app().await;
        let fake_id = format!("vs_{}", uuid::Uuid::new_v4());

        let (status, body) = delete_json(&app, &format!("/api/v1/vector_stores/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_list_pagination() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vs-pagination-org").await;

        // Create 5 vector stores
        for i in 0..5 {
            let (status, _) = post_json(
                &app,
                "/api/v1/vector_stores",
                json!({
                    "owner": {"type": "organization", "organization_id": org_id},
                    "name": format!("Store {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        // Request with limit=2
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores?owner_type=organization&owner_id={}&limit=2",
                org_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
        assert!(body["has_more"].as_bool().unwrap());
    }

    // ============================================================================
    // Vector Store Files API Tests
    // ============================================================================

    /// Helper to upload a file and return its ID (for vector store file tests)
    async fn upload_file_for_vector_store(
        app: &axum::Router,
        owner_type: &str,
        owner_id: &str,
        filename: &str,
    ) -> String {
        let (content_type, body) = create_file_upload_multipart(
            b"Test file content for vector store",
            filename,
            owner_type,
            owner_id,
            Some("assistants"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK, "File upload failed: {:?}", json);
        json["id"].as_str().unwrap().to_string()
    }

    /// Helper to upload a file with unique content (avoids content deduplication)
    async fn upload_file_with_unique_content(
        app: &axum::Router,
        owner_type: &str,
        owner_id: &str,
        filename: &str,
    ) -> String {
        // Include filename and UUID in content to ensure uniqueness
        let content = format!("Unique content for {} - {}", filename, uuid::Uuid::new_v4());
        let (content_type, body) = create_file_upload_multipart(
            content.as_bytes(),
            filename,
            owner_type,
            owner_id,
            Some("assistants"),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(status, StatusCode::OK, "File upload failed: {:?}", json);
        json["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_vector_store_file_create_vector_store_not_found() {
        let app = test_app().await;
        let fake_vs_id = format!("vs_{}", uuid::Uuid::new_v4());
        let fake_file_id = format!("file-{}", uuid::Uuid::new_v4());

        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", fake_vs_id),
            json!({"file_id": fake_file_id}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_create_file_not_found() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-file-not-found-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Test Store for File Not Found"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Try to add a non-existent file
        let fake_file_id = format!("file-{}", uuid::Uuid::new_v4());
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": fake_file_id}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_create_service_unavailable() {
        // The default test_app() doesn't configure file_search_service,
        // so validate_embedding_model_compatibility returns 503
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-service-unavail-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Test Store for Service Unavailable"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id = upload_file_for_vector_store(&app, "organization", &org_id, "test.txt").await;

        // Try to add the file to the vector store
        // This should fail with 503 because file_search_service is not configured
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "embedding_service_unavailable");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("File search service is not configured")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_list_vector_store_not_found() {
        let app = test_app().await;
        let fake_vs_id = format!("vs_{}", uuid::Uuid::new_v4());

        let (status, body) =
            get_json(&app, &format!("/api/v1/vector_stores/{}/files", fake_vs_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_list_empty() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-list-empty-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Empty Vector Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // List files (should be empty)
        let (status, body) =
            get_json(&app, &format!("/api/v1/vector_stores/{}/files", vs_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        assert!(body["data"].as_array().unwrap().is_empty());
        assert_eq!(body["has_more"], false);
        assert!(body["first_id"].is_null());
        assert!(body["last_id"].is_null());
    }

    #[tokio::test]
    async fn test_vector_store_file_list_invalid_cursor() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-list-cursor-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Cursor Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Try to list with invalid cursor format
        let (status, body) = get_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files?after=invalid-cursor", vs_id),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
    }

    #[tokio::test]
    async fn test_vector_store_file_list_cursor_not_found() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-list-cursor-nf-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Cursor Not Found Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Try to list with valid format but non-existent cursor
        let fake_file_id = format!("file-{}", uuid::Uuid::new_v4());
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores/{}/files?after={}",
                vs_id, fake_file_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found for cursor")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_list_invalid_filter() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-list-filter-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Filter Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Try to list with invalid filter
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores/{}/files?filter=invalid_status",
                vs_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_filter");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid filter status")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_list_with_limit() {
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsf-list-limit-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Limit Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // List with limit parameter (should work even on empty store)
        let (status, body) = get_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files?limit=5", vs_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    // ============================================================================
    // Vector Store File Success Tests (POST /v1/vector_stores/{id}/files)
    // These tests use test_app_with_file_search() which has FileSearchService configured
    // ============================================================================

    #[tokio::test]
    async fn test_vector_store_file_create_success() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-create-success-org").await;

        // Create a vector store (uses default embedding model: text-embedding-3-small)
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Success Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "success.txt").await;

        // Add the file to the vector store
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["object"], "vector_store.file");
        assert_eq!(body["vector_store_id"], vs_id);
        // Note: file_id in response is the vector store_file's file_id, not the vector store file ID
        assert_eq!(body["status"], "in_progress"); // No document processor, so stays in_progress
        // VectorStoreFileId uses "file-" prefix per prefixed_id.rs
        assert!(body["id"].as_str().unwrap().starts_with("file-"));
    }

    #[tokio::test]
    async fn test_vector_store_file_create_idempotent() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-idempotent-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Idempotent Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "idempotent.txt").await;

        // Add the file to the vector store (first time)
        let (status1, body1) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status1, StatusCode::CREATED);
        let vector_store_file_id = body1["id"].as_str().unwrap();

        // Add the same file again (should be idempotent)
        let (status2, body2) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;

        // Should return 200 OK with the existing entry
        assert_eq!(status2, StatusCode::OK);
        // Note: After model change, id IS the file_id (file- prefix)
        assert_eq!(body2["id"], vector_store_file_id);
        assert_eq!(body2["vector_store_id"], vs_id);
    }

    #[tokio::test]
    async fn test_vector_store_file_create_reprocess_failed() {
        let (app, db) = test_app_with_file_search_and_db().await;
        let org_id = create_org_for_vector_store(&app, "vsf-reprocess-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Reprocess Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "reprocess.txt").await;

        // Add the file to the vector store
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let returned_file_id = body["id"].as_str().unwrap();

        // Manually mark the file as failed using the vector stores repo
        // After model change, body["id"] is the file_id (file- prefix).
        // We need to look up the internal junction record ID to update the status.
        let file_uuid: uuid::Uuid = returned_file_id
            .strip_prefix("file-")
            .unwrap()
            .parse()
            .unwrap();
        let vs_uuid: uuid::Uuid = vs_id.strip_prefix("vs_").unwrap().parse().unwrap();

        // Look up the vector store file to get the internal junction ID
        let vector_store_file = db
            .vector_stores()
            .find_vector_store_file_by_file_id(vs_uuid, file_uuid)
            .await
            .expect("Failed to find vector store file")
            .expect("VectorStore file not found");
        let internal_id = vector_store_file.internal_id;

        // Update the status using the vector stores repo
        use crate::models::{FileError, FileErrorCode, VectorStoreFileStatus};
        db.vector_stores()
            .update_vector_store_file_status(
                internal_id,
                VectorStoreFileStatus::Failed,
                Some(FileError {
                    code: FileErrorCode::ServerError,
                    message: "Test failure".to_string(),
                }),
            )
            .await
            .expect("Failed to update status");

        // Try to add the file again (should trigger re-processing)
        let (status2, body2) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;

        // Should return 200 OK with re-processing triggered
        assert_eq!(status2, StatusCode::OK);
        assert_eq!(body2["id"], returned_file_id);
        // Status will be "in_progress" (async processing) or "completed" (inline processing)
        // The test app uses inline processing, so file is processed before response returns
        assert!(
            body2["status"] == "in_progress" || body2["status"] == "completed",
            "Expected status 'in_progress' or 'completed', got '{}'",
            body2["status"]
        );
        // last_error should be cleared (re-processing was triggered successfully)
        assert!(body2["last_error"].is_null());
    }

    #[tokio::test]
    async fn test_vector_store_file_create_content_dedup() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-dedup-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Dedup Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload two files with identical content
        let content = b"Identical content for deduplication test";
        let (content_type1, body1) = create_file_upload_multipart(
            content,
            "file1.txt",
            "organization",
            &org_id,
            Some("assistants"),
        );
        let request1 = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type1)
            .body(Body::from(body1))
            .unwrap();
        let response1 = app.clone().oneshot(request1).await.unwrap();
        assert_eq!(response1.status(), StatusCode::OK);
        let body1 = axum::body::to_bytes(response1.into_body(), usize::MAX)
            .await
            .unwrap();
        let json1: Value = serde_json::from_slice(&body1).unwrap();
        let file1_id = json1["id"].as_str().unwrap();

        let (content_type2, body2) = create_file_upload_multipart(
            content,
            "file2.txt",
            "organization",
            &org_id,
            Some("assistants"),
        );
        let request2 = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type2)
            .body(Body::from(body2))
            .unwrap();
        let response2 = app.clone().oneshot(request2).await.unwrap();
        assert_eq!(response2.status(), StatusCode::OK);
        let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX)
            .await
            .unwrap();
        let json2: Value = serde_json::from_slice(&body2).unwrap();
        let file2_id = json2["id"].as_str().unwrap();

        // File IDs should be different
        assert_ne!(file1_id, file2_id);

        // Add the first file to the vector store
        let (status1, body1) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file1_id}),
        )
        .await;
        assert_eq!(status1, StatusCode::CREATED);
        let vector_store_file_id = body1["id"].as_str().unwrap();

        // Add the second file (same content, same owner) - should detect duplicate
        let (status2, body2) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file2_id}),
        )
        .await;

        // Should return 200 OK with the existing vector store file
        assert_eq!(status2, StatusCode::OK);
        // Note: After model change, id IS the file_id (file- prefix)
        // The returned id should be the original file, not the duplicate
        assert_eq!(body2["id"], vector_store_file_id);
    }

    #[tokio::test]
    async fn test_vector_store_file_create_embedding_model_mismatch() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-mismatch-org").await;

        // Create a vector store with a DIFFERENT embedding model than the configured one
        // The test app uses text-embedding-3-small, so use a different model
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Mismatch Test Store",
                "embedding_model": "text-embedding-ada-002",
                "embedding_dimensions": 1536
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "mismatch.txt").await;

        // Try to add the file - should fail with embedding model mismatch
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "embedding_model_mismatch");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("text-embedding-ada-002")
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("text-embedding-3-small")
        );
    }

    // ============================================================================
    // Vector Store File Delete Tests (DELETE /v1/vector_stores/{id}/files/{file_id})
    // ============================================================================

    #[tokio::test]
    async fn test_vector_store_file_delete_success() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-delete-success-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Delete Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "delete-test.txt").await;

        // Add the file to the vector store
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Delete the file from the vector store
        let (status, body) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], file_id);
        assert_eq!(body["object"], "vector_store.file.deleted");
        assert_eq!(body["deleted"], true);
    }

    #[tokio::test]
    async fn test_vector_store_file_delete_vector_store_not_found() {
        let app = test_app().await;
        let fake_vs_id = format!("vs_{}", uuid::Uuid::new_v4());
        let fake_file_id = format!("file-{}", uuid::Uuid::new_v4());

        let (status, body) = delete_json(
            &app,
            &format!(
                "/api/v1/vector_stores/{}/files/{}",
                fake_vs_id, fake_file_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Vector store")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_delete_file_not_in_store() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-delete-not-in-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Delete Not In Store Test"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file but DON'T add it to the vector store
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "not-in-store.txt").await;

        // Try to delete the file from the vector store (should fail - file not in store)
        let (status, body) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found in vector store")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_delete_already_deleted() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-delete-twice-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Delete Twice Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file and add to vector store
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "delete-twice.txt").await;
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Delete the file (first time - should succeed)
        let (status, _) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Try to delete again (should fail - already deleted)
        let (status, body) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn test_vector_store_file_delete_preserves_original_file() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-delete-preserve-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Delete Preserve Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file and add to vector store
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "preserve.txt").await;
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Delete the file from vector store
        let (status, _) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Verify the original file still exists in Files API
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], file_id);
        assert_eq!(body["object"], "file");
    }

    #[tokio::test]
    async fn test_vector_store_file_delete_removes_from_list() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsf-delete-list-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Delete List Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload two files with unique content (to avoid deduplication) and add to vector store
        let file1_id =
            upload_file_with_unique_content(&app, "organization", &org_id, "list-file1.txt").await;
        let file2_id =
            upload_file_with_unique_content(&app, "organization", &org_id, "list-file2.txt").await;

        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file1_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file2_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Verify both files are in the list
        let (status, body) =
            get_json(&app, &format!("/api/v1/vector_stores/{}/files", vs_id)).await;
        assert_eq!(status, StatusCode::OK);
        let files = body["data"].as_array().unwrap();
        assert_eq!(files.len(), 2);

        // Delete file1 from vector store
        let (status, _) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file1_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Verify only file2 remains in the list
        let (status, body) =
            get_json(&app, &format!("/api/v1/vector_stores/{}/files", vs_id)).await;
        assert_eq!(status, StatusCode::OK);
        let files = body["data"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        // Note: After model change, id IS the file_id (file- prefix)
        assert_eq!(files[0]["id"], file2_id);
    }

    // ============================================================================
    // Vector Store File Batch Tests (POST /v1/vector_stores/{id}/file_batches)
    // ============================================================================

    #[tokio::test]
    async fn test_vector_store_file_batch_create_vector_store_not_found() {
        let app = test_app_with_file_search().await;
        let fake_vs_id = format!("vs_{}", uuid::Uuid::new_v4());
        let fake_file_id = uuid::Uuid::new_v4();

        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", fake_vs_id),
            json!({"file_ids": [fake_file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_embedding_model_mismatch() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-mismatch-org").await;

        // Create a vector store with a DIFFERENT embedding model than the configured one
        // The test app uses text-embedding-3-small, so use a different model
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Mismatch Batch Test Store",
                "embedding_model": "text-embedding-ada-002",
                "embedding_dimensions": 1536
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-mismatch.txt").await;
        // Strip the "file-" prefix to get raw UUID for the request body
        let file_id = file_id_prefixed.strip_prefix("file-").unwrap();

        // Try to create a file batch - should fail with embedding model mismatch
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "embedding_model_mismatch");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("text-embedding-ada-002")
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("text-embedding-3-small")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_service_unavailable() {
        // The default test_app() doesn't configure file_search_service,
        // so validate_embedding_model_compatibility returns 503
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-service-unavail-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Test Store for Batch Service Unavailable"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-unavail.txt").await;
        // Strip the "file-" prefix to get raw UUID for the request body
        let file_id = file_id_prefixed.strip_prefix("file-").unwrap();

        // Try to create a file batch
        // This should fail with 503 because file_search_service is not configured
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "embedding_service_unavailable");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("File search service is not configured")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_basic() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-basic-org").await;

        // Create a vector store (uses default embedding model: text-embedding-3-small)
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Basic Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        // Response vector_store_id is raw UUID without prefix
        let vs_id_raw = vs_id.strip_prefix("vs_").unwrap();

        // Upload a file
        let file_id_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-basic.txt").await;
        let file_id = file_id_prefixed.strip_prefix("file-").unwrap();

        // Create a file batch
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["object"], "vector_store.file_batch");
        assert_eq!(body["vector_store_id"], vs_id_raw);
        assert_eq!(body["status"], "completed");
        assert!(body["id"].as_str().unwrap().starts_with("vsfb_"));
        assert_eq!(body["file_counts"]["total"], 1);
        assert_eq!(body["file_counts"]["completed"], 1);
        assert_eq!(body["file_counts"]["failed"], 0);
        assert_eq!(body["file_counts"]["in_progress"], 0);
        assert_eq!(body["file_counts"]["cancelled"], 0);
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_multiple_files() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-multi-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Multi File Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload multiple files
        let file1_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-multi-1.txt").await;
        let file2_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-multi-2.txt").await;
        let file3_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-multi-3.txt").await;

        let file1_id = file1_prefixed.strip_prefix("file-").unwrap();
        let file2_id = file2_prefixed.strip_prefix("file-").unwrap();
        let file3_id = file3_prefixed.strip_prefix("file-").unwrap();

        // Create a file batch with multiple files
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file1_id, file2_id, file3_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["file_counts"]["total"], 3);
        assert_eq!(body["file_counts"]["completed"], 3);
        assert_eq!(body["file_counts"]["failed"], 0);
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_with_chunking() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-chunk-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Chunking Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-chunk.txt").await;
        let file_id = file_id_prefixed.strip_prefix("file-").unwrap();

        // Create a file batch with chunking strategy
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({
                "file_ids": [file_id],
                "chunking_strategy": {
                    "type": "static",
                    "static": {
                        "max_chunk_size_tokens": 500,
                        "chunk_overlap_tokens": 100
                    }
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["file_counts"]["completed"], 1);
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_idempotent() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-idemp-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Idempotent Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file
        let file_id_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-idemp.txt").await;
        let file_id = file_id_prefixed.strip_prefix("file-").unwrap();

        // Add the file individually first
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id_prefixed}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Now create a batch with the same file - should still succeed (idempotent)
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["status"], "completed");
        // File was already in vector_store, so counts as completed
        assert_eq!(body["file_counts"]["total"], 1);
        assert_eq!(body["file_counts"]["completed"], 1);
        assert_eq!(body["file_counts"]["failed"], 0);
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_partial_failure() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-partial-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Partial Failure Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload one real file
        let file1_prefixed =
            upload_file_for_vector_store(&app, "organization", &org_id, "batch-partial.txt").await;
        let file1_id = file1_prefixed.strip_prefix("file-").unwrap();

        // Use a fake file ID that doesn't exist
        let fake_file_id = uuid::Uuid::new_v4();

        // Create a batch with one real file and one fake file
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": [file1_id, fake_file_id]}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        // Status is "failed" because at least one file failed
        assert_eq!(body["status"], "failed");
        assert_eq!(body["file_counts"]["total"], 2);
        assert_eq!(body["file_counts"]["completed"], 1);
        assert_eq!(body["file_counts"]["failed"], 1);
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_create_empty() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "vsfb-empty-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Empty Batch Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Create a batch with no files
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/file_batches", vs_id),
            json!({"file_ids": []}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["file_counts"]["total"], 0);
        assert_eq!(body["file_counts"]["completed"], 0);
        assert_eq!(body["file_counts"]["failed"], 0);
    }

    // Vector Store File Batch Stub Endpoint Tests
    // These endpoints return errors because file batches are executed synchronously
    // and not persisted. The batch ID returned from create is for reference only.

    #[tokio::test]
    async fn test_vector_store_file_batch_get_not_persisted() {
        let app = test_app().await;
        let fake_vs_id = uuid::Uuid::new_v4();
        let fake_batch_id = "vsfb_12345";

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores/vs_{}/file_batches/{}",
                fake_vs_id, fake_batch_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not persisted")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_cancel_not_supported() {
        let app = test_app().await;
        let fake_vs_id = uuid::Uuid::new_v4();
        let fake_batch_id = "vsfb_12345";

        let (status, body) = delete_json(
            &app,
            &format!(
                "/api/v1/vector_stores/vs_{}/file_batches/{}",
                fake_vs_id, fake_batch_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "not_supported");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("cannot be cancelled")
        );
    }

    #[tokio::test]
    async fn test_vector_store_file_batch_list_files_not_persisted() {
        let app = test_app().await;
        let fake_vs_id = uuid::Uuid::new_v4();
        let fake_batch_id = "vsfb_12345";

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/vector_stores/vs_{}/file_batches/{}/files",
                fake_vs_id, fake_batch_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not persisted")
        );
    }

    // ============================================================================
    // Vector Store Search Tests (POST /v1/vector_stores/{id}/search)
    // ============================================================================

    #[tokio::test]
    async fn test_vector_store_search_vector_store_not_found() {
        let app = test_app_with_file_search().await;
        let fake_vs_id = uuid::Uuid::new_v4();

        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/vs_{}/search", fake_vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_vector_store_search_file_search_not_configured() {
        // Use test_app() which does NOT have file_search_service configured
        let app = test_app().await;
        let org_id = create_org_for_vector_store(&app, "search-no-fs-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Try to search - should fail because file_search_service is not configured
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "not_configured");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("File search is not configured")
        );
    }

    #[tokio::test]
    async fn test_vector_store_search_invalid_score_threshold_too_high() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-threshold-high-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search with score_threshold > 1.0
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "ranking_options": {
                    "score_threshold": 1.5
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_parameter");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("score_threshold must be between 0.0 and 1.0")
        );
    }

    #[tokio::test]
    async fn test_vector_store_search_invalid_score_threshold_negative() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-threshold-neg-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search with score_threshold < 0.0
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "ranking_options": {
                    "score_threshold": -0.5
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_parameter");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("score_threshold must be between 0.0 and 1.0")
        );
    }

    #[tokio::test]
    async fn test_vector_store_search_basic_empty_results() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-empty-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search - should return empty results (TestVectorStore returns empty)
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "vector_store.search_results");
        assert_eq!(body["query"], "test query");
        assert!(body["data"].is_array());
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_vector_store_search_with_max_num_results() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-max-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search with max_num_results
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "max_num_results": 5
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "vector_store.search_results");
        assert_eq!(body["query"], "test query");
    }

    #[tokio::test]
    async fn test_vector_store_search_with_ranking_options() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-ranking-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search with ranking options
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "ranking_options": {
                    "ranker": "vector",
                    "score_threshold": 0.5
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "vector_store.search_results");
    }

    #[tokio::test]
    async fn test_vector_store_search_with_filters() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-filters-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Search with filters
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "filters": {
                    "type": "eq",
                    "key": "category",
                    "value": "documentation"
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "vector_store.search_results");
    }

    // Vector Store Search Tests with Mock Results
    // These tests use MockableTestVectorStore to inject mock search results

    #[tokio::test]
    async fn test_vector_store_search_returns_results() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-results-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        // Extract the UUID from vs_XXX format
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        let chunk_id = uuid::Uuid::new_v4();
        let file_id = uuid::Uuid::new_v4();

        // Set up mock search results
        *mock_handle.lock().unwrap() = vec![crate::cache::vector_store::ChunkSearchResult {
            chunk_id,
            vector_store_id: vs_uuid,
            file_id,
            chunk_index: 0,
            content: "This is the matching content from the document.".to_string(),
            score: 0.95,
            metadata: Some(serde_json::json!({"source": "test.pdf"})),
        }];

        // Search
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "matching content"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "vector_store.search_results");
        assert_eq!(body["query"], "matching content");

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);

        let result = &data[0];
        assert_eq!(result["object"], "vector_store.search_result");
        assert!(result["chunk_id"].as_str().unwrap().starts_with("chunk_"));
        assert_eq!(
            result["vector_store_id"].as_str().unwrap(),
            format!("vs_{}", vs_uuid)
        );
        assert!(result["file_id"].as_str().unwrap().starts_with("file-"));
        assert_eq!(result["chunk_index"], 0);
        assert_eq!(
            result["content"],
            "This is the matching content from the document."
        );
        assert_eq!(result["score"], 0.95);
        assert_eq!(result["metadata"]["source"], "test.pdf");
    }

    #[tokio::test]
    async fn test_vector_store_search_multiple_results() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-multi-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        let file_id = uuid::Uuid::new_v4();

        // Set up multiple mock search results
        *mock_handle.lock().unwrap() = vec![
            crate::cache::vector_store::ChunkSearchResult {
                chunk_id: uuid::Uuid::new_v4(),
                vector_store_id: vs_uuid,
                file_id,
                chunk_index: 0,
                content: "First result with highest score.".to_string(),
                score: 0.98,
                metadata: None,
            },
            crate::cache::vector_store::ChunkSearchResult {
                chunk_id: uuid::Uuid::new_v4(),
                vector_store_id: vs_uuid,
                file_id,
                chunk_index: 1,
                content: "Second result with medium score.".to_string(),
                score: 0.85,
                metadata: None,
            },
            crate::cache::vector_store::ChunkSearchResult {
                chunk_id: uuid::Uuid::new_v4(),
                vector_store_id: vs_uuid,
                file_id,
                chunk_index: 2,
                content: "Third result with lower score.".to_string(),
                score: 0.72,
                metadata: None,
            },
        ];

        // Search
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);

        // Verify order and scores
        assert_eq!(data[0]["score"], 0.98);
        assert_eq!(data[0]["chunk_index"], 0);
        assert_eq!(data[1]["score"], 0.85);
        assert_eq!(data[1]["chunk_index"], 1);
        assert_eq!(data[2]["score"], 0.72);
        assert_eq!(data[2]["chunk_index"], 2);
    }

    #[tokio::test]
    async fn test_vector_store_search_respects_max_num_results() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-limit-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        let file_id = uuid::Uuid::new_v4();

        // Set up more results than we'll request
        *mock_handle.lock().unwrap() = (0..10)
            .map(|i| crate::cache::vector_store::ChunkSearchResult {
                chunk_id: uuid::Uuid::new_v4(),
                vector_store_id: vs_uuid,
                file_id,
                chunk_index: i,
                content: format!("Result {}", i),
                score: 0.9 - (i as f64 * 0.05),
                metadata: None,
            })
            .collect();

        // Request only 3 results
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query",
                "max_num_results": 3
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);
    }

    #[tokio::test]
    async fn test_vector_store_search_with_metadata() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-meta-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        // Set up result with rich metadata
        *mock_handle.lock().unwrap() = vec![crate::cache::vector_store::ChunkSearchResult {
            chunk_id: uuid::Uuid::new_v4(),
            vector_store_id: vs_uuid,
            file_id: uuid::Uuid::new_v4(),
            chunk_index: 0,
            content: "Content with metadata".to_string(),
            score: 0.9,
            metadata: Some(serde_json::json!({
                "category": "documentation",
                "author": "test-author",
                "page": 42,
                "tags": ["api", "reference"]
            })),
        }];

        // Search
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);

        let metadata = &data[0]["metadata"];
        assert_eq!(metadata["category"], "documentation");
        assert_eq!(metadata["author"], "test-author");
        assert_eq!(metadata["page"], 42);
        assert!(metadata["tags"].as_array().unwrap().contains(&json!("api")));
    }

    #[tokio::test]
    async fn test_vector_store_search_without_metadata() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-no-meta-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        // Set up result without metadata
        *mock_handle.lock().unwrap() = vec![crate::cache::vector_store::ChunkSearchResult {
            chunk_id: uuid::Uuid::new_v4(),
            vector_store_id: vs_uuid,
            file_id: uuid::Uuid::new_v4(),
            chunk_index: 0,
            content: "Content without metadata".to_string(),
            score: 0.9,
            metadata: None,
        }];

        // Search
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);

        // metadata should be omitted when None (not present in JSON)
        assert!(data[0].get("metadata").is_none() || data[0]["metadata"].is_null());
    }

    #[tokio::test]
    async fn test_vector_store_search_id_prefixes() {
        let (app, _db, mock_handle) = test_app_with_mockable_file_search().await;
        let org_id = create_org_for_vector_store(&app, "search-prefix-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();
        let vs_uuid = uuid::Uuid::parse_str(&vs_id[3..]).unwrap();

        let chunk_uuid = uuid::Uuid::new_v4();
        let file_uuid = uuid::Uuid::new_v4();

        // Set up result
        *mock_handle.lock().unwrap() = vec![crate::cache::vector_store::ChunkSearchResult {
            chunk_id: chunk_uuid,
            vector_store_id: vs_uuid,
            file_id: file_uuid,
            chunk_index: 5,
            content: "Test content".to_string(),
            score: 0.88,
            metadata: None,
        }];

        // Search
        let (status, body) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/search", vs_id),
            json!({
                "query": "test query"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let result = &body["data"][0];

        // Verify ID prefixes are correctly applied
        assert_eq!(
            result["chunk_id"].as_str().unwrap(),
            format!("chunk_{}", chunk_uuid)
        );
        assert_eq!(
            result["vector_store_id"].as_str().unwrap(),
            format!("vs_{}", vs_uuid)
        );
        assert_eq!(
            result["file_id"].as_str().unwrap(),
            format!("file-{}", file_uuid)
        );
    }

    // ============================================================================
    // Files List API Tests (GET /v1/files)
    // ============================================================================

    /// Helper to upload a file and return its ID (for file list tests)
    async fn upload_file_for_list(
        app: &axum::Router,
        owner_type: &str,
        owner_id: &str,
        filename: &str,
        purpose: Option<&str>,
    ) -> String {
        let content = format!("Content for {}", filename);
        let (content_type, body) = create_file_upload_multipart(
            content.as_bytes(),
            filename,
            owner_type,
            owner_id,
            purpose,
        );

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();
        json["id"].as_str().unwrap().to_string()
    }

    /// Helper to upload a file with specific content and return its ID (for content download tests)
    async fn upload_file_with_content(
        app: &axum::Router,
        owner_type: &str,
        owner_id: &str,
        filename: &str,
        content: &[u8],
    ) -> String {
        let (content_type, body) =
            create_file_upload_multipart(content, filename, owner_type, owner_id, None);

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/files")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();
        json["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_file_list_empty() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-empty-user").await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=user&owner_id={}", owner_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        assert!(body["data"].as_array().unwrap().is_empty());
        assert_eq!(body["has_more"], false);
        assert!(body["first_id"].is_null());
        assert!(body["last_id"].is_null());
    }

    #[tokio::test]
    async fn test_file_list_with_files() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-with-files-user").await;

        // Upload two files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "document1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "document2.txt", None).await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=user&owner_id={}", owner_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);

        // Default order is desc, so file2 should be first
        assert_eq!(data[0]["id"], file2_id);
        assert_eq!(data[1]["id"], file1_id);

        assert_eq!(body["has_more"], false);
        assert_eq!(body["first_id"], file2_id);
        assert_eq!(body["last_id"], file1_id);
    }

    #[tokio::test]
    async fn test_file_list_with_limit() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-limit-user").await;

        // Upload three files
        let _file1_id = upload_file_for_list(&app, "user", &owner_id, "doc1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "doc2.txt", None).await;
        let file3_id = upload_file_for_list(&app, "user", &owner_id, "doc3.txt", None).await;

        // Request with limit=2
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&limit=2",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(body["has_more"], true);

        // Default order is desc, so file3 and file2 should be returned
        assert_eq!(data[0]["id"], file3_id);
        assert_eq!(data[1]["id"], file2_id);
    }

    #[tokio::test]
    async fn test_file_list_pagination_after() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-after-user").await;

        // Upload three files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "doc1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "doc2.txt", None).await;
        let file3_id = upload_file_for_list(&app, "user", &owner_id, "doc3.txt", None).await;

        // Get files after file3 (most recent in desc order)
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&after={}",
                owner_id, file3_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["id"], file2_id);
        assert_eq!(data[1]["id"], file1_id);
        assert_eq!(body["has_more"], false);
    }

    #[tokio::test]
    async fn test_file_list_pagination_before() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-before-user").await;

        // Upload three files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "doc1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "doc2.txt", None).await;
        let file3_id = upload_file_for_list(&app, "user", &owner_id, "doc3.txt", None).await;

        // Get files before file1 (oldest in desc order)
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&before={}",
                owner_id, file1_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        // Before cursor returns items in same order direction
        assert_eq!(data[0]["id"], file3_id);
        assert_eq!(data[1]["id"], file2_id);
        assert_eq!(body["has_more"], false);
    }

    #[tokio::test]
    async fn test_file_list_filter_by_purpose() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-purpose-user").await;

        // Upload files with different purposes
        let _assistants_file =
            upload_file_for_list(&app, "user", &owner_id, "assistant.txt", Some("assistants"))
                .await;
        let batch_file =
            upload_file_for_list(&app, "user", &owner_id, "batch.jsonl", Some("batch")).await;
        let _fine_tune_file =
            upload_file_for_list(&app, "user", &owner_id, "train.jsonl", Some("fine-tune")).await;

        // Filter by batch purpose
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&purpose=batch",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], batch_file);
        assert_eq!(data[0]["purpose"], "batch");
    }

    #[tokio::test]
    async fn test_file_list_order_asc() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-asc-user").await;

        // Upload three files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "doc1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "doc2.txt", None).await;
        let file3_id = upload_file_for_list(&app, "user", &owner_id, "doc3.txt", None).await;

        // Request with ascending order
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&order=asc",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);
        // Ascending order: oldest first
        assert_eq!(data[0]["id"], file1_id);
        assert_eq!(data[1]["id"], file2_id);
        assert_eq!(data[2]["id"], file3_id);
    }

    #[tokio::test]
    async fn test_file_list_order_desc() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-desc-user").await;

        // Upload three files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "doc1.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "doc2.txt", None).await;
        let file3_id = upload_file_for_list(&app, "user", &owner_id, "doc3.txt", None).await;

        // Request with descending order (explicit)
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&order=desc",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);
        // Descending order: newest first
        assert_eq!(data[0]["id"], file3_id);
        assert_eq!(data[1]["id"], file2_id);
        assert_eq!(data[2]["id"], file1_id);
    }

    #[tokio::test]
    async fn test_file_list_organization_owner() {
        let app = test_app().await;
        let org_id = create_org_for_files(&app, "file-list-org").await;

        // Upload file to organization
        let file_id =
            upload_file_for_list(&app, "organization", &org_id, "org-doc.txt", None).await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=organization&owner_id={}", org_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], file_id);
    }

    #[tokio::test]
    async fn test_file_list_project_owner() {
        let app = test_app().await;
        let org_slug = "file-list-proj-org";
        let _org_id = create_org_for_files(&app, org_slug).await;
        let project_id = create_project_for_files(&app, org_slug, "file-list-project").await;

        // Upload file to project
        let file_id =
            upload_file_for_list(&app, "project", &project_id, "project-doc.txt", None).await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=project&owner_id={}", project_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], file_id);
    }

    #[tokio::test]
    async fn test_file_list_team_owner() {
        let app = test_app().await;
        let org_slug = "file-list-team-org";
        let _org_id = create_org_for_files(&app, org_slug).await;
        let team_id = create_team_for_files(&app, org_slug, "file-list-team").await;

        // Upload file to team
        let file_id = upload_file_for_list(&app, "team", &team_id, "team-doc.txt", None).await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=team&owner_id={}", team_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], file_id);
    }

    #[tokio::test]
    async fn test_file_list_invalid_owner_type() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/api/v1/files?owner_type=invalid&owner_id=00000000-0000-0000-0000-000000000000",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_owner_type");
    }

    #[tokio::test]
    async fn test_file_list_invalid_owner_id() {
        let app = test_app().await;

        let (status, _body) =
            get_json(&app, "/api/v1/files?owner_type=user&owner_id=not-a-uuid").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_list_missing_owner_type() {
        let app = test_app().await;

        let (status, _body) = get_json(
            &app,
            "/api/v1/files?owner_id=00000000-0000-0000-0000-000000000000",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_list_missing_owner_id() {
        let app = test_app().await;

        let (status, _body) = get_json(&app, "/api/v1/files?owner_type=user").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_list_invalid_after_cursor_format() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-invalid-after-user").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&after=not-a-valid-file-id",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
    }

    #[tokio::test]
    async fn test_file_list_invalid_before_cursor_format() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-invalid-before-user").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&before=invalid-cursor",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
    }

    #[tokio::test]
    async fn test_file_list_after_cursor_not_found() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-after-notfound-user").await;

        // Use a valid file ID format but non-existent file
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&after=file-00000000-0000-0000-0000-000000000000",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_file_list_before_cursor_not_found() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-before-notfound-user").await;

        // Use a valid file ID format but non-existent file
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&before=file-00000000-0000-0000-0000-000000000000",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_cursor");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_file_list_invalid_purpose() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-invalid-purpose-user").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&purpose=invalid-purpose",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_purpose");
    }

    #[tokio::test]
    async fn test_file_list_limit_capped_at_100() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-limit-cap-user").await;

        // Upload one file
        let file_id = upload_file_for_list(&app, "user", &owner_id, "doc.txt", None).await;

        // Request with limit > 100 (should be capped)
        let (status, body) = get_json(
            &app,
            &format!(
                "/api/v1/files?owner_type=user&owner_id={}&limit=500",
                owner_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], file_id);
    }

    #[tokio::test]
    async fn test_file_list_validates_file_metadata() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-list-metadata-user").await;

        // Upload a file
        let _file_id =
            upload_file_for_list(&app, "user", &owner_id, "metadata-test.txt", None).await;

        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=user&owner_id={}", owner_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);

        let file = &data[0];
        assert_eq!(file["object"], "file");
        assert!(file["id"].as_str().unwrap().starts_with("file-"));
        assert_eq!(file["filename"], "metadata-test.txt");
        assert_eq!(file["purpose"], "assistants"); // Default purpose
        assert!(file["bytes"].is_number());
        assert!(file["created_at"].is_string());
    }

    // ============================================================================
    // File Get (GET /v1/files/{file_id})
    // ============================================================================

    #[tokio::test]
    async fn test_file_get_basic() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-get-basic-user").await;

        // Upload a file first
        let file_id = upload_file_for_list(&app, "user", &owner_id, "get-test.txt", None).await;

        // GET the file by ID
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "file");
        assert_eq!(body["id"], file_id);
        assert_eq!(body["filename"], "get-test.txt");
        assert_eq!(body["purpose"], "assistants");
        assert!(body["bytes"].is_number());
        assert!(body["created_at"].is_string());
        assert_eq!(body["owner_type"], "user");
        assert_eq!(body["owner_id"], owner_id);
    }

    #[tokio::test]
    async fn test_file_get_not_found() {
        let app = test_app().await;

        // Try to GET a non-existent file
        let non_existent_id = "file-00000000-0000-0000-0000-000000000000";
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", non_existent_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_file_get_invalid_id_format() {
        let app = test_app().await;

        // Try to GET with an invalid file ID format
        let (status, _body) = get_json(&app, "/api/v1/files/not-a-valid-uuid").await;

        // Invalid path parameter format returns 400 (Axum's default path rejection)
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_get_with_purpose() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-get-purpose-user").await;

        // Upload a file with a specific purpose
        let file_id =
            upload_file_for_list(&app, "user", &owner_id, "batch-file.jsonl", Some("batch")).await;

        // GET the file and verify purpose is preserved
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["purpose"], "batch");
        assert_eq!(body["filename"], "batch-file.jsonl");
    }

    #[tokio::test]
    async fn test_file_get_organization_owner() {
        let app = test_app().await;
        let org_id = create_org_for_files(&app, "file-get-org").await;

        // Upload a file owned by organization
        let file_id =
            upload_file_for_list(&app, "organization", &org_id, "org-file.txt", None).await;

        // GET the file and verify owner info
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["owner_type"], "organization");
        assert_eq!(body["owner_id"], org_id);
    }

    #[tokio::test]
    async fn test_file_get_project_owner() {
        let app = test_app().await;
        let _org_id = create_org_for_files(&app, "file-get-proj-org").await;
        let project_id = create_project_for_files(&app, "file-get-proj-org", "file-get-proj").await;

        // Upload a file owned by project
        let file_id =
            upload_file_for_list(&app, "project", &project_id, "project-file.txt", None).await;

        // GET the file and verify owner info
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["owner_type"], "project");
        assert_eq!(body["owner_id"], project_id);
    }

    #[tokio::test]
    async fn test_file_get_team_owner() {
        let app = test_app().await;
        let _org_id = create_org_for_files(&app, "file-get-team-org").await;
        let team_id = create_team_for_files(&app, "file-get-team-org", "file-get-team").await;

        // Upload a file owned by team
        let file_id = upload_file_for_list(&app, "team", &team_id, "team-file.txt", None).await;

        // GET the file and verify owner info
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["owner_type"], "team");
        assert_eq!(body["owner_id"], team_id);
    }

    #[tokio::test]
    async fn test_file_get_validates_all_response_fields() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-get-fields-user").await;

        // Upload a file
        let file_id = upload_file_for_list(&app, "user", &owner_id, "fields-test.txt", None).await;

        // GET the file
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);

        // Validate all expected fields are present
        assert!(body["id"].is_string(), "id should be a string");
        assert!(body["object"].is_string(), "object should be a string");
        assert!(body["filename"].is_string(), "filename should be a string");
        assert!(body["purpose"].is_string(), "purpose should be a string");
        assert!(body["bytes"].is_number(), "bytes should be a number");
        assert!(
            body["created_at"].is_string(),
            "created_at should be a string"
        );
        assert!(
            body["owner_type"].is_string(),
            "owner_type should be a string"
        );
        assert!(body["owner_id"].is_string(), "owner_id should be a string");
        assert!(body["status"].is_string(), "status should be a string");

        // Verify specific values
        assert_eq!(body["object"], "file");
        assert_eq!(body["status"], "uploaded"); // Default status after upload
    }

    // ============================================================================
    // File Content Download Tests
    // ============================================================================

    #[tokio::test]
    async fn test_file_content_download_basic() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-basic-user").await;

        // Upload a file with known content
        let expected_content = b"Hello, this is test file content for download!";
        let file_id = upload_file_with_content(
            &app,
            "user",
            &owner_id,
            "download-test.txt",
            expected_content,
        )
        .await;

        // Download the content
        let (status, headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, expected_content);

        // Verify headers are present
        assert!(headers.contains_key("content-type"));
        assert!(headers.contains_key("content-disposition"));
    }

    #[tokio::test]
    async fn test_file_content_download_not_found() {
        let app = test_app().await;

        // Try to download content for non-existent file
        let non_existent_id = "file-00000000-0000-0000-0000-000000000000";
        let (status, _headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", non_existent_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn test_file_content_download_invalid_id_format() {
        let app = test_app().await;

        // Try to download with invalid file ID format
        let (status, _headers, _body) =
            get_raw(&app, "/api/v1/files/not-a-valid-uuid/content").await;

        // Invalid path parameter format returns 400 (Axum's default path rejection)
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_content_download_content_type_header() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-type-user").await;

        // Upload a text file
        let file_id =
            upload_file_with_content(&app, "user", &owner_id, "test.txt", b"text content").await;

        let (status, headers, _body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);

        // Content-Type should default to application/octet-stream (since we upload as binary)
        let content_type = headers
            .get("content-type")
            .expect("content-type header should be present")
            .to_str()
            .unwrap();
        assert_eq!(content_type, "application/octet-stream");
    }

    #[tokio::test]
    async fn test_file_content_download_content_disposition_header() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-disp-user").await;

        // Upload a file with a specific filename
        let file_id =
            upload_file_with_content(&app, "user", &owner_id, "my-document.pdf", b"PDF content")
                .await;

        let (status, headers, _body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);

        // Content-Disposition should include the filename
        let disposition = headers
            .get("content-disposition")
            .expect("content-disposition header should be present")
            .to_str()
            .unwrap();
        assert_eq!(disposition, "attachment; filename=\"my-document.pdf\"");
    }

    #[tokio::test]
    async fn test_file_content_download_binary_content() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-binary-user").await;

        // Upload binary content (non-UTF8) - use .png extension since .bin is not allowed
        let binary_content: Vec<u8> = (0..=255).collect();
        let file_id =
            upload_file_with_content(&app, "user", &owner_id, "binary.png", &binary_content).await;

        let (status, _headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, binary_content);
    }

    #[tokio::test]
    async fn test_file_content_download_unicode_filename() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-unicode-user").await;

        // Upload a file with unicode filename
        let file_id = upload_file_with_content(
            &app,
            "user",
            &owner_id,
            "文档-日本語-émojis-🎉.txt",
            b"Unicode filename test",
        )
        .await;

        let (status, headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"Unicode filename test");

        // Content-Disposition header contains unicode - check using raw bytes
        let disposition = headers
            .get("content-disposition")
            .expect("content-disposition header should be present");
        // Convert to bytes and check for presence of expected filename
        let disposition_bytes = disposition.as_bytes();
        assert!(disposition_bytes.starts_with(b"attachment; filename=\""));
        // The unicode filename should be present in the header value
        let expected_filename = "文档-日本語-émojis-🎉.txt".as_bytes();
        assert!(
            disposition_bytes
                .windows(expected_filename.len())
                .any(|window| window == expected_filename),
            "Content-Disposition should contain the unicode filename"
        );
    }

    #[tokio::test]
    async fn test_file_content_download_organization_owner() {
        let app = test_app().await;
        let org_id = create_org_for_files(&app, "file-content-org").await;

        // Upload a file owned by organization
        let file_id = upload_file_with_content(
            &app,
            "organization",
            &org_id,
            "org-file.txt",
            b"Org content",
        )
        .await;

        let (status, _headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"Org content");
    }

    #[tokio::test]
    async fn test_file_content_download_empty_file() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-content-empty-user").await;

        // Upload an empty file
        let file_id = upload_file_with_content(&app, "user", &owner_id, "empty.txt", b"").await;

        let (status, _headers, body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
    }

    // ============================================================================
    // File Delete Tests
    // ============================================================================

    #[tokio::test]
    async fn test_file_delete_basic() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-delete-basic-user").await;

        // Upload a file
        let file_id = upload_file_for_list(&app, "user", &owner_id, "delete-me.txt", None).await;

        // Delete the file
        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], file_id);
        assert_eq!(body["object"], "file");
        assert_eq!(body["deleted"], true);
    }

    #[tokio::test]
    async fn test_file_delete_not_found() {
        let app = test_app().await;
        let fake_id = format!("file-{}", uuid::Uuid::new_v4());

        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_file_delete_invalid_id_format() {
        let app = test_app().await;

        let (status, _body) = delete_json(&app, "/api/v1/files/not-a-valid-uuid").await;

        // Invalid UUID format returns 400 (bad request due to path parsing)
        // Axum path rejection may not include a JSON body
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_file_delete_file_in_use() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "file-delete-in-use-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "File In Use Test Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file and add it to the vector store
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "in-use-file.txt").await;
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Try to delete the file (should fail - file is in use)
        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "file_in_use");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("referenced")
        );
    }

    #[tokio::test]
    async fn test_file_delete_after_removing_from_vector_store() {
        let app = test_app_with_file_search().await;
        let org_id = create_org_for_vector_store(&app, "file-delete-after-remove-org").await;

        // Create a vector store
        let (status, vs) = post_json(
            &app,
            "/api/v1/vector_stores",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "Remove Then Delete Store"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let vs_id = vs["id"].as_str().unwrap();

        // Upload a file and add it to the vector store
        let file_id =
            upload_file_for_vector_store(&app, "organization", &org_id, "remove-then-delete.txt")
                .await;
        let (status, _) = post_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files", vs_id),
            json!({"file_id": file_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Remove the file from the vector store
        let (status, _) = delete_json(
            &app,
            &format!("/api/v1/vector_stores/{}/files/{}", vs_id, file_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Now delete the file (should succeed)
        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], file_id);
        assert_eq!(body["object"], "file");
        assert_eq!(body["deleted"], true);
    }

    #[tokio::test]
    async fn test_file_delete_verify_actually_deleted() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-delete-verify-user").await;

        // Upload a file
        let file_id =
            upload_file_for_list(&app, "user", &owner_id, "verify-delete.txt", None).await;

        // Verify file exists
        let (status, _) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Delete the file
        let (status, _) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Verify file no longer exists
        let (status, body) = get_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn test_file_delete_organization_owner() {
        let app = test_app().await;
        let org_id = create_org_for_files(&app, "file-delete-org-owner").await;

        // Upload a file owned by the organization
        let file_id =
            upload_file_for_list(&app, "organization", &org_id, "org-delete.txt", None).await;

        // Delete the file
        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], true);
    }

    #[tokio::test]
    async fn test_file_delete_double_delete() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-delete-double-user").await;

        // Upload a file
        let file_id =
            upload_file_for_list(&app, "user", &owner_id, "double-delete.txt", None).await;

        // Delete the file (first time - should succeed)
        let (status, _) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Try to delete again (should fail - file no longer exists)
        let (status, body) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn test_file_delete_content_not_accessible() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-delete-content-user").await;

        // Upload a file with specific content
        let file_id = upload_file_with_content(
            &app,
            "user",
            &owner_id,
            "content-delete.txt",
            b"secret data",
        )
        .await;

        // Delete the file
        let (status, _) = delete_json(&app, &format!("/api/v1/files/{}", file_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Verify content is not accessible
        let (status, _headers, _body) =
            get_raw(&app, &format!("/api/v1/files/{}/content", file_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_file_delete_not_in_list() {
        let app = test_app().await;
        let owner_id = create_user_for_files(&app, "file-delete-list-user").await;

        // Upload two files
        let file1_id = upload_file_for_list(&app, "user", &owner_id, "keep-me.txt", None).await;
        let file2_id = upload_file_for_list(&app, "user", &owner_id, "delete-me.txt", None).await;

        // Delete the second file
        let (status, _) = delete_json(&app, &format!("/api/v1/files/{}", file2_id)).await;
        assert_eq!(status, StatusCode::OK);

        // List files - should only contain the first file
        let (status, body) = get_json(
            &app,
            &format!("/api/v1/files?owner_type=user&owner_id={}", owner_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let files = body["data"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["id"], file1_id);
    }

    // ============================================================================
    // Image Generation Tests
    // ============================================================================

    #[tokio::test]
    async fn test_image_generation_basic() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A cute baby sea otter",
                "model": "test/test-model"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["created"].is_number());
        assert!(body["data"].is_array());

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert!(data[0]["url"].is_string());
        assert!(data[0]["revised_prompt"].is_string());
    }

    #[tokio::test]
    async fn test_image_generation_multiple_images() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A sunset over mountains",
                "model": "test/test-model",
                "n": 3
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);

        let data = body["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);

        // Each image should have a unique URL
        let urls: Vec<&str> = data
            .iter()
            .map(|img| img["url"].as_str().unwrap())
            .collect();
        assert!(urls[0] != urls[1] && urls[1] != urls[2]);
    }

    #[tokio::test]
    async fn test_image_generation_with_size() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A beautiful landscape",
                "model": "test/test-model",
                "size": "1024x1024"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_generation_with_quality() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A detailed portrait",
                "model": "test/test-model",
                "quality": "hd"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_generation_with_style() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "An abstract painting",
                "model": "test/test-model",
                "style": "vivid"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_generation_missing_prompt() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/images/generations",
            json!({
                "model": "test/test-model"
            }),
        )
        .await;

        // Validation errors return 422 UNPROCESSABLE_ENTITY
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body.to_lowercase().contains("prompt"),
            "Expected error about 'prompt', got: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_image_generation_invalid_n_value() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "Test image",
                "model": "test/test-model",
                "n": 0
            }),
        )
        .await;

        // Business logic validation returns 400 BAD_REQUEST for invalid n value
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Should contain error about n value
        assert!(!body.is_empty(), "Expected error response, got empty body");
    }

    #[tokio::test]
    async fn test_image_generation_n_exceeds_max() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "Test image",
                "model": "test/test-model",
                "n": 100
            }),
        )
        .await;

        // Business logic validation returns 400 BAD_REQUEST for n exceeding max
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Should contain error about n value
        assert!(!body.is_empty(), "Expected error response, got empty body");
    }

    #[tokio::test]
    async fn test_image_generation_unknown_provider() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "Test image",
                "model": "unknown-provider/model"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].is_object());
    }

    #[tokio::test]
    async fn test_image_generation_with_user_field() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A test image",
                "model": "test/test-model",
                "user": "user-12345"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_generation_response_format_url() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "A test image",
                "model": "test/test-model",
                "response_format": "url"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().unwrap();
        assert!(data[0]["url"].is_string());
    }

    #[tokio::test]
    async fn test_image_generation_unicode_prompt() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/images/generations",
            json!({
                "prompt": "Un chat mignon avec des étoiles",
                "model": "test/test-model"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_edit_basic() {
        let app = test_app().await;

        // Create a minimal PNG file (1x1 transparent pixel)
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49,
            0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D,
            0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60,
            0x82,
        ];

        // Build multipart form
        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Add image field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"image\"; filename=\"test.png\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body_bytes.extend_from_slice(&png_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        // Add prompt field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt\"\r\n\r\n");
        body_bytes.extend_from_slice(b"Add a rainbow\r\n");

        // Add model field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        // End boundary
        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/images/edits")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_edit_with_mask() {
        let app = test_app().await;

        // Create a minimal PNG file (1x1 transparent pixel)
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Add image field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"image\"; filename=\"test.png\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body_bytes.extend_from_slice(&png_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        // Add mask field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"mask\"; filename=\"mask.png\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body_bytes.extend_from_slice(&png_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        // Add prompt field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt\"\r\n\r\n");
        body_bytes.extend_from_slice(b"Replace masked area with a cat\r\n");

        // Add model field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/images/edits")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["data"].is_array());
    }

    #[tokio::test]
    async fn test_image_variation_basic() {
        let app = test_app().await;

        // Create a minimal PNG file
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Add image field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"image\"; filename=\"test.png\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
        body_bytes.extend_from_slice(&png_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        // Add model field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/images/variations")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["data"].is_array());
    }

    // ============================================================================
    // Audio Speech (TTS) Tests
    // ============================================================================

    #[tokio::test]
    async fn test_audio_speech_basic() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "test/test-model",
                "input": "Hello, this is a test.",
                "voice": "alloy"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        // Response should be audio data (not JSON)
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn test_audio_speech_with_response_format() {
        let app = test_app().await;

        // Test MP3 format (default)
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/speech")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&json!({
                    "model": "test/test-model",
                    "input": "Testing different formats",
                    "voice": "nova",
                    "response_format": "mp3"
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "audio/mpeg"
        );
    }

    #[tokio::test]
    async fn test_audio_speech_opus_format() {
        let app = test_app().await;

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/speech")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&json!({
                    "model": "test/test-model",
                    "input": "Testing opus format",
                    "voice": "echo",
                    "response_format": "opus"
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "audio/opus"
        );
    }

    #[tokio::test]
    async fn test_audio_speech_all_voices() {
        let app = test_app().await;
        let voices = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

        for voice in voices {
            let (status, _) = post_json_raw(
                &app,
                "/api/v1/audio/speech",
                json!({
                    "model": "test/test-model",
                    "input": "Testing voice",
                    "voice": voice
                }),
            )
            .await;

            assert_eq!(status, StatusCode::OK, "Voice {} should work", voice);
        }
    }

    #[tokio::test]
    async fn test_audio_speech_with_speed() {
        let app = test_app().await;

        let (status, _) = post_json_raw(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "test/test-model",
                "input": "Testing speed parameter",
                "voice": "alloy",
                "speed": 1.5
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn test_audio_speech_missing_input() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "test/test-model",
                "voice": "alloy"
            }),
        )
        .await;

        // Validation errors return 422 UNPROCESSABLE_ENTITY
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // The validation error message should mention the missing 'input' field
        assert!(
            body.to_lowercase().contains("input"),
            "Expected error about 'input', got: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_audio_speech_missing_voice() {
        let app = test_app().await;

        let (status, body) = post_json_raw(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "test/test-model",
                "input": "Hello"
            }),
        )
        .await;

        // Validation errors return 422 UNPROCESSABLE_ENTITY
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // The validation error message should mention the missing 'voice' field
        assert!(
            body.to_lowercase().contains("voice"),
            "Expected error about 'voice', got: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_audio_speech_invalid_speed() {
        let app = test_app().await;

        // Speed too low (must be between 0.25 and 4.0)
        let (status, body) = post_json_raw(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "test/test-model",
                "input": "Testing invalid speed",
                "voice": "alloy",
                "speed": 0.1
            }),
        )
        .await;

        // Speed validation returns 400 BAD_REQUEST (range validation)
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // The error message should mention speed validation
        assert!(
            body.to_lowercase().contains("speed"),
            "Expected error about 'speed', got: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_audio_speech_unknown_provider() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/api/v1/audio/speech",
            json!({
                "model": "unknown-provider/model",
                "input": "Test",
                "voice": "alloy"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].is_object());
    }

    // ============================================================================
    // Audio Transcription Tests
    // ============================================================================

    #[tokio::test]
    async fn test_audio_transcription_basic() {
        let app = test_app().await;

        // Create mock audio bytes (minimal valid structure)
        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Add file field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        // Add model field
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["text"].is_string());
    }

    #[tokio::test]
    async fn test_audio_transcription_verbose_json() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"verbose_json\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["text"].is_string());
        assert!(json["duration"].is_number());
        assert!(json["words"].is_array());
    }

    #[tokio::test]
    async fn test_audio_transcription_text_format() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"text\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );
    }

    #[tokio::test]
    async fn test_audio_transcription_srt_format() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"srt\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("-->"));
    }

    #[tokio::test]
    async fn test_audio_transcription_vtt_format() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"vtt\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("WEBVTT"));
    }

    #[tokio::test]
    async fn test_audio_transcription_missing_file() {
        let app = test_app().await;

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Only add model, no file
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_audio_transcription_missing_model() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        // Only add file, no model
        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/transcriptions")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ============================================================================
    // Audio Translation Tests
    // ============================================================================

    #[tokio::test]
    async fn test_audio_translation_basic() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/translations")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["text"].is_string());
    }

    #[tokio::test]
    async fn test_audio_translation_verbose_json() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"verbose_json\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/translations")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        assert_eq!(status, StatusCode::OK);
        assert!(json["text"].is_string());
        assert!(json["duration"].is_number());
    }

    #[tokio::test]
    async fn test_audio_translation_text_format() {
        let app = test_app().await;

        let audio_bytes: Vec<u8> = vec![
            0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"test.mp3\"\r\n",
        );
        body_bytes.extend_from_slice(b"Content-Type: audio/mpeg\r\n\r\n");
        body_bytes.extend_from_slice(&audio_bytes);
        body_bytes.extend_from_slice(b"\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes
            .extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
        body_bytes.extend_from_slice(b"text\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/translations")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );
    }

    #[tokio::test]
    async fn test_audio_translation_missing_file() {
        let app = test_app().await;

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let mut body_bytes = Vec::new();

        body_bytes.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body_bytes.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body_bytes.extend_from_slice(b"test/test-model\r\n");

        body_bytes.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/audio/translations")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={}", boundary),
            )
            .body(Body::from(body_bytes))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
