use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use uuid::Uuid;

use crate::{
    db::{DateRange, DbPool, DbResult, ListParams, ListResult, UserDeletionResult},
    models::{
        AuditActorType, AuditLogQuery, ConversationOwnerType, CreateUser, ExportedApiKey,
        ExportedSession, ExportedUsageSummary, MembershipSource, UpdateUser, User, UserDataExport,
        UserMemberships,
    },
};

/// Service layer for user operations
#[derive(Clone)]
pub struct UserService {
    db: Arc<DbPool>,
}

impl UserService {
    pub fn new(db: Arc<DbPool>) -> Self {
        Self { db }
    }

    /// Create a new user
    pub async fn create(&self, input: CreateUser) -> DbResult<User> {
        self.db.users().create(input).await
    }

    /// Get user by ID
    pub async fn get_by_id(&self, id: Uuid) -> DbResult<Option<User>> {
        self.db.users().get_by_id(id).await
    }

    /// Get user by external ID
    pub async fn get_by_external_id(&self, external_id: &str) -> DbResult<Option<User>> {
        self.db.users().get_by_external_id(external_id).await
    }

    /// List users with pagination
    pub async fn list(&self, params: ListParams) -> DbResult<ListResult<User>> {
        self.db.users().list(params).await
    }

    /// Count all users
    pub async fn count(&self, include_deleted: bool) -> DbResult<i64> {
        self.db.users().count(include_deleted).await
    }

    /// Update a user by ID
    pub async fn update(&self, id: Uuid, input: UpdateUser) -> DbResult<User> {
        self.db.users().update(id, input).await
    }

    /// Add a user to an organization with a specified role.
    ///
    /// Enforces single-org membership via database constraint: a user can only belong
    /// to one organization. If the user already belongs to a different org, the database
    /// will return a conflict error (handled by `idx_org_memberships_single_org`).
    pub async fn add_to_org(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        role: &str,
        source: MembershipSource,
    ) -> DbResult<()> {
        // Single-org membership is enforced by database unique index.
        // This is race-condition safe - concurrent requests will be serialized by the DB.
        self.db
            .users()
            .add_to_org(user_id, org_id, role, source)
            .await
    }

    /// Update a user's role in an organization
    pub async fn update_org_member_role(
        &self,
        user_id: Uuid,
        org_id: Uuid,
        role: &str,
    ) -> DbResult<()> {
        self.db
            .users()
            .update_org_member_role(user_id, org_id, role)
            .await
    }

    /// Remove a user from an organization
    pub async fn remove_from_org(&self, user_id: Uuid, org_id: Uuid) -> DbResult<()> {
        self.db.users().remove_from_org(user_id, org_id).await
    }

    /// List members of an organization
    pub async fn list_org_members(
        &self,
        org_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<User>> {
        self.db.users().list_org_members(org_id, params).await
    }

    /// Count members of an organization
    pub async fn count_org_members(&self, org_id: Uuid, include_deleted: bool) -> DbResult<i64> {
        self.db
            .users()
            .count_org_members(org_id, include_deleted)
            .await
    }

    /// Add a user to a project with a specified role
    pub async fn add_to_project(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        role: &str,
        source: MembershipSource,
    ) -> DbResult<()> {
        self.db
            .users()
            .add_to_project(user_id, project_id, role, source)
            .await
    }

    /// Update a user's role in a project
    pub async fn update_project_member_role(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        role: &str,
    ) -> DbResult<()> {
        self.db
            .users()
            .update_project_member_role(user_id, project_id, role)
            .await
    }

    /// Remove a user from a project
    pub async fn remove_from_project(&self, user_id: Uuid, project_id: Uuid) -> DbResult<()> {
        self.db
            .users()
            .remove_from_project(user_id, project_id)
            .await
    }

    /// List members of a project
    pub async fn list_project_members(
        &self,
        project_id: Uuid,
        params: ListParams,
    ) -> DbResult<ListResult<User>> {
        self.db
            .users()
            .list_project_members(project_id, params)
            .await
    }

    /// Count members of a project
    pub async fn count_project_members(
        &self,
        project_id: Uuid,
        include_deleted: bool,
    ) -> DbResult<i64> {
        self.db
            .users()
            .count_project_members(project_id, include_deleted)
            .await
    }

    // ==================== Membership Query Methods ====================

    /// Get all organization memberships for a user
    pub async fn get_org_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> DbResult<Vec<crate::models::UserOrgMembership>> {
        self.db.users().get_org_memberships_for_user(user_id).await
    }

    /// Get all team memberships for a user
    pub async fn get_team_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> DbResult<Vec<crate::models::TeamMembership>> {
        self.db.users().get_team_memberships_for_user(user_id).await
    }

    /// Get all project memberships for a user
    pub async fn get_project_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> DbResult<Vec<crate::models::UserProjectMembership>> {
        self.db
            .users()
            .get_project_memberships_for_user(user_id)
            .await
    }

    // ==================== GDPR Export Methods ====================

    /// Export all data associated with a user (GDPR Article 15 - Right of Access)
    ///
    /// This method collects all personal data for a user including:
    /// - User profile
    /// - Organization and project memberships
    /// - API keys (excluding sensitive hash)
    /// - Conversations
    /// - Active sessions (when enhanced session management is enabled)
    /// - Usage summary
    /// - Audit logs where user was the actor
    pub async fn export_user_data(
        &self,
        user_id: Uuid,
        #[cfg(feature = "sso")] session_store: Option<
            &crate::auth::session_store::SharedSessionStore,
        >,
    ) -> DbResult<UserDataExport> {
        // Get user profile
        let user = self
            .db
            .users()
            .get_by_id(user_id)
            .await?
            .ok_or(crate::db::DbError::NotFound)?;

        // Get memberships
        let org_memberships = self
            .db
            .users()
            .get_org_memberships_for_user(user_id)
            .await?;
        let team_memberships = self
            .db
            .users()
            .get_team_memberships_for_user(user_id)
            .await?;
        let project_memberships = self
            .db
            .users()
            .get_project_memberships_for_user(user_id)
            .await?;

        // Get API keys owned by user (paginated, but we'll get all)
        let mut api_keys = Vec::new();
        let mut cursor = None;
        loop {
            let result = self
                .db
                .api_keys()
                .list_by_user(
                    user_id,
                    ListParams {
                        limit: Some(100),
                        include_deleted: true,
                        cursor: cursor.clone(),
                        ..Default::default()
                    },
                )
                .await?;

            api_keys.extend(result.items.into_iter().map(|key| ExportedApiKey {
                id: key.id,
                key_prefix: key.key_prefix,
                name: key.name,
                budget_limit_cents: key.budget_limit_cents,
                budget_period: key.budget_period.map(|p| p.as_str().to_string()),
                created_at: key.created_at,
                expires_at: key.expires_at,
                revoked_at: key.revoked_at,
                last_used_at: key.last_used_at,
            }));

            if !result.has_more {
                break;
            }
            cursor = result.cursors.next;
        }

        // Get conversations owned by user
        let mut conversations = Vec::new();
        let mut cursor = None;
        loop {
            let result = self
                .db
                .conversations()
                .list_by_owner(
                    ConversationOwnerType::User,
                    user_id,
                    ListParams {
                        limit: Some(100),
                        include_deleted: true,
                        cursor: cursor.clone(),
                        ..Default::default()
                    },
                )
                .await?;

            conversations.extend(result.items);

            if !result.has_more {
                break;
            }
            cursor = result.cursors.next;
        }

        // Get usage summary (all time)
        // Use a very wide date range to get all usage
        let all_time_range = DateRange {
            start: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2100, 12, 31).unwrap(),
            record_type: None,
        };
        let usage_summary = self
            .db
            .usage()
            .get_summary_by_user(user_id, all_time_range)
            .await?;

        // Get audit logs where user was the actor
        let mut audit_logs = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let result = self
                .db
                .audit_logs()
                .list(AuditLogQuery {
                    actor_type: Some(AuditActorType::User),
                    actor_id: Some(user_id),
                    limit: Some(100),
                    cursor: cursor.clone(),
                    ..Default::default()
                })
                .await?;

            audit_logs.extend(result.items);

            if !result.has_more {
                break;
            }
            cursor = result.cursors.next.map(|c| c.encode());
        }

        // Get active sessions if session store is provided
        #[cfg(feature = "sso")]
        let sessions = if let Some(store) = session_store {
            match store.list_user_sessions(&user.external_id).await {
                Ok(user_sessions) => user_sessions
                    .into_iter()
                    .map(|s| ExportedSession {
                        id: s.id,
                        created_at: s.created_at,
                        expires_at: s.expires_at,
                        last_activity: s.last_activity,
                        device_description: s
                            .device
                            .as_ref()
                            .and_then(|d| d.device_description.clone()),
                        ip_address: s.device.as_ref().and_then(|d| d.ip_address.clone()),
                    })
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        user_id = %user_id,
                        error = %e,
                        "Failed to fetch sessions for user data export"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        #[cfg(not(feature = "sso"))]
        let sessions: Vec<ExportedSession> = Vec::new();

        Ok(UserDataExport {
            exported_at: Utc::now(),
            user,
            memberships: UserMemberships {
                organizations: org_memberships,
                teams: team_memberships,
                projects: project_memberships,
            },
            api_keys,
            conversations,
            sessions,
            usage_summary: ExportedUsageSummary {
                total_cost_microcents: usage_summary.total_cost_microcents,
                total_tokens: usage_summary.total_tokens,
                request_count: usage_summary.request_count,
                first_request_at: usage_summary.first_request_at,
                last_request_at: usage_summary.last_request_at,
            },
            audit_logs,
        })
    }

    // ==================== GDPR Deletion Methods ====================

    /// Delete a user and all associated data (GDPR Article 17 - Right to Erasure)
    ///
    /// This permanently deletes:
    /// - User record
    /// - Organization memberships
    /// - Project memberships
    /// - API keys owned by the user
    /// - Conversations owned by the user
    /// - Dynamic providers owned by the user
    /// - Usage records for user's API keys
    ///
    /// Returns details about what was deleted.
    pub async fn delete_user(&self, user_id: Uuid) -> DbResult<UserDeletionResult> {
        self.db.users().hard_delete(user_id).await
    }
}
