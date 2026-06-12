pub mod access_reviews;
pub mod api_keys;
pub mod audit_logs;
pub mod conversations;
#[cfg(feature = "csv-export")]
pub(super) mod csv_export;
pub mod dlq;
#[cfg(feature = "sso")]
pub mod domain_verifications;
#[cfg(feature = "server")]
pub mod dynamic_providers;
mod error;
pub mod me;
pub mod me_api_keys;
pub mod me_providers;
#[cfg(feature = "sso")]
pub mod me_sessions;
pub mod model_pricing;
pub mod oauth;
pub mod org_rbac_policies;
#[cfg(feature = "sso")]
pub mod org_sso_configs;
pub mod organizations;
pub mod projects;
pub mod providers;
#[cfg(feature = "sso")]
pub mod scim_configs;
pub mod service_accounts;
pub mod session_info;
#[cfg(feature = "sso")]
pub mod sessions;
#[cfg(feature = "sso")]
pub mod sso_connections;
#[cfg(feature = "sso")]
pub mod sso_group_mappings;
pub mod teams;
pub mod templates;
pub mod ui_config;
pub mod usage;
pub mod users;

#[cfg(any(feature = "server", feature = "wasm"))]
use axum::Router;
#[cfg(feature = "server")]
use axum::routing::{delete, get, patch, post, put};
pub use error::{AdminError, AuditActor};

#[cfg(any(feature = "server", feature = "wasm"))]
use crate::AppState;
#[cfg(feature = "wasm")]
use crate::compat::wasm_routing::{delete, get, patch, post, put};

#[cfg(any(feature = "server", feature = "wasm"))]
pub fn get_admin_routes() -> Router<AppState> {
    Router::new().nest("/v1", admin_v1_routes())
}

/// Get admin routes with authentication middleware applied.
/// This requires UI auth (Zero Trust or OIDC) to be configured.
/// Note: The middleware layer is applied in main.rs where state is available.
#[cfg(any(feature = "server", feature = "wasm"))]
pub fn get_protected_admin_routes() -> Router<AppState> {
    // The protection is applied in build_app via route_layer
    Router::new().nest("/v1", admin_v1_routes())
}

/// Get public admin routes that don't require authentication.
/// These are needed for frontend bootstrap before the user logs in.
#[cfg(any(feature = "server", feature = "wasm"))]
pub fn get_public_admin_routes() -> Router<AppState> {
    Router::new().nest("/v1", public_admin_v1_routes())
}

#[cfg(any(feature = "server", feature = "wasm"))]
pub(crate) fn public_admin_v1_routes() -> Router<AppState> {
    Router::new()
        // UI Configuration (unauthenticated - needed for frontend bootstrap)
        .route("/ui/config", get(ui_config::get_ui_config))
}

#[cfg(any(feature = "server", feature = "wasm"))]
pub(crate) fn admin_v1_routes() -> Router<AppState> {
    let router = Router::new()
        // Self-service endpoints (current user)
        .route("/me", delete(me::delete))
        .route("/me/export", get(me::export))
        .route("/me/eligible-owners", get(me::eligible_owners))
        .route(
            "/me/providers",
            get(me_providers::list).merge(post(me_providers::create)),
        )
        .route(
            "/me/providers/{id}",
            get(me_providers::get)
                .merge(patch(me_providers::update))
                .merge(delete(me_providers::delete)),
        )
        .route(
            "/me/providers/test-credentials",
            post(me_providers::test_credentials),
        )
        .route(
            "/me/providers/{id}/test",
            post(me_providers::test_connectivity),
        )
        .route(
            "/me/built-in-providers",
            get(me_providers::built_in_providers),
        )
        .route(
            "/me/api-keys",
            get(me_api_keys::list).merge(post(me_api_keys::create)),
        )
        .route(
            "/me/api-keys/{key_id}",
            get(me_api_keys::get).merge(delete(me_api_keys::revoke)),
        )
        .route("/me/api-keys/{key_id}/rotate", post(me_api_keys::rotate))
        // OAuth-style PKCE flow for issuing user-scoped keys to external apps
        .route("/oauth/authorize", post(oauth::authorize))
        .route("/oauth/preflight", get(oauth::preflight))
        // Organizations
        .route(
            "/organizations",
            post(organizations::create).merge(get(organizations::list)),
        )
        .route(
            "/organizations/{slug}",
            get(organizations::get)
                .merge(patch(organizations::update))
                .merge(delete(organizations::delete)),
        )
        // Projects
        .route(
            "/organizations/{org_slug}/projects",
            post(projects::create).merge(get(projects::list)),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}",
            get(projects::get)
                .merge(patch(projects::update))
                .merge(delete(projects::delete)),
        )
        // Teams
        .route(
            "/organizations/{org_slug}/teams",
            post(teams::create).merge(get(teams::list)),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}",
            get(teams::get)
                .merge(patch(teams::update))
                .merge(delete(teams::delete)),
        )
        // Team memberships
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/members",
            get(teams::list_members).merge(post(teams::add_member)),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/members/{user_id}",
            patch(teams::update_member).merge(delete(teams::remove_member)),
        )
        // Service Accounts
        .route(
            "/organizations/{org_slug}/service-accounts",
            post(service_accounts::create).merge(get(service_accounts::list)),
        )
        .route(
            "/organizations/{org_slug}/service-accounts/{sa_slug}",
            get(service_accounts::get)
                .merge(patch(service_accounts::update))
                .merge(delete(service_accounts::delete)),
        )
        // Users (top-level)
        .route("/users", post(users::create).merge(get(users::list)))
        .route(
            "/users/{user_id}",
            get(users::get)
                .merge(patch(users::update))
                .merge(delete(users::delete)),
        )
        .route("/users/{user_id}/export", get(users::export))
        // Organization memberships
        .route(
            "/organizations/{org_slug}/members",
            get(users::list_org_members).merge(post(users::add_org_member)),
        )
        .route(
            "/organizations/{org_slug}/members/{user_id}",
            delete(users::remove_org_member).merge(patch(users::update_org_member)),
        )
        // Project memberships
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/members",
            get(users::list_project_members).merge(post(users::add_project_member)),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/members/{user_id}",
            delete(users::remove_project_member).merge(patch(users::update_project_member)),
        )
        // API Keys
        .route("/api-keys", post(api_keys::create))
        .route("/api-keys/{key_id}", delete(api_keys::revoke))
        .route("/api-keys/{key_id}/rotate", post(api_keys::rotate))
        .route(
            "/organizations/{org_slug}/api-keys",
            get(api_keys::list_by_org),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/api-keys",
            get(api_keys::list_by_project),
        )
        .route("/users/{user_id}/api-keys", get(api_keys::list_by_user))
        .route(
            "/organizations/{org_slug}/service-accounts/{sa_slug}/api-keys",
            get(api_keys::list_by_service_account),
        );
    // Dynamic Providers (requires server feature — module is cfg-gated)
    #[cfg(feature = "server")]
    let router = router
        .route("/dynamic-providers", post(dynamic_providers::create))
        .route(
            "/dynamic-providers/{id}",
            get(dynamic_providers::get)
                .patch(dynamic_providers::update)
                .delete(dynamic_providers::delete),
        )
        .route(
            "/dynamic-providers/{id}/test",
            post(dynamic_providers::test_connectivity),
        )
        .route(
            "/dynamic-providers/test-credentials",
            post(dynamic_providers::test_credentials),
        )
        .route(
            "/organizations/{org_slug}/dynamic-providers",
            get(dynamic_providers::list_by_org),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/dynamic-providers",
            get(dynamic_providers::list_by_project),
        )
        .route(
            "/users/{user_id}/dynamic-providers",
            get(dynamic_providers::list_by_user),
        );
    // Usage endpoints - API Key level
    let router = router
        .route("/api-keys/{key_id}/usage", get(usage::get_summary))
        .route("/api-keys/{key_id}/usage/by-date", get(usage::get_by_date))
        .route(
            "/api-keys/{key_id}/usage/by-model",
            get(usage::get_by_model),
        )
        .route(
            "/api-keys/{key_id}/usage/by-referer",
            get(usage::get_by_referer),
        )
        .route(
            "/api-keys/{key_id}/usage/by-provider",
            get(usage::get_by_provider),
        )
        .route(
            "/api-keys/{key_id}/usage/by-date-model",
            get(usage::get_by_date_model),
        )
        .route(
            "/api-keys/{key_id}/usage/by-date-provider",
            get(usage::get_by_date_provider),
        )
        .route(
            "/api-keys/{key_id}/usage/by-pricing-source",
            get(usage::get_by_pricing_source),
        )
        .route(
            "/api-keys/{key_id}/usage/by-date-pricing-source",
            get(usage::get_by_date_pricing_source),
        )
        .route(
            "/api-keys/{key_id}/usage/forecast",
            get(usage::get_forecast),
        )
        // Usage endpoints - Organization level
        .route("/organizations/{slug}/usage", get(usage::get_org_summary))
        .route(
            "/organizations/{slug}/usage/by-date",
            get(usage::get_org_by_date),
        )
        .route(
            "/organizations/{slug}/usage/by-model",
            get(usage::get_org_by_model),
        )
        .route(
            "/organizations/{slug}/usage/by-provider",
            get(usage::get_org_by_provider),
        )
        .route(
            "/organizations/{slug}/usage/by-date-model",
            get(usage::get_org_by_date_model),
        )
        .route(
            "/organizations/{slug}/usage/by-date-provider",
            get(usage::get_org_by_date_provider),
        )
        .route(
            "/organizations/{slug}/usage/by-pricing-source",
            get(usage::get_org_by_pricing_source),
        )
        .route(
            "/organizations/{slug}/usage/by-date-pricing-source",
            get(usage::get_org_by_date_pricing_source),
        )
        .route(
            "/organizations/{slug}/usage/by-user",
            get(usage::get_org_by_user),
        )
        .route(
            "/organizations/{slug}/usage/by-date-user",
            get(usage::get_org_by_date_user),
        )
        .route(
            "/organizations/{slug}/usage/by-project",
            get(usage::get_org_by_project),
        )
        .route(
            "/organizations/{slug}/usage/by-date-project",
            get(usage::get_org_by_date_project),
        )
        .route(
            "/organizations/{slug}/usage/by-team",
            get(usage::get_org_by_team),
        )
        .route(
            "/organizations/{slug}/usage/by-date-team",
            get(usage::get_org_by_date_team),
        )
        .route(
            "/organizations/{slug}/usage/forecast",
            get(usage::get_org_forecast),
        )
        // Usage endpoints - Project level
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage",
            get(usage::get_project_summary),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-date",
            get(usage::get_project_by_date),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-model",
            get(usage::get_project_by_model),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-provider",
            get(usage::get_project_by_provider),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-date-model",
            get(usage::get_project_by_date_model),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-date-provider",
            get(usage::get_project_by_date_provider),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-pricing-source",
            get(usage::get_project_by_pricing_source),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-date-pricing-source",
            get(usage::get_project_by_date_pricing_source),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-user",
            get(usage::get_project_by_user),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/by-date-user",
            get(usage::get_project_by_date_user),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/usage/forecast",
            get(usage::get_project_forecast),
        )
        // Usage endpoints - User level
        .route("/users/{user_id}/usage", get(usage::get_user_summary))
        .route(
            "/users/{user_id}/usage/by-date",
            get(usage::get_user_by_date),
        )
        .route(
            "/users/{user_id}/usage/by-model",
            get(usage::get_user_by_model),
        )
        .route(
            "/users/{user_id}/usage/by-provider",
            get(usage::get_user_by_provider),
        )
        .route(
            "/users/{user_id}/usage/by-date-model",
            get(usage::get_user_by_date_model),
        )
        .route(
            "/users/{user_id}/usage/by-date-provider",
            get(usage::get_user_by_date_provider),
        )
        .route(
            "/users/{user_id}/usage/by-pricing-source",
            get(usage::get_user_by_pricing_source),
        )
        .route(
            "/users/{user_id}/usage/by-date-pricing-source",
            get(usage::get_user_by_date_pricing_source),
        )
        .route(
            "/users/{user_id}/usage/forecast",
            get(usage::get_user_forecast),
        )
        // Usage endpoints - Provider level
        .route(
            "/providers/{provider}/usage",
            get(usage::get_provider_summary),
        )
        .route(
            "/providers/{provider}/usage/by-date",
            get(usage::get_provider_by_date),
        )
        .route(
            "/providers/{provider}/usage/by-model",
            get(usage::get_provider_by_model),
        )
        .route(
            "/providers/{provider}/usage/forecast",
            get(usage::get_provider_forecast),
        )
        // Usage endpoints - Team level
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage",
            get(usage::get_team_summary),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date",
            get(usage::get_team_by_date),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-model",
            get(usage::get_team_by_model),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-provider",
            get(usage::get_team_by_provider),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date-model",
            get(usage::get_team_by_date_model),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date-provider",
            get(usage::get_team_by_date_provider),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-pricing-source",
            get(usage::get_team_by_pricing_source),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date-pricing-source",
            get(usage::get_team_by_date_pricing_source),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-user",
            get(usage::get_team_by_user),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date-user",
            get(usage::get_team_by_date_user),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-project",
            get(usage::get_team_by_project),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/by-date-project",
            get(usage::get_team_by_date_project),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/usage/forecast",
            get(usage::get_team_forecast),
        )
        // Usage endpoints - Self-service (current user)
        .route("/me/usage", get(usage::get_me_summary))
        .route("/me/usage/by-date", get(usage::get_me_by_date))
        .route("/me/usage/by-model", get(usage::get_me_by_model))
        .route("/me/usage/by-provider", get(usage::get_me_by_provider))
        .route("/me/usage/by-date-model", get(usage::get_me_by_date_model))
        .route(
            "/me/usage/by-date-provider",
            get(usage::get_me_by_date_provider),
        )
        .route(
            "/me/usage/by-pricing-source",
            get(usage::get_me_by_pricing_source),
        )
        .route(
            "/me/usage/by-date-pricing-source",
            get(usage::get_me_by_date_pricing_source),
        )
        .route("/me/usage/logs", get(usage::list_me_logs))
        .route("/me/usage/logs/export", get(usage::export_me_logs))
        // Usage endpoints - Global (all organizations)
        .route("/usage", get(usage::get_global_summary))
        .route("/usage/by-date", get(usage::get_global_by_date))
        .route("/usage/by-model", get(usage::get_global_by_model))
        .route("/usage/by-provider", get(usage::get_global_by_provider))
        .route(
            "/usage/by-pricing-source",
            get(usage::get_global_by_pricing_source),
        )
        .route("/usage/by-date-model", get(usage::get_global_by_date_model))
        .route(
            "/usage/by-date-provider",
            get(usage::get_global_by_date_provider),
        )
        .route(
            "/usage/by-date-pricing-source",
            get(usage::get_global_by_date_pricing_source),
        )
        .route("/usage/by-user", get(usage::get_global_by_user))
        .route("/usage/by-date-user", get(usage::get_global_by_date_user))
        .route("/usage/by-project", get(usage::get_global_by_project))
        .route(
            "/usage/by-date-project",
            get(usage::get_global_by_date_project),
        )
        .route("/usage/by-team", get(usage::get_global_by_team))
        .route("/usage/by-date-team", get(usage::get_global_by_date_team))
        .route("/usage/by-org", get(usage::get_global_by_org))
        .route("/usage/by-date-org", get(usage::get_global_by_date_org))
        .route("/usage/logs", get(usage::list_logs))
        .route("/usage/logs/export", get(usage::export_logs))
        // Model Pricing
        .route(
            "/model-pricing",
            post(model_pricing::create).merge(get(model_pricing::list_global)),
        )
        .route("/model-pricing/upsert", post(model_pricing::upsert))
        .route("/model-pricing/bulk", post(model_pricing::bulk_upsert))
        .route(
            "/model-pricing/{id}",
            get(model_pricing::get)
                .merge(patch(model_pricing::update))
                .merge(delete(model_pricing::delete)),
        )
        .route(
            "/model-pricing/provider/{provider}",
            get(model_pricing::list_by_provider),
        )
        .route(
            "/organizations/{org_slug}/model-pricing",
            get(model_pricing::list_by_org),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/model-pricing",
            get(model_pricing::list_by_project),
        )
        .route(
            "/users/{user_id}/model-pricing",
            get(model_pricing::list_by_user),
        )
        // Conversations
        .route("/conversations", post(conversations::create))
        .route(
            "/conversations/{id}",
            get(conversations::get)
                .merge(patch(conversations::update))
                .merge(delete(conversations::delete)),
        )
        .route(
            "/conversations/{id}/messages",
            post(conversations::append_messages),
        )
        .route("/conversations/{id}/pin", put(conversations::set_pin))
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/conversations",
            get(conversations::list_by_project),
        )
        .route(
            "/users/{user_id}/conversations",
            get(conversations::list_by_user),
        )
        .route(
            "/users/{user_id}/conversations/accessible",
            get(conversations::list_accessible_for_user),
        )
        // Templates
        .route("/templates", post(templates::create))
        .route(
            "/templates/{id}",
            get(templates::get)
                .merge(patch(templates::update))
                .merge(delete(templates::delete)),
        )
        .route(
            "/organizations/{org_slug}/templates",
            get(templates::list_by_org),
        )
        .route(
            "/organizations/{org_slug}/teams/{team_slug}/templates",
            get(templates::list_by_team),
        )
        .route(
            "/organizations/{org_slug}/projects/{project_slug}/templates",
            get(templates::list_by_project),
        )
        .route("/users/{user_id}/templates", get(templates::list_by_user))
        // (Skills moved to the OpenAI-compatible `/v1/skills` surface.)
        // Provider management
        .route(
            "/providers/circuit-breakers",
            get(providers::list_circuit_breakers),
        )
        .route(
            "/providers/{provider_name}/circuit-breaker",
            get(providers::get_circuit_breaker),
        )
        .route("/providers/health", get(providers::list_provider_health))
        .route(
            "/providers/{provider_name}/health",
            get(providers::get_provider_health),
        )
        // Provider Stats
        .route("/providers/stats", get(providers::list_provider_stats))
        .route(
            "/providers/{provider_name}/stats",
            get(providers::get_provider_stats),
        )
        .route(
            "/providers/{provider_name}/stats/history",
            get(providers::get_provider_stats_history),
        )
        // Dead Letter Queue
        .route("/dlq", get(dlq::list).merge(delete(dlq::purge)))
        .route("/dlq/stats", get(dlq::stats))
        .route("/dlq/prune", post(dlq::prune))
        .route("/dlq/{id}", get(dlq::get).merge(delete(dlq::delete)))
        .route("/dlq/{id}/retry", post(dlq::retry))
        // Audit Logs
        .route("/audit-logs", get(audit_logs::list))
        .route("/audit-logs/{id}", get(audit_logs::get))
        // Access Reviews
        .route(
            "/access-reviews/inventory",
            get(access_reviews::get_inventory),
        )
        .route(
            "/access-reviews/stale",
            get(access_reviews::get_stale_access),
        )
        .route(
            "/organizations/{org_slug}/access-report",
            get(access_reviews::get_org_access_report),
        )
        .route(
            "/users/{user_id}/access-summary",
            get(access_reviews::get_user_access_summary),
        )
        // Organization RBAC Policies
        .route(
            "/organizations/{org_slug}/rbac-policies",
            get(org_rbac_policies::list).merge(post(org_rbac_policies::create)),
        )
        .route(
            "/organizations/{org_slug}/rbac-policies/{policy_id}",
            get(org_rbac_policies::get)
                .merge(patch(org_rbac_policies::update))
                .merge(delete(org_rbac_policies::delete)),
        )
        .route(
            "/organizations/{org_slug}/rbac-policies/{policy_id}/versions",
            get(org_rbac_policies::list_versions),
        )
        .route(
            "/organizations/{org_slug}/rbac-policies/{policy_id}/rollback",
            post(org_rbac_policies::rollback),
        )
        .route(
            "/organizations/{org_slug}/rbac-policies/simulate",
            post(org_rbac_policies::simulate),
        )
        .route("/rbac-policies/validate", post(org_rbac_policies::validate));

    // Session info (available in all builds including WASM)
    let router = router.route("/session-info", get(session_info::get));

    // SSO routes (only available when sso feature is enabled)
    #[cfg(feature = "sso")]
    let router = router
        // Self-service sessions (current user)
        .route("/me/sessions", get(me_sessions::list))
        .route("/me/sessions/{session_id}", delete(me_sessions::delete_one))
        // User Sessions (admin)
        .route(
            "/users/{user_id}/sessions",
            get(sessions::list).delete(sessions::delete_all),
        )
        .route(
            "/users/{user_id}/sessions/{session_id}",
            delete(sessions::delete_one),
        )
        // SSO Connections (read-only, from config)
        .route("/sso-connections", get(sso_connections::list))
        .route("/sso-connections/{name}", get(sso_connections::get))
        // SSO Group Mappings
        .route(
            "/organizations/{org_slug}/sso-group-mappings",
            get(sso_group_mappings::list).post(sso_group_mappings::create),
        )
        .route(
            "/organizations/{org_slug}/sso-group-mappings/test",
            post(sso_group_mappings::test),
        )
        .route(
            "/organizations/{org_slug}/sso-group-mappings/export",
            get(sso_group_mappings::export),
        )
        .route(
            "/organizations/{org_slug}/sso-group-mappings/import",
            post(sso_group_mappings::import),
        )
        .route(
            "/organizations/{org_slug}/sso-group-mappings/{mapping_id}",
            get(sso_group_mappings::get)
                .patch(sso_group_mappings::update)
                .delete(sso_group_mappings::delete),
        )
        // Organization SSO Configuration (one per org)
        .route(
            "/organizations/{org_slug}/sso-config",
            get(org_sso_configs::get)
                .post(org_sso_configs::create)
                .patch(org_sso_configs::update)
                .delete(org_sso_configs::delete),
        )
        // Domain Verifications (nested under org SSO config)
        .route(
            "/organizations/{org_slug}/sso-config/domains",
            get(domain_verifications::list).post(domain_verifications::create),
        )
        .route(
            "/organizations/{org_slug}/sso-config/domains/{domain_id}",
            get(domain_verifications::get).delete(domain_verifications::delete),
        )
        .route(
            "/organizations/{org_slug}/sso-config/domains/{domain_id}/instructions",
            get(domain_verifications::get_instructions),
        )
        .route(
            "/organizations/{org_slug}/sso-config/domains/{domain_id}/verify",
            post(domain_verifications::verify),
        )
        // Organization SCIM Configuration (one per org)
        .route(
            "/organizations/{org_slug}/scim-config",
            get(scim_configs::get)
                .post(scim_configs::create)
                .patch(scim_configs::update)
                .delete(scim_configs::delete),
        )
        .route(
            "/organizations/{org_slug}/scim-config/rotate-token",
            post(scim_configs::rotate_token),
        );

    // SAML metadata endpoints (only available when saml feature is enabled)
    #[cfg(feature = "saml")]
    let router = router
        .route(
            "/organizations/{org_slug}/sso-config/saml/parse-metadata",
            post(org_sso_configs::parse_saml_metadata),
        )
        .route(
            "/organizations/{org_slug}/sso-config/saml/sp-metadata",
            get(org_sso_configs::get_sp_metadata),
        );

    router
}

#[cfg(all(test, feature = "database-sqlite", feature = "server"))]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Create a test application with an in-memory database
    /// Each call creates a unique database to avoid test interference
    async fn test_app() -> axum::Router {
        use std::sync::atomic::{AtomicU64, Ordering};

        // Initialize tracing for tests
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        // Create a unique database name for each test
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
path = "file:test_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers.test-openai]
type = "open_ai"
api_key = "sk-test-key"
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
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    /// Helper to make a JSON PATCH request
    async fn patch_json(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("PATCH")
            .uri(uri)
            .header("content-type", "application/json")
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
    // Organization Tests
    // ============================================================================

    #[tokio::test]
    async fn test_create_organization() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({
                "slug": "test-org",
                "name": "Test Organization"
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
        assert_eq!(body["slug"], "test-org");
        assert_eq!(body["name"], "Test Organization");
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_create_organization_duplicate_slug() {
        let app = test_app().await;

        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "duplicate-org", "name": "First Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "duplicate-org", "name": "Second Org"}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"]["code"].is_string());
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_get_organization() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "get-test-org", "name": "Get Test Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(&app, "/admin/v1/organizations/get-test-org").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], created["id"]);
        assert_eq!(body["slug"], "get-test-org");
    }

    #[tokio::test]
    async fn test_get_organization_not_found() {
        let app = test_app().await;
        let (status, body) = get_json(&app, "/admin/v1/organizations/nonexistent").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_list_organizations() {
        let app = test_app().await;

        // Get initial org count (includes default "local" org when auth is disabled)
        let (_, initial_body) = get_json(&app, "/admin/v1/organizations").await;
        let initial_count = initial_body["data"].as_array().unwrap().len();

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/organizations",
                json!({"slug": format!("list-org-{}", i), "name": format!("List Org {}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(&app, "/admin/v1/organizations").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), initial_count + 3);
        assert_eq!(body["pagination"]["limit"], 100);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_update_organization() {
        let app = test_app().await;

        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "update-org", "name": "Original Name"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = patch_json(
            &app,
            "/admin/v1/organizations/update-org",
            json!({"name": "Updated Name"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "Updated Name");
    }

    #[tokio::test]
    async fn test_delete_organization() {
        let app = test_app().await;

        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "delete-org", "name": "To Be Deleted"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = delete_json(&app, "/admin/v1/organizations/delete-org").await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(&app, "/admin/v1/organizations/delete-org").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ============================================================================
    // Project Tests
    // ============================================================================

    async fn create_org(app: &axum::Router, slug: &str) -> String {
        let (status, _) = post_json(
            app,
            "/admin/v1/organizations",
            json!({"slug": slug, "name": format!("Org {}", slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        slug.to_string()
    }

    #[tokio::test]
    async fn test_create_project() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-test-org").await;

        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "test-project", "name": "Test Project"}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["slug"], "test-project");
        assert_eq!(body["name"], "Test Project");
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_get_project() {
        let app = test_app().await;
        let org_slug = create_org(&app, "get-proj-org").await;

        let (status, created) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "get-project", "name": "Get Project"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects/get-project", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], created["id"]);
    }

    #[tokio::test]
    async fn test_list_projects() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-proj-org").await;

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                &format!("/admin/v1/organizations/{}/projects", org_slug),
                json!({"slug": format!("project-{}", i), "name": format!("Project {}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_delete_project() {
        let app = test_app().await;
        let org_slug = create_org(&app, "delete-proj-org").await;

        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "delete-project", "name": "To Be Deleted"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = delete_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/delete-project",
                org_slug
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/delete-project",
                org_slug
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ============================================================================
    // User Tests
    // ============================================================================

    #[tokio::test]
    async fn test_create_user() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "user-123", "email": "test@example.com", "name": "Test User"}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["external_id"], "user-123");
        assert_eq!(body["email"], "test@example.com");
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_get_user() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "get-user-123"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let user_id = created["id"].as_str().unwrap();
        let (status, body) = get_json(&app, &format!("/admin/v1/users/{}", user_id)).await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], user_id);
    }

    #[tokio::test]
    async fn test_list_users() {
        let app = test_app().await;

        // Get initial user count (includes default anonymous user when auth is disabled)
        let (_, initial_body) = get_json(&app, "/admin/v1/users").await;
        let initial_count = initial_body["data"].as_array().unwrap().len();

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/users",
                json!({"external_id": format!("list-user-{}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(&app, "/admin/v1/users").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), initial_count + 3);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_update_user() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "update-user-123", "name": "Original Name"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let user_id = created["id"].as_str().unwrap();
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/users/{}", user_id),
            json!({"name": "Updated Name", "email": "updated@example.com"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "Updated Name");
        assert_eq!(body["email"], "updated@example.com");
    }

    #[tokio::test]
    async fn test_delete_user() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "delete-user-123", "name": "To Be Deleted"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let user_id = created["id"].as_str().unwrap();
        let (status, body) = delete_json(&app, &format!("/admin/v1/users/{}", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], true);
        assert_eq!(body["user_id"], user_id);

        // Verify user is gone
        let (status, _) = get_json(&app, &format!("/admin/v1/users/{}", user_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_user_not_found() {
        let app = test_app().await;

        let (status, body) =
            delete_json(&app, "/admin/v1/users/00000000-0000-0000-0000-000000000000").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    // ============================================================================
    // API Key Tests
    // ============================================================================

    async fn create_org_with_id(app: &axum::Router, slug: &str) -> String {
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
    async fn test_create_api_key_for_org() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "api-key-org").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "Test API Key", "owner": {"type": "organization", "org_id": org_id}}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "Test API Key");
        assert!(body["key"].as_str().unwrap().starts_with("gw_live_"));
    }

    #[tokio::test]
    async fn test_list_api_keys_by_org() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "list-key-org").await;

        for i in 0..3 {
            let (status, resp) = post_json(
                &app,
                "/admin/v1/api-keys",
                json!({"name": format!("Key {}", i), "owner": {"type": "organization", "org_id": org_id}}),
            )
            .await;
            if status != StatusCode::CREATED {
                eprintln!(
                    "Failed to create key {}: {}",
                    i,
                    serde_json::to_string_pretty(&resp).unwrap()
                );
            }
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(&app, "/admin/v1/organizations/list-key-org/api-keys").await;
        if status != StatusCode::OK {
            eprintln!(
                "List keys error: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert!(body["pagination"].is_object());
    }

    #[tokio::test]
    async fn test_revoke_api_key() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "revoke-key-org").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "To Be Revoked", "owner": {"type": "organization", "org_id": org_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let key_id = created["id"].as_str().unwrap();
        let (status, _) = delete_json(&app, &format!("/admin/v1/api-keys/{}", key_id)).await;
        assert_eq!(status, StatusCode::OK);
    }

    // ============================================================================
    // Dynamic Provider Tests
    // ============================================================================

    #[tokio::test]
    async fn test_create_dynamic_provider() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "provider-org").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/dynamic-providers",
            json!({
                "name": "my-openai",
                "owner": {"type": "organization", "org_id": org_id},
                "provider_type": "open_ai",
                "base_url": "https://api.openai.com/v1",
                "models": ["gpt-4", "gpt-3.5-turbo"]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "my-openai");
        assert_eq!(body["provider_type"], "open_ai");
        assert!(body["is_enabled"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_get_dynamic_provider() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "get-provider-org").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/dynamic-providers",
            json!({
                "name": "get-provider",
                "owner": {"type": "organization", "org_id": org_id},
                "provider_type": "anthropic",
                "base_url": "https://api.anthropic.com/v1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let provider_id = created["id"].as_str().unwrap();
        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/dynamic-providers/{}", provider_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], provider_id);
        assert_eq!(body["name"], "get-provider");
    }

    #[tokio::test]
    async fn test_update_dynamic_provider() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "update-provider-org").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/dynamic-providers",
            json!({
                "name": "update-provider",
                "owner": {"type": "organization", "org_id": org_id},
                "provider_type": "open_ai",
                "base_url": "https://api.openai.com/v1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let provider_id = created["id"].as_str().unwrap();
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/dynamic-providers/{}", provider_id),
            json!({"base_url": "https://api.openai.com/v2", "is_enabled": false}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["base_url"], "https://api.openai.com/v2");
        assert!(!body["is_enabled"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_delete_dynamic_provider() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "delete-provider-org").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/dynamic-providers",
            json!({
                "name": "delete-provider",
                "owner": {"type": "organization", "org_id": org_id},
                "provider_type": "open_ai",
                "base_url": "https://api.openai.com/v1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let provider_id = created["id"].as_str().unwrap();
        let (status, _) = delete_json(
            &app,
            &format!("/admin/v1/dynamic-providers/{}", provider_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(
            &app,
            &format!("/admin/v1/dynamic-providers/{}", provider_id),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_dynamic_providers_by_org() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "list-provider-org").await;

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/dynamic-providers",
                json!({
                    "name": format!("provider-{}", i),
                    "owner": {"type": "organization", "org_id": org_id},
                    "provider_type": "open_ai",
                    "base_url": format!("https://93.184.215.{}/v1", i + 1)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/list-provider-org/dynamic-providers",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert!(body["pagination"].is_object());
    }

    // ============================================================================
    // Conversation Tests
    // ============================================================================

    async fn create_project_with_id(
        app: &axum::Router,
        org_slug: &str,
        project_slug: &str,
    ) -> String {
        let (status, project) = post_json(
            app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": project_slug, "name": format!("Project {}", project_slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        project["id"].as_str().unwrap().to_string()
    }

    async fn create_user_with_id(app: &axum::Router, external_id: &str) -> String {
        let (status, user) =
            post_json(app, "/admin/v1/users", json!({"external_id": external_id})).await;
        assert_eq!(status, StatusCode::CREATED);
        user["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_create_conversation_for_project() {
        let app = test_app().await;
        create_org(&app, "conv-proj-org").await;
        let project_id = create_project_with_id(&app, "conv-proj-org", "conv-project").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "project", "project_id": project_id},
                "title": "Test Conversation",
                "models": ["gpt-4", "claude-3"],
                "messages": [
                    {"role": "user", "content": "Hello!"},
                    {"role": "assistant", "content": "Hi there!"}
                ]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["title"], "Test Conversation");
        assert_eq!(body["models"].as_array().unwrap().len(), 2);
        assert_eq!(body["models"][0], "gpt-4");
        assert_eq!(body["owner_type"], "project");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_create_conversation_for_user() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "conv-user-123").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "title": "User Conversation"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["title"], "User Conversation");
        assert_eq!(body["owner_type"], "user");
        assert!(body["messages"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_conversation() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "get-conv-user").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "title": "Get Test Conversation"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let conv_id = created["id"].as_str().unwrap();
        let (status, body) = get_json(&app, &format!("/admin/v1/conversations/{}", conv_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], conv_id);
        assert_eq!(body["title"], "Get Test Conversation");
    }

    #[tokio::test]
    async fn test_list_conversations_by_project() {
        let app = test_app().await;
        create_org(&app, "list-conv-org").await;
        let project_id = create_project_with_id(&app, "list-conv-org", "list-conv-project").await;

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/conversations",
                json!({
                    "owner": {"type": "project", "project_id": project_id},
                    "title": format!("Conversation {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/list-conv-org/projects/list-conv-project/conversations",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert!(body["pagination"].is_object());
    }

    #[tokio::test]
    async fn test_list_conversations_by_user() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "list-conv-user").await;

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/conversations",
                json!({
                    "owner": {"type": "user", "user_id": user_id},
                    "title": format!("User Conversation {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/conversations", user_id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert!(body["pagination"].is_object());
    }

    #[tokio::test]
    async fn test_update_conversation() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "update-conv-user").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "title": "Original Title"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let conv_id = created["id"].as_str().unwrap();
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/conversations/{}", conv_id),
            json!({"title": "Updated Title", "models": ["claude-3", "gpt-4"]}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["title"], "Updated Title");
        assert_eq!(body["models"].as_array().unwrap().len(), 2);
        assert_eq!(body["models"][0], "claude-3");
    }

    #[tokio::test]
    async fn test_append_messages() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "append-msg-user").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "title": "Message Append Test",
                "messages": [{"role": "user", "content": "First message"}]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let conv_id = created["id"].as_str().unwrap();
        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/conversations/{}/messages", conv_id),
            json!({
                "messages": [
                    {"role": "assistant", "content": "Second message"},
                    {"role": "user", "content": "Third message"}
                ]
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.as_array().unwrap().len(), 3);
        assert_eq!(body[0]["content"], "First message");
        assert_eq!(body[1]["content"], "Second message");
        assert_eq!(body[2]["content"], "Third message");
    }

    #[tokio::test]
    async fn test_delete_conversation() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "delete-conv-user").await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "title": "To Be Deleted"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let conv_id = created["id"].as_str().unwrap();
        let (status, _) = delete_json(&app, &format!("/admin/v1/conversations/{}", conv_id)).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(&app, &format!("/admin/v1/conversations/{}", conv_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_conversation_owner_not_found() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/conversations",
            json!({
                "owner": {"type": "user", "user_id": "00000000-0000-0000-0000-000000000000"},
                "title": "Should Fail"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
        assert!(body["error"]["message"].is_string());
    }

    // ============================================================================
    // Provider Health Tests
    // ============================================================================

    #[tokio::test]
    async fn test_list_provider_health_empty() {
        let app = test_app().await;

        // By default, no providers have health checks enabled
        let (status, body) = get_json(&app, "/admin/v1/providers/health").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["providers"].is_array());
        assert!(body["providers"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_provider_health_not_found() {
        let app = test_app().await;

        // Provider without health checks enabled should return 404
        let (status, body) = get_json(&app, "/admin/v1/providers/nonexistent/health").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Health status not found")
        );
    }

    #[tokio::test]
    async fn test_list_circuit_breakers() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/providers/circuit-breakers").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["circuit_breakers"].is_array());
    }

    #[tokio::test]
    async fn test_get_circuit_breaker_not_found() {
        let app = test_app().await;

        let (status, body) =
            get_json(&app, "/admin/v1/providers/nonexistent/circuit-breaker").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Circuit breaker not found")
        );
    }

    // ============================================================================
    // Team Tests
    // ============================================================================

    #[tokio::test]
    async fn test_create_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "team-test-org").await;

        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "engineering", "name": "Engineering Team"}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["slug"], "engineering");
        assert_eq!(body["name"], "Engineering Team");
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_create_team_duplicate_slug() {
        let app = test_app().await;
        let org_slug = create_org(&app, "dup-team-org").await;

        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "duplicate-team", "name": "First Team"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "duplicate-team", "name": "Second Team"}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_get_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "get-team-org").await;

        let (status, created) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "get-team", "name": "Get Team"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams/get-team", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], created["id"]);
        assert_eq!(body["slug"], "get-team");
    }

    #[tokio::test]
    async fn test_get_team_not_found() {
        let app = test_app().await;
        let org_slug = create_org(&app, "team-404-org").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams/nonexistent", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_teams() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-team-org").await;

        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                &format!("/admin/v1/organizations/{}/teams", org_slug),
                json!({"slug": format!("team-{}", i), "name": format!("Team {}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) =
            get_json(&app, &format!("/admin/v1/organizations/{}/teams", org_slug)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_update_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "update-team-org").await;

        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "update-team", "name": "Original Name"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams/update-team", org_slug),
            json!({"name": "Updated Name"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "Updated Name");
    }

    #[tokio::test]
    async fn test_delete_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "delete-team-org").await;

        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": "delete-team", "name": "To Be Deleted"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = delete_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams/delete-team", org_slug),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/teams/delete-team", org_slug),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ============================================================================
    // Team Membership Tests
    // ============================================================================

    async fn create_team(app: &axum::Router, org_slug: &str, team_slug: &str) -> String {
        let (status, team) = post_json(
            app,
            &format!("/admin/v1/organizations/{}/teams", org_slug),
            json!({"slug": team_slug, "name": format!("Team {}", team_slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        team["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_list_team_members_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "member-list-org").await;
        create_team(&app, &org_slug, "empty-team").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/empty-team/members",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].as_array().unwrap().is_empty());
        assert!(body["pagination"].is_object());
    }

    #[tokio::test]
    async fn test_add_team_member() {
        let app = test_app().await;
        let org_slug = create_org(&app, "add-member-org").await;
        create_team(&app, &org_slug, "add-member-team").await;
        let user_id = create_user_with_id(&app, "team-member-user").await;

        let (status, body) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/add-member-team/members",
                org_slug
            ),
            json!({"user_id": user_id, "role": "developer"}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["user_id"], user_id);
        assert_eq!(body["role"], "developer");
    }

    #[tokio::test]
    async fn test_add_team_member_default_role() {
        let app = test_app().await;
        let org_slug = create_org(&app, "default-role-org").await;
        create_team(&app, &org_slug, "default-role-team").await;
        let user_id = create_user_with_id(&app, "default-role-user").await;

        let (status, body) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/default-role-team/members",
                org_slug
            ),
            json!({"user_id": user_id}),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["role"], "member");
    }

    #[tokio::test]
    async fn test_add_team_member_duplicate() {
        let app = test_app().await;
        let org_slug = create_org(&app, "dup-member-org").await;
        create_team(&app, &org_slug, "dup-member-team").await;
        let user_id = create_user_with_id(&app, "dup-member-user").await;

        let (status, _) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/dup-member-team/members",
                org_slug
            ),
            json!({"user_id": user_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/dup-member-team/members",
                org_slug
            ),
            json!({"user_id": user_id}),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_team_members() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-members-org").await;
        create_team(&app, &org_slug, "list-members-team").await;

        for i in 0..3 {
            let user_id = create_user_with_id(&app, &format!("list-member-{}", i)).await;
            let (status, _) = post_json(
                &app,
                &format!(
                    "/admin/v1/organizations/{}/teams/list-members-team/members",
                    org_slug
                ),
                json!({"user_id": user_id}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/list-members-team/members",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_update_team_member() {
        let app = test_app().await;
        let org_slug = create_org(&app, "update-member-org").await;
        create_team(&app, &org_slug, "update-member-team").await;
        let user_id = create_user_with_id(&app, "update-member-user").await;

        let (status, _) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/update-member-team/members",
                org_slug
            ),
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = patch_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/update-member-team/members/{}",
                org_slug, user_id
            ),
            json!({"role": "admin"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["role"], "admin");
    }

    #[tokio::test]
    async fn test_remove_team_member() {
        let app = test_app().await;
        let org_slug = create_org(&app, "remove-member-org").await;
        create_team(&app, &org_slug, "remove-member-team").await;
        let user_id = create_user_with_id(&app, "remove-member-user").await;

        let (status, _) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/remove-member-team/members",
                org_slug
            ),
            json!({"user_id": user_id}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = delete_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/remove-member-team/members/{}",
                org_slug, user_id
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Verify member is gone
        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/remove-member-team/members",
                org_slug
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_remove_team_member_not_found() {
        let app = test_app().await;
        let org_slug = create_org(&app, "remove-404-org").await;
        create_team(&app, &org_slug, "remove-404-team").await;

        let (status, body) = delete_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/remove-404-team/members/00000000-0000-0000-0000-000000000000",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    // ============================================================================
    // Usage Endpoint Tests
    // ============================================================================

    async fn create_api_key_with_id(app: &axum::Router, org_id: &str) -> String {
        let (status, key) = post_json(
            app,
            "/admin/v1/api-keys",
            json!({"name": "Usage Test Key", "owner": {"type": "organization", "org_id": org_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        key["id"].as_str().unwrap().to_string()
    }

    // --- API Key Usage Tests ---

    #[tokio::test]
    async fn test_get_api_key_usage_summary_empty() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-sum-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(&app, &format!("/admin/v1/api-keys/{}/usage", key_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["total_tokens"], 0);
        assert_eq!(body["request_count"], 0);
    }

    #[tokio::test]
    async fn test_get_api_key_usage_summary_with_date_range() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-date-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/api-keys/{}/usage?start_date=2024-01-01&end_date=2024-12-31",
                key_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
    }

    #[tokio::test]
    async fn test_get_api_key_usage_summary_invalid_date_range() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-invalid-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        // end_date before start_date should return 400
        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/api-keys/{}/usage?start_date=2024-12-31&end_date=2024-01-01",
                key_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("end_date must be >= start_date")
        );
    }

    #[tokio::test]
    async fn test_get_api_key_usage_by_date_empty() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-bydate-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/api-keys/{}/usage/by-date", key_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_api_key_usage_by_model_empty() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-bymodel-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/api-keys/{}/usage/by-model", key_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_api_key_usage_by_referer_empty() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-byref-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/api-keys/{}/usage/by-referer", key_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_api_key_usage_forecast() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-forecast-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/api-keys/{}/usage/forecast", key_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
        assert_eq!(body["avg_daily_spend"], 0.0);
        assert!(body["sample_days"].is_i64());
    }

    #[tokio::test]
    async fn test_get_api_key_usage_forecast_with_params() {
        let app = test_app().await;
        let org_id = create_org_with_id(&app, "usage-forecast-params-org").await;
        let key_id = create_api_key_with_id(&app, &org_id).await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/api-keys/{}/usage/forecast?lookback_days=7&forecast_days=14",
                key_id
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
    }

    #[tokio::test]
    async fn test_get_api_key_usage_summary_nonexistent_returns_not_found() {
        let app = test_app().await;

        // The usage helper now pre-fetches the key to derive its tenant scope
        // (issue 2), so non-existent ids return 404 instead of an empty 200 —
        // an empty 200 would let an attacker probe key ids and distinguish
        // "exists in another tenant" (403) from "doesn't exist" (200).
        let (status, body) = get_json(
            &app,
            "/admin/v1/api-keys/00000000-0000-0000-0000-000000000000/usage",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_get_api_key_usage_forecast_not_found() {
        let app = test_app().await;

        // Forecast endpoint validates the API key exists
        let (status, body) = get_json(
            &app,
            "/admin/v1/api-keys/00000000-0000-0000-0000-000000000000/usage/forecast",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    // --- Organization Usage Tests ---

    #[tokio::test]
    async fn test_get_org_usage_summary_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "org-usage-sum").await;

        let (status, body) =
            get_json(&app, &format!("/admin/v1/organizations/{}/usage", org_slug)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["total_tokens"], 0);
        assert_eq!(body["request_count"], 0);
    }

    #[tokio::test]
    async fn test_get_org_usage_by_date_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "org-usage-bydate").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/usage/by-date", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_org_usage_by_model_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "org-usage-bymodel").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/usage/by-model", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_org_usage_by_provider_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "org-usage-byprov").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/usage/by-provider", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_org_usage_forecast() {
        let app = test_app().await;
        let org_slug = create_org(&app, "org-usage-forecast").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/usage/forecast", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
        assert_eq!(body["avg_daily_spend"], 0.0);
    }

    #[tokio::test]
    async fn test_get_org_usage_not_found() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/organizations/nonexistent-org/usage").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Organization not found")
        );
    }

    // --- Project Usage Tests ---

    #[tokio::test]
    async fn test_get_project_usage_summary_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-usage-sum-org").await;
        create_project_with_id(&app, &org_slug, "proj-usage-sum").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/proj-usage-sum/usage",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["total_tokens"], 0);
        assert_eq!(body["request_count"], 0);
    }

    #[tokio::test]
    async fn test_get_project_usage_by_date_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-usage-bydate-org").await;
        create_project_with_id(&app, &org_slug, "proj-usage-bydate").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/proj-usage-bydate/usage/by-date",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_project_usage_by_model_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-usage-bymodel-org").await;
        create_project_with_id(&app, &org_slug, "proj-usage-bymodel").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/proj-usage-bymodel/usage/by-model",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_project_usage_forecast() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-usage-forecast-org").await;
        create_project_with_id(&app, &org_slug, "proj-usage-forecast").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/proj-usage-forecast/usage/forecast",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
        assert_eq!(body["avg_daily_spend"], 0.0);
    }

    #[tokio::test]
    async fn test_get_project_usage_org_not_found() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/nonexistent-org/projects/some-project/usage",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Organization not found")
        );
    }

    #[tokio::test]
    async fn test_get_project_usage_project_not_found() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-usage-404-org").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/nonexistent-project/usage",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Project not found")
        );
    }

    // --- User Usage Tests ---

    #[tokio::test]
    async fn test_get_user_usage_summary_empty() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "user-usage-sum").await;

        let (status, body) = get_json(&app, &format!("/admin/v1/users/{}/usage", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["total_tokens"], 0);
        assert_eq!(body["request_count"], 0);
    }

    #[tokio::test]
    async fn test_get_user_usage_by_date_empty() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "user-usage-bydate").await;

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/usage/by-date", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_user_usage_by_model_empty() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "user-usage-bymodel").await;

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/usage/by-model", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_user_usage_forecast() {
        let app = test_app().await;
        let user_id = create_user_with_id(&app, "user-usage-forecast").await;

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/usage/forecast", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
        assert_eq!(body["avg_daily_spend"], 0.0);
    }

    #[tokio::test]
    async fn test_get_user_usage_not_found() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/admin/v1/users/00000000-0000-0000-0000-000000000000/usage",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("User not found")
        );
    }

    // --- Provider Usage Tests ---

    #[tokio::test]
    async fn test_get_provider_usage_summary_empty() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/providers/openai/usage").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["total_tokens"], 0);
        assert_eq!(body["request_count"], 0);
    }

    #[tokio::test]
    async fn test_get_provider_usage_by_date_empty() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/providers/anthropic/usage/by-date").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_provider_usage_by_model_empty() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/providers/openai/usage/by-model").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_provider_usage_forecast() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/providers/openai/usage/forecast").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
        assert_eq!(body["avg_daily_spend"], 0.0);
    }

    #[tokio::test]
    async fn test_get_provider_usage_forecast_with_params() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/admin/v1/providers/anthropic/usage/forecast?lookback_days=14&forecast_days=7",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["current_spend"], 0.0);
    }

    // ============================================================================
    // Model Pricing Tests
    // ============================================================================

    #[tokio::test]
    async fn test_create_model_pricing_global() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4",
                "input_per_1m_tokens": 30000000,
                "output_per_1m_tokens": 60000000
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
        assert!(body["id"].is_string());
        assert_eq!(body["provider"], "openai");
        assert_eq!(body["model"], "gpt-4");
        assert_eq!(body["input_per_1m_tokens"], 30000000);
        assert_eq!(body["output_per_1m_tokens"], 60000000);
        assert_eq!(body["owner"]["type"], "global");
    }

    #[tokio::test]
    async fn test_create_model_pricing_org_scope() {
        let app = test_app().await;
        let org_slug = create_org(&app, "pricing-org").await;

        // Get org ID
        let (status, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        assert_eq!(status, StatusCode::OK);
        let org_id = org["id"].as_str().unwrap();

        let (status, body) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "organization", "org_id": org_id},
                "provider": "anthropic",
                "model": "claude-3-opus",
                "input_per_1m_tokens": 15000000,
                "output_per_1m_tokens": 75000000
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["owner"]["type"], "organization");
        assert_eq!(body["owner"]["org_id"], org_id);
        assert_eq!(body["provider"], "anthropic");
        assert_eq!(body["model"], "claude-3-opus");
    }

    #[tokio::test]
    async fn test_get_model_pricing() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-turbo",
                "input_per_1m_tokens": 10000000,
                "output_per_1m_tokens": 30000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let pricing_id = created["id"].as_str().unwrap();
        let (status, body) =
            get_json(&app, &format!("/admin/v1/model-pricing/{}", pricing_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], pricing_id);
        assert_eq!(body["model"], "gpt-4-turbo");
    }

    #[tokio::test]
    async fn test_get_model_pricing_not_found() {
        let app = test_app().await;

        let fake_id = "00000000-0000-0000-0000-000000000000";
        let (status, body) = get_json(&app, &format!("/admin/v1/model-pricing/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_update_model_pricing() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-3.5-turbo",
                "input_per_1m_tokens": 500000,
                "output_per_1m_tokens": 1500000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let pricing_id = created["id"].as_str().unwrap();
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/model-pricing/{}", pricing_id),
            json!({
                "input_per_1m_tokens": 250000,
                "output_per_1m_tokens": 750000,
                "cached_input_per_1m_tokens": 125000
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["input_per_1m_tokens"], 250000);
        assert_eq!(body["output_per_1m_tokens"], 750000);
        assert_eq!(body["cached_input_per_1m_tokens"], 125000);
    }

    #[tokio::test]
    async fn test_update_model_pricing_not_found() {
        let app = test_app().await;

        let fake_id = "00000000-0000-0000-0000-000000000000";
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/model-pricing/{}", fake_id),
            json!({"input_per_1m_tokens": 100000}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_delete_model_pricing() {
        let app = test_app().await;

        let (status, created) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-delete-test",
                "input_per_1m_tokens": 1000000,
                "output_per_1m_tokens": 2000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let pricing_id = created["id"].as_str().unwrap();
        let (status, _) =
            delete_json(&app, &format!("/admin/v1/model-pricing/{}", pricing_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Verify deletion
        let (status, _) = get_json(&app, &format!("/admin/v1/model-pricing/{}", pricing_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_model_pricing_not_found() {
        let app = test_app().await;

        let fake_id = "00000000-0000-0000-0000-000000000000";
        let (status, body) =
            delete_json(&app, &format!("/admin/v1/model-pricing/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_list_global_model_pricing() {
        let app = test_app().await;

        // Create some global pricing entries
        for model in ["gpt-4-list-1", "gpt-4-list-2", "gpt-4-list-3"] {
            let (status, _) = post_json(
                &app,
                "/admin/v1/model-pricing",
                json!({
                    "owner": {"type": "global"},
                    "provider": "openai",
                    "model": model,
                    "input_per_1m_tokens": 1000000,
                    "output_per_1m_tokens": 2000000
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(&app, "/admin/v1/model-pricing").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert!(body["data"].as_array().unwrap().len() >= 3);
        assert!(body["pagination"]["limit"].is_number());
    }

    #[tokio::test]
    async fn test_list_org_model_pricing() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-pricing-org").await;

        // Get org ID
        let (status, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        assert_eq!(status, StatusCode::OK);
        let org_id = org["id"].as_str().unwrap();

        // Create org-scoped pricing
        let (status, _) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "organization", "org_id": org_id},
                "provider": "anthropic",
                "model": "claude-3-sonnet",
                "input_per_1m_tokens": 3000000,
                "output_per_1m_tokens": 15000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/model-pricing", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_list_org_model_pricing_org_not_found() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/nonexistent-org/model-pricing",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["message"].is_string());
    }

    #[tokio::test]
    async fn test_list_project_model_pricing() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-pricing-org").await;

        // Create project
        let (status, project) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "pricing-project", "name": "Pricing Project"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let project_id = project["id"].as_str().unwrap();

        // Create project-scoped pricing
        let (status, _) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "project", "project_id": project_id},
                "provider": "openai",
                "model": "gpt-4-proj",
                "input_per_1m_tokens": 5000000,
                "output_per_1m_tokens": 10000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/pricing-project/model-pricing",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_list_user_model_pricing() {
        let app = test_app().await;

        // Create user
        let (status, user) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "pricing-user", "name": "Pricing User"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let user_id = user["id"].as_str().unwrap();

        // Create user-scoped pricing
        let (status, _) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "provider": "openai",
                "model": "gpt-4-user",
                "input_per_1m_tokens": 2000000,
                "output_per_1m_tokens": 4000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/model-pricing", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_list_by_provider() {
        let app = test_app().await;

        // Create pricing for specific provider
        let (status, _) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "test-provider",
                "model": "test-model-1",
                "input_per_1m_tokens": 1000000,
                "output_per_1m_tokens": 2000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = get_json(&app, "/admin/v1/model-pricing/provider/test-provider").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert!(!body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_upsert_model_pricing_create() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/model-pricing/upsert",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-upsert-new",
                "input_per_1m_tokens": 30000000,
                "output_per_1m_tokens": 60000000
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["id"].is_string());
        assert_eq!(body["model"], "gpt-4-upsert-new");
    }

    #[tokio::test]
    async fn test_upsert_model_pricing_update() {
        let app = test_app().await;

        // Create initial pricing
        let (status, created) = post_json(
            &app,
            "/admin/v1/model-pricing/upsert",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-upsert-existing",
                "input_per_1m_tokens": 30000000,
                "output_per_1m_tokens": 60000000
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let original_id = created["id"].as_str().unwrap();

        // Upsert with updated values
        let (status, updated) = post_json(
            &app,
            "/admin/v1/model-pricing/upsert",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-upsert-existing",
                "input_per_1m_tokens": 25000000,
                "output_per_1m_tokens": 50000000
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["id"], original_id); // Same ID
        assert_eq!(updated["input_per_1m_tokens"], 25000000);
        assert_eq!(updated["output_per_1m_tokens"], 50000000);
    }

    #[tokio::test]
    async fn test_bulk_upsert_model_pricing() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/model-pricing/bulk",
            json!([
                {
                    "owner": {"type": "global"},
                    "provider": "openai",
                    "model": "gpt-4-bulk-1",
                    "input_per_1m_tokens": 30000000,
                    "output_per_1m_tokens": 60000000
                },
                {
                    "owner": {"type": "global"},
                    "provider": "openai",
                    "model": "gpt-4-bulk-2",
                    "input_per_1m_tokens": 10000000,
                    "output_per_1m_tokens": 30000000
                },
                {
                    "owner": {"type": "global"},
                    "provider": "anthropic",
                    "model": "claude-3-bulk",
                    "input_per_1m_tokens": 15000000,
                    "output_per_1m_tokens": 75000000
                }
            ]),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["count"], 3);
    }

    #[tokio::test]
    async fn test_model_pricing_with_optional_fields() {
        let app = test_app().await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/model-pricing",
            json!({
                "owner": {"type": "global"},
                "provider": "openai",
                "model": "gpt-4-vision",
                "input_per_1m_tokens": 10000000,
                "output_per_1m_tokens": 30000000,
                "per_image": 500000,
                "cached_input_per_1m_tokens": 5000000,
                "cache_write_per_1m_tokens": 7500000,
                "reasoning_per_1m_tokens": 15000000,
                "source": "manual"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["per_image"], 500000);
        assert_eq!(body["cached_input_per_1m_tokens"], 5000000);
        assert_eq!(body["cache_write_per_1m_tokens"], 7500000);
        assert_eq!(body["reasoning_per_1m_tokens"], 15000000);
        assert_eq!(body["source"], "manual");
    }

    // ============================================================================
    // Audit Log Tests
    // ============================================================================

    #[tokio::test]
    async fn test_list_audit_logs_empty() {
        let app = test_app().await;

        // Fresh database should have no audit logs (or only system-generated ones)
        let (status, body) = get_json(&app, "/admin/v1/audit-logs").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert!(body["pagination"]["limit"].is_number());
    }

    #[tokio::test]
    async fn test_list_audit_logs_after_org_create() {
        let app = test_app().await;

        // Create an organization - this generates an audit log
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "audit-test-org", "name": "Audit Test Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // List audit logs - should contain the organization.create entry
        let (status, body) = get_json(&app, "/admin/v1/audit-logs").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());

        let logs = body["data"].as_array().unwrap();
        let org_create_log = logs.iter().find(|l| {
            l["action"] == "organization.create" && l["details"]["slug"] == "audit-test-org"
        });
        assert!(
            org_create_log.is_some(),
            "Should find organization.create audit log"
        );

        let log = org_create_log.unwrap();
        assert_eq!(log["resource_type"], "organization");
        assert!(log["id"].is_string());
        assert!(log["timestamp"].is_string());
    }

    #[tokio::test]
    async fn test_list_audit_logs_filter_by_action() {
        let app = test_app().await;

        // Create an organization to generate audit log
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "filter-action-org", "name": "Filter Action Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Filter by action
        let (status, body) =
            get_json(&app, "/admin/v1/audit-logs?action=organization.create").await;

        assert_eq!(status, StatusCode::OK);
        let logs = body["data"].as_array().unwrap();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|l| l["action"] == "organization.create"));
    }

    #[tokio::test]
    async fn test_list_audit_logs_filter_by_resource_type() {
        let app = test_app().await;

        // Create an organization
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "filter-resource-org", "name": "Filter Resource Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Filter by resource_type
        let (status, body) =
            get_json(&app, "/admin/v1/audit-logs?resource_type=organization").await;

        assert_eq!(status, StatusCode::OK);
        let logs = body["data"].as_array().unwrap();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|l| l["resource_type"] == "organization"));
    }

    #[tokio::test]
    async fn test_list_audit_logs_filter_by_org_id() {
        let app = test_app().await;

        // Create an organization
        let (status, created_org) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "filter-org-id-org", "name": "Filter Org ID Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let org_id = created_org["id"].as_str().unwrap();

        // Filter by org_id
        let (status, body) =
            get_json(&app, &format!("/admin/v1/audit-logs?org_id={}", org_id)).await;

        assert_eq!(status, StatusCode::OK);
        let logs = body["data"].as_array().unwrap();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|l| l["org_id"] == org_id));
    }

    #[tokio::test]
    async fn test_list_audit_logs_pagination_limit() {
        let app = test_app().await;

        // Create multiple organizations to generate multiple audit logs
        for i in 0..5 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/organizations",
                json!({"slug": format!("pagination-org-{}", i), "name": format!("Pagination Org {}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        // Request with limit=2
        let (status, body) = get_json(&app, "/admin/v1/audit-logs?limit=2").await;

        assert_eq!(status, StatusCode::OK);
        let logs = body["data"].as_array().unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(body["pagination"]["limit"], 2);
    }

    #[tokio::test]
    async fn test_list_audit_logs_cursor_pagination() {
        let app = test_app().await;

        // Create multiple organizations to generate multiple audit logs
        for i in 0..5 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/organizations",
                json!({"slug": format!("cursor-org-{}", i), "name": format!("Cursor Org {}", i)}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        // Get first page filtered by action to ensure consistent results
        let (_, first_page) = get_json(
            &app,
            "/admin/v1/audit-logs?action=organization.create&limit=2",
        )
        .await;
        let first_logs = first_page["data"].as_array().unwrap();
        assert_eq!(first_logs.len(), 2, "Should have 2 logs on first page");
        assert!(first_page["pagination"]["has_more"].as_bool().unwrap());

        // Get the next cursor from pagination
        let next_cursor = first_page["pagination"]["next_cursor"]
            .as_str()
            .expect("Should have next_cursor for pagination");

        // Get second page using cursor
        let (status, second_page) = get_json(
            &app,
            &format!(
                "/admin/v1/audit-logs?action=organization.create&limit=2&cursor={}",
                next_cursor
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let second_logs = second_page["data"].as_array().unwrap();
        assert_eq!(second_logs.len(), 2, "Should have 2 logs on second page");

        // Collect all IDs from both pages
        let first_page_ids: Vec<&str> = first_logs
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        let second_page_ids: Vec<&str> = second_logs
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();

        // Ensure no overlap between pages
        for id in &second_page_ids {
            assert!(
                !first_page_ids.contains(id),
                "Second page should not contain any IDs from first page"
            );
        }
    }

    #[tokio::test]
    async fn test_list_audit_logs_invalid_direction() {
        let app = test_app().await;

        // Invalid direction should return 400
        let (status, body) = get_json(&app, "/admin/v1/audit-logs?direction=invalid").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid direction")
        );
    }

    #[tokio::test]
    async fn test_get_audit_log_by_id() {
        let app = test_app().await;

        // Create an organization to generate an audit log
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "get-audit-org", "name": "Get Audit Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // List audit logs to get an ID
        let (_, list_body) = get_json(&app, "/admin/v1/audit-logs").await;
        let logs = list_body["data"].as_array().unwrap();
        assert!(!logs.is_empty());

        let log_id = logs[0]["id"].as_str().unwrap();

        // Get the specific audit log by ID
        let (status, body) = get_json(&app, &format!("/admin/v1/audit-logs/{}", log_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], log_id);
        assert!(body["action"].is_string());
        assert!(body["resource_type"].is_string());
        assert!(body["timestamp"].is_string());
    }

    #[tokio::test]
    async fn test_get_audit_log_not_found() {
        let app = test_app().await;

        let fake_id = "00000000-0000-0000-0000-000000000000";
        let (status, body) = get_json(&app, &format!("/admin/v1/audit-logs/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_get_audit_log_invalid_id() {
        let app = test_app().await;

        let (status, _) = get_json(&app, "/admin/v1/audit-logs/not-a-uuid").await;

        // Invalid UUID format should return 400
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_audit_logs_multiple_actions() {
        let app = test_app().await;

        // Create organization
        let (status, org) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "multi-action-org", "name": "Multi Action Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let org_slug = org["slug"].as_str().unwrap();

        // Update organization (generates organization.update audit log)
        let (status, _) = patch_json(
            &app,
            &format!("/admin/v1/organizations/{}", org_slug),
            json!({"name": "Updated Multi Action Org"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // List all audit logs for this org
        let org_id = org["id"].as_str().unwrap();
        let (status, body) =
            get_json(&app, &format!("/admin/v1/audit-logs?org_id={}", org_id)).await;

        assert_eq!(status, StatusCode::OK);
        let logs = body["data"].as_array().unwrap();

        // Should have both create and update logs
        let actions: Vec<&str> = logs.iter().map(|l| l["action"].as_str().unwrap()).collect();
        assert!(actions.contains(&"organization.create"));
        assert!(actions.contains(&"organization.update"));
    }

    // ============================================================================
    // Access Review Tests
    // ============================================================================

    #[tokio::test]
    async fn test_access_inventory_empty() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/access-reviews/inventory").await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert!(body["generated_at"].is_string());
        assert!(body["total_users"].is_number());
        assert!(body["users"].is_array());
        assert!(body["summary"].is_object());
        assert!(body["summary"]["total_organizations"].is_number());
        assert!(body["summary"]["total_projects"].is_number());
    }

    #[tokio::test]
    async fn test_access_inventory_with_users() {
        let app = test_app().await;

        // Create an org
        let org_slug = create_org(&app, "inventory-org").await;

        // Create a user
        let user_id = create_user_with_id(&app, "inventory-user").await;

        // Add user to org
        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/members", org_slug),
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get inventory
        let (status, body) = get_json(&app, "/admin/v1/access-reviews/inventory").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["total_users"].as_i64().unwrap() >= 1);

        // Find our user in the inventory
        let users = body["users"].as_array().unwrap();
        let our_user = users.iter().find(|u| u["external_id"] == "inventory-user");
        assert!(our_user.is_some(), "User should appear in inventory");

        let user_entry = our_user.unwrap();
        assert!(user_entry["organizations"].is_array());
        assert!(!user_entry["organizations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_access_inventory_with_org_filter() {
        let app = test_app().await;

        // Create two orgs
        create_org(&app, "filter-org-1").await;
        let (_, org2) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "filter-org-2", "name": "Filter Org 2"}),
        )
        .await;
        let org2_id = org2["id"].as_str().unwrap();

        // Create users and add to different orgs
        let user1_id = create_user_with_id(&app, "filter-user-1").await;
        let user2_id = create_user_with_id(&app, "filter-user-2").await;

        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations/filter-org-1/members",
            json!({"user_id": user1_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations/filter-org-2/members",
            json!({"user_id": user2_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get inventory filtered by org2
        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/access-reviews/inventory?org_id={}", org2_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let users = body["users"].as_array().unwrap();

        // Only user2 should be in the filtered results
        let user_ids: Vec<&str> = users
            .iter()
            .map(|u| u["external_id"].as_str().unwrap())
            .collect();
        assert!(user_ids.contains(&"filter-user-2"));
        assert!(!user_ids.contains(&"filter-user-1"));
    }

    #[tokio::test]
    async fn test_access_inventory_pagination() {
        let app = test_app().await;

        // Get inventory with limit
        let (status, body) =
            get_json(&app, "/admin/v1/access-reviews/inventory?limit=5&offset=0").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["users"].is_array());
        // The limit should be respected (may have fewer if not enough users)
        assert!(body["users"].as_array().unwrap().len() <= 5);
    }

    #[tokio::test]
    async fn test_stale_access_empty() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/access-reviews/stale").await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert!(body["generated_at"].is_string());
        assert!(body["inactive_days_threshold"].is_number());
        assert!(body["cutoff_date"].is_string());
        assert!(body["stale_users"].is_array());
        assert!(body["stale_api_keys"].is_array());
        assert!(body["never_active_users"].is_array());
        assert!(body["summary"].is_object());
    }

    #[tokio::test]
    async fn test_stale_access_with_inactive_days_param() {
        let app = test_app().await;

        // Test with custom inactive_days
        let (status, body) =
            get_json(&app, "/admin/v1/access-reviews/stale?inactive_days=30").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["inactive_days_threshold"], 30);
    }

    #[tokio::test]
    async fn test_stale_access_with_org_filter() {
        let app = test_app().await;

        // Create org
        let (_, org) = post_json(
            &app,
            "/admin/v1/organizations",
            json!({"slug": "stale-org", "name": "Stale Org"}),
        )
        .await;
        let org_id = org["id"].as_str().unwrap();

        // Get stale access filtered by org
        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/access-reviews/stale?org_id={}", org_id),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["summary"]["total_users_scanned"].is_number());
    }

    #[tokio::test]
    async fn test_stale_access_never_active_users() {
        let app = test_app().await;

        // Create a user (who has never performed any action that would be logged)
        let user_id = create_user_with_id(&app, "never-active-user").await;

        // Create an org and add user to it
        create_org(&app, "never-active-org").await;
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations/never-active-org/members",
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get stale access - the user should appear as never active
        // (since they haven't performed any actions as an actor)
        let (status, body) = get_json(&app, "/admin/v1/access-reviews/stale").await;

        assert_eq!(status, StatusCode::OK);
        // The user might appear in never_active_users since they haven't had audit log activity
        assert!(body["never_active_users"].is_array());
    }

    #[tokio::test]
    async fn test_org_access_report() {
        let app = test_app().await;

        // Create org with members and projects
        let org_slug = create_org(&app, "access-report-org").await;
        let user_id = create_user_with_id(&app, "access-report-user").await;

        // Add user to org
        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/members", org_slug),
            json!({"user_id": user_id, "role": "admin"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Create a project
        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "access-report-proj", "name": "Access Report Project"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get access report for the org
        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/access-report", org_slug),
        )
        .await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert!(body["generated_at"].is_string());
        assert_eq!(body["org_slug"], org_slug);
        assert!(body["members"].is_array());
        assert!(body["api_keys"].is_array());
        assert!(body["access_history"].is_array());
        assert!(body["summary"].is_object());

        // Verify member is in the report
        let members = body["members"].as_array().unwrap();
        let our_member = members
            .iter()
            .find(|m| m["external_id"] == "access-report-user");
        assert!(our_member.is_some());
        // Verify member has a role field (actual role depends on endpoint behavior)
        assert!(our_member.unwrap()["role"].is_string());
    }

    #[tokio::test]
    async fn test_org_access_report_not_found() {
        let app = test_app().await;

        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/nonexistent-org/access-report",
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
    async fn test_org_access_report_with_api_keys() {
        let app = test_app().await;

        // Create org
        let org_id = create_org_with_id(&app, "api-key-report-org").await;

        // Create API key for the org
        let (status, _) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "Report Test Key", "owner": {"type": "organization", "org_id": org_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get access report
        let (status, body) = get_json(
            &app,
            "/admin/v1/organizations/api-key-report-org/access-report",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let api_keys = body["api_keys"].as_array().unwrap();
        assert!(!api_keys.is_empty());

        // Verify our key is in the report
        let our_key = api_keys.iter().find(|k| k["name"] == "Report Test Key");
        assert!(our_key.is_some());
        assert!(our_key.unwrap()["is_active"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_user_access_summary() {
        let app = test_app().await;

        // Create user
        let user_id = create_user_with_id(&app, "summary-user").await;

        // Create org and add user
        let org_slug = create_org(&app, "summary-org").await;
        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/members", org_slug),
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Create project and add user
        let project_id = create_project_with_id(&app, &org_slug, "summary-project").await;
        let (status, _) = post_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/summary-project/members",
                org_slug
            ),
            json!({"user_id": user_id, "role": "contributor"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Create API key for the project owned by user
        let (status, _) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "User Summary Key", "owner": {"type": "project", "project_id": project_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get user access summary
        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/access-summary", user_id)).await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);
        assert!(body["generated_at"].is_string());
        assert_eq!(body["user_id"], user_id);
        assert_eq!(body["external_id"], "summary-user");
        assert!(body["organizations"].is_array());
        assert!(body["projects"].is_array());
        assert!(body["api_keys"].is_array());
        assert!(body["summary"].is_object());

        // Verify org membership
        let orgs = body["organizations"].as_array().unwrap();
        let our_org = orgs.iter().find(|o| o["org_slug"] == "summary-org");
        assert!(our_org.is_some());
        // Verify org entry has expected fields
        assert!(our_org.unwrap()["role"].is_string());
        assert!(our_org.unwrap()["granted_at"].is_string());

        // Verify project membership
        let projects = body["projects"].as_array().unwrap();
        let our_project = projects
            .iter()
            .find(|p| p["project_slug"] == "summary-project");
        assert!(our_project.is_some());
        // Verify project entry has expected fields
        assert!(our_project.unwrap()["role"].is_string());
        assert!(our_project.unwrap()["granted_at"].is_string());
    }

    #[tokio::test]
    async fn test_user_access_summary_not_found() {
        let app = test_app().await;

        let fake_id = "00000000-0000-0000-0000-000000000000";
        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/access-summary", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn test_user_access_summary_invalid_id() {
        let app = test_app().await;

        let (status, _) = get_json(&app, "/admin/v1/users/not-a-uuid/access-summary").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_access_inventory_summary_stats() {
        let app = test_app().await;

        // Create some data
        let org_slug = create_org(&app, "stats-org").await;
        create_project_with_id(&app, &org_slug, "stats-project").await;
        let user_id = create_user_with_id(&app, "stats-user").await;

        // Add user to org
        let (status, _) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/members", org_slug),
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Get inventory and check summary
        let (status, body) = get_json(&app, "/admin/v1/access-reviews/inventory").await;

        assert_eq!(status, StatusCode::OK);
        let summary = &body["summary"];

        // Summary should have expected fields with non-negative values
        assert!(summary["total_organizations"].as_i64().unwrap() >= 1);
        assert!(summary["total_projects"].as_i64().unwrap() >= 1);
        assert!(summary["total_org_memberships"].as_i64().unwrap() >= 1);
        assert!(summary["total_project_memberships"].as_i64().unwrap() >= 0);
        assert!(summary["total_active_api_keys"].as_i64().unwrap() >= 0);
    }

    #[tokio::test]
    async fn test_stale_access_summary_stats() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/access-reviews/stale").await;

        assert_eq!(status, StatusCode::OK);
        let summary = &body["summary"];

        // Summary should have all expected fields
        assert!(summary["total_users_scanned"].is_number());
        assert!(summary["stale_users_count"].is_number());
        assert!(summary["never_active_users_count"].is_number());
        assert!(summary["total_api_keys_scanned"].is_number());
        assert!(summary["stale_api_keys_count"].is_number());
        assert!(summary["never_used_api_keys_count"].is_number());
    }

    // ============================================================================
    // Dead Letter Queue (DLQ) Tests
    // ============================================================================

    use std::sync::Arc;

    /// Create a test application with DLQ configured.
    /// Returns the app and a reference to the DLQ for seeding test data.
    async fn test_app_with_dlq() -> (axum::Router, Arc<dyn crate::dlq::DeadLetterQueue>) {
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
path = "file:test_dlq_db_{db_id}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000
{session_section}
[providers.test-openai]
type = "open_ai"
api_key = "sk-test-key"

[observability.dead_letter_queue]
type = "database"
table_name = "dead_letter_queue"
max_entries = 1000
ttl_secs = 86400
"#
        );

        let config =
            crate::config::GatewayConfig::parse(&config_str).expect("Failed to parse test config");
        let state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");

        let dlq = state.dlq.clone().expect("DLQ should be configured");
        let app = crate::build_app(&config, state);

        (app, dlq)
    }

    /// Helper to create a DLQ entry for testing.
    fn create_test_dlq_entry(entry_type: &str, payload: &str, error: &str) -> crate::dlq::DlqEntry {
        crate::dlq::DlqEntry::new(entry_type, payload, error)
    }

    #[tokio::test]
    async fn test_dlq_not_configured() {
        // Use standard test_app which doesn't have DLQ configured
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/dlq").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not configured")
        );
    }

    #[tokio::test]
    async fn test_dlq_list_empty() {
        let (app, _dlq) = test_app_with_dlq().await;

        let (status, body) = get_json(&app, "/admin/v1/dlq").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 0);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_dlq_list_with_entries() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed some entries
        for i in 0..3 {
            let entry = create_test_dlq_entry(
                "usage_log",
                &format!(r#"{{"request_id": "req-{}"}}"#, i),
                "Test error",
            );
            dlq.push(entry).await.expect("Failed to push entry");
        }

        let (status, body) = get_json(&app, "/admin/v1/dlq").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 3);

        // Verify entry structure
        let first_entry = &body["data"][0];
        assert!(first_entry["id"].is_string());
        assert_eq!(first_entry["entry_type"], "usage_log");
        assert!(first_entry["payload"].is_object());
        assert_eq!(first_entry["error"], "Test error");
        assert_eq!(first_entry["retry_count"], 0);
        assert!(first_entry["created_at"].is_string());
    }

    #[tokio::test]
    async fn test_dlq_list_filter_by_entry_type() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed entries of different types
        dlq.push(create_test_dlq_entry("usage_log", "{}", "Error 1"))
            .await
            .unwrap();
        dlq.push(create_test_dlq_entry("webhook", "{}", "Error 2"))
            .await
            .unwrap();
        dlq.push(create_test_dlq_entry("usage_log", "{}", "Error 3"))
            .await
            .unwrap();

        let (status, body) = get_json(&app, "/admin/v1/dlq?entry_type=usage_log").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);

        for entry in body["data"].as_array().unwrap() {
            assert_eq!(entry["entry_type"], "usage_log");
        }
    }

    #[tokio::test]
    async fn test_dlq_list_filter_by_max_retries() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed entries and mark some as retried
        let entry1 = create_test_dlq_entry("usage_log", "{}", "Error 1");
        let entry1_id = entry1.id;
        dlq.push(entry1).await.unwrap();

        let entry2 = create_test_dlq_entry("usage_log", "{}", "Error 2");
        dlq.push(entry2).await.unwrap();

        // Mark entry1 as retried multiple times
        dlq.mark_retried(entry1_id).await.unwrap();
        dlq.mark_retried(entry1_id).await.unwrap();
        dlq.mark_retried(entry1_id).await.unwrap();

        // Filter for entries with less than 2 retries
        let (status, body) = get_json(&app, "/admin/v1/dlq?max_retries=2").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"][0]["retry_count"], 0);
    }

    #[tokio::test]
    async fn test_dlq_list_pagination() {
        let (app, dlq) = test_app_with_dlq().await;

        // Clear any existing entries first
        dlq.clear().await.unwrap();

        // Seed exactly 5 entries with distinct timestamps
        for i in 0..5 {
            let entry = create_test_dlq_entry("usage_log", &format!("{{\"n\":{}}}", i), "Error");
            dlq.push(entry).await.unwrap();
            // Longer delay to ensure distinct timestamps for stable ordering
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        // Verify we have exactly 5 entries
        assert_eq!(dlq.len().await.unwrap(), 5);

        // Test that pagination works: first page with limit
        let (status, body) = get_json(&app, "/admin/v1/dlq?limit=2").await;

        assert_eq!(status, StatusCode::OK);
        let page1_data = body["data"].as_array().unwrap();
        assert_eq!(page1_data.len(), 2);
        assert_eq!(body["pagination"]["has_more"], true);
        assert!(body["pagination"]["next_cursor"].is_string());

        // Verify we can use the cursor to get the next page
        let next_cursor = body["pagination"]["next_cursor"].as_str().unwrap();
        let (status, body2) = get_json(
            &app,
            &format!("/admin/v1/dlq?limit=2&cursor={}", next_cursor),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let page2_data = body2["data"].as_array().unwrap();
        assert!(!page2_data.is_empty(), "Second page should have entries");

        // Verify no duplicate entries between pages
        let page1_ids: Vec<&str> = page1_data
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        let page2_ids: Vec<&str> = page2_data
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();

        for id in &page2_ids {
            assert!(
                !page1_ids.contains(id),
                "Entry {} appears in both pages - pagination not working correctly",
                id
            );
        }
    }

    #[tokio::test]
    async fn test_dlq_list_invalid_cursor() {
        let (app, _dlq) = test_app_with_dlq().await;

        let (status, body) = get_json(&app, "/admin/v1/dlq?cursor=invalid-cursor").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid cursor")
        );
    }

    #[tokio::test]
    async fn test_dlq_list_invalid_direction() {
        let (app, _dlq) = test_app_with_dlq().await;

        let (status, body) = get_json(&app, "/admin/v1/dlq?direction=sideways").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid direction")
        );
    }

    #[tokio::test]
    async fn test_dlq_get_entry() {
        let (app, dlq) = test_app_with_dlq().await;

        let entry = create_test_dlq_entry("usage_log", r#"{"test": true}"#, "Test error");
        let entry_id = entry.id;
        dlq.push(entry).await.unwrap();

        let (status, body) = get_json(&app, &format!("/admin/v1/dlq/{}", entry_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], entry_id.to_string());
        assert_eq!(body["entry_type"], "usage_log");
        assert_eq!(body["error"], "Test error");
    }

    #[tokio::test]
    async fn test_dlq_get_entry_not_found() {
        let (app, _dlq) = test_app_with_dlq().await;

        let fake_id = uuid::Uuid::new_v4();
        let (status, body) = get_json(&app, &format!("/admin/v1/dlq/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("DLQ entry")
        );
    }

    #[tokio::test]
    async fn test_dlq_delete_entry() {
        let (app, dlq) = test_app_with_dlq().await;

        let entry = create_test_dlq_entry("usage_log", "{}", "Test error");
        let entry_id = entry.id;
        dlq.push(entry).await.unwrap();

        let (status, body) = delete_json(&app, &format!("/admin/v1/dlq/{}", entry_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], true);

        // Verify it's gone
        let (status, _) = get_json(&app, &format!("/admin/v1/dlq/{}", entry_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dlq_delete_entry_not_found() {
        let (app, _dlq) = test_app_with_dlq().await;

        let fake_id = uuid::Uuid::new_v4();
        let (status, body) = delete_json(&app, &format!("/admin/v1/dlq/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("DLQ entry")
        );
    }

    #[tokio::test]
    async fn test_dlq_retry_unsupported_type() {
        let (app, dlq) = test_app_with_dlq().await;

        // Create an entry with an unsupported type
        let entry = create_test_dlq_entry("webhook", r#"{"url": "https://example.com"}"#, "Failed");
        let entry_id = entry.id;
        dlq.push(entry).await.unwrap();

        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/dlq/{}/retry", entry_id),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Unsupported entry type")
        );
    }

    #[tokio::test]
    async fn test_dlq_retry_not_found() {
        let (app, _dlq) = test_app_with_dlq().await;

        let fake_id = uuid::Uuid::new_v4();
        let (status, body) =
            post_json(&app, &format!("/admin/v1/dlq/{}/retry", fake_id), json!({})).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("DLQ entry")
        );
    }

    #[tokio::test]
    async fn test_dlq_retry_usage_log_invalid_payload() {
        let (app, dlq) = test_app_with_dlq().await;

        // Create an entry with an invalid usage_log payload
        let entry = create_test_dlq_entry("usage_log", "not valid json", "Original error");
        let entry_id = entry.id;
        dlq.push(entry).await.unwrap();

        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/dlq/{}/retry", entry_id),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid usage_log payload")
        );
    }

    #[tokio::test]
    async fn test_dlq_stats_empty() {
        let (app, _dlq) = test_app_with_dlq().await;

        let (status, body) = get_json(&app, "/admin/v1/dlq/stats").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_entries"], 0);
        assert_eq!(body["is_empty"], true);
        assert!(body["by_type"].is_object());
        assert!(body["by_retry_count"].is_object());
    }

    #[tokio::test]
    async fn test_dlq_stats_with_entries() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed entries of different types
        dlq.push(create_test_dlq_entry("usage_log", "{}", "Error 1"))
            .await
            .unwrap();
        dlq.push(create_test_dlq_entry("usage_log", "{}", "Error 2"))
            .await
            .unwrap();
        dlq.push(create_test_dlq_entry("webhook", "{}", "Error 3"))
            .await
            .unwrap();

        // Mark one as retried
        let entry_for_retry = create_test_dlq_entry("usage_log", "{}", "Error 4");
        let retry_id = entry_for_retry.id;
        dlq.push(entry_for_retry).await.unwrap();
        dlq.mark_retried(retry_id).await.unwrap();

        let (status, body) = get_json(&app, "/admin/v1/dlq/stats").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_entries"], 4);
        assert_eq!(body["is_empty"], false);

        // Check by_type breakdown
        assert_eq!(body["by_type"]["usage_log"], 3);
        assert_eq!(body["by_type"]["webhook"], 1);

        // Check by_retry_count breakdown
        assert_eq!(body["by_retry_count"]["0"], 3);
        assert_eq!(body["by_retry_count"]["1"], 1);
    }

    #[tokio::test]
    async fn test_dlq_purge() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed some entries
        for i in 0..5 {
            dlq.push(create_test_dlq_entry(
                "usage_log",
                &format!("{{\"n\":{}}}", i),
                "Error",
            ))
            .await
            .unwrap();
        }

        // Verify entries exist
        assert_eq!(dlq.len().await.unwrap(), 5);

        // Purge all
        let (status, body) = delete_json(&app, "/admin/v1/dlq").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["purged"], 5);

        // Verify empty
        assert_eq!(dlq.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_dlq_prune() {
        let (app, dlq) = test_app_with_dlq().await;

        // Seed some entries
        for i in 0..3 {
            dlq.push(create_test_dlq_entry(
                "usage_log",
                &format!("{{\"n\":{}}}", i),
                "Error",
            ))
            .await
            .unwrap();
        }

        // Prune with a very large value (entries must be older than 1 year)
        // Since entries were just created, none should be pruned
        let (status, body) = post_json(
            &app,
            "/admin/v1/dlq/prune?older_than_secs=31536000",
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pruned"], 0);
        assert_eq!(body["older_than_secs"], 31536000);
        assert_eq!(dlq.len().await.unwrap(), 3);

        // Prune with 0 seconds (cutoff = now, so all entries are older and get pruned)
        let (status, body) =
            post_json(&app, "/admin/v1/dlq/prune?older_than_secs=0", json!({})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pruned"], 3);
        assert_eq!(dlq.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_dlq_prune_old_entries() {
        let (app, dlq) = test_app_with_dlq().await;

        // Create an entry with an old timestamp by using the trait directly
        use chrono::{Duration, Utc};

        let mut old_entry = create_test_dlq_entry("usage_log", "{}", "Old error");
        old_entry.created_at = Utc::now() - Duration::days(10);
        dlq.push(old_entry).await.unwrap();

        // Create a new entry
        dlq.push(create_test_dlq_entry("usage_log", "{}", "New error"))
            .await
            .unwrap();

        assert_eq!(dlq.len().await.unwrap(), 2);

        // Prune entries older than 1 day (86400 seconds)
        let (status, body) =
            post_json(&app, "/admin/v1/dlq/prune?older_than_secs=86400", json!({})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pruned"], 1);
        assert_eq!(body["older_than_secs"], 86400);

        // Should have 1 entry left
        assert_eq!(dlq.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_dlq_retry_usage_log_success() {
        let (app, dlq) = test_app_with_dlq().await;

        // First create an org, project, and API key to have a valid api_key_id
        let org_slug = create_org(&app, "dlq-retry-org").await;
        let (_, project_body) = post_json(
            &app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": "dlq-retry-project", "name": "DLQ Retry Project"}),
        )
        .await;
        let project_id = project_body["id"].as_str().unwrap();

        // Create an API key with proper owner format
        let (status, api_key_body) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({
                "name": "test-dlq-key",
                "owner": {"type": "project", "project_id": project_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let api_key_id = api_key_body["id"].as_str().unwrap();

        // Create a valid usage_log entry
        let usage_payload = json!({
            "request_id": format!("req-{}", uuid::Uuid::new_v4()),
            "api_key_id": api_key_id,
            "model": "gpt-4",
            "provider": "openai",
            "http_referer": null,
            "input_tokens": 100,
            "output_tokens": 50,
            "cost_microcents": 1500,
            "request_at": chrono::Utc::now().to_rfc3339(),
            "streamed": false,
            "cached_tokens": 0,
            "reasoning_tokens": 0,
            "finish_reason": "stop",
            "latency_ms": 500,
            "cancelled": false,
            "status_code": 200
        });

        let entry = create_test_dlq_entry(
            "usage_log",
            &usage_payload.to_string(),
            "DB temporarily unavailable",
        );
        let entry_id = entry.id;
        dlq.push(entry).await.unwrap();

        // Retry the entry
        let (status, body) = post_json(
            &app,
            &format!("/admin/v1/dlq/{}/retry", entry_id),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("processed and removed")
        );

        // Entry should be removed from DLQ
        let (status, _) = get_json(&app, &format!("/admin/v1/dlq/{}", entry_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ============================================================================
    // Template Tests
    // ============================================================================

    async fn create_project(app: &axum::Router, org_slug: &str, project_slug: &str) -> String {
        let (status, project) = post_json(
            app,
            &format!("/admin/v1/organizations/{}/projects", org_slug),
            json!({"slug": project_slug, "name": format!("Project {}", project_slug)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        project["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn test_create_template_for_organization() {
        let app = test_app().await;
        let org_slug = create_org(&app, "template-org").await;

        // Get org ID
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "test-template",
                "content": "You are a helpful assistant.",
                "description": "A test template"
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
        assert_eq!(body["name"], "test-template");
        assert_eq!(body["content"], "You are a helpful assistant.");
        assert_eq!(body["description"], "A test template");
        assert_eq!(body["owner_type"], "organization");
        assert_eq!(body["owner_id"], org_id);
        assert!(body["id"].is_string());
    }

    #[tokio::test]
    async fn test_create_template_for_project() {
        let app = test_app().await;
        let org_slug = create_org(&app, "template-proj-org").await;
        let project_id = create_project(&app, &org_slug, "template-project").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "project", "project_id": project_id},
                "name": "project-template",
                "content": "Project system template"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "project-template");
        assert_eq!(body["owner_type"], "project");
        assert_eq!(body["owner_id"], project_id);
    }

    #[tokio::test]
    async fn test_create_template_for_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "template-team-org").await;
        let team_id = create_team(&app, &org_slug, "template-team").await;

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "team", "team_id": team_id},
                "name": "team-template",
                "content": "Team system template"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "team-template");
        assert_eq!(body["owner_type"], "team");
        assert_eq!(body["owner_id"], team_id);
    }

    #[tokio::test]
    async fn test_create_template_for_user() {
        let app = test_app().await;

        // Create a user
        let (status, user) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "template-user", "email": "template@example.com", "name": "Template User"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let user_id = user["id"].as_str().unwrap();

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "user", "user_id": user_id},
                "name": "user-template",
                "content": "User system template"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["name"], "user-template");
        assert_eq!(body["owner_type"], "user");
        assert_eq!(body["owner_id"], user_id);
    }

    #[tokio::test]
    async fn test_create_template_with_metadata() {
        let app = test_app().await;
        let org_slug = create_org(&app, "template-meta-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "meta-template",
                "content": "System template with metadata",
                "metadata": {
                    "temperature": 0.7,
                    "max_tokens": 1000,
                    "tags": ["coding", "assistant"]
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert!(body["metadata"].is_object());
        assert_eq!(body["metadata"]["temperature"], 0.7);
        assert_eq!(body["metadata"]["max_tokens"], 1000);
    }

    #[tokio::test]
    async fn test_create_template_duplicate_name_same_owner() {
        let app = test_app().await;
        let org_slug = create_org(&app, "dup-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, _) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "duplicate-template",
                "content": "First template"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "duplicate-template",
                "content": "Second template"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_create_template_same_name_different_owners() {
        let app = test_app().await;
        let org1_slug = create_org(&app, "template-org1").await;
        let org2_slug = create_org(&app, "template-org2").await;
        let (_, org1) = get_json(&app, &format!("/admin/v1/organizations/{}", org1_slug)).await;
        let (_, org2) = get_json(&app, &format!("/admin/v1/organizations/{}", org2_slug)).await;

        let (status, _) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org1["id"]},
                "name": "shared-name",
                "content": "Org 1 template"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org2["id"]},
                "name": "shared-name",
                "content": "Org 2 template"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_get_template() {
        let app = test_app().await;
        let org_slug = create_org(&app, "get-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, created) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "get-test-template",
                "content": "Test content"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let template_id = created["id"].as_str().unwrap();

        let (status, body) = get_json(&app, &format!("/admin/v1/templates/{}", template_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], template_id);
        assert_eq!(body["name"], "get-test-template");
        assert_eq!(body["content"], "Test content");
    }

    #[tokio::test]
    async fn test_get_template_not_found() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let (status, body) = get_json(&app, &format!("/admin/v1/templates/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_update_template() {
        let app = test_app().await;
        let org_slug = create_org(&app, "update-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, created) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "update-test",
                "content": "Original content"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let template_id = created["id"].as_str().unwrap();

        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/templates/{}", template_id),
            json!({
                "name": "updated-name",
                "content": "Updated content",
                "description": "New description"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "updated-name");
        assert_eq!(body["content"], "Updated content");
        assert_eq!(body["description"], "New description");
    }

    #[tokio::test]
    async fn test_update_template_partial() {
        let app = test_app().await;
        let org_slug = create_org(&app, "partial-update-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, created) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "partial-update",
                "content": "Original content"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let template_id = created["id"].as_str().unwrap();

        // Only update content
        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/templates/{}", template_id),
            json!({"content": "Only content changed"}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "partial-update"); // name unchanged
        assert_eq!(body["content"], "Only content changed");
    }

    #[tokio::test]
    async fn test_update_template_not_found() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let (status, body) = patch_json(
            &app,
            &format!("/admin/v1/templates/{}", fake_id),
            json!({"name": "new-name"}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_delete_template() {
        let app = test_app().await;
        let org_slug = create_org(&app, "delete-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        let (status, created) = post_json(
            &app,
            "/admin/v1/templates",
            json!({
                "owner": {"type": "organization", "organization_id": org_id},
                "name": "delete-test",
                "content": "To be deleted"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let template_id = created["id"].as_str().unwrap();

        let (status, _) = delete_json(&app, &format!("/admin/v1/templates/{}", template_id)).await;
        assert_eq!(status, StatusCode::OK);

        // Verify it's deleted
        let (status, _) = get_json(&app, &format!("/admin/v1/templates/{}", template_id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_template_not_found() {
        let app = test_app().await;
        let fake_id = uuid::Uuid::new_v4();

        let (status, body) = delete_json(&app, &format!("/admin/v1/templates/{}", fake_id)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_templates_by_organization() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        // Create 3 templates for the org
        for i in 0..3 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/templates",
                json!({
                    "owner": {"type": "organization", "organization_id": org_id},
                    "name": format!("org-template-{}", i),
                    "content": format!("Content {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/templates", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].is_array());
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
        assert_eq!(body["pagination"]["has_more"], false);
    }

    #[tokio::test]
    async fn test_list_templates_by_team() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-team-template-org").await;
        let team_id = create_team(&app, &org_slug, "template-list-team").await;

        // Create 2 templates for the team
        for i in 0..2 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/templates",
                json!({
                    "owner": {"type": "team", "team_id": team_id},
                    "name": format!("team-template-{}", i),
                    "content": format!("Content {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/template-list-team/templates",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_templates_by_project() {
        let app = test_app().await;
        let org_slug = create_org(&app, "list-proj-template-org").await;
        let project_id = create_project(&app, &org_slug, "template-list-project").await;

        // Create 2 templates for the project
        for i in 0..2 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/templates",
                json!({
                    "owner": {"type": "project", "project_id": project_id},
                    "name": format!("project-template-{}", i),
                    "content": format!("Content {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/template-list-project/templates",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_templates_by_user() {
        let app = test_app().await;

        // Create a user
        let (status, user) = post_json(
            &app,
            "/admin/v1/users",
            json!({"external_id": "list-template-user", "email": "listtemplate@example.com", "name": "List User"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let user_id = user["id"].as_str().unwrap();

        // Create 2 templates for the user
        for i in 0..2 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/templates",
                json!({
                    "owner": {"type": "user", "user_id": user_id},
                    "name": format!("user-template-{}", i),
                    "content": format!("Content {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        let (status, body) =
            get_json(&app, &format!("/admin/v1/users/{}/templates", user_id)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_templates_empty() {
        let app = test_app().await;
        let org_slug = create_org(&app, "empty-template-org").await;

        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/templates", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_list_templates_org_not_found() {
        let app = test_app().await;

        let (status, body) =
            get_json(&app, "/admin/v1/organizations/nonexistent-org/templates").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_templates_team_not_found() {
        let app = test_app().await;
        let org_slug = create_org(&app, "team-404-template-org").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/teams/nonexistent-team/templates",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_templates_project_not_found() {
        let app = test_app().await;
        let org_slug = create_org(&app, "proj-404-template-org").await;

        let (status, body) = get_json(
            &app,
            &format!(
                "/admin/v1/organizations/{}/projects/nonexistent-project/templates",
                org_slug
            ),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["code"].is_string());
    }

    #[tokio::test]
    async fn test_list_templates_pagination() {
        let app = test_app().await;
        let org_slug = create_org(&app, "paginate-template-org").await;
        let (_, org) = get_json(&app, &format!("/admin/v1/organizations/{}", org_slug)).await;
        let org_id = org["id"].as_str().unwrap();

        // Create 5 templates
        for i in 0..5 {
            let (status, _) = post_json(
                &app,
                "/admin/v1/templates",
                json!({
                    "owner": {"type": "organization", "organization_id": org_id},
                    "name": format!("paginate-template-{}", i),
                    "content": format!("Content {}", i)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }

        // Request with limit=2
        let (status, body) = get_json(
            &app,
            &format!("/admin/v1/organizations/{}/templates?limit=2", org_slug),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
        assert_eq!(body["pagination"]["has_more"], true);
        assert!(body["pagination"]["next_cursor"].is_string());
    }

    // ============================================================================
    // UI Config Tests
    // ============================================================================

    /// Create a test application with a custom config string
    async fn test_app_with_config(config_str: &str) -> axum::Router {
        #[cfg_attr(not(feature = "sso"), allow(unused_mut))]
        let mut config =
            crate::config::GatewayConfig::parse(config_str).expect("Failed to parse test config");
        // The SCIM token pepper is mandatory; tests that don't override
        // [auth.session] still need a secret so AppState can construct.
        #[cfg(feature = "sso")]
        if config
            .auth
            .session
            .as_ref()
            .and_then(|s| s.secret.as_ref())
            .is_none()
        {
            let session = config
                .auth
                .session
                .get_or_insert_with(crate::config::SessionConfig::default);
            session.secret =
                Some("test-session-secret-must-be-long-enough-for-hmac-pepper-32b".to_string());
        }
        let state = crate::AppState::new(config.clone())
            .await
            .expect("Failed to create AppState");
        crate::build_app(&config, state)
    }

    /// Generate a unique in-memory database path for tests
    fn unique_db_config() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static UI_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(10000);
        let db_id = UI_CONFIG_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!(
            r#"
[database]
type = "sqlite"
path = "file:test_ui_config_db_{}?mode=memory&cache=shared"
create_if_missing = true
run_migrations = true
wal_mode = false
busy_timeout_ms = 5000

[providers.test-openai]
type = "open_ai"
api_key = "sk-test-key"
"#,
            db_id
        )
    }

    #[tokio::test]
    async fn test_get_ui_config_default() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);

        // Check structure
        assert!(body["branding"].is_object());
        assert!(body["chat"].is_object());
        assert!(body["admin"].is_object());
        assert!(body["auth"].is_object());

        // Check defaults
        assert_eq!(body["branding"]["title"], "Hadrian Gateway");
        assert_eq!(body["branding"]["tagline"], Value::Null);
        assert_eq!(body["branding"]["show_version"], false);

        // Chat defaults
        assert_eq!(body["chat"]["enabled"], true);
        assert_eq!(body["chat"]["file_uploads_enabled"], false);

        // Admin defaults
        assert_eq!(body["admin"]["enabled"], true);

        // Auth - with no auth configured, should be "none"
        assert!(body["auth"]["methods"].is_array());
        assert!(
            body["auth"]["methods"]
                .as_array()
                .unwrap()
                .contains(&json!("none"))
        );
    }

    #[tokio::test]
    async fn test_get_ui_config_custom_branding() {
        let config_str = format!(
            r#"
{}

[ui.branding]
title = "Acme AI Gateway"
tagline = "Powering the future with AI"
logo_url = "https://example.com/logo.png"
logo_dark_url = "https://example.com/logo-dark.png"
favicon_url = "https://example.com/favicon.ico"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["branding"]["title"], "Acme AI Gateway");
        assert_eq!(body["branding"]["tagline"], "Powering the future with AI");
        assert_eq!(body["branding"]["logo_url"], "https://example.com/logo.png");
        assert_eq!(
            body["branding"]["logo_dark_url"],
            "https://example.com/logo-dark.png"
        );
        assert_eq!(
            body["branding"]["favicon_url"],
            "https://example.com/favicon.ico"
        );
    }

    #[tokio::test]
    async fn test_get_ui_config_colors() {
        let config_str = format!(
            r##"
{}

[ui.branding.colors]
primary = "#3b82f6"
secondary = "#64748b"
accent = "#f59e0b"
background = "#ffffff"
foreground = "#1e293b"
muted = "#f1f5f9"
border = "#e2e8f0"
"##,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["branding"]["colors"]["primary"], "#3b82f6");
        assert_eq!(body["branding"]["colors"]["secondary"], "#64748b");
        assert_eq!(body["branding"]["colors"]["accent"], "#f59e0b");
        assert_eq!(body["branding"]["colors"]["background"], "#ffffff");
        assert_eq!(body["branding"]["colors"]["foreground"], "#1e293b");
        assert_eq!(body["branding"]["colors"]["muted"], "#f1f5f9");
        assert_eq!(body["branding"]["colors"]["border"], "#e2e8f0");
    }

    #[tokio::test]
    async fn test_get_ui_config_colors_dark() {
        let config_str = format!(
            r##"
{}

[ui.branding.colors_dark]
primary = "#60a5fa"
background = "#0f172a"
foreground = "#f8fafc"
"##,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["branding"]["colors_dark"].is_object());
        assert_eq!(body["branding"]["colors_dark"]["primary"], "#60a5fa");
        assert_eq!(body["branding"]["colors_dark"]["background"], "#0f172a");
        assert_eq!(body["branding"]["colors_dark"]["foreground"], "#f8fafc");
    }

    #[tokio::test]
    async fn test_get_ui_config_fonts() {
        let config_str = format!(
            r#"
{}

[ui.branding.fonts]
heading = "Inter"
body = "Roboto"
mono = "JetBrains Mono"

[[ui.branding.fonts.custom]]
name = "CustomFont"
url = "https://example.com/fonts/custom.woff2"
weight = "400 700"
style = "normal"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["branding"]["fonts"]["heading"], "Inter");
        assert_eq!(body["branding"]["fonts"]["body"], "Roboto");
        assert_eq!(body["branding"]["fonts"]["mono"], "JetBrains Mono");

        let custom = &body["branding"]["fonts"]["custom"];
        assert!(custom.is_array());
        assert_eq!(custom[0]["name"], "CustomFont");
        assert_eq!(custom[0]["url"], "https://example.com/fonts/custom.woff2");
        assert_eq!(custom[0]["weight"], "400 700");
        assert_eq!(custom[0]["style"], "normal");
    }

    #[tokio::test]
    async fn test_get_ui_config_footer() {
        let config_str = format!(
            r#"
{}

[ui.branding]
footer_text = "© 2024 Acme Corp. All rights reserved."

[[ui.branding.footer_links]]
label = "Privacy Policy"
url = "https://example.com/privacy"

[[ui.branding.footer_links]]
label = "Terms of Service"
url = "https://example.com/terms"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["branding"]["footer_text"],
            "© 2024 Acme Corp. All rights reserved."
        );

        let links = body["branding"]["footer_links"].as_array().unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["label"], "Privacy Policy");
        assert_eq!(links[0]["url"], "https://example.com/privacy");
        assert_eq!(links[1]["label"], "Terms of Service");
        assert_eq!(links[1]["url"], "https://example.com/terms");
    }

    #[tokio::test]
    async fn test_get_ui_config_version_shown() {
        let config_str = format!(
            r#"
{}

[ui.branding]
show_version = true
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["branding"]["show_version"], true);
        assert!(body["branding"]["version"].is_string());
        // Version should be the cargo package version
        let version = body["branding"]["version"].as_str().unwrap();
        assert!(!version.is_empty());
    }

    #[tokio::test]
    async fn test_get_ui_config_version_hidden() {
        let config_str = format!(
            r#"
{}

[ui.branding]
show_version = false
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["branding"]["show_version"], false);
        // Version should not be included when show_version is false
        assert_eq!(body["branding"]["version"], Value::Null);
    }

    #[tokio::test]
    async fn test_get_ui_config_chat_settings() {
        let config_str = format!(
            r#"
{}

[ui.chat]
enabled = true
default_model = "gpt-4o"
available_models = ["gpt-4o", "gpt-4o-mini", "claude-3-opus"]

[ui.chat.file_uploads]
enabled = true
max_size_bytes = 52428800
allowed_types = ["image/png", "image/jpeg", "application/pdf"]
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["chat"]["enabled"], true);
        assert_eq!(body["chat"]["default_model"], "gpt-4o");

        let models = body["chat"]["available_models"].as_array().unwrap();
        assert_eq!(models.len(), 3);
        assert!(models.contains(&json!("gpt-4o")));
        assert!(models.contains(&json!("claude-3-opus")));

        assert_eq!(body["chat"]["file_uploads_enabled"], true);
        assert_eq!(body["chat"]["max_file_size_bytes"], 52428800);

        let allowed_types = body["chat"]["allowed_file_types"].as_array().unwrap();
        assert_eq!(allowed_types.len(), 3);
    }

    #[tokio::test]
    async fn test_get_ui_config_chat_disabled() {
        let config_str = format!(
            r#"
{}

[ui.chat]
enabled = false
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["chat"]["enabled"], false);
    }

    #[tokio::test]
    async fn test_get_ui_config_admin_disabled() {
        let config_str = format!(
            r#"
{}

[ui.admin]
enabled = false
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["admin"]["enabled"], false);
    }

    #[tokio::test]
    async fn test_get_ui_config_pages_containers_default() {
        let app = test_app_with_config(&unique_db_config()).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pages"]["containers"]["status"], "enabled");
    }

    #[tokio::test]
    async fn test_get_ui_config_pages_containers_feature_disabled() {
        let config_str = format!(
            r#"
{}

[features.containers]
enabled = false
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pages"]["containers"]["status"], "disabled");
    }

    #[tokio::test]
    async fn test_get_ui_config_pages_containers_page_disabled() {
        let config_str = format!(
            r#"
{}

[ui.pages]
containers = "disabled"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pages"]["containers"]["status"], "disabled");
    }

    #[tokio::test]
    async fn test_get_ui_config_auth_api_key() {
        let config_str = format!(
            r#"
{}

[auth.mode]
type = "api_key"

[auth.api_key]
key_prefix = "gw_"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        let methods = body["auth"]["methods"].as_array().unwrap();
        assert!(methods.contains(&json!("api_key")));
    }

    #[cfg(feature = "sso")]
    #[tokio::test]
    async fn test_get_ui_config_auth_session() {
        let config_str = format!(
            r#"
{}

[auth.mode]
type = "idp"

[auth.session]
secret = "test-session-secret-must-be-long-enough-for-hmac-pepper-32b"
secure = true
cookie_name = "__gw_session"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        let methods = body["auth"]["methods"].as_array().unwrap();
        // Session auth returns "session" as the auth method
        // Users authenticate via per-org SSO discovered from email domain
        assert!(
            methods.contains(&json!("session")),
            "Expected session auth method, got {:?}",
            methods
        );
    }

    #[tokio::test]
    async fn test_get_ui_config_auth_proxy() {
        let config_str = format!(
            r#"
{}

[server]
host = "127.0.0.1"

[server.trusted_proxies]
cidrs = ["127.0.0.0/8"]

[auth.mode]
type = "iap"
identity_header = "X-Forwarded-User"
email_header = "X-Forwarded-Email"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        let methods = body["auth"]["methods"].as_array().unwrap();
        assert!(methods.contains(&json!("header")));
    }

    #[tokio::test]
    async fn test_get_ui_config_login_customization() {
        let config_str = format!(
            r#"
{}

[ui.branding.login]
title = "Sign in to AI Gateway"
subtitle = "Use your corporate credentials"
background_image = "https://example.com/login-bg.jpg"
show_logo = false
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body["branding"]["login"].is_object());
        assert_eq!(body["branding"]["login"]["title"], "Sign in to AI Gateway");
        assert_eq!(
            body["branding"]["login"]["subtitle"],
            "Use your corporate credentials"
        );
        assert_eq!(
            body["branding"]["login"]["background_image"],
            "https://example.com/login-bg.jpg"
        );
        assert_eq!(body["branding"]["login"]["show_logo"], false);
    }

    #[tokio::test]
    async fn test_get_ui_config_mcp_favorites_default() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        let favorites = body["mcp"]["favorites"]
            .as_array()
            .expect("favorites array");
        let urls: Vec<&str> = favorites
            .iter()
            .map(|f| f["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls.len(), 6);
        assert!(urls.contains(&"io.github.hadriangateway/platter"));
        assert!(urls.contains(&"https://mcp.atlassian.com/v1/mcp"));
        assert!(urls.contains(&"https://mcp.notion.com/mcp"));
        assert!(urls.contains(&"https://huggingface.co/mcp"));
        assert!(urls.contains(&"https://mcp.miro.com/"));
        assert!(urls.contains(&"https://mcp.vercel.com"));
    }

    #[tokio::test]
    async fn test_get_ui_config_mcp_favorites_custom() {
        let config_str = format!(
            r#"
{}

[[ui.mcp.favorites]]
name = "Internal Wiki"
url = "https://mcp.internal.example.com/mcp"
"#,
            unique_db_config()
        );

        let app = test_app_with_config(&config_str).await;
        let (status, body) = get_json(&app, "/admin/v1/ui/config").await;

        assert_eq!(status, StatusCode::OK);
        let favorites = body["mcp"]["favorites"]
            .as_array()
            .expect("favorites array");
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0]["name"], "Internal Wiki");
        assert_eq!(favorites[0]["url"], "https://mcp.internal.example.com/mcp");
    }

    // ============================================================================
    // Me (Self-Service) Tests
    // ============================================================================
    //
    // These tests verify the GDPR self-service endpoints:
    // - GET /admin/v1/me/export - Export user's own data (Article 15 - Right of Access)
    // - DELETE /admin/v1/me - Delete user's own account (Article 17 - Right to Erasure)
    //
    // When auth is disabled (test environment), the permissive_authz_middleware
    // creates an AdminAuth with the default anonymous user, which these endpoints use.

    #[tokio::test]
    async fn test_me_export_returns_user_data() {
        let app = test_app().await;

        let (status, body) = get_json(&app, "/admin/v1/me/export").await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);

        // Verify the export contains expected fields
        assert!(body["exported_at"].is_string());
        assert!(body["user"].is_object());
        assert_eq!(body["user"]["external_id"], "anonymous");
        assert_eq!(body["user"]["email"], "anonymous@localhost");

        // Verify memberships structure (orgs, teams, and projects)
        assert!(body["memberships"].is_object());
        assert!(body["memberships"]["organizations"].is_array());
        assert!(body["memberships"]["teams"].is_array());
        assert!(body["memberships"]["projects"].is_array());

        // Verify other export fields
        assert!(body["api_keys"].is_array());
        assert!(body["conversations"].is_array());
        assert!(body["audit_logs"].is_array());
        assert!(body["usage_summary"].is_object());
    }

    #[tokio::test]
    async fn test_me_export_includes_org_membership() {
        let app = test_app().await;

        // The anonymous user is automatically added to the "local" organization
        let (status, body) = get_json(&app, "/admin/v1/me/export").await;

        assert_eq!(status, StatusCode::OK);

        // Verify the user is a member of the local org
        let orgs = body["memberships"]["organizations"].as_array().unwrap();
        assert!(
            orgs.iter().any(|o| o["org_slug"] == "local"),
            "Expected user to be member of 'local' organization, found: {:?}",
            orgs
        );
    }

    #[tokio::test]
    async fn test_me_export_includes_team_membership() {
        let app = test_app().await;

        // Get the anonymous user's ID
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);
        let user_id = export["user"]["id"].as_str().unwrap();

        // Create a team in the local org
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations/local/teams",
            json!({"slug": "export-test-team", "name": "Export Test Team"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Add the user to the team
        let (status, _) = post_json(
            &app,
            "/admin/v1/organizations/local/teams/export-test-team/members",
            json!({"user_id": user_id, "role": "member"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Export again and verify the team membership is included
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);

        let teams = export["memberships"]["teams"].as_array().unwrap();
        assert!(
            teams.iter().any(|t| t["team_slug"] == "export-test-team"),
            "Expected team membership to be in export, found: {:?}",
            teams
        );

        // Verify team membership has expected fields
        let team_membership = teams
            .iter()
            .find(|t| t["team_slug"] == "export-test-team")
            .unwrap();
        assert_eq!(team_membership["team_name"], "Export Test Team");
        assert_eq!(team_membership["role"], "member");
        assert!(team_membership["team_id"].is_string());
        assert!(team_membership["org_id"].is_string());
        assert!(team_membership["joined_at"].is_string());
    }

    #[tokio::test]
    async fn test_me_delete_removes_user_and_data() {
        // Use a fresh test app to get a fresh anonymous user
        let app = test_app().await;

        // First verify the user exists
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);
        let user_id = export["user"]["id"].as_str().unwrap();

        // Delete the user
        let (status, body) = delete_json(&app, "/admin/v1/me").await;

        if status != StatusCode::OK {
            eprintln!(
                "Error response: {}",
                serde_json::to_string_pretty(&body).unwrap()
            );
        }
        assert_eq!(status, StatusCode::OK);

        // Verify the response structure
        assert_eq!(body["deleted"], true);
        assert_eq!(body["user_id"], user_id);
        assert!(body["api_keys_deleted"].is_number());
        assert!(body["conversations_deleted"].is_number());
        assert!(body["dynamic_providers_deleted"].is_number());
        assert!(body["usage_records_deleted"].is_number());
    }

    #[tokio::test]
    async fn test_me_delete_cascades_api_keys() {
        let app = test_app().await;

        // Get the anonymous user's ID
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);
        let user_id = export["user"]["id"].as_str().unwrap();

        // Create an API key for the user
        let (status, _) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "User's Key", "owner": {"type": "user", "user_id": user_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Delete the user
        let (status, body) = delete_json(&app, "/admin/v1/me").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], true);

        // The API key should have been deleted
        assert!(
            body["api_keys_deleted"].as_u64().unwrap() >= 1,
            "Expected at least 1 API key to be deleted"
        );
    }

    #[tokio::test]
    async fn test_me_delete_cascades_dynamic_providers() {
        let app = test_app().await;

        // Get the anonymous user's ID
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);
        let user_id = export["user"]["id"].as_str().unwrap();

        // Create a dynamic provider for the user
        let (status, _) = post_json(
            &app,
            "/admin/v1/dynamic-providers",
            json!({
                "name": "user-provider",
                "owner": {"type": "user", "user_id": user_id},
                "provider_type": "open_ai",
                "base_url": "https://api.openai.com/v1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Delete the user
        let (status, body) = delete_json(&app, "/admin/v1/me").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], true);

        // The dynamic provider should have been deleted
        assert!(
            body["dynamic_providers_deleted"].as_u64().unwrap() >= 1,
            "Expected at least 1 dynamic provider to be deleted"
        );
    }

    #[tokio::test]
    async fn test_me_export_after_creating_api_key() {
        let app = test_app().await;

        // Get the anonymous user's ID
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);
        let user_id = export["user"]["id"].as_str().unwrap();

        // Create an API key for the user
        let (status, key) = post_json(
            &app,
            "/admin/v1/api-keys",
            json!({"name": "Export Test Key", "owner": {"type": "user", "user_id": user_id}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let key_id = key["id"].as_str().unwrap();

        // Export again and verify the key is included
        let (status, export) = get_json(&app, "/admin/v1/me/export").await;
        assert_eq!(status, StatusCode::OK);

        let api_keys = export["api_keys"].as_array().unwrap();
        assert!(
            api_keys.iter().any(|k| k["id"] == key_id),
            "Expected newly created API key to be in export"
        );

        // Verify sensitive data (key hash) is not included
        for key in api_keys {
            assert!(
                key.get("key_hash").is_none(),
                "API key hash should not be exported"
            );
        }
    }
}
