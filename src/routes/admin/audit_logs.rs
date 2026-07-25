use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use serde::Serialize;
use uuid::Uuid;

use super::error::AdminError;
use crate::{
    AppState,
    middleware::AuthzContext,
    models::{AuditLog, AuditLogQuery},
    openapi::PaginationMeta,
    services::Services,
};

/// Paginated list of audit logs
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct AuditLogListResponse {
    /// List of audit log entries
    pub data: Vec<AuditLog>,
    /// Pagination metadata
    pub pagination: PaginationMeta,
}

fn get_services(state: &AppState) -> Result<&Services, AdminError> {
    state.services.as_ref().ok_or(AdminError::ServicesRequired)
}

/// List audit logs
///
/// When neither `from` nor `to` is supplied, results default to the last
/// 7 days (the audit log is append-only and an unbounded scan would hit
/// the entire table). Pass an explicit `from` for older entries.
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/audit-logs",
    tag = "audit-logs",
    operation_id = "audit_log_list",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "List of audit log entries (defaults to the last 7 days when no from/to range is given)", body = AuditLogListResponse),
        (status = 400, description = "Invalid cursor or direction", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn list(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Query(query): Query<AuditLogQuery>,
) -> Result<Json<AuditLogListResponse>, AdminError> {
    let services = get_services(&state)?;

    // Clamp to [1, 1000] like the usage-log endpoints: previously any limit
    // was honored verbatim (including negative values, which SQLite treats
    // as "no limit").
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);

    // Validate direction if provided
    if let Some(ref dir) = query.direction
        && dir != "forward"
        && dir != "backward"
    {
        return Err(AdminError::BadRequest(format!(
            "Invalid direction '{}': must be 'forward' or 'backward'",
            dir
        )));
    }

    // Reject garbage cursors with the 400 the OpenAPI annotation promises;
    // the repo layer surfaces decode failures as 500 database errors.
    if let Some(ref c) = query.cursor {
        crate::db::repos::Cursor::decode(c)
            .map_err(|e| AdminError::BadRequest(format!("Invalid cursor: {}", e)))?;
    }

    // Cap unbounded scans: when no time range is supplied, default to the last
    // 7 days. The audit log is append-only and grows fast; an unfiltered list
    // hits the entire table with `ORDER BY ts DESC` which can DoS the gateway.
    let mut query = query;
    query.limit = Some(limit);
    if query.from.is_none() && query.to.is_none() {
        query.from = Some(chrono::Utc::now() - chrono::Duration::days(7));
    }

    // Constrain `org_id` to the caller's organization. Without this, anyone
    // with the `audit_log:list` permission could read any tenant's logs by
    // sending an arbitrary `?org_id=` query parameter. Subjects with no
    // membership (e.g. super-admins) are allowed through unconstrained.
    //
    // Users in this codebase only ever belong to one organization, so
    // `org_ids` is a single-element set in practice. We pin to that single
    // org rather than aggregating across `org_ids` — multi-org membership
    // would require a different model (and is unreachable today).
    if let Some(membership) = authz.subject.org_ids.first() {
        let scoped: Uuid = membership.parse().map_err(|_| {
            AdminError::Internal(
                "audit_log:list authz subject has a non-UUID org membership".to_string(),
            )
        })?;
        match query.org_id {
            Some(requested) if requested != scoped => {
                return Err(AdminError::Forbidden(
                    "audit_log:list scoped outside your organization".to_string(),
                ));
            }
            _ => {
                query.org_id = Some(scoped);
            }
        }
    }

    // Run authz with the effective org scope so policies see the tenant they
    // need to allow/deny against. `authz.require` evaluated with all-None
    // would let anyone with `audit_log:list` see logs across orgs.
    let org_scope = query.org_id.map(|id| id.to_string());
    authz.require("audit_log", "list", None, org_scope.as_deref(), None, None)?;

    let result = services.audit_logs.list(query).await?;

    let pagination = PaginationMeta::with_cursors(
        limit,
        result.has_more,
        result.cursors.next.map(|c| c.encode()),
        result.cursors.prev.map(|c| c.encode()),
    );

    Ok(Json(AuditLogListResponse {
        data: result.items,
        pagination,
    }))
}

/// Get an audit log entry by ID
#[cfg_attr(feature = "utoipa", utoipa::path(
    get,
    path = "/admin/v1/audit-logs/{id}",
    tag = "audit-logs",
    operation_id = "audit_log_get",
    params(("id" = Uuid, Path, description = "Audit log entry ID")),
    responses(
        (status = 200, description = "Audit log entry found", body = AuditLog),
        (status = 404, description = "Audit log entry not found", body = crate::openapi::ErrorResponse),
    )
))]
pub async fn get(
    State(state): State<AppState>,
    Extension(authz): Extension<AuthzContext>,
    Path(id): Path<Uuid>,
) -> Result<Json<AuditLog>, AdminError> {
    let services = get_services(&state)?;

    // Pre-fetch the row so authz can see the entry's org/project rather than
    // an all-None scope; otherwise a permissive policy would expose every
    // tenant's audit history through this endpoint.
    let entry = services
        .audit_logs
        .get_by_id(id)
        .await?
        .ok_or_else(|| AdminError::NotFound("Audit log entry not found".to_string()))?;

    let id_str = id.to_string();
    let org_scope = entry.org_id.map(|o| o.to_string());
    let project_scope = entry.project_id.map(|p| p.to_string());
    authz.require(
        "audit_log",
        "read",
        Some(&id_str),
        org_scope.as_deref(),
        None,
        project_scope.as_deref(),
    )?;

    Ok(Json(entry))
}
