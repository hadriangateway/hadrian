use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use axum_valid::Valid;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{AuditActor, error::AdminError, organizations::ListQuery};
use crate::{
    AppState,
    middleware::{AdminAuth, AuthzContext, ClientInfo},
    models::{
        CreateAuditLog, CreateModelPricing, DbModelPricing, PricingOwner, UpdateModelPricing,
    },
    openapi::PaginationMeta,
    services::Services,
};

/// Authorization scope derived from a pricing entry's owner. Maps the row's
/// PricingOwner to the (resource_id, org_id, team_id, project_id) tuple that
/// `authz.require` consumes.
struct PricingAuthzScope {
    resource_id: Option<String>,
    org: Option<String>,
    team: Option<String>,
    project: Option<String>,
}

fn pricing_authz_scope(owner: &PricingOwner, fallback_id: &str) -> PricingAuthzScope {
    match owner {
        PricingOwner::Global => PricingAuthzScope {
            resource_id: Some(fallback_id.to_string()),
            org: None,
            team: None,
            project: None,
        },
        PricingOwner::Organization { org_id } => PricingAuthzScope {
            resource_id: Some(fallback_id.to_string()),
            org: Some(org_id.to_string()),
            team: None,
            project: None,
        },
        PricingOwner::Team { team_id } => PricingAuthzScope {
            resource_id: Some(fallback_id.to_string()),
            org: None,
            team: Some(team_id.to_string()),
            project: None,
        },
        PricingOwner::Project { project_id } => PricingAuthzScope {
            resource_id: Some(fallback_id.to_string()),
            org: None,
            team: None,
            project: Some(project_id.to_string()),
        },
        PricingOwner::User { user_id } => PricingAuthzScope {
            resource_id: Some(user_id.to_string()),
            org: None,
            team: None,
            project: None,
        },
    }
}

/// Paginated list of model pricing entries
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ModelPricingListResponse {
    /// List of model pricing entries
    pub data: Vec<DbModelPricing>,
    /// Pagination metadata
    pub pagination: PaginationMeta,
}

fn get_services(state: &AppState) -> Result<&Services, AdminError> {
    state.services.as_ref().ok_or(AdminError::ServicesRequired)
}

/// Best-effort refresh of the in-memory pricing overlay after a mutation so
/// admin pricing changes affect billing on this replica immediately. Other
/// replicas converge via the periodic overlay refresh job. Failures are
/// logged, not surfaced: the DB write already succeeded and the overlay
/// self-heals on the next refresh.
async fn refresh_pricing_overlay(state: &AppState, services: &Services) {
    let Some(overlay) = state.pricing.overlay() else {
        return;
    };
    if let Err(e) = services.model_pricing.refresh_overlay(overlay).await {
        tracing::warn!(
            error = %e,
            "Failed to refresh model pricing overlay after admin mutation"
        );
    }
}

/// Create a new model pricing entry
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/admin/v1/model-pricing",
    tag = "model-pricing",
    operation_id = "model_pricing_create",
    request_body = CreateModelPricing,
    responses(
        (status = 201, description = "Model pricing created", body = DbModelPricing),
        (status = 409, description = "Conflict (pricing already exists)", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn create(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Extension(client_info): Extension<ClientInfo>,
    Valid(Json(input)): Valid<Json<CreateModelPricing>>,
) -> Result<(StatusCode, Json<DbModelPricing>), AdminError> {
    // Authorize against the requested owner scope so a permissive policy
    // can't be tricked into accepting a global write request from someone
    // who only has org-scoped privileges.
    let scope = pricing_authz_scope(&input.owner, "");
    authz.require(
        "model_pricing",
        "create",
        scope.resource_id.as_deref(),
        scope.org.as_deref(),
        scope.team.as_deref(),
        scope.project.as_deref(),
    )?;
    let services = get_services(&state)?;
    let actor = AuditActor::from(&admin_auth);

    let pricing = services.model_pricing.create(input).await?;
    refresh_pricing_overlay(&state, services).await;

    // Extract org_id and project_id from owner for audit log context
    let (org_id, project_id) = match &pricing.owner {
        crate::models::PricingOwner::Organization { org_id } => (Some(*org_id), None),
        crate::models::PricingOwner::Project { project_id } => (None, Some(*project_id)),
        _ => (None, None),
    };

    // Log audit event (fire-and-forget)
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: actor.actor_type,
            actor_id: actor.actor_id,
            action: "model_pricing.create".to_string(),
            resource_type: "model_pricing".to_string(),
            resource_id: pricing.id,
            org_id,
            project_id,
            details: json!({
                "provider": pricing.provider,
                "model": pricing.model,
                "owner": pricing.owner,
                "input_per_1m_tokens": pricing.input_per_1m_tokens,
                "output_per_1m_tokens": pricing.output_per_1m_tokens,
            }),
            ip_address: client_info.ip_address,
            user_agent: client_info.user_agent,
        })
        .await;

    Ok((StatusCode::CREATED, Json(pricing)))
}

/// Get a model pricing entry by ID
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/model-pricing/{id}",
    tag = "model-pricing",
    operation_id = "model_pricing_get",
    params(("id" = Uuid, Path, description = "Model pricing ID")),
    responses(
        (status = 200, description = "Model pricing found", body = DbModelPricing),
        (status = 404, description = "Model pricing not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<DbModelPricing>, AdminError> {
    let services = get_services(&state)?;

    // Pre-fetch the row so authz can scope by the pricing entry's actual
    // owner; `authz.require` with all-None lets a permissive policy expose
    // every tenant's pricing through a single endpoint.
    let pricing = services
        .model_pricing
        .get_by_id(id)
        .await?
        .ok_or_else(|| AdminError::NotFound("Model pricing not found".to_string()))?;
    let id_str = id.to_string();
    let scope = pricing_authz_scope(&pricing.owner, &id_str);
    authz.require(
        "model_pricing",
        "read",
        scope.resource_id.as_deref(),
        scope.org.as_deref(),
        scope.team.as_deref(),
        scope.project.as_deref(),
    )?;

    Ok(Json(pricing))
}

/// Update a model pricing entry
#[cfg_attr(feature = "utoipa", utoipa::path(
    patch,
    path = "/admin/v1/model-pricing/{id}",
    tag = "model-pricing",
    operation_id = "model_pricing_update",
    params(("id" = Uuid, Path, description = "Model pricing ID")),
    request_body = UpdateModelPricing,
    responses(
        (status = 200, description = "Model pricing updated", body = DbModelPricing),
        (status = 404, description = "Model pricing not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn update(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Extension(client_info): Extension<ClientInfo>,
    Path(id): Path<Uuid>,
    Valid(Json(input)): Valid<Json<UpdateModelPricing>>,
) -> Result<Json<DbModelPricing>, AdminError> {
    let services = get_services(&state)?;
    let actor = AuditActor::from(&admin_auth);

    // Resolve the existing pricing row so authz sees its real owner before
    // we mutate anything.
    let existing = services
        .model_pricing
        .get_by_id(id)
        .await?
        .ok_or_else(|| AdminError::NotFound("Model pricing not found".to_string()))?;
    let id_str = id.to_string();
    let scope = pricing_authz_scope(&existing.owner, &id_str);
    authz.require(
        "model_pricing",
        "update",
        scope.resource_id.as_deref(),
        scope.org.as_deref(),
        scope.team.as_deref(),
        scope.project.as_deref(),
    )?;

    // Capture what's being changed for audit log
    let changes = json!({
        "input_per_1m_tokens": input.input_per_1m_tokens,
        "output_per_1m_tokens": input.output_per_1m_tokens,
        "per_image": input.per_image,
        "per_request": input.per_request,
        "cached_input_per_1m_tokens": input.cached_input_per_1m_tokens,
        "cache_write_per_1m_tokens": input.cache_write_per_1m_tokens,
        "reasoning_per_1m_tokens": input.reasoning_per_1m_tokens,
        "source": input.source,
    });

    let pricing = services.model_pricing.update(id, input).await?;
    refresh_pricing_overlay(&state, services).await;

    // Extract org_id and project_id from owner for audit log context
    let (org_id, project_id) = match &pricing.owner {
        crate::models::PricingOwner::Organization { org_id } => (Some(*org_id), None),
        crate::models::PricingOwner::Project { project_id } => (None, Some(*project_id)),
        _ => (None, None),
    };

    // Log audit event (fire-and-forget)
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: actor.actor_type,
            actor_id: actor.actor_id,
            action: "model_pricing.update".to_string(),
            resource_type: "model_pricing".to_string(),
            resource_id: pricing.id,
            org_id,
            project_id,
            details: json!({
                "provider": pricing.provider,
                "model": pricing.model,
                "changes": changes,
            }),
            ip_address: client_info.ip_address,
            user_agent: client_info.user_agent,
        })
        .await;

    Ok(Json(pricing))
}

/// Delete a model pricing entry
#[cfg_attr(feature = "utoipa", utoipa::path(
    delete,
    path = "/admin/v1/model-pricing/{id}",
    tag = "model-pricing",
    operation_id = "model_pricing_delete",
    params(("id" = Uuid, Path, description = "Model pricing ID")),
    responses(
        (status = 200, description = "Model pricing deleted"),
        (status = 404, description = "Model pricing not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn delete(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Extension(client_info): Extension<ClientInfo>,
    Path(id): Path<Uuid>,
) -> Result<Json<()>, AdminError> {
    let services = get_services(&state)?;
    let actor = AuditActor::from(&admin_auth);

    // Pre-fetch the pricing row so authz scopes by its real owner; reuse the
    // row for the audit log below.
    let pricing = services
        .model_pricing
        .get_by_id(id)
        .await?
        .ok_or_else(|| AdminError::NotFound("Model pricing not found".to_string()))?;
    let id_str = id.to_string();
    let scope = pricing_authz_scope(&pricing.owner, &id_str);
    authz.require(
        "model_pricing",
        "delete",
        scope.resource_id.as_deref(),
        scope.org.as_deref(),
        scope.team.as_deref(),
        scope.project.as_deref(),
    )?;

    // Extract org_id and project_id from owner for audit log context
    let (org_id, project_id) = match &pricing.owner {
        crate::models::PricingOwner::Organization { org_id } => (Some(*org_id), None),
        crate::models::PricingOwner::Project { project_id } => (None, Some(*project_id)),
        _ => (None, None),
    };

    // Capture details for audit log
    let provider = pricing.provider.clone();
    let model = pricing.model.clone();
    let owner = pricing.owner.clone();

    services.model_pricing.delete(id).await?;
    refresh_pricing_overlay(&state, services).await;

    // Log audit event (fire-and-forget)
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: actor.actor_type,
            actor_id: actor.actor_id,
            action: "model_pricing.delete".to_string(),
            resource_type: "model_pricing".to_string(),
            resource_id: id,
            org_id,
            project_id,
            details: json!({
                "provider": provider,
                "model": model,
                "owner": owner,
            }),
            ip_address: client_info.ip_address,
            user_agent: client_info.user_agent,
        })
        .await;

    Ok(Json(()))
}

/// List global model pricing
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/model-pricing",
    tag = "model-pricing",
    operation_id = "model_pricing_list_global",
    params(ListQuery),
    responses(
        (status = 200, description = "List of global model pricing", body = ModelPricingListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list_global(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ModelPricingListResponse>, AdminError> {
    authz.require("model_pricing", "list", None, None, None, None)?;
    let services = get_services(&state)?;

    let limit = query.limit.unwrap_or(100);
    let params = query.try_into_with_cursor()?;

    let result = services.model_pricing.list_global(params).await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(ModelPricingListResponse {
        data: result.items,
        pagination,
    }))
}

/// List model pricing for an organization
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/model-pricing",
    tag = "model-pricing",
    operation_id = "model_pricing_list_by_org",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ListQuery,
    ),
    responses(
        (status = 200, description = "List of organization model pricing", body = ModelPricingListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Organization not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list_by_org(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path(org_slug): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ModelPricingListResponse>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&org_slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization '{}' not found", org_slug)))?;

    authz.require(
        "model_pricing",
        "list",
        None,
        Some(&org.id.to_string()),
        None,
        None,
    )?;

    let limit = query.limit.unwrap_or(100);
    let params = query.try_into_with_cursor()?;

    let result = services.model_pricing.list_by_org(org.id, params).await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(ModelPricingListResponse {
        data: result.items,
        pagination,
    }))
}

/// List model pricing for a project
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/organizations/{org_slug}/projects/{project_slug}/model-pricing",
    tag = "model-pricing",
    operation_id = "model_pricing_list_by_project",
    params(
        ("org_slug" = String, Path, description = "Organization slug"),
        ("project_slug" = String, Path, description = "Project slug"),
        ListQuery,
    ),
    responses(
        (status = 200, description = "List of project model pricing", body = ModelPricingListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
        (status = 404, description = "Organization or project not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list_by_project(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path((org_slug, project_slug)): Path<(String, String)>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ModelPricingListResponse>, AdminError> {
    let services = get_services(&state)?;

    let org = services
        .organizations
        .get_by_slug(&org_slug)
        .await?
        .ok_or_else(|| AdminError::NotFound(format!("Organization '{}' not found", org_slug)))?;

    let project = services
        .projects
        .get_by_slug(org.id, &project_slug)
        .await?
        .ok_or_else(|| {
            AdminError::NotFound(format!(
                "Project '{}' not found in organization '{}'",
                project_slug, org_slug
            ))
        })?;

    authz.require(
        "model_pricing",
        "list",
        None,
        Some(&org.id.to_string()),
        None,
        Some(&project.id.to_string()),
    )?;

    let limit = query.limit.unwrap_or(100);
    let params = query.try_into_with_cursor()?;

    let result = services
        .model_pricing
        .list_by_project(project.id, params)
        .await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(ModelPricingListResponse {
        data: result.items,
        pagination,
    }))
}

/// List model pricing for a user
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/users/{user_id}/model-pricing",
    tag = "model-pricing",
    operation_id = "model_pricing_list_by_user",
    params(
        ("user_id" = Uuid, Path, description = "User ID"),
        ListQuery,
    ),
    responses(
        (status = 200, description = "List of user model pricing", body = ModelPricingListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list_by_user(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ModelPricingListResponse>, AdminError> {
    let user_id_str = user_id.to_string();
    authz.require(
        "model_pricing",
        "list",
        Some(&user_id_str),
        None,
        None,
        None,
    )?;
    let services = get_services(&state)?;

    let limit = query.limit.unwrap_or(100);
    let params = query.try_into_with_cursor()?;

    let result = services.model_pricing.list_by_user(user_id, params).await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(ModelPricingListResponse {
        data: result.items,
        pagination,
    }))
}

/// List all pricing for a specific provider (across all scopes)
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/model-pricing/provider/{provider}",
    tag = "model-pricing",
    operation_id = "model_pricing_list_by_provider",
    params(
        ("provider" = String, Path, description = "Provider name"),
        ListQuery,
    ),
    responses(
        (status = 200, description = "List of pricing for provider", body = ModelPricingListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list_by_provider(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path(provider): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<ModelPricingListResponse>, AdminError> {
    // Pass the provider name through `resource_id` so policies can scope by
    // provider; with all-None a permissive policy would expose every
    // tenant-scoped pricing row this endpoint surfaces.
    authz.require("model_pricing", "list", Some(&provider), None, None, None)?;
    let services = get_services(&state)?;

    let limit = query.limit.unwrap_or(100);
    let params = query.try_into_with_cursor()?;

    let result = services
        .model_pricing
        .list_by_provider(&provider, params)
        .await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(ModelPricingListResponse {
        data: result.items,
        pagination,
    }))
}

/// Upsert model pricing (create or update based on owner/provider/model)
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/admin/v1/model-pricing/upsert",
    tag = "model-pricing",
    operation_id = "model_pricing_upsert",
    request_body = CreateModelPricing,
    responses(
        (status = 200, description = "Model pricing upserted", body = DbModelPricing),
    )
))]
pub async fn upsert(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Extension(client_info): Extension<ClientInfo>,
    Valid(Json(input)): Valid<Json<CreateModelPricing>>,
) -> Result<Json<DbModelPricing>, AdminError> {
    let scope = pricing_authz_scope(&input.owner, "");
    authz.require(
        "model_pricing",
        "update",
        scope.resource_id.as_deref(),
        scope.org.as_deref(),
        scope.team.as_deref(),
        scope.project.as_deref(),
    )?;
    let services = get_services(&state)?;
    let actor = AuditActor::from(&admin_auth);

    let pricing = services.model_pricing.upsert(input).await?;
    refresh_pricing_overlay(&state, services).await;

    // Extract org_id and project_id from owner for audit log context
    let (org_id, project_id) = match &pricing.owner {
        crate::models::PricingOwner::Organization { org_id } => (Some(*org_id), None),
        crate::models::PricingOwner::Project { project_id } => (None, Some(*project_id)),
        _ => (None, None),
    };

    // Log audit event (fire-and-forget)
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: actor.actor_type,
            actor_id: actor.actor_id,
            action: "model_pricing.upsert".to_string(),
            resource_type: "model_pricing".to_string(),
            resource_id: pricing.id,
            org_id,
            project_id,
            details: json!({
                "provider": pricing.provider,
                "model": pricing.model,
                "owner": pricing.owner,
                "input_per_1m_tokens": pricing.input_per_1m_tokens,
                "output_per_1m_tokens": pricing.output_per_1m_tokens,
            }),
            ip_address: client_info.ip_address,
            user_agent: client_info.user_agent,
        })
        .await;

    Ok(Json(pricing))
}

/// Response for bulk upsert operation
#[derive(serde::Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct BulkUpsertResponse {
    /// Number of entries upserted
    pub count: usize,
}

/// Bulk upsert model pricing entries
#[cfg_attr(feature = "utoipa", utoipa::path(
    post,
    path = "/admin/v1/model-pricing/bulk",
    tag = "model-pricing",
    operation_id = "model_pricing_bulk_upsert",
    request_body = Vec<CreateModelPricing>,
    responses(
        (status = 200, description = "Bulk upsert completed", body = BulkUpsertResponse),
    )
))]
pub async fn bulk_upsert(
    State(state): State<AppState>,
    Extension(admin_auth): Extension<AdminAuth>,
    Extension(authz): Extension<AuthzContext>,
    Extension(client_info): Extension<ClientInfo>,
    Json(entries): Json<Vec<CreateModelPricing>>,
) -> Result<Json<BulkUpsertResponse>, AdminError> {
    // Bulk upserts span owners; require authz against every distinct owner
    // in the payload so a caller scoped to one tenant can't smuggle global
    // or cross-tenant pricing rows through this endpoint.
    let services = get_services(&state)?;
    let actor = AuditActor::from(&admin_auth);
    for entry in &entries {
        let scope = pricing_authz_scope(&entry.owner, "");
        authz.require(
            "model_pricing",
            "update",
            scope.resource_id.as_deref(),
            scope.org.as_deref(),
            scope.team.as_deref(),
            scope.project.as_deref(),
        )?;
    }

    // Capture summary for audit log before bulk operation
    let entry_count = entries.len();
    let unique_providers: std::collections::HashSet<String> =
        entries.iter().map(|e| e.provider.clone()).collect();

    let count = services.model_pricing.bulk_upsert(entries).await?;
    refresh_pricing_overlay(&state, services).await;

    // Log audit event (fire-and-forget)
    // Use Uuid::nil() as resource_id since this is a bulk operation
    let _ = services
        .audit_logs
        .create(CreateAuditLog {
            actor_type: actor.actor_type,
            actor_id: actor.actor_id,
            action: "model_pricing.bulk_upsert".to_string(),
            resource_type: "model_pricing".to_string(),
            resource_id: Uuid::nil(),
            org_id: None,
            project_id: None,
            details: json!({
                "entries_submitted": entry_count,
                "entries_upserted": count,
                "providers": unique_providers.into_iter().collect::<Vec<_>>(),
            }),
            ip_address: client_info.ip_address,
            user_agent: client_info.user_agent,
        })
        .await;

    Ok(Json(BulkUpsertResponse { count }))
}
