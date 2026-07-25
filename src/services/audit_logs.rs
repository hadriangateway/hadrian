use std::sync::Arc;

use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::{
    db::{DbPool, DbResult, repos::ListResult},
    events::{EventBus, ServerEvent},
    models::{AuditActorType, AuditLog, AuditLogQuery, CreateAuditLog},
};

/// Auth event types for audit logging
pub mod auth_events {
    /// OIDC login success
    pub const OIDC_LOGIN: &str = "auth.oidc.login";
    /// OIDC login failure
    pub const OIDC_LOGIN_FAILED: &str = "auth.oidc.login_failed";
    /// SAML login success
    pub const SAML_LOGIN: &str = "auth.saml.login";
    /// SAML login failure
    pub const SAML_LOGIN_FAILED: &str = "auth.saml.login_failed";
    /// Emergency access success
    pub const EMERGENCY_LOGIN: &str = "auth.emergency.login";
    /// Emergency access failure
    pub const EMERGENCY_LOGIN_FAILED: &str = "auth.emergency.login_failed";
    /// Bootstrap key success
    pub const BOOTSTRAP_LOGIN: &str = "auth.bootstrap.login";
    /// Bootstrap key failure
    pub const BOOTSTRAP_LOGIN_FAILED: &str = "auth.bootstrap.login_failed";
    /// Logout (any provider)
    pub const LOGOUT: &str = "auth.logout";
}

/// Parameters for logging an auth event
pub struct AuthEventParams<'a> {
    /// The auth event action (use constants from `auth_events` module)
    pub action: &'a str,
    /// The session ID (use `Uuid::nil()` for failed logins where no session was created)
    pub session_id: Uuid,
    /// The external user ID from the IdP
    pub external_id: Option<&'a str>,
    /// User's email address
    pub email: Option<&'a str>,
    /// Organization context (for org-specific SSO)
    pub org_id: Option<Uuid>,
    /// Client IP address
    pub ip_address: Option<String>,
    /// Client user agent
    pub user_agent: Option<String>,
    /// Additional details as JSON (provider, error info, etc.)
    pub details: JsonValue,
}

/// Service layer for audit log operations
#[derive(Clone)]
pub struct AuditLogService {
    db: Arc<DbPool>,
    event_bus: Option<Arc<EventBus>>,
}

impl AuditLogService {
    pub fn new(db: Arc<DbPool>) -> Self {
        Self {
            db,
            event_bus: None,
        }
    }

    /// Create a new audit log service with EventBus for real-time notifications.
    pub fn with_event_bus(db: Arc<DbPool>, event_bus: Arc<EventBus>) -> Self {
        Self {
            db,
            event_bus: Some(event_bus),
        }
    }

    /// Create a new audit log entry
    pub async fn create(&self, input: CreateAuditLog) -> DbResult<AuditLog> {
        let audit_log = self.db.audit_logs().create(input).await?;

        // Publish event to WebSocket subscribers
        if let Some(event_bus) = &self.event_bus {
            event_bus.publish(ServerEvent::AuditLogCreated {
                id: audit_log.id,
                timestamp: audit_log.timestamp,
                action: audit_log.action.clone(),
                resource_type: audit_log.resource_type.clone(),
                resource_id: Some(audit_log.resource_id.to_string()),
                actor_type: format!("{:?}", audit_log.actor_type).to_lowercase(),
                actor_id: audit_log.actor_id,
                org_id: audit_log.org_id,
                project_id: audit_log.project_id,
            });
        }

        Ok(audit_log)
    }

    /// Get an audit log entry by ID
    pub async fn get_by_id(&self, id: Uuid) -> DbResult<Option<AuditLog>> {
        self.db.audit_logs().get_by_id(id).await
    }

    /// List audit logs with optional filtering and pagination
    ///
    /// Supports cursor-based pagination (`cursor` + `direction`).
    pub async fn list(&self, query: AuditLogQuery) -> DbResult<ListResult<AuditLog>> {
        self.db.audit_logs().list(query).await
    }

    /// Count audit logs matching the query (ignores pagination parameters)
    pub async fn count(&self, query: AuditLogQuery) -> DbResult<i64> {
        self.db.audit_logs().count(query).await
    }

    /// Log a system-initiated action
    pub async fn log_system_action(
        &self,
        action: &str,
        resource_type: &str,
        resource_id: Uuid,
        org_id: Option<Uuid>,
        project_id: Option<Uuid>,
        details: JsonValue,
    ) -> DbResult<AuditLog> {
        self.create(CreateAuditLog {
            actor_type: AuditActorType::System,
            actor_id: None,
            action: action.to_string(),
            resource_type: resource_type.to_string(),
            resource_id,
            org_id,
            project_id,
            details,
            ip_address: None,
            user_agent: None,
        })
        .await
    }

    /// Log an authentication event.
    ///
    /// This logs auth lifecycle events (login, logout) to the audit log for
    /// compliance and security monitoring purposes.
    pub async fn log_auth_event(&self, params: AuthEventParams<'_>) -> DbResult<AuditLog> {
        // Merge details with external_id and email
        let merged_details = match params.details {
            JsonValue::Object(mut map) => {
                if let Some(ext_id) = params.external_id {
                    map.insert(
                        "external_id".to_string(),
                        JsonValue::String(ext_id.to_string()),
                    );
                }
                if let Some(e) = params.email {
                    map.insert("email".to_string(), JsonValue::String(e.to_string()));
                }
                JsonValue::Object(map)
            }
            _ => serde_json::json!({
                "external_id": params.external_id,
                "email": params.email,
            }),
        };

        self.create(CreateAuditLog {
            actor_type: AuditActorType::System,
            actor_id: None,
            action: params.action.to_string(),
            resource_type: "session".to_string(),
            resource_id: params.session_id,
            org_id: params.org_id,
            project_id: None,
            details: merged_details,
            ip_address: params.ip_address,
            user_agent: params.user_agent,
        })
        .await
    }
}
