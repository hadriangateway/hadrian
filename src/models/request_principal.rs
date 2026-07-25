//! Canonical request-principal attribution.
//!
//! Every subsystem that attributes work to a caller (usage rows, the
//! responses pipeline, shell-tool billing, media cost scoping) derives
//! the same six fields from the request's auth context. This module is
//! the single definition of that derivation; call sites compose the
//! strict [`RequestPrincipal::from_auth`] constructor with the explicit
//! [`RequestPrincipal::or_defaults`] and
//! [`RequestPrincipal::with_project_fallback`] combinators instead of
//! re-implementing the field-by-field extraction.

use uuid::Uuid;

use crate::{auth::AuthenticatedRequest, models::UsageLogEntry};

/// The six attribution fields shared by every per-request accounting
/// surface: usage rows, container ownership, ACL scoping, and pricing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestPrincipal {
    pub api_key_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
}

/// Deployment-default identity used for anonymous requests when auth is
/// disabled. See [`crate::AppState::default_identity`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DefaultIdentity {
    pub user_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
}

impl RequestPrincipal {
    /// Extract attribution from the request's auth context. Strict: no
    /// deployment-default fallbacks; compose with [`Self::or_defaults`]
    /// for those.
    ///
    /// `org_id` prefers the API key's bound org, then the authenticated
    /// principal's org (a user's single org, a service account's org).
    pub fn from_auth(auth: Option<&AuthenticatedRequest>) -> Self {
        let Some(auth) = auth else {
            return Self::default();
        };
        let api_key = auth.api_key();
        Self {
            api_key_id: api_key.map(|k| k.key.id),
            user_id: auth.user_id(),
            org_id: api_key
                .and_then(|k| k.org_id)
                .or_else(|| auth.principal().org_id()),
            project_id: api_key.and_then(|k| k.project_id),
            team_id: api_key.and_then(|k| k.team_id),
            service_account_id: api_key.and_then(|k| k.service_account_id),
        }
    }

    /// Fill missing `user_id` / `org_id` from the deployment defaults
    /// (anonymous / no-auth mode). Other fields are untouched.
    pub fn or_defaults(self, defaults: DefaultIdentity) -> Self {
        Self {
            user_id: self.user_id.or(defaults.user_id),
            org_id: self.org_id.or(defaults.org_id),
            ..self
        }
    }

    /// Fill a missing `project_id` from an explicit fallback (the
    /// `X-Hadrian-Project` header). A project-scoped credential always
    /// wins over the header.
    pub fn with_project_fallback(self, project_id: Option<Uuid>) -> Self {
        Self {
            project_id: self.project_id.or(project_id),
            ..self
        }
    }

    /// Copy the six attribution fields onto a usage entry.
    pub fn apply_to(&self, entry: &mut UsageLogEntry) {
        entry.api_key_id = self.api_key_id;
        entry.user_id = self.user_id;
        entry.org_id = self.org_id;
        entry.project_id = self.project_id;
        entry.team_id = self.team_id;
        entry.service_account_id = self.service_account_id;
    }
}

/// Resolve the org a request operates in: the API key's bound org, else
/// the authenticated principal's org, else the deployment `default_org_id`
/// (no-auth / anonymous dev mode).
///
/// This is the single source of truth for request → org scoping, and
/// every code path that *writes* an org-scoped row and a later path that
/// *reads* it back must agree on it. In particular the MCP approval gate:
/// park (`RequestPrincipal::from_auth` → `McpExecutor` `org_id`) and
/// resume (`routes::api::chat`) both call this, so a parked approval and
/// its `mcp_approval_response` land in the same partition. Dropping the
/// `default_org_id` fallback on either side made anonymous deployments
/// park under the default org but resume under `None`, so the claim never
/// hit and every approval re-prompted forever.
pub fn resolve_request_org(
    auth: Option<&AuthenticatedRequest>,
    default_org_id: Option<Uuid>,
) -> Option<Uuid> {
    RequestPrincipal::from_auth(auth)
        .or_defaults(DefaultIdentity {
            user_id: None,
            org_id: default_org_id,
        })
        .org_id
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::{
        auth::{ApiKeyAuth, Identity, IdentityKind},
        models::{ApiKey, ApiKeyOwner},
    };

    fn make_api_key(owner: ApiKeyOwner) -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            key_prefix: "test_".to_string(),
            name: "Test Key".to_string(),
            owner,
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

    fn team_key_auth(org_id: Uuid, team_id: Uuid) -> AuthenticatedRequest {
        AuthenticatedRequest::new(IdentityKind::ApiKey(Box::new(ApiKeyAuth {
            key: make_api_key(ApiKeyOwner::Team { team_id }),
            org_id: Some(org_id),
            team_id: Some(team_id),
            project_id: None,
            user_id: None,
            service_account_id: None,
            service_account_roles: None,
        })))
    }

    fn identity_auth(user_id: Uuid, org_id: Uuid) -> AuthenticatedRequest {
        AuthenticatedRequest::new(IdentityKind::Identity(Identity {
            external_id: "user@example.com".to_string(),
            email: Some("user@example.com".to_string()),
            name: None,
            user_id: Some(user_id),
            roles: vec![],
            idp_groups: vec![],
            org_ids: vec![org_id.to_string()],
            team_ids: vec![],
            project_ids: vec![],
        }))
    }

    fn both_auth(org_id: Uuid, project_id: Uuid, user_id: Uuid) -> AuthenticatedRequest {
        AuthenticatedRequest::new(IdentityKind::Both {
            api_key: Box::new(ApiKeyAuth {
                key: make_api_key(ApiKeyOwner::Project { project_id }),
                org_id: Some(org_id),
                team_id: None,
                project_id: Some(project_id),
                user_id: None,
                service_account_id: None,
                service_account_roles: None,
            }),
            identity: Identity {
                external_id: "user@example.com".to_string(),
                email: None,
                name: None,
                user_id: Some(user_id),
                roles: vec![],
                idp_groups: vec![],
                org_ids: vec![],
                team_ids: vec![],
                project_ids: vec![],
            },
        })
    }

    #[test]
    fn from_auth_none_is_all_none() {
        assert_eq!(
            RequestPrincipal::from_auth(None),
            RequestPrincipal::default()
        );
    }

    #[test]
    fn from_auth_api_key_carries_key_scope() {
        let org_id = Uuid::new_v4();
        let team_id = Uuid::new_v4();
        let auth = team_key_auth(org_id, team_id);
        let p = RequestPrincipal::from_auth(Some(&auth));
        assert_eq!(p.api_key_id, Some(auth.api_key().unwrap().key.id));
        assert_eq!(p.user_id, None);
        assert_eq!(p.org_id, Some(org_id));
        assert_eq!(p.project_id, None);
        assert_eq!(p.team_id, Some(team_id));
        assert_eq!(p.service_account_id, None);
    }

    #[test]
    fn from_auth_identity_uses_principal_org() {
        let user_id = Uuid::new_v4();
        let org_id = Uuid::new_v4();
        let auth = identity_auth(user_id, org_id);
        let p = RequestPrincipal::from_auth(Some(&auth));
        assert_eq!(p.api_key_id, None);
        assert_eq!(p.user_id, Some(user_id));
        assert_eq!(p.org_id, Some(org_id));
        assert_eq!(p.team_id, None);
    }

    #[test]
    fn from_auth_both_prefers_key_org_and_identity_user() {
        let org_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let auth = both_auth(org_id, project_id, user_id);
        let p = RequestPrincipal::from_auth(Some(&auth));
        assert_eq!(p.user_id, Some(user_id));
        assert_eq!(p.org_id, Some(org_id));
        assert_eq!(p.project_id, Some(project_id));
    }

    #[test]
    fn or_defaults_fills_only_missing_user_and_org() {
        let defaults = DefaultIdentity {
            user_id: Some(Uuid::new_v4()),
            org_id: Some(Uuid::new_v4()),
        };
        // Empty principal takes both defaults.
        let filled = RequestPrincipal::default().or_defaults(defaults);
        assert_eq!(filled.user_id, defaults.user_id);
        assert_eq!(filled.org_id, defaults.org_id);
        assert_eq!(filled.api_key_id, None);
        // Populated principal is untouched.
        let populated = RequestPrincipal {
            user_id: Some(Uuid::new_v4()),
            org_id: Some(Uuid::new_v4()),
            ..Default::default()
        };
        assert_eq!(populated.or_defaults(defaults), populated);
    }

    #[test]
    fn with_project_fallback_never_overrides_existing_project() {
        let scoped = RequestPrincipal {
            project_id: Some(Uuid::new_v4()),
            ..Default::default()
        };
        let header = Some(Uuid::new_v4());
        assert_eq!(
            scoped.with_project_fallback(header).project_id,
            scoped.project_id
        );
        assert_eq!(
            RequestPrincipal::default()
                .with_project_fallback(header)
                .project_id,
            header
        );
    }

    #[test]
    fn apply_to_sets_all_six_fields() {
        let p = RequestPrincipal {
            api_key_id: Some(Uuid::new_v4()),
            user_id: Some(Uuid::new_v4()),
            org_id: Some(Uuid::new_v4()),
            project_id: Some(Uuid::new_v4()),
            team_id: Some(Uuid::new_v4()),
            service_account_id: Some(Uuid::new_v4()),
        };
        let mut entry = UsageLogEntry {
            request_id: Uuid::new_v4().to_string(),
            client_request_id: None,
            api_key_id: None,
            user_id: None,
            org_id: None,
            project_id: None,
            team_id: None,
            service_account_id: None,
            model: "m".to_string(),
            provider: "p".to_string(),
            http_referer: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_microcents: None,
            request_at: Utc::now(),
            streamed: false,
            cached_tokens: 0,
            reasoning_tokens: 0,
            finish_reason: None,
            latency_ms: None,
            cancelled: false,
            status_code: None,
            pricing_source: crate::pricing::CostPricingSource::None,
            image_count: None,
            audio_seconds: None,
            character_count: None,
            provider_source: None,
            record_type: "model".to_string(),
            tool_name: None,
            tool_query: None,
            tool_url: None,
            tool_bytes_fetched: None,
            tool_results_count: None,
            tool_runtime_seconds: None,
            tool_exit_code: None,
        };
        p.apply_to(&mut entry);
        assert_eq!(entry.api_key_id, p.api_key_id);
        assert_eq!(entry.user_id, p.user_id);
        assert_eq!(entry.org_id, p.org_id);
        assert_eq!(entry.project_id, p.project_id);
        assert_eq!(entry.team_id, p.team_id);
        assert_eq!(entry.service_account_id, p.service_account_id);
    }

    #[test]
    fn resolve_request_org_is_from_auth_with_org_default() {
        // Structural pin: `resolve_request_org` must stay a projection of
        // the canonical constructor for every auth shape, or org-scoped
        // writes and reads (the MCP park/resume invariant) can diverge.
        let default_org = Uuid::new_v4();
        let key_auth = team_key_auth(Uuid::new_v4(), Uuid::new_v4());
        let id_auth = identity_auth(Uuid::new_v4(), Uuid::new_v4());
        let dual_auth = both_auth(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for auth in [None, Some(&key_auth), Some(&id_auth), Some(&dual_auth)] {
            for default in [None, Some(default_org)] {
                assert_eq!(
                    resolve_request_org(auth, default),
                    RequestPrincipal::from_auth(auth)
                        .or_defaults(DefaultIdentity {
                            user_id: None,
                            org_id: default,
                        })
                        .org_id
                );
            }
        }
    }

    #[test]
    fn anonymous_request_falls_back_to_default_org() {
        // The MCP approval loop bug: park resolved org via this helper
        // (with the `default_org_id` fallback) while resume omitted the
        // fallback, so an anonymous resume looked under `None` and never
        // claimed the parked row. Both paths now share this resolver, so an
        // anonymous request must land on the deployment default on BOTH.
        let default_org = Uuid::new_v4();
        assert_eq!(
            resolve_request_org(None, Some(default_org)),
            Some(default_org),
            "anonymous request must fall back to default_org_id"
        );
    }

    #[test]
    fn anonymous_request_without_default_resolves_none() {
        // No auth and no configured default → no org. Park then fails
        // closed (it can't persist a parked approval without an org) rather
        // than parking, so there is nothing for resume to claim — the two
        // paths stay symmetric in this case too.
        assert_eq!(resolve_request_org(None, None), None);
    }
}
