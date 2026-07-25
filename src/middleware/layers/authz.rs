//! Authorization middleware for enforcing policy-based access control.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    AppState,
    auth::AuthenticatedRequest,
    authz::{AuthzEngine, Subject},
    middleware::{AdminAuth, AuthzContext},
};

// AuthzContext struct and impl are in crate::middleware::types
// (always available on all targets). Middleware functions below use it.

/// Middleware that builds authorization context from the authenticated request.
/// This must run after admin_auth_middleware.
///
/// IMPORTANT: This middleware always inserts an AuthzContext, even when RBAC is disabled.
/// This ensures handlers can require AuthzContext (fail-closed) rather than using Option
/// (fail-open). When RBAC is disabled, the AuthzContext will allow all operations.
pub async fn authz_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthzResponse> {
    // Build the authorization engine from config
    let engine = match AuthzEngine::new(state.config.auth.rbac.clone()) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            tracing::error!(error = %e, "Failed to create authorization engine");
            return Err(AuthzResponse::InternalError(format!(
                "Authorization configuration error: {}",
                e
            )));
        }
    };

    // Extract request metadata for audit logging
    let request_ip = extract_client_ip(&req, &state.config.server.trusted_proxies);
    let request_user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Get audit service if available
    let audit_service = state.services.as_ref().map(|s| s.audit_logs.clone());

    // Build subject from AdminAuth if available, otherwise use empty subject
    // When RBAC is disabled, the engine will allow all operations regardless of subject
    let subject = if let Some(admin_auth) = req.extensions().get::<AdminAuth>() {
        let identity = &admin_auth.identity;
        let mapped_roles = engine.map_roles(&identity.roles);

        let subject = Subject::new()
            .with_external_id(&identity.external_id)
            .with_roles(mapped_roles)
            .with_org_ids(identity.org_ids.clone())
            .with_team_ids(identity.team_ids.clone())
            .with_project_ids(identity.project_ids.clone());

        // Add optional fields
        let subject = if let Some(user_id) = identity.user_id {
            subject.with_user_id(user_id.to_string())
        } else {
            subject
        };
        if let Some(email) = &identity.email {
            subject.with_email(email)
        } else {
            subject
        }
    } else if engine.is_enabled() {
        // RBAC is enabled but no AdminAuth - this is an error
        return Err(AuthzResponse::Unauthorized("Not authenticated".to_string()));
    } else {
        // RBAC is disabled, use empty subject (engine will allow all)
        Subject::new()
    };

    // Always add authz context to request (fail-closed pattern)
    // Admin endpoints use the main RBAC default effect (typically "deny")
    req.extensions_mut().insert(AuthzContext::new(
        subject,
        engine,
        state.policy_registry.clone(),
        audit_service,
        Some(state.task_tracker.clone()),
        request_ip,
        request_user_agent,
        state.config.auth.rbac.audit.clone(),
        state.config.auth.rbac.default_effect,
    ));

    Ok(next.run(req).await)
}

/// Extract client IP address from request, respecting trusted proxy configuration.
fn extract_client_ip(
    req: &Request,
    trusted_proxies: &crate::config::TrustedProxiesConfig,
) -> Option<String> {
    super::rate_limit::extract_client_ip(req, trusted_proxies).map(|ip| ip.to_string())
}

/// Permissive authorization middleware for unprotected (development) routes.
///
/// This middleware always inserts an AuthzContext with RBAC disabled,
/// which means all authorization checks will pass. This is used when
/// auth is not configured (development mode or external auth proxy).
///
/// SECURITY NOTE: Only use this for development or when an external auth
/// proxy handles authentication/authorization.
pub async fn permissive_authz_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthzResponse> {
    // Create a disabled RBAC config
    let mut disabled_config = state.config.auth.rbac.clone();
    disabled_config.enabled = false;

    let engine = match AuthzEngine::new(disabled_config) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            tracing::error!(error = %e, "Failed to create authorization engine");
            return Err(AuthzResponse::InternalError(format!(
                "Authorization configuration error: {}",
                e
            )));
        }
    };

    // Insert ClientInfo for unprotected routes (no admin middleware to
    // extract it). Populate the real client IP / user agent so audit rows
    // under auth.mode = none aren't blank.
    let client_info = crate::middleware::ClientInfo {
        ip_address: extract_client_ip(&req, &state.config.server.trusted_proxies),
        user_agent: req
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(String::from),
    };
    req.extensions_mut().insert(client_info);

    // Insert a default AdminAuth with system identity for unprotected routes.
    // This allows handlers to extract AdminAuth for audit logging purposes.
    // Use the default_user_id and default_org_id if available (for anonymous access).
    use crate::auth::Identity;
    req.extensions_mut().insert(AdminAuth {
        identity: Identity {
            external_id: "anonymous".to_string(),
            email: Some("anonymous@localhost".to_string()),
            name: Some("Anonymous User".to_string()),
            user_id: state.default_user_id,
            roles: vec!["admin".to_string()],
            idp_groups: Vec::new(),
            org_ids: state
                .default_org_id
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            team_ids: Vec::new(),
            project_ids: Vec::new(),
        },
    });

    // Insert permissive AuthzContext with empty subject
    // Since RBAC is disabled, all authorization checks will pass (no denials to log)
    req.extensions_mut()
        .insert(AuthzContext::permissive(engine));

    Ok(next.run(req).await)
}

/// Authorization middleware for API endpoints (`/v1/*`).
///
/// This middleware builds authorization context from `AuthenticatedRequest`
/// (set by `api_middleware`). Unlike admin authz, this supports:
/// - API key authentication (from `ApiKeyAuth`)
/// - Identity authentication (from proxy headers or OIDC)
/// - Combined authentication (both API key and identity)
///
/// Configuration (`auth.rbac.gateway`):
/// - `enabled`: When false (default), all API requests are allowed (backwards compatible)
/// - `default_effect`: Default is "allow" (fail-open) for backwards compatibility
///
/// When API RBAC is disabled, an AuthzContext is still created with a permissive engine
/// so handlers can optionally call authorization methods without errors.
///
/// IMPORTANT: This must run AFTER `api_middleware` which sets up `AuthenticatedRequest`.
pub async fn api_authz_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthzResponse> {
    // Check if API RBAC is enabled
    let api_rbac_config = &state.config.auth.rbac.gateway;

    // Build the authorization engine
    // If API RBAC is disabled, create an engine with enabled=false so all checks pass
    let rbac_config = if api_rbac_config.enabled {
        // Use main RBAC config with API-specific default effect
        let mut config = state.config.auth.rbac.clone();
        config.enabled = true;
        config.default_effect = api_rbac_config.default_effect;
        config
    } else {
        // Create a disabled config - all authorization checks will pass
        let mut config = state.config.auth.rbac.clone();
        config.enabled = false;
        config
    };

    let engine = match AuthzEngine::new(rbac_config) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            tracing::error!(error = %e, "Failed to create authorization engine");
            return Err(AuthzResponse::InternalError(format!(
                "Authorization configuration error: {}",
                e
            )));
        }
    };

    // Extract request metadata for audit logging
    let request_ip = extract_client_ip(&req, &state.config.server.trusted_proxies);
    let request_user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Get audit service if available
    let audit_service = state.services.as_ref().map(|s| s.audit_logs.clone());

    // Build subject from AuthenticatedRequest if available
    let subject = if let Some(auth) = req.extensions().get::<AuthenticatedRequest>() {
        build_subject_from_auth(auth, &engine)
    } else if engine.is_enabled() {
        // RBAC is enabled but no auth - use empty subject
        // The policy's default_effect will determine if this is allowed
        Subject::new()
    } else {
        // RBAC is disabled, use empty subject (engine will allow all)
        Subject::new()
    };

    // Always add authz context to request
    // API endpoints use the API-specific default effect (typically "allow" for backwards compatibility)
    req.extensions_mut().insert(AuthzContext::new(
        subject,
        engine,
        state.policy_registry.clone(),
        audit_service,
        Some(state.task_tracker.clone()),
        request_ip,
        request_user_agent,
        state.config.auth.rbac.audit.clone(),
        api_rbac_config.default_effect,
    ));

    Ok(next.run(req).await)
}

/// Build a Subject from AuthenticatedRequest.
///
/// Extracts identity information from the authenticated request,
/// prioritizing identity auth over API key auth when both are present.
///
/// For service account-owned API keys, the service account's roles are
/// mapped through the role_mappings configuration and injected into the subject,
/// enabling RBAC evaluation for machine identities.
fn build_subject_from_auth(auth: &AuthenticatedRequest, engine: &AuthzEngine) -> Subject {
    // Start with empty subject
    let mut subject = Subject::new();

    // Extract from API key if present
    if let Some(api_key) = auth.api_key() {
        // API key provides org_id, team_id, project_id, user_id
        if let Some(org_id) = api_key.org_id {
            subject = subject.with_org_ids(vec![org_id.to_string()]);
        }
        if let Some(team_id) = api_key.team_id {
            subject = subject.with_team_ids(vec![team_id.to_string()]);
        }
        if let Some(project_id) = api_key.project_id {
            subject = subject.with_project_ids(vec![project_id.to_string()]);
        }
        if let Some(user_id) = api_key.user_id {
            subject = subject.with_user_id(user_id.to_string());
        }

        // For service account-owned API keys, inject the service account ID and roles
        if let Some(service_account_id) = api_key.service_account_id {
            subject = subject.with_service_account_id(service_account_id.to_string());

            // Map service account roles through the role_mappings config
            if let Some(sa_roles) = &api_key.service_account_roles {
                let mapped_roles = engine.map_roles(sa_roles);
                subject = subject.with_roles(mapped_roles);
            }
        }
    }

    // Extract from identity if present (overrides API key values including SA roles)
    if let Some(identity) = auth.identity() {
        let mapped_roles = engine.map_roles(&identity.roles);
        subject = subject
            .with_external_id(&identity.external_id)
            .with_roles(mapped_roles);

        // Use identity's org/team/project IDs if available
        if !identity.org_ids.is_empty() {
            subject = subject.with_org_ids(identity.org_ids.clone());
        }
        if !identity.team_ids.is_empty() {
            subject = subject.with_team_ids(identity.team_ids.clone());
        }
        if !identity.project_ids.is_empty() {
            subject = subject.with_project_ids(identity.project_ids.clone());
        }

        // Use identity's user_id if available
        if let Some(user_id) = identity.user_id {
            subject = subject.with_user_id(user_id.to_string());
        }
        if let Some(email) = &identity.email {
            subject = subject.with_email(email);
        }
    }

    subject
}

/// Response type for authorization errors.
#[derive(Debug)]
pub enum AuthzResponse {
    Unauthorized(String),
    #[allow(dead_code)] // Error variant for completeness in authorization responses
    Forbidden(String),
    InternalError(String),
}

impl IntoResponse for AuthzResponse {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "unauthorized", msg),
            Self::Forbidden(msg) => (StatusCode::FORBIDDEN, "forbidden", msg),
            Self::InternalError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg),
        };

        let body = crate::openapi::ErrorResponse::new(code, message);

        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::{
        auth::{ApiKeyAuth, AuthenticatedRequest, Identity, IdentityKind},
        config::RbacConfig,
        models::{ApiKey, ApiKeyOwner},
    };

    fn test_engine() -> AuthzEngine {
        AuthzEngine::new(RbacConfig::default()).expect("Failed to create test engine")
    }

    fn make_test_api_key() -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            key_prefix: "test_".to_string(),
            name: "Test Key".to_string(),
            owner: ApiKeyOwner::Organization {
                org_id: Uuid::new_v4(),
            },
            budget_limit_cents: None,
            budget_period: None,
            created_at: Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            scopes: None,
            allowed_models: None,
            ip_allowlist: None,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
            rotated_from_key_id: None,
            rotation_grace_until: None,
            sovereignty_requirements: None,
        }
    }

    #[test]
    fn test_build_subject_service_account_roles() {
        let engine = test_engine();

        // Create an API key owned by a service account with roles
        let sa_id = Uuid::new_v4();
        let org_id = Uuid::new_v4();

        let api_key_auth = ApiKeyAuth {
            key: make_test_api_key(),
            org_id: Some(org_id),
            team_id: None,
            project_id: None,
            user_id: None,
            service_account_id: Some(sa_id),
            service_account_roles: Some(vec!["deployer".to_string(), "viewer".to_string()]),
        };

        let auth = AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(api_key_auth)));
        let subject = build_subject_from_auth(&auth, &engine);

        // Verify service account ID is set
        assert_eq!(subject.service_account_id, Some(sa_id.to_string()));

        // Verify roles are set from service account
        assert_eq!(subject.roles, vec!["deployer", "viewer"]);

        // Verify org_id is set
        assert_eq!(subject.org_ids, vec![org_id.to_string()]);
    }

    #[test]
    fn test_build_subject_service_account_role_mapping() {
        // Create engine with role mappings
        let config = RbacConfig {
            role_mapping: [("deployer".to_string(), "deploy_admin".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let engine = AuthzEngine::new(config).expect("Failed to create test engine");

        let sa_id = Uuid::new_v4();
        let api_key_auth = ApiKeyAuth {
            key: make_test_api_key(),
            org_id: None,
            team_id: None,
            project_id: None,
            user_id: None,
            service_account_id: Some(sa_id),
            service_account_roles: Some(vec!["deployer".to_string()]),
        };

        let auth = AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(api_key_auth)));
        let subject = build_subject_from_auth(&auth, &engine);

        // Verify role is mapped through role_mapping
        assert_eq!(subject.roles, vec!["deploy_admin"]);
    }

    #[test]
    fn test_build_subject_identity_overrides_service_account_roles() {
        let engine = test_engine();

        let sa_id = Uuid::new_v4();
        let api_key_auth = ApiKeyAuth {
            key: make_test_api_key(),
            org_id: None,
            team_id: None,
            project_id: None,
            user_id: None,
            service_account_id: Some(sa_id),
            service_account_roles: Some(vec!["sa_role".to_string()]),
        };

        let identity = Identity {
            external_id: "user@example.com".to_string(),
            email: Some("user@example.com".to_string()),
            name: Some("Test User".to_string()),
            user_id: None,
            roles: vec!["identity_role".to_string()],
            idp_groups: vec![],
            org_ids: vec![],
            team_ids: vec![],
            project_ids: vec![],
        };

        let auth = AuthenticatedRequest::new(IdentityKind::Both {
            api_key: Box::new(api_key_auth),
            identity,
        });
        let subject = build_subject_from_auth(&auth, &engine);

        // Identity roles should override service account roles
        assert_eq!(subject.roles, vec!["identity_role"]);

        // Service account ID should still be set
        assert_eq!(subject.service_account_id, Some(sa_id.to_string()));
    }

    #[test]
    fn test_build_subject_api_key_without_service_account() {
        let engine = test_engine();

        // API key owned by an organization (not a service account)
        let org_id = Uuid::new_v4();
        let api_key_auth = ApiKeyAuth {
            key: make_test_api_key(),
            org_id: Some(org_id),
            team_id: None,
            project_id: None,
            user_id: None,
            service_account_id: None,
            service_account_roles: None,
        };

        let auth = AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(api_key_auth)));
        let subject = build_subject_from_auth(&auth, &engine);

        // No service account ID
        assert!(subject.service_account_id.is_none());

        // No roles (no identity auth, no service account)
        assert!(subject.roles.is_empty());

        // Org ID should still be set
        assert_eq!(subject.org_ids, vec![org_id.to_string()]);
    }
}
