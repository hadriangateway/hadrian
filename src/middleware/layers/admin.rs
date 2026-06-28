//! Admin authentication middleware.
//!
//! This middleware protects admin routes by requiring authentication via:
//! - Bearer token (JWT from OIDC provider, for service accounts / automation)
//! - Proxy auth headers (from reverse proxy like Cloudflare Access)
//! - OIDC session (from browser-based SSO)
//!
//! Bearer tokens enable programmatic admin access for:
//! - Service accounts using OAuth client credentials flow
//! - E2E testing and automation
//! - Infrastructure-as-code provisioning
//!
//! **Security:** Proxy auth headers are only trusted when the request originates
//! from a trusted proxy IP (configured via `server.trusted_proxies`). This prevents
//! header spoofing attacks where an attacker connects directly to the gateway.

use std::net::IpAddr;

#[cfg(feature = "server")]
use axum::extract::ConnectInfo;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::{
    AppState,
    auth::{AuthError, AuthenticatedRequest, Identity, IdentityKind},
    middleware::{AdminAuth, ClientInfo, RequestId},
    observability::metrics,
    services::audit_logs::{AuthEventParams, auth_events},
};

/// Middleware that requires admin authentication.
/// This will reject requests without valid Proxy auth headers or OIDC session.
pub async fn admin_auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    let headers = req.headers().clone();
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.as_str().to_string());

    // Check if this is an XHR/API request (should return 401, not redirect)
    let is_xhr = is_xhr_request(&headers);

    // Get cookies from request extensions (set by CookieManagerLayer)
    let cookies = req.extensions().get::<Cookies>().cloned();

    // Extract connecting IP for trusted proxy validation
    #[cfg(feature = "server")]
    let connecting_ip = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    #[cfg(not(feature = "server"))]
    let connecting_ip: Option<IpAddr> = None;

    let client_info = ClientInfo {
        ip_address: connecting_ip.map(|ip| ip.to_string()),
        user_agent: headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    };

    // Try to authenticate via Proxy auth or OIDC
    let identity = match try_admin_auth(
        &headers,
        cookies.as_ref(),
        connecting_ip,
        &state,
        &client_info,
    )
    .await
    {
        Ok(identity) => identity,
        Err(AuthError::OidcAuthRequired { .. }) if is_xhr => {
            // For XHR requests, return 401 instead of redirect to avoid CORS issues
            return Err(AuthError::MissingCredentials);
        }
        Err(e) => return Err(e),
    };

    tracing::debug!(
        request_id = ?request_id,
        external_id = %identity.external_id,
        email = ?identity.email,
        "Admin request authenticated"
    );

    metrics::record_auth_attempt("admin", true);

    // Log audit events for emergency and bootstrap auth
    // These auth types are significant security events that should be logged
    if let Some(services) = &state.services {
        if identity.roles.contains(&EMERGENCY_ADMIN_ROLE.to_string()) {
            let _ = services
                .audit_logs
                .log_auth_event(AuthEventParams {
                    action: auth_events::EMERGENCY_LOGIN,
                    session_id: Uuid::new_v4(), // Session ID not applicable for emergency auth
                    external_id: Some(&identity.external_id),
                    email: identity.email.as_deref(),
                    org_id: None,
                    ip_address: client_info.ip_address.clone(),
                    user_agent: client_info.user_agent.clone(),
                    details: serde_json::json!({
                        "provider": "emergency",
                        "account_name": identity.name,
                    }),
                })
                .await;
        } else if identity.roles.contains(&BOOTSTRAP_ROLE.to_string()) {
            let _ = services
                .audit_logs
                .log_auth_event(AuthEventParams {
                    action: auth_events::BOOTSTRAP_LOGIN,
                    session_id: Uuid::new_v4(), // Session ID not applicable for bootstrap auth
                    external_id: Some(&identity.external_id),
                    email: None,
                    org_id: None,
                    ip_address: client_info.ip_address.clone(),
                    user_agent: client_info.user_agent.clone(),
                    details: serde_json::json!({
                        "provider": "bootstrap",
                    }),
                })
                .await;
        }
    }

    // Add identity and client info to request extensions
    let auth = AuthenticatedRequest::new(IdentityKind::Identity(identity.clone()));
    req.extensions_mut().insert(auth);
    req.extensions_mut().insert(AdminAuth { identity });
    req.extensions_mut().insert(client_info);

    Ok(next.run(req).await)
}

/// Check if the request is an XHR/API request (as opposed to a browser navigation).
/// XHR requests should receive 401 responses, not redirects, to avoid CORS issues.
fn is_xhr_request(headers: &axum::http::HeaderMap) -> bool {
    // Check X-Requested-With header (set by many JS frameworks)
    if headers
        .get("x-requested-with")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("xmlhttprequest"))
    {
        return true;
    }

    // Check Accept header - if it prefers JSON, it's likely an API request
    if let Some(accept) = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
    {
        // If Accept explicitly requests JSON and doesn't include text/html, it's an API request
        if accept.contains("application/json") && !accept.contains("text/html") {
            return true;
        }
    }

    false
}

/// The special role for bootstrap authentication.
/// This role is only used during initial setup and cannot be assigned by IdPs.
/// Roles starting with `_` are reserved for internal use.
pub const BOOTSTRAP_ROLE: &str = "_system_bootstrap";

/// The special role for emergency access authentication.
/// This role is granted to emergency admin accounts for break-glass access.
/// Roles starting with `_` are reserved for internal use and cannot be assigned by IdPs.
pub const EMERGENCY_ADMIN_ROLE: &str = "_emergency_admin";

/// Drop any role with the reserved `_` prefix from a list. IdPs and proxy
/// headers must never be able to claim these roles, since the gateway grants
/// extra trust to them (bootstrap / emergency break-glass).
pub fn strip_reserved_roles(roles: Vec<String>) -> Vec<String> {
    roles.into_iter().filter(|r| !r.starts_with('_')).collect()
}

/// Try to authenticate via bootstrap API key.
///
/// Bootstrap authentication is only valid when:
/// 1. A bootstrap API key is configured in `[auth.bootstrap]`
/// 2. The database has no organizations AND no users
///
/// This enables automated deployments and E2E testing to set up SSO
/// before any users can authenticate through normal means.
///
/// **Security:**
/// - The key is compared using constant-time comparison to prevent timing attacks
/// - Once the first org/user is created, bootstrap auth is permanently disabled
/// - The `_system_bootstrap` role cannot be assigned by IdPs (reserved prefix)
async fn try_bootstrap_auth(
    headers: &axum::http::HeaderMap,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    use crate::cache::CacheKeys;

    // Check if bootstrap API key is configured
    let bootstrap_key = match &state.config.auth.bootstrap {
        Some(bootstrap) => match &bootstrap.api_key {
            Some(key) if !key.is_empty() => key,
            _ => return Ok(None),
        },
        None => return Ok(None),
    };

    // Extract key from Authorization: Bearer or X-API-Key header
    let provided_key = extract_api_key(headers);
    let provided_key = match provided_key {
        Some(key) => key,
        None => return Ok(None),
    };

    // The throttle below is meant to deter brute-forcing the bootstrap key.
    // Legitimate JWT bearer tokens and other-shaped API keys come through this
    // function on every admin request, so counting *every* non-matching token
    // would let a single user's normal traffic exhaust the throttle and lock
    // their own IP out of bearer auth. Only tokens that are the same length as
    // the configured bootstrap key could be a guess of it; anything else is
    // trivially not the bootstrap key, so we silently fall through without
    // touching the throttle or lockout state.
    let could_be_bootstrap_guess = provided_key.len() == bootstrap_key.len();
    if !could_be_bootstrap_guess {
        return Ok(None);
    }

    // Per-IP throttle: refuse further attempts when this source IP is locked out.
    //
    // We deliberately skip rate-limiting when no source IP is available: a single
    // shared "unknown" bucket would let one attacker lock out every other
    // bootstrapper sharing that proxy. Bootstrap is also self-disabling once the
    // first user is created, and the key compare is constant-time, so this
    // degraded path is acceptable. Operators behind a proxy that strips the
    // client IP should configure `trusted_proxies` to recover the throttle.
    let ip_str = connecting_ip.map(|ip| ip.to_string());
    if let (Some(ip_str), Some(cache)) = (ip_str.as_deref(), &state.cache) {
        let lockout_key = CacheKeys::bootstrap_lockout(ip_str);
        if let Ok(Some(_)) = cache.get_bytes(&lockout_key).await {
            tracing::warn!(
                ip = %ip_str,
                event = "bootstrap_auth.locked_out",
                "Bootstrap auth attempt blocked: IP is locked out"
            );
            return Err(AuthError::Forbidden("Bootstrap auth denied".to_string()));
        }
    }

    // Constant-time comparison to prevent timing attacks
    use subtle::ConstantTimeEq;
    let keys_match: bool = provided_key
        .as_bytes()
        .ct_eq(bootstrap_key.as_bytes())
        .into();

    if !keys_match {
        // Log failed bootstrap attempt
        if let Some(services) = &state.services {
            let _ = services
                .audit_logs
                .log_auth_event(AuthEventParams {
                    action: auth_events::BOOTSTRAP_LOGIN_FAILED,
                    session_id: Uuid::nil(), // Nil UUID indicates no session was created
                    external_id: None,
                    email: None,
                    org_id: None,
                    ip_address: connecting_ip.map(|ip| ip.to_string()),
                    user_agent: headers
                        .get(axum::http::header::USER_AGENT)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string()),
                    details: serde_json::json!({
                        "provider": "bootstrap",
                        "reason": "invalid_key",
                    }),
                })
                .await;
        }
        if let Some(ip_str) = ip_str.as_deref() {
            increment_bootstrap_rate_limit(ip_str, state).await;
        }
        return Ok(None);
    }

    // Check if database has no users yet
    // Bootstrap is valid until the first user is created (via SSO login or manual creation)
    // This allows bootstrap to:
    // 1. Create the initial organization
    // 2. Create SSO configuration
    // Then the first IdP login provisions a user and bootstrap is disabled
    let db = match &state.db {
        Some(db) => db,
        None => {
            tracing::warn!("Bootstrap auth attempted but database is not configured");
            return Ok(None);
        }
    };

    // Check for users (include_deleted=false to only count active users)
    let user_count = db
        .users()
        .count(false)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?;

    if user_count > 0 {
        tracing::debug!(
            user_count = user_count,
            "Bootstrap auth rejected: database has users"
        );
        // Treat post-bootstrap probing as a failed attempt to deter scanners.
        if let Some(ip_str) = ip_str.as_deref() {
            increment_bootstrap_rate_limit(ip_str, state).await;
        }
        return Ok(None);
    }

    tracing::info!("Admin request authenticated via bootstrap API key");

    // Return a special bootstrap identity with the reserved role
    Ok(Some(Identity {
        external_id: "_bootstrap".to_string(),
        email: None,
        name: Some("Bootstrap Admin".to_string()),
        user_id: None,
        roles: vec![BOOTSTRAP_ROLE.to_string()],
        idp_groups: vec![],
        org_ids: vec![],
        team_ids: vec![],
        project_ids: vec![],
    }))
}

/// Per-IP throttle parameters for bootstrap auth failures.
///
/// Bootstrap is unauthenticated until the first user is created and is exposed
/// on every admin route, so an attacker can make unlimited guesses. We cap
/// failures and lock the source IP out for an hour after exceeding the
/// threshold. Values are intentionally hardcoded — bootstrap auth is a narrow
/// installer flow, so additional configuration would just be footgun surface.
const BOOTSTRAP_MAX_ATTEMPTS: i64 = 10;
const BOOTSTRAP_WINDOW_SECS: u64 = 900;
const BOOTSTRAP_LOCKOUT_SECS: u64 = 3600;

/// Increment the bootstrap auth rate-limit counter for an IP and lock the IP
/// out once attempts exceed [`BOOTSTRAP_MAX_ATTEMPTS`].
async fn increment_bootstrap_rate_limit(ip_str: &str, state: &AppState) {
    use std::time::Duration;

    use crate::cache::CacheKeys;

    let Some(cache) = &state.cache else {
        return;
    };

    let rate_limit_key = CacheKeys::bootstrap_rate_limit(ip_str);
    let count = cache
        .incr(&rate_limit_key, Duration::from_secs(BOOTSTRAP_WINDOW_SECS))
        .await
        .unwrap_or(1);

    if count >= BOOTSTRAP_MAX_ATTEMPTS {
        let lockout_key = CacheKeys::bootstrap_lockout(ip_str);
        let _ = cache
            .set_bytes(
                &lockout_key,
                b"1",
                Duration::from_secs(BOOTSTRAP_LOCKOUT_SECS),
            )
            .await;

        tracing::warn!(
            ip = %ip_str,
            attempts = count,
            lockout_secs = BOOTSTRAP_LOCKOUT_SECS,
            event = "bootstrap_auth.lockout_triggered",
            "Bootstrap auth lockout triggered after {} failed attempts",
            count
        );
    }
}

/// Try to authenticate via emergency access key.
///
/// Emergency authentication provides break-glass access when SSO is unavailable.
/// It is designed to work even if the database is corrupted.
///
/// **Security:**
/// - Keys are compared using constant-time comparison to prevent timing attacks
/// - All attempts (success and failure) are logged at WARN level
/// - IP restrictions and rate limiting provide defense in depth
/// - The `_emergency_admin` role cannot be assigned by IdPs (reserved prefix)
async fn try_emergency_auth(
    headers: &axum::http::HeaderMap,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    use subtle::ConstantTimeEq;

    use crate::cache::CacheKeys;

    // Check if emergency access is configured and enabled
    let emergency_config = match &state.config.auth.emergency {
        Some(config) if config.is_enabled() => config,
        _ => return Ok(None),
    };

    // Extract emergency key from headers
    // Check X-Emergency-Key header first, then Authorization: EmergencyKey <key>
    let provided_key = extract_emergency_key(headers);
    let provided_key = match provided_key {
        Some(key) => key,
        None => return Ok(None),
    };

    // Get IP address for rate limiting and IP restrictions
    let ip_str = connecting_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Check for lockout before doing any key comparison
    if let Some(cache) = &state.cache {
        let lockout_key = CacheKeys::emergency_lockout(&ip_str);
        if let Ok(Some(_)) = cache.get_bytes(&lockout_key).await {
            tracing::warn!(
                ip = %ip_str,
                event = "emergency_access.locked_out",
                "Emergency access attempt blocked: IP is locked out"
            );
            return Err(AuthError::Forbidden("Emergency access denied".to_string()));
        }
    }

    // Check global IP restrictions
    if !emergency_config.allowed_ips.is_empty() {
        let parsed_cidrs = emergency_config.parsed_allowed_ips();
        let ip_allowed =
            connecting_ip.is_some_and(|ip| parsed_cidrs.iter().any(|cidr| cidr.contains(&ip)));

        if !ip_allowed {
            tracing::warn!(
                ip = %ip_str,
                event = "emergency_access.ip_rejected",
                "Emergency access attempt rejected: IP not in allowed list"
            );
            // Increment rate limit even for IP failures to prevent key enumeration
            increment_emergency_rate_limit(&ip_str, state, emergency_config).await;
            return Ok(None);
        }
    }

    // Try to match the key against configured accounts
    let mut matched_account: Option<&crate::config::EmergencyAccount> = None;

    for account in &emergency_config.accounts {
        // Constant-time comparison to prevent timing attacks
        let keys_match: bool = provided_key.as_bytes().ct_eq(account.key.as_bytes()).into();

        if keys_match {
            // Check per-account IP restrictions
            if !account.allowed_ips.is_empty() {
                let account_cidrs = account.parsed_allowed_ips();
                let ip_allowed = connecting_ip
                    .is_some_and(|ip| account_cidrs.iter().any(|cidr| cidr.contains(&ip)));

                if !ip_allowed {
                    tracing::warn!(
                        ip = %ip_str,
                        account_id = %account.id,
                        event = "emergency_access.account_ip_rejected",
                        "Emergency access attempt rejected: IP not allowed for this account"
                    );
                    // Key matched but IP not allowed - stop here, don't try other accounts
                    increment_emergency_rate_limit(&ip_str, state, emergency_config).await;
                    return Ok(None);
                }
            }

            matched_account = Some(account);
            break;
        }
    }

    // Handle successful or failed authentication
    match matched_account {
        Some(account) => {
            tracing::warn!(
                ip = %ip_str,
                account_id = %account.id,
                account_name = %account.name,
                email = ?account.email,
                event = "emergency_access.success",
                "Emergency access authentication successful"
            );

            // Ensure _emergency_admin role is included
            let mut roles = account.roles.clone();
            if !roles.contains(&EMERGENCY_ADMIN_ROLE.to_string()) {
                roles.push(EMERGENCY_ADMIN_ROLE.to_string());
            }

            Ok(Some(Identity {
                external_id: format!("_emergency:{}", account.id),
                email: account.email.clone(),
                name: Some(account.name.clone()),
                user_id: None,
                roles,
                idp_groups: vec![],
                org_ids: vec![],
                team_ids: vec![],
                project_ids: vec![],
            }))
        }
        None => {
            tracing::warn!(
                ip = %ip_str,
                event = "emergency_access.invalid_key",
                "Emergency access attempt failed: invalid key"
            );

            // Log failed authentication attempt to audit log
            if let Some(services) = &state.services {
                let _ = services
                    .audit_logs
                    .log_auth_event(AuthEventParams {
                        action: auth_events::EMERGENCY_LOGIN_FAILED,
                        session_id: Uuid::nil(), // Nil UUID indicates no session was created
                        external_id: None,
                        email: None,
                        org_id: None,
                        ip_address: Some(ip_str.to_string()),
                        user_agent: headers
                            .get(axum::http::header::USER_AGENT)
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string()),
                        details: serde_json::json!({
                            "provider": "emergency",
                            "reason": "invalid_key",
                        }),
                    })
                    .await;
            }

            increment_emergency_rate_limit(&ip_str, state, emergency_config).await;

            Ok(None)
        }
    }
}

/// Increment the emergency access rate limit counter for an IP address.
///
/// This should be called on any failed emergency access attempt (invalid key,
/// IP not allowed) to prevent key enumeration attacks.
async fn increment_emergency_rate_limit(
    ip_str: &str,
    state: &AppState,
    emergency_config: &crate::config::EmergencyAccessConfig,
) {
    use std::time::Duration;

    use crate::cache::CacheKeys;

    if let Some(cache) = &state.cache {
        let rate_limit_key = CacheKeys::emergency_rate_limit(ip_str);
        let rate_limit = &emergency_config.rate_limit;
        let window_duration = Duration::from_secs(rate_limit.window_secs);

        // Increment counter (incr handles expiry automatically)
        let count = cache
            .incr(&rate_limit_key, window_duration)
            .await
            .unwrap_or(1);

        // Check if we should lockout
        if count >= rate_limit.max_attempts as i64 {
            let lockout_key = CacheKeys::emergency_lockout(ip_str);
            let lockout_duration = Duration::from_secs(rate_limit.lockout_secs);
            let _ = cache.set_bytes(&lockout_key, b"1", lockout_duration).await;

            tracing::warn!(
                ip = %ip_str,
                attempts = count,
                lockout_secs = rate_limit.lockout_secs,
                event = "emergency_access.lockout_triggered",
                "Emergency access lockout triggered after {} failed attempts",
                count
            );
        }
    }
}

/// Extract emergency key from request headers.
///
/// Checks in order:
/// 1. `X-Emergency-Key: <key>`
/// 2. `Authorization: EmergencyKey <key>`
fn extract_emergency_key(headers: &axum::http::HeaderMap) -> Option<String> {
    // Try X-Emergency-Key header first
    if let Some(key_header) = headers.get("X-Emergency-Key")
        && let Ok(key) = key_header.to_str()
    {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    // Try Authorization: EmergencyKey <key>
    if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION)
        && let Ok(auth_str) = auth_header.to_str()
        && let Some(key) = auth_str.strip_prefix("EmergencyKey ")
    {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    None
}

/// Extract API key from request headers.
///
/// Checks in order:
/// 1. `Authorization: Bearer <key>`
/// 2. `X-API-Key: <key>`
fn extract_api_key(headers: &axum::http::HeaderMap) -> Option<String> {
    // Try Authorization: Bearer first
    if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION)
        && let Ok(auth_str) = auth_header.to_str()
        && let Some(key) = auth_str.strip_prefix("Bearer ")
    {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    // Try X-API-Key header
    if let Some(api_key_header) = headers.get("X-API-Key")
        && let Ok(key) = api_key_header.to_str()
    {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    None
}

/// Try to authenticate an admin request.
async fn try_admin_auth(
    headers: &axum::http::HeaderMap,
    cookies: Option<&Cookies>,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
    client_info: &ClientInfo,
) -> Result<Identity, AuthError> {
    #[cfg(not(feature = "sso"))]
    let _ = &cookies;
    #[cfg(not(feature = "sso"))]
    let _ = &client_info;
    // Try bootstrap API key first (for initial setup when database is empty)
    if let Some(identity) = try_bootstrap_auth(headers, connecting_ip, state).await? {
        return Ok(identity);
    }

    // Try emergency access key (for break-glass access when SSO is unavailable)
    if let Some(identity) = try_emergency_auth(headers, connecting_ip, state).await? {
        return Ok(identity);
    }

    // Try API key (for ApiKey mode — admin panel sends key via Authorization/X-API-Key)
    if matches!(state.config.auth.mode, crate::config::AuthMode::ApiKey)
        && let Some(identity) = try_api_key_admin_auth(headers, state).await?
    {
        return Ok(identity);
    }

    // Try Bearer token (for service accounts / automation via per-org SSO)
    #[cfg(feature = "sso")]
    if let Some(identity) = try_bearer_token_auth(headers, state).await? {
        return Ok(identity);
    }

    // Try Proxy auth headers (from trusted reverse proxy)
    if let Some(identity) = try_proxy_auth_auth(headers, connecting_ip, state).await? {
        return Ok(identity);
    }

    // Try OIDC session using the shared authenticator
    #[cfg(feature = "sso")]
    if let Some(identity) = try_oidc_session_auth(cookies, state, client_info).await? {
        return Ok(identity);
    }

    // Try SAML session using the SAML registry
    #[cfg(feature = "saml")]
    if let Some(identity) = try_saml_session_auth(cookies, state, client_info).await? {
        return Ok(identity);
    }

    // No valid authentication found
    // If IdP mode is configured and we have a single org with SSO, redirect to its login
    #[cfg(feature = "sso")]
    if state.config.auth.requires_session()
        && let Some(registry) = &state.oidc_registry
    {
        let org_ids = registry.list_orgs().await;
        // Only redirect automatically when there's exactly one org (unambiguous)
        if let [org_id] = org_ids.as_slice()
            && let Some(authenticator) = registry.get(*org_id).await
        {
            let (redirect_url, _) = authenticator
                .authorization_url_with_org(None, Some(*org_id))
                .await?;
            return Err(AuthError::OidcAuthRequired { redirect_url });
        }
    }

    Err(AuthError::MissingCredentials)
}

/// Try to authenticate via API key for admin access (ApiKey mode).
///
/// Validates the API key from `Authorization: Bearer` or `X-API-Key` headers
/// using the same logic as API endpoint authentication. Builds an `Identity`
/// from the key owner's information (user, service account, or org).
async fn try_api_key_admin_auth(
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    let api_key_auth = match super::api::try_api_key_auth(headers, state).await? {
        Some(auth) => auth,
        None => return Ok(None),
    };

    // Build Identity from the API key's owner information.
    // For user-owned keys, look up the user's memberships from the database.
    // For service-account-owned keys, use the SA roles.
    // For org/team/project-owned keys, use the org context.
    let identity = if let Some(user_id) = api_key_auth.user_id {
        // User-owned API key — look up user and memberships
        let db = state
            .db
            .as_ref()
            .ok_or_else(|| AuthError::Internal("Database not configured".to_string()))?;

        let user = db
            .users()
            .get_by_id(user_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        let (email, name, external_id) = match &user {
            Some(u) => (u.email.clone(), u.name.clone(), u.external_id.clone()),
            None => (None, None, format!("api-key:{}", api_key_auth.key.id)),
        };

        // Look up memberships
        let org_ids: Vec<String> = if let Some(org_id) = api_key_auth.org_id {
            vec![org_id.to_string()]
        } else {
            db.users()
                .get_org_memberships_for_user(user_id)
                .await
                .map_err(|e| AuthError::Internal(e.to_string()))?
                .iter()
                .map(|m| m.org_id.to_string())
                .collect()
        };

        let team_ids: Vec<String> = if let Some(team_id) = api_key_auth.team_id {
            vec![team_id.to_string()]
        } else {
            db.users()
                .get_team_memberships_for_user(user_id)
                .await
                .map_err(|e| AuthError::Internal(e.to_string()))?
                .iter()
                .map(|m| m.team_id.to_string())
                .collect()
        };

        let project_ids: Vec<String> = if let Some(project_id) = api_key_auth.project_id {
            vec![project_id.to_string()]
        } else {
            db.users()
                .get_project_memberships_for_user(user_id)
                .await
                .map_err(|e| AuthError::Internal(e.to_string()))?
                .iter()
                .map(|m| m.project_id.to_string())
                .collect()
        };

        Identity {
            external_id,
            email,
            name,
            user_id: Some(user_id),
            roles: vec![],
            idp_groups: vec![],
            org_ids,
            team_ids,
            project_ids,
        }
    } else if let Some(sa_id) = api_key_auth.service_account_id {
        // Service-account-owned API key
        Identity {
            external_id: format!("service-account:{sa_id}"),
            email: None,
            name: Some(format!("Service Account {sa_id}")),
            user_id: None,
            roles: api_key_auth.service_account_roles.unwrap_or_default(),
            idp_groups: vec![],
            org_ids: api_key_auth
                .org_id
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            team_ids: vec![],
            project_ids: vec![],
        }
    } else {
        // Org/team/project-owned API key (machine credential)
        Identity {
            external_id: format!("api-key:{}", api_key_auth.key.id),
            email: None,
            name: Some(api_key_auth.key.name.clone()),
            user_id: None,
            roles: vec![],
            idp_groups: vec![],
            org_ids: api_key_auth
                .org_id
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            team_ids: api_key_auth
                .team_id
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            project_ids: api_key_auth
                .project_id
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
        }
    };

    Ok(Some(identity))
}

/// Try to authenticate via Bearer token (JWT).
///
/// This enables programmatic admin access for service accounts using
/// OAuth client credentials flow, or for users who have obtained a token
/// via the authorization code flow.
#[cfg(feature = "sso")]
async fn try_bearer_token_auth(
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    // Extract bearer token from Authorization header
    let auth_header = match headers.get(axum::http::header::AUTHORIZATION) {
        Some(h) => h,
        None => return Ok(None),
    };

    let auth_str = auth_header
        .to_str()
        .map_err(|_| AuthError::Internal("Invalid Authorization header encoding".to_string()))?;

    // Must be Bearer scheme
    let token = match auth_str.strip_prefix("Bearer ") {
        Some(t) => t.trim(),
        None => return Ok(None),
    };

    // Validate the token using per-org SSO configuration.
    // The token must contain an 'org' or 'hadrian_org' claim that specifies
    // which organization's SSO config to use for validation.
    // Returns both claims and the external_id extracted using the org's identity_claim.
    let (claims, external_id) = validate_bearer_token(token, state).await?;

    tracing::debug!(
        sub = %claims.sub,
        external_id = %external_id,
        "Validated bearer token and extracted external_id"
    );

    // Look up internal user and their memberships from the database
    let (user_id, org_ids, team_ids, project_ids) = if let Some(db) = &state.db {
        match db
            .users()
            .get_by_external_id(&external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
        {
            Some(user) => {
                let user_id = user.id;

                // Fetch org memberships
                let org_memberships = db
                    .users()
                    .get_org_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let org_ids: Vec<String> = org_memberships
                    .iter()
                    .map(|m| m.org_id.to_string())
                    .collect();

                // Fetch team memberships
                let team_memberships = db
                    .users()
                    .get_team_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let team_ids: Vec<String> = team_memberships
                    .iter()
                    .map(|m| m.team_id.to_string())
                    .collect();

                // Fetch project memberships
                let project_memberships = db
                    .users()
                    .get_project_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let project_ids: Vec<String> = project_memberships
                    .iter()
                    .map(|m| m.project_id.to_string())
                    .collect();

                (Some(user_id), org_ids, team_ids, project_ids)
            }
            None => {
                tracing::warn!(
                    external_id = %external_id,
                    sub = %claims.sub,
                    "User not found in database for bearer token auth - user_id will be None"
                );
                (None, Vec::new(), Vec::new(), Vec::new())
            }
        }
    } else {
        (None, Vec::new(), Vec::new(), Vec::new())
    };

    // Extract roles from token, stripping any `_`-prefixed reserved roles
    // (bootstrap/emergency) — IdPs must never be able to claim these.
    let roles = strip_reserved_roles(claims.roles.clone().unwrap_or_default());

    tracing::debug!(
        sub = %claims.sub,
        external_id = %external_id,
        roles = ?roles,
        user_id = ?user_id,
        "Admin request authenticated via bearer token"
    );

    Ok(Some(Identity {
        external_id,
        email: claims.email,
        name: claims.name,
        user_id,
        roles,
        idp_groups: claims.groups.clone().unwrap_or_default(),
        org_ids,
        team_ids,
        project_ids,
    }))
}

/// Validate a bearer token using per-org SSO configuration.
///
/// This function:
/// 1. Decodes the JWT unverified to extract the `org` (or `hadrian_org`) claim
/// 2. Looks up that organization's SSO config from the database
/// 3. Validates the JWT against that org's IdP JWKS
///
/// This enables multi-tenant deployments where different orgs use different IdPs.
///
/// Returns a tuple of (claims, external_id) where external_id is extracted using
/// the org's configured identity_claim (e.g., "preferred_username" or "email").
#[cfg(feature = "sso")]
async fn validate_bearer_token(
    token: &str,
    state: &AppState,
) -> Result<(crate::auth::jwt::JwtClaims, String), AuthError> {
    use base64::Engine;

    use crate::models::SsoProviderType;

    // Step 1: Decode JWT unverified to extract org claim
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AuthError::InvalidToken);
    }

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AuthError::InvalidToken)?;

    let claims: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| AuthError::InvalidToken)?;

    // Look for org claim (try both "org" and "hadrian_org")
    let org_slug = claims
        .get("org")
        .or_else(|| claims.get("hadrian_org"))
        .and_then(|v: &serde_json::Value| v.as_str())
        .ok_or_else(|| {
            AuthError::Forbidden(
                "JWT must contain 'org' or 'hadrian_org' claim for per-org SSO validation"
                    .to_string(),
            )
        })?;

    tracing::debug!(org_slug = %org_slug, "Extracted org from bearer token for SSO lookup");

    // Step 2: Look up org's SSO config from database
    let db = state.db.as_ref().ok_or_else(|| {
        AuthError::Internal("Database required for per-org SSO bearer token validation".to_string())
    })?;

    // First, look up the org by slug
    let org = db
        .organizations()
        .get_by_slug(org_slug)
        .await
        .map_err(|e| AuthError::Internal(format!("Failed to look up org: {}", e)))?
        .ok_or_else(|| {
            AuthError::Forbidden(format!(
                "Organization '{}' not found for bearer token validation",
                org_slug
            ))
        })?;

    // Then look up the org's SSO config
    let sso_config = db
        .org_sso_configs()
        .get_by_org_id(org.id)
        .await
        .map_err(|e| AuthError::Internal(format!("Failed to look up SSO config: {}", e)))?
        .ok_or_else(|| {
            AuthError::Forbidden(format!(
                "Organization '{}' has no SSO configuration",
                org_slug
            ))
        })?;

    // Ensure SSO config is enabled
    if !sso_config.enabled {
        return Err(AuthError::Forbidden(format!(
            "SSO is disabled for organization '{}'",
            org_slug
        )));
    }

    // Step 3: Build JWT validator from org's SSO config
    // Currently only OIDC supports bearer token validation
    if sso_config.provider_type != SsoProviderType::Oidc {
        return Err(AuthError::Internal(
            "SAML SSO configurations do not support bearer token validation".to_string(),
        ));
    }

    let issuer = sso_config
        .issuer
        .clone()
        .ok_or_else(|| AuthError::Internal("SSO config missing issuer".to_string()))?;
    let expected_issuer = issuer.clone(); // Keep a copy for post-validation check
    let client_id = sso_config
        .client_id
        .clone()
        .ok_or_else(|| AuthError::Internal("SSO config missing client_id".to_string()))?;
    let discovery_url = sso_config
        .discovery_url
        .clone()
        .unwrap_or_else(|| issuer.clone());
    let identity_claim = sso_config
        .identity_claim
        .clone()
        .unwrap_or_else(|| "sub".to_string());

    // Fetch JWKS URI from OIDC discovery (standard-compliant, works with any IdP)
    let jwks_url = crate::auth::oidc::fetch_jwks_uri(
        &discovery_url,
        &state.http_client,
        state.config.server.allow_loopback_urls,
        state.config.server.allow_private_urls,
    )
    .await
    .map_err(|e| {
        tracing::error!(
            org_slug = %org_slug,
            discovery_url = %discovery_url,
            error = %e,
            "Failed to fetch JWKS URI from OIDC discovery"
        );
        AuthError::Internal(format!(
            "Failed to fetch OIDC discovery for org '{}': {}",
            org_slug, e
        ))
    })?;

    let jwt_config = crate::config::JwtAuthConfig {
        issuer,
        audience: crate::config::OneOrMany::One(client_id),
        jwks_url,
        jwks_refresh_secs: 3600,
        identity_claim,
        org_claim: sso_config.org_claim.clone(),
        additional_claims: vec![],
        allow_expired: false,
        allowed_algorithms: vec![
            crate::config::JwtAlgorithm::RS256,
            crate::config::JwtAlgorithm::RS384,
            crate::config::JwtAlgorithm::RS512,
            crate::config::JwtAlgorithm::ES256,
            crate::config::JwtAlgorithm::ES384,
        ],
    };

    let validator = crate::auth::jwt::JwtValidator::with_options(
        jwt_config,
        crate::validation::UrlValidationOptions {
            allow_loopback: state.config.server.allow_loopback_urls,
            allow_private: state.config.server.allow_private_urls,
        },
    )?;

    let claims = validator.validate(token).await?;

    // Security: Verify the token's issuer matches the org's configured issuer.
    // This prevents attacks where an attacker with a valid token from IdP-A
    // modifies the org claim to access an org configured with IdP-B.
    if claims.iss != expected_issuer {
        tracing::warn!(
            org_slug = %org_slug,
            expected_issuer = %expected_issuer,
            actual_issuer = %claims.iss,
            "JWT issuer mismatch - token issuer does not match org SSO configuration"
        );
        return Err(AuthError::Forbidden(
            "Token issuer does not match organization's SSO configuration".to_string(),
        ));
    }

    // Extract identity using the configured identity_claim
    let external_id = validator.extract_identity(&claims);

    Ok((claims, external_id))
}

/// Try to authenticate via Proxy auth headers.
///
/// **Security:** This function validates that the connecting IP is from a trusted
/// proxy before trusting identity headers. This prevents header spoofing attacks
/// where an attacker connects directly to the gateway and sets fake identity headers.
async fn try_proxy_auth_auth(
    headers: &axum::http::HeaderMap,
    connecting_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<Option<Identity>, AuthError> {
    let config = match state.config.auth.iap_config() {
        Some(config) => config,
        None => return Ok(None),
    };

    // SECURITY: Identity headers may only be trusted when the request comes
    // from a trusted proxy. Config validation refuses startup if IAP is
    // enabled without `server.trusted_proxies` set, so by this point the
    // section must be configured — anything here that isn't from a trusted
    // source is dropped.
    let trusted_proxies = &state.config.server.trusted_proxies;
    let parsed_cidrs = trusted_proxies.parsed_cidrs();

    let is_trusted = match connecting_ip {
        Some(ip) => trusted_proxies.is_trusted_ip(ip, &parsed_cidrs),
        // No connecting IP available — only trust if `dangerously_trust_all`
        // is explicitly set (e.g. unit tests or fully air-gapped envs).
        None => trusted_proxies.dangerously_trust_all,
    };

    if !is_trusted {
        if let Some(ip) = connecting_ip
            && headers.contains_key(&config.identity_header)
        {
            tracing::warn!(
                connecting_ip = %ip,
                identity_header = %config.identity_header,
                "Ignoring Proxy auth identity header from untrusted IP - \
                 configure server.trusted_proxies to trust this source"
            );
        }
        return Ok(None);
    }

    // Check for identity header
    let external_id = match headers.get(&config.identity_header) {
        Some(h) => h
            .to_str()
            .map_err(|_| AuthError::Internal("Invalid identity header encoding".to_string()))?
            .to_string(),
        None => {
            return Ok(None);
        }
    };

    // Extract optional email
    let email = config
        .email_header
        .as_ref()
        .and_then(|h| headers.get(h))
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Extract optional name
    let name = config
        .name_header
        .as_ref()
        .and_then(|h| headers.get(h))
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Look up internal user ID
    let user_id = if let Some(db) = &state.db {
        db.users()
            .get_by_external_id(&external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
            .map(|u| u.id)
    } else {
        None
    };

    // Extract roles from groups header if configured. Strip any `_`-prefixed
    // reserved roles — proxy headers can be spoofed if `trusted_proxies` is
    // misconfigured, so even with that gate we never want to honour a claim
    // for `_emergency_admin`/`_system_bootstrap`.
    let roles = strip_reserved_roles(
        config
            .groups_header
            .as_ref()
            .and_then(|h| headers.get(h))
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                // Try JSON array first, then comma-separated
                serde_json::from_str::<Vec<String>>(v)
                    .unwrap_or_else(|_| v.split(',').map(|s| s.trim().to_string()).collect())
            })
            .unwrap_or_default(),
    );

    // For proxy auth, the groups header contains both roles and raw groups
    // Store them in both fields for backwards compatibility and debugging
    Ok(Some(Identity {
        external_id,
        email,
        name,
        user_id,
        roles: roles.clone(),
        idp_groups: roles,
        org_ids: Vec::new(),
        team_ids: Vec::new(),
        project_ids: Vec::new(),
    }))
}

/// Try to authenticate via OIDC session cookie.
#[cfg(feature = "sso")]
async fn try_oidc_session_auth(
    cookies: Option<&Cookies>,
    state: &AppState,
    client_info: &ClientInfo,
) -> Result<Option<Identity>, AuthError> {
    // Get the OIDC registry which holds the shared session store
    let registry = match &state.oidc_registry {
        Some(reg) => reg,
        None => return Ok(None),
    };

    // Get session config from auth config (or use defaults)
    let session_config = state.config.auth.session_config_or_default();

    let cookies = match cookies {
        Some(c) => c,
        None => return Ok(None),
    };

    // Get session ID from cookie
    let session_cookie = match cookies.get(&session_config.cookie_name) {
        Some(c) => c,
        None => return Ok(None),
    };

    let session_id: Uuid = session_cookie
        .value()
        .parse()
        .map_err(|_| AuthError::InvalidToken)?;

    // Validate session (checks expiration, inactivity timeout, updates last_activity)
    let session = match crate::auth::session_store::validate_and_refresh_session(
        registry.session_store().as_ref(),
        session_id,
        &session_config.enhanced,
    )
    .await
    {
        Ok(s) => s,
        Err(
            crate::auth::session_store::SessionError::NotFound
            | crate::auth::session_store::SessionError::Expired,
        ) => return Ok(None),
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "Session validation failed");
            return Ok(None);
        }
    };

    // Look up internal user and their memberships from the database
    // The database is the source of truth for org/team/project membership
    let (user_id, org_ids, team_ids, project_ids) = if let Some(db) = &state.db {
        match db
            .users()
            .get_by_external_id(&session.external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
        {
            Some(user) => {
                let user_id = user.id;

                // Note: attribute sync is now handled per-org via SSO config
                // The session contains the org context for looking up sync settings

                // Fetch org memberships
                let org_memberships = db
                    .users()
                    .get_org_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let org_ids: Vec<String> = org_memberships
                    .iter()
                    .map(|m| m.org_id.to_string())
                    .collect();

                // Fetch team memberships
                let team_memberships = db
                    .users()
                    .get_team_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let team_ids: Vec<String> = team_memberships
                    .iter()
                    .map(|m| m.team_id.to_string())
                    .collect();

                // Fetch project memberships
                let project_memberships = db
                    .users()
                    .get_project_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let project_ids: Vec<String> = project_memberships
                    .iter()
                    .map(|m| m.project_id.to_string())
                    .collect();

                (Some(user_id), org_ids, team_ids, project_ids)
            }
            None => {
                // User not found - try JIT provisioning if enabled for the org
                // Look up provisioning settings from the session's SSO org
                let provisioned = if let Some(sso_org_id) = session.sso_org_id {
                    // Get the org's SSO config to check provisioning settings
                    if let Ok(Some(sso_config)) =
                        db.org_sso_configs().get_by_org_id(sso_org_id).await
                    {
                        if sso_config.provisioning_enabled && sso_config.create_users {
                            // Convert OrgSsoConfig to ProvisioningConfig for jit_provision_org_scoped
                            let provisioning = crate::config::ProvisioningConfig {
                                enabled: sso_config.provisioning_enabled,
                                create_users: sso_config.create_users,
                                organization_id: Some(sso_org_id.to_string()),
                                default_team_id: sso_config
                                    .default_team_id
                                    .map(|id| id.to_string()),
                                default_org_role: sso_config.default_org_role.clone(),
                                default_team_role: sso_config.default_team_role.clone(),
                                allowed_email_domains: sso_config.allowed_email_domains.clone(),
                                sync_attributes_on_login: sso_config.sync_attributes_on_login,
                                sync_memberships_on_login: sso_config.sync_memberships_on_login,
                            };
                            match jit_provision_org_scoped(db, &session, &provisioning, client_info)
                                .await
                            {
                                Ok(provisioned) => Some(provisioned),
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        external_id = %session.external_id,
                                        org_id = %sso_org_id,
                                        "JIT provisioning failed, continuing without user"
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                provisioned.unwrap_or((None, Vec::new(), Vec::new(), Vec::new()))
            }
        }
    } else {
        (None, Vec::new(), Vec::new(), Vec::new())
    };

    // Use session.roles for actual role names (super_admin, org_admin, etc.)
    // If roles is empty, fall back to groups for backwards compatibility
    let roles = if session.roles.is_empty() {
        session.groups.clone()
    } else {
        session.roles.clone()
    };

    // Check SSO enforcement for all orgs the user is a member of
    // If any org requires SSO and the user didn't authenticate through that org's SSO, reject
    if let Some(services) = &state.services {
        let org_uuids: Vec<Uuid> = org_ids
            .iter()
            .filter_map(|id| id.parse::<Uuid>().ok())
            .collect();

        check_sso_enforcement(
            services,
            &org_uuids,
            session.sso_org_id,
            &session.email,
            client_info,
        )
        .await?;
    }

    Ok(Some(Identity {
        external_id: session.external_id,
        email: session.email,
        name: session.name,
        user_id,
        roles,
        idp_groups: session.groups.clone(),
        org_ids,
        team_ids,
        project_ids,
    }))
}

#[cfg(feature = "saml")]
/// Try to authenticate via SAML session cookie.
///
/// SAML sessions are stored in the shared session store (same as OIDC sessions)
/// but are created through the SAML ACS endpoint rather than OIDC callback.
async fn try_saml_session_auth(
    cookies: Option<&Cookies>,
    state: &AppState,
    client_info: &ClientInfo,
) -> Result<Option<Identity>, AuthError> {
    // Get the SAML registry
    let registry = match &state.saml_registry {
        Some(reg) => reg,
        None => return Ok(None),
    };

    let cookies = match cookies {
        Some(c) => c,
        None => return Ok(None),
    };

    // Get session config from the registry
    let session_config = registry.default_session_config();

    // Get session ID from cookie
    let session_cookie = match cookies.get(&session_config.cookie_name) {
        Some(c) => c,
        None => return Ok(None),
    };

    let session_id: Uuid = session_cookie
        .value()
        .parse()
        .map_err(|_| AuthError::InvalidToken)?;

    // Get session from the registry's shared session store
    let session = match registry.get_session(session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(None),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "Failed to retrieve SAML session"
            );
            return Ok(None);
        }
    };

    // Look up internal user and their memberships from the database
    let (user_id, org_ids, team_ids, project_ids) = if let Some(db) = &state.db {
        match db
            .users()
            .get_by_external_id(&session.external_id)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?
        {
            Some(user) => {
                let user_id = user.id;

                // Fetch org memberships
                let org_memberships = db
                    .users()
                    .get_org_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let org_ids: Vec<String> = org_memberships
                    .iter()
                    .map(|m| m.org_id.to_string())
                    .collect();

                // Fetch team memberships
                let team_memberships = db
                    .users()
                    .get_team_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let team_ids: Vec<String> = team_memberships
                    .iter()
                    .map(|m| m.team_id.to_string())
                    .collect();

                // Fetch project memberships
                let project_memberships = db
                    .users()
                    .get_project_memberships_for_user(user_id)
                    .await
                    .map_err(|e| AuthError::Internal(e.to_string()))?;
                let project_ids: Vec<String> = project_memberships
                    .iter()
                    .map(|m| m.project_id.to_string())
                    .collect();

                (Some(user_id), org_ids, team_ids, project_ids)
            }
            None => {
                // User not found in database - they authenticated via SAML but haven't been provisioned
                // For SAML, we typically rely on SCIM or manual provisioning, so return minimal identity
                tracing::debug!(
                    external_id = %session.external_id,
                    "SAML session valid but user not found in database"
                );
                (None, Vec::new(), Vec::new(), Vec::new())
            }
        }
    } else {
        (None, Vec::new(), Vec::new(), Vec::new())
    };

    // Use session.roles for actual role names (super_admin, org_admin, etc.)
    // If roles is empty, fall back to groups for backwards compatibility
    let roles = if session.roles.is_empty() {
        session.groups.clone()
    } else {
        session.roles.clone()
    };

    // Check SSO enforcement for all orgs the user is a member of
    if let Some(services) = &state.services {
        let org_uuids: Vec<Uuid> = org_ids
            .iter()
            .filter_map(|id| id.parse::<Uuid>().ok())
            .collect();

        check_sso_enforcement(
            services,
            &org_uuids,
            session.sso_org_id,
            &session.email,
            client_info,
        )
        .await?;
    }

    tracing::debug!(
        session_id = %session_id,
        external_id = %session.external_id,
        email = ?session.email,
        user_id = ?user_id,
        "SAML session authenticated"
    );

    Ok(Some(Identity {
        external_id: session.external_id,
        email: session.email,
        name: session.name,
        user_id,
        roles,
        idp_groups: session.groups.clone(),
        org_ids,
        team_ids,
        project_ids,
    }))
}

/// Extract domain from an email address after validating the email format.
///
/// Returns `None` if the email format is invalid according to HTML5 email validation rules.
/// Uses the `validator` crate to ensure proper email format before extracting the domain.
#[cfg(feature = "sso")]
fn extract_email_domain(email: &str) -> Option<&str> {
    use validator::ValidateEmail;
    if !email.validate_email() {
        return None;
    }
    email.split('@').nth(1)
}

/// Check SSO enforcement for the user's organizations.
///
/// This function verifies that the user authenticated correctly based on their
/// organization's SSO enforcement settings:
///
/// - **Optional**: Any authentication method is allowed
/// - **Required**: User must authenticate through the org's SSO provider
/// - **Test**: Same as optional, but logs a warning for audit purposes (shadow mode)
///
/// Enforcement only applies when the user's email domain is verified.
#[cfg(feature = "sso")]
async fn check_sso_enforcement(
    services: &crate::services::Services,
    org_ids: &[Uuid],
    sso_org_id: Option<Uuid>,
    user_email: &Option<String>,
    client_info: &ClientInfo,
) -> Result<(), AuthError> {
    use crate::models::SsoEnforcementMode;

    for org_id in org_ids {
        // Get SSO config for this org
        let sso_config = match services.org_sso_configs.get_by_org_id(*org_id).await {
            Ok(Some(config)) => config,
            Ok(None) => continue, // No SSO config for this org
            Err(e) => {
                tracing::warn!(
                    org_id = %org_id,
                    error = %e,
                    "Failed to check SSO config for enforcement"
                );
                continue;
            }
        };

        // Skip if SSO is not enabled
        if !sso_config.enabled {
            continue;
        }

        // Check if the user's email domain is verified for this SSO config
        let domain_verified = if let Some(email) = user_email {
            if let Some(domain) = extract_email_domain(email) {
                match services
                    .domain_verifications
                    .get_by_config_and_domain(sso_config.id, domain)
                    .await
                {
                    Ok(Some(verification)) => verification.is_verified(),
                    Ok(None) => false,
                    Err(e) => {
                        tracing::warn!(
                            org_id = %org_id,
                            domain = %domain,
                            error = %e,
                            "Failed to check domain verification for SSO enforcement"
                        );
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        // Enforcement only applies to verified domains
        if !domain_verified {
            continue;
        }

        // Check if user authenticated through this org's SSO
        let authenticated_via_org_sso = sso_org_id == Some(*org_id);

        match sso_config.enforcement_mode {
            SsoEnforcementMode::Required => {
                if !authenticated_via_org_sso {
                    tracing::warn!(
                        org_id = %org_id,
                        sso_org_id = ?sso_org_id,
                        user_email = ?user_email,
                        "SSO enforcement: Blocking non-SSO authentication"
                    );
                    return Err(AuthError::Forbidden(
                        "Organization requires SSO authentication. Please sign in using your organization's identity provider.".to_string()
                    ));
                }
            }
            SsoEnforcementMode::Test => {
                if !authenticated_via_org_sso {
                    // Test mode (shadow mode): Log but don't block
                    tracing::warn!(
                        org_id = %org_id,
                        sso_org_id = ?sso_org_id,
                        user_email = ?user_email,
                        enforcement_mode = "test",
                        "SSO test mode: Would have blocked non-SSO authentication (shadow mode)"
                    );
                    let _ = services
                        .audit_logs
                        .create(crate::models::CreateAuditLog {
                            actor_type: crate::models::AuditActorType::System,
                            actor_id: None,
                            action: "sso.enforcement_test".to_string(),
                            resource_type: "organization".to_string(),
                            resource_id: *org_id,
                            org_id: Some(*org_id),
                            project_id: None,
                            details: serde_json::json!({
                                "enforcement_mode": "test",
                                "user_email": user_email,
                                "authenticated_via_org_sso": false,
                            }),
                            ip_address: client_info.ip_address.clone(),
                            user_agent: client_info.user_agent.clone(),
                        })
                        .await;
                }
            }
            SsoEnforcementMode::Optional => {
                // SSO is optional, any auth method is allowed
            }
        }
    }

    Ok(())
}

/// JIT provision using org-scoped provisioning (new, IdP-agnostic approach).
///
/// This function is called when `organization_id` is configured. All users authenticating
/// via this SSO connection are provisioned into the specified organization, regardless
/// of their IdP group claim format.
///
/// This approach works with any IdP (Okta, Azure AD, Auth0, Google, etc.) since it
/// doesn't rely on parsing group claim formats.
#[cfg(feature = "sso")]
async fn jit_provision_org_scoped(
    db: &crate::db::DbPool,
    session: &crate::auth::session_store::OidcSession,
    provisioning: &crate::config::ProvisioningConfig,
    client_info: &ClientInfo,
) -> Result<(Option<Uuid>, Vec<String>, Vec<String>, Vec<String>), AuthError> {
    use crate::{
        db::DbError,
        models::{AddTeamMember, AuditActorType, CreateAuditLog},
        observability::metrics,
    };

    let org_id_or_slug = provisioning
        .organization_id
        .as_ref()
        .expect("organization_id must be set when using org-scoped provisioning");

    tracing::info!(
        external_id = %session.external_id,
        email = ?session.email,
        organization_id = %org_id_or_slug,
        default_team_id = ?provisioning.default_team_id,
        groups = ?session.groups,
        "JIT provisioning user (org-scoped)"
    );

    // Step 1: Resolve the organization
    let org_id = resolve_org_id_or_slug(db, org_id_or_slug).await?;

    // Step 2: Get or create user
    let user_id = if provisioning.create_users {
        let user = get_or_create_user(db, session, client_info).await?;
        Some(user.id)
    } else {
        None
    };

    let mut current_org_ids = Vec::new();
    let mut current_team_ids = Vec::new();

    if let Some(user_id) = user_id {
        current_org_ids.push(org_id);

        // Step 3: Add user to organization
        // Single-org membership is enforced by database unique index (idx_org_memberships_single_org).
        // This is race-condition safe - concurrent requests are serialized by the DB.
        match db
            .users()
            .add_to_org(
                user_id,
                org_id,
                &provisioning.default_org_role,
                crate::models::MembershipSource::Jit,
            )
            .await
        {
            Ok(()) => {
                tracing::debug!(
                    user_id = %user_id,
                    org_id = %org_id,
                    role = %provisioning.default_org_role,
                    "JIT added user to organization (org-scoped)"
                );
                metrics::record_jit_provision("org_membership", "created");

                // Audit log for org membership
                let _ = db
                    .audit_logs()
                    .create(CreateAuditLog {
                        actor_type: AuditActorType::System,
                        actor_id: None,
                        action: "org_membership.jit_provision".to_string(),
                        resource_type: "org_membership".to_string(),
                        resource_id: user_id,
                        org_id: Some(org_id),
                        project_id: None,
                        details: serde_json::json!({
                            "user_id": user_id,
                            "org_id": org_id,
                            "role": provisioning.default_org_role,
                            "provisioning_mode": "org_scoped",
                        }),
                        ip_address: client_info.ip_address.clone(),
                        user_agent: client_info.user_agent.clone(),
                    })
                    .await;
            }
            Err(DbError::Conflict(msg)) => {
                // Check if this is a single-org constraint violation vs already-a-member
                if msg.contains("already belongs to another organization")
                    || msg.contains("single_org")
                {
                    tracing::warn!(
                        user_id = %user_id,
                        target_org_id = %org_id,
                        "User already belongs to another organization"
                    );
                    return Err(AuthError::Forbidden(
                        "User already belongs to another organization. \
                        Contact your administrator."
                            .to_string(),
                    ));
                }
                // Already a member of this org - ignore
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to add user to org during JIT (org-scoped)");
            }
        }

        // Step 4: Add user to default team if configured
        if let Some(team_id_or_slug) = &provisioning.default_team_id {
            let team_id = resolve_team_id_or_slug(db, org_id, team_id_or_slug).await?;
            current_team_ids.push(team_id);

            let add_member = AddTeamMember {
                user_id,
                role: provisioning.default_team_role.clone(),
                source: crate::models::MembershipSource::Jit,
            };

            match db.teams().add_member(team_id, add_member).await {
                Ok(_) => {
                    tracing::debug!(
                        user_id = %user_id,
                        team_id = %team_id,
                        role = %provisioning.default_team_role,
                        "JIT added user to default team (org-scoped)"
                    );
                    metrics::record_jit_provision("team_membership", "created");

                    // Audit log for team membership
                    let _ = db
                        .audit_logs()
                        .create(CreateAuditLog {
                            actor_type: AuditActorType::System,
                            actor_id: None,
                            action: "team_membership.jit_provision".to_string(),
                            resource_type: "team_membership".to_string(),
                            resource_id: user_id,
                            org_id: Some(org_id),
                            project_id: None,
                            details: serde_json::json!({
                                "user_id": user_id,
                                "team_id": team_id,
                                "role": provisioning.default_team_role,
                                "provisioning_mode": "org_scoped",
                            }),
                            ip_address: client_info.ip_address.clone(),
                            user_agent: client_info.user_agent.clone(),
                        })
                        .await;
                }
                Err(DbError::Conflict(_)) => {
                    // Already a member, ignore
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to add user to default team during JIT (org-scoped)");
                }
            }
        }

        // Step 5: Resolve SSO group mappings and add user to mapped teams
        if !session.groups.is_empty() {
            // One SSO config per org, so connection name is always "default"
            let sso_connection_name = "default";
            let resolved_memberships = resolve_group_mappings(
                db,
                sso_connection_name,
                org_id,
                &session.groups,
                &provisioning.default_team_role,
            )
            .await;

            let mut mapped_groups = Vec::new();
            let mut unmapped_groups = session.groups.clone();

            for membership in resolved_memberships {
                // Track which groups were mapped
                if !mapped_groups.contains(&membership.from_idp_group) {
                    mapped_groups.push(membership.from_idp_group.clone());
                    unmapped_groups.retain(|g| g != &membership.from_idp_group);
                }

                // Skip if already in this team (e.g., from default_team_id)
                if current_team_ids.contains(&membership.team_id) {
                    tracing::debug!(
                        user_id = %user_id,
                        team_id = %membership.team_id,
                        idp_group = %membership.from_idp_group,
                        "Skipping group mapping - user already in team"
                    );
                    continue;
                }

                current_team_ids.push(membership.team_id);

                let add_member = AddTeamMember {
                    user_id,
                    role: membership.role.clone(),
                    source: crate::models::MembershipSource::Jit,
                };

                match db.teams().add_member(membership.team_id, add_member).await {
                    Ok(_) => {
                        tracing::info!(
                            user_id = %user_id,
                            team_id = %membership.team_id,
                            role = %membership.role,
                            idp_group = %membership.from_idp_group,
                            "JIT added user to team via group mapping"
                        );
                        metrics::record_jit_provision("team_membership", "created");

                        // Audit log for group-mapped team membership
                        let _ = db
                            .audit_logs()
                            .create(CreateAuditLog {
                                actor_type: AuditActorType::System,
                                actor_id: None,
                                action: "team_membership.jit_group_mapping".to_string(),
                                resource_type: "team_membership".to_string(),
                                resource_id: user_id,
                                org_id: Some(org_id),
                                project_id: None,
                                details: serde_json::json!({
                                    "user_id": user_id,
                                    "team_id": membership.team_id,
                                    "role": membership.role,
                                    "idp_group": membership.from_idp_group,
                                    "sso_connection": sso_connection_name,
                                }),
                                ip_address: client_info.ip_address.clone(),
                                user_agent: client_info.user_agent.clone(),
                            })
                            .await;
                    }
                    Err(DbError::Conflict(_)) => {
                        // Already a member (race condition), ignore
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            team_id = %membership.team_id,
                            idp_group = %membership.from_idp_group,
                            "Failed to add user to team via group mapping"
                        );
                    }
                }
            }

            // Log summary for admin visibility
            if !mapped_groups.is_empty() {
                tracing::info!(
                    user_id = %user_id,
                    mapped_groups = ?mapped_groups,
                    "JIT group mappings resolved"
                );
            }
            if !unmapped_groups.is_empty() {
                tracing::debug!(
                    user_id = %user_id,
                    unmapped_groups = ?unmapped_groups,
                    sso_connection = sso_connection_name,
                    org_id = %org_id,
                    "IdP groups have no configured mappings - configure via Admin UI"
                );
            }
        }

        // Step 6: Sync memberships if enabled
        if provisioning.sync_memberships_on_login {
            sync_memberships(
                db,
                user_id,
                &current_org_ids,
                &current_team_ids,
                client_info,
            )
            .await;
        }
    }

    let org_ids: Vec<String> = current_org_ids.iter().map(|id| id.to_string()).collect();
    let team_ids: Vec<String> = current_team_ids.iter().map(|id| id.to_string()).collect();
    Ok((user_id, org_ids, team_ids, Vec::new()))
}

/// Get or create a user by external_id, handling race conditions.
///
/// If two requests try to create the same user simultaneously, one will fail with
/// a conflict error. This function catches that and retries with a lookup.
#[cfg(feature = "sso")]
async fn get_or_create_user(
    db: &crate::db::DbPool,
    session: &crate::auth::session_store::OidcSession,
    client_info: &ClientInfo,
) -> Result<crate::models::User, AuthError> {
    use crate::{
        db::DbError,
        models::{AuditActorType, CreateAuditLog, CreateUser},
        observability::metrics,
    };

    // First, try to get existing user (in case of race condition)
    if let Some(user) = db
        .users()
        .get_by_external_id(&session.external_id)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?
    {
        return Ok(user);
    }

    // Try to create the user
    let create_user = CreateUser {
        external_id: session.external_id.clone(),
        email: session.email.clone(),
        name: session.name.clone(),
    };

    match db.users().create(create_user).await {
        Ok(user) => {
            tracing::info!(user_id = %user.id, external_id = %session.external_id, "JIT created user");
            metrics::record_jit_provision("user", "created");

            // Audit log for user creation
            let _ = db
                .audit_logs()
                .create(CreateAuditLog {
                    actor_type: AuditActorType::System,
                    actor_id: None,
                    action: "user.jit_provision".to_string(),
                    resource_type: "user".to_string(),
                    resource_id: user.id,
                    org_id: None,
                    project_id: None,
                    details: serde_json::json!({
                        "external_id": session.external_id,
                        "email": session.email,
                        "name": session.name,
                        "groups": session.groups,
                    }),
                    ip_address: client_info.ip_address.clone(),
                    user_agent: client_info.user_agent.clone(),
                })
                .await;

            Ok(user)
        }
        Err(DbError::Conflict(_)) => {
            // Race condition: another request created the user. Fetch it.
            tracing::debug!(
                external_id = %session.external_id,
                "JIT user creation conflict, fetching existing user"
            );
            db.users()
                .get_by_external_id(&session.external_id)
                .await
                .map_err(|e| AuthError::Internal(e.to_string()))?
                .ok_or_else(|| {
                    AuthError::Internal(format!(
                        "User {} disappeared after conflict",
                        session.external_id
                    ))
                })
        }
        Err(e) => Err(AuthError::Internal(e.to_string())),
    }
}

/// Sync user's org/team memberships by removing JIT-created memberships not in current groups.
///
/// IMPORTANT: This function ONLY removes memberships with `source = 'jit'`. Memberships created
/// manually (via admin API/UI) or via SCIM provisioning are preserved. This ensures that:
/// - Admins can manually assign users to teams/orgs without those memberships being removed
/// - SCIM-provisioned memberships are controlled by the IdP, not overwritten by OIDC login
#[cfg(feature = "sso")]
async fn sync_memberships(
    db: &crate::db::DbPool,
    user_id: Uuid,
    current_org_ids: &[Uuid],
    current_team_ids: &[Uuid],
    client_info: &ClientInfo,
) {
    use crate::{
        models::{AuditActorType, CreateAuditLog, MembershipSource},
        observability::metrics,
    };

    // Remove JIT-created org memberships that are no longer in current OIDC groups.
    // Manual and SCIM memberships are preserved.
    match db
        .users()
        .remove_org_memberships_by_source(user_id, MembershipSource::Jit, current_org_ids)
        .await
    {
        Ok(count) if count > 0 => {
            tracing::info!(
                user_id = %user_id,
                removed_count = count,
                "JIT removed user from organizations (sync - JIT memberships only)"
            );
            metrics::record_jit_provisions("org_membership", "removed", count);

            // Audit log for removal
            let _ = db
                .audit_logs()
                .create(CreateAuditLog {
                    actor_type: AuditActorType::System,
                    actor_id: None,
                    action: "org_membership.jit_sync_removed".to_string(),
                    resource_type: "org_membership".to_string(),
                    resource_id: user_id,
                    org_id: None,
                    project_id: None,
                    details: serde_json::json!({
                        "user_id": user_id,
                        "removed_count": count,
                        "reason": "not_in_oidc_groups",
                        "source": "jit",
                    }),
                    ip_address: client_info.ip_address.clone(),
                    user_agent: client_info.user_agent.clone(),
                })
                .await;
        }
        Ok(_) => {
            // No memberships removed - this is normal
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                user_id = %user_id,
                "Failed to remove JIT org memberships during sync"
            );
        }
    }

    // Remove JIT-created team memberships that are no longer in current OIDC groups.
    // Manual and SCIM memberships are preserved.
    match db
        .teams()
        .remove_memberships_by_source(user_id, MembershipSource::Jit, current_team_ids)
        .await
    {
        Ok(count) if count > 0 => {
            tracing::info!(
                user_id = %user_id,
                removed_count = count,
                "JIT removed user from teams (sync - JIT memberships only)"
            );
            metrics::record_jit_provisions("team_membership", "removed", count);

            // Audit log for removal
            let _ = db
                .audit_logs()
                .create(CreateAuditLog {
                    actor_type: AuditActorType::System,
                    actor_id: None,
                    action: "team_membership.jit_sync_removed".to_string(),
                    resource_type: "team_membership".to_string(),
                    resource_id: user_id,
                    org_id: None,
                    project_id: None,
                    details: serde_json::json!({
                        "user_id": user_id,
                        "removed_count": count,
                        "reason": "not_in_oidc_groups",
                        "source": "jit",
                    }),
                    ip_address: client_info.ip_address.clone(),
                    user_agent: client_info.user_agent.clone(),
                })
                .await;
        }
        Ok(_) => {
            // No memberships removed - this is normal
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                user_id = %user_id,
                "Failed to remove JIT team memberships during sync"
            );
        }
    }
}

/// Resolve an organization by ID (UUID) or slug.
///
/// The `id_or_slug` parameter can be either a UUID string or a slug.
/// Returns the organization ID if found, or an error if not found.
#[cfg(feature = "sso")]
async fn resolve_org_id_or_slug(
    db: &crate::db::DbPool,
    id_or_slug: &str,
) -> Result<Uuid, AuthError> {
    // Try parsing as UUID first
    if let Ok(uuid) = Uuid::parse_str(id_or_slug) {
        // Verify the org exists
        let org = db
            .organizations()
            .get_by_id(uuid)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        if let Some(org) = org {
            return Ok(org.id);
        } else {
            return Err(AuthError::Internal(format!(
                "Organization with ID {} not found",
                id_or_slug
            )));
        }
    }

    // Otherwise, treat as slug
    let org = db
        .organizations()
        .get_by_slug(id_or_slug)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?;

    org.map(|o| o.id).ok_or_else(|| {
        AuthError::Internal(format!("Organization with slug '{}' not found", id_or_slug))
    })
}

/// Resolve a team by ID (UUID) or slug within an organization.
///
/// The `id_or_slug` parameter can be either a UUID string or a slug.
/// Returns the team ID if found, or an error if not found.
#[cfg(feature = "sso")]
async fn resolve_team_id_or_slug(
    db: &crate::db::DbPool,
    org_id: Uuid,
    id_or_slug: &str,
) -> Result<Uuid, AuthError> {
    // Try parsing as UUID first
    if let Ok(uuid) = Uuid::parse_str(id_or_slug) {
        // Verify the team exists and belongs to the org
        let team = db
            .teams()
            .get_by_id(uuid)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        if let Some(team) = team {
            if team.org_id == org_id {
                return Ok(team.id);
            } else {
                return Err(AuthError::Internal(format!(
                    "Team {} does not belong to organization {}",
                    id_or_slug, org_id
                )));
            }
        } else {
            return Err(AuthError::Internal(format!(
                "Team with ID {} not found",
                id_or_slug
            )));
        }
    }

    // Otherwise, treat as slug
    let team = db
        .teams()
        .get_by_slug(org_id, id_or_slug)
        .await
        .map_err(|e| AuthError::Internal(e.to_string()))?;

    team.map(|t| t.id).ok_or_else(|| {
        AuthError::Internal(format!(
            "Team with slug '{}' not found in organization",
            id_or_slug
        ))
    })
}

/// Resolve IdP groups to Hadrian team memberships using configured mappings.
///
/// This function looks up SSO group mappings in the database and returns
/// the teams that the user should be added to based on their IdP groups.
///
/// # Arguments
/// * `db` - Database connection
/// * `sso_connection_name` - The SSO connection identifier (defaults to "default")
/// * `org_id` - The organization to resolve memberships within
/// * `idp_groups` - List of IdP group names from the user's token
/// * `default_role` - Default role when a mapping doesn't specify one
///
/// # Returns
/// A list of resolved memberships. Each mapping can specify a team and role.
/// Mappings without a team_id are skipped (they represent org-level roles only).
#[cfg(feature = "sso")]
async fn resolve_group_mappings(
    db: &crate::db::DbPool,
    sso_connection_name: &str,
    org_id: Uuid,
    idp_groups: &[String],
    default_role: &str,
) -> Vec<crate::models::ResolvedMembership> {
    if idp_groups.is_empty() {
        return Vec::new();
    }

    // Find all mappings that match the user's IdP groups
    let mappings = match db
        .sso_group_mappings()
        .find_mappings_for_groups(sso_connection_name, org_id, idp_groups)
        .await
    {
        Ok(mappings) => mappings,
        Err(e) => {
            tracing::warn!(
                error = %e,
                sso_connection = sso_connection_name,
                org_id = %org_id,
                "Failed to resolve SSO group mappings"
            );
            return Vec::new();
        }
    };

    // Convert mappings to resolved memberships, filtering out org-level-only mappings
    mappings
        .into_iter()
        .filter_map(|mapping| {
            // Skip mappings without a team_id (org-level role only)
            let team_id = mapping.team_id?;

            Some(crate::models::ResolvedMembership {
                team_id,
                role: mapping.role.unwrap_or_else(|| default_role.to_string()),
                from_idp_group: mapping.idp_group,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::task::TaskTracker;

    use super::*;
    use crate::config::{AuthMode, GatewayConfig, IapConfig, TrustedProxiesConfig};

    /// Create a minimal AppState for testing with ProxyAuth config
    fn create_test_state(identity_header: &str, trusted_proxies: TrustedProxiesConfig) -> AppState {
        // Create minimal config from empty TOML
        let mut config = GatewayConfig::parse("").unwrap();
        config.auth.mode = AuthMode::Iap(Box::new(IapConfig {
            identity_header: identity_header.to_string(),
            email_header: Some("X-Email".to_string()),
            name_header: None,
            groups_header: Some("X-Groups".to_string()),
            jwt_assertion: None,
            require_identity: true,
        }));
        config.server.trusted_proxies = trusted_proxies;

        AppState {
            config: Arc::new(config),
            db: None,
            services: None,
            cache: None,
            secrets: None,
            dlq: None,
            pricing: Arc::new(crate::pricing::PricingConfig::default()),
            circuit_breakers: crate::providers::CircuitBreakerRegistry::new(),
            provider_health: crate::jobs::ProviderHealthStateRegistry::new(),
            task_tracker: TaskTracker::new(),
            usage_drain: {
                let tracker = TaskTracker::new();
                crate::streaming::UsageDrainHandle::spawn(&tracker, 16)
            },
            #[cfg(feature = "sso")]
            oidc_registry: None,
            #[cfg(feature = "saml")]
            saml_registry: None,
            gateway_jwt_registry: None,
            policy_registry: None,
            usage_buffer: None,
            response_cache: None,
            semantic_cache: None,
            input_guardrails: None,
            output_guardrails: None,
            event_bus: Arc::new(crate::events::EventBus::new()),
            file_search_service: None,
            shell_runtime: None,
            #[cfg(feature = "mcp")]
            mcp_service: None,
            #[cfg(feature = "mcp")]
            tool_search_embeddings: None,
            responses_store: None,
            video_store: None,
            containers_service: None,
            container_session_registry: std::sync::Arc::new(
                crate::services::container_session::ContainerSessionRegistry::new(),
            ),
            response_event_buffer: None,
            #[cfg(any(
                feature = "document-extraction-basic",
                feature = "document-extraction-full"
            ))]
            document_processor: None,
            http_client: reqwest::Client::new(),
            default_user_id: None,
            default_org_id: None,
            provider_metrics: Arc::new(
                crate::services::ProviderMetricsService::with_local_metrics(|| None),
            ),
            model_catalog: crate::catalog::ModelCatalogRegistry::new(),
            static_models_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    fn make_headers(headers: Vec<(&str, &str)>) -> axum::http::HeaderMap {
        let mut map = axum::http::HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                axum::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::header::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    // ========== No trusted_proxies configured (now fails closed) ==========

    #[tokio::test]
    async fn test_proxy_auth_no_proxy_config_drops_headers() {
        // Config validation refuses startup in this case, but we still want
        // the middleware itself to fail closed defensively if it ever runs.
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig::default(), // No proxy config
        );
        let headers = make_headers(vec![("X-Forwarded-User", "alice@example.com")]);

        let result = try_proxy_auth_auth(&headers, Some("192.168.1.100".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(
            result.is_none(),
            "headers must be dropped when trusted_proxies is unset"
        );
    }

    #[tokio::test]
    async fn test_proxy_auth_no_proxy_config_no_connecting_ip() {
        // No trusted_proxies and no connecting IP — still fail closed.
        let state = create_test_state("X-Forwarded-User", TrustedProxiesConfig::default());
        let headers = make_headers(vec![("X-Forwarded-User", "bob@example.com")]);

        let result = try_proxy_auth_auth(&headers, None, &state).await.unwrap();

        assert!(result.is_none());
    }

    // ========== dangerously_trust_all mode ==========

    #[tokio::test]
    async fn test_proxy_auth_trust_all_accepts_any_ip() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: true,
                cidrs: vec![],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![("X-Forwarded-User", "charlie@example.com")]);

        // Any IP should be trusted
        let result = try_proxy_auth_auth(&headers, Some("1.2.3.4".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().external_id, "charlie@example.com");
    }

    // ========== CIDR-based trust (SECURITY CRITICAL) ==========

    #[tokio::test]
    async fn test_proxy_auth_cidr_accepts_trusted_ip() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: false,
                cidrs: vec!["10.0.0.0/8".to_string()],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![
            ("X-Forwarded-User", "david@example.com"),
            ("X-Email", "david@example.com"),
            ("X-Groups", "admin,developers"),
        ]);

        // Request from trusted proxy (10.0.0.1) should be accepted
        let result = try_proxy_auth_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_some());
        let identity = result.unwrap();
        assert_eq!(identity.external_id, "david@example.com");
        assert_eq!(identity.email, Some("david@example.com".to_string()));
        assert_eq!(identity.roles, vec!["admin", "developers"]);
    }

    #[tokio::test]
    async fn test_proxy_auth_cidr_rejects_untrusted_ip() {
        // SECURITY CRITICAL: Headers from untrusted IPs must be ignored
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: false,
                cidrs: vec!["10.0.0.0/8".to_string()], // Only trust 10.x.x.x
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![
            ("X-Forwarded-User", "attacker@evil.com"),
            ("X-Groups", "admin"),
        ]);

        // Request from UNTRUSTED IP (192.168.1.100) should NOT trust headers
        let result = try_proxy_auth_auth(&headers, Some("192.168.1.100".parse().unwrap()), &state)
            .await
            .unwrap();

        // Headers should be ignored - no identity returned
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_proxy_auth_cidr_no_connecting_ip_rejects() {
        // When CIDR is configured but no connecting IP, reject (fail-closed)
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: false,
                cidrs: vec!["10.0.0.0/8".to_string()],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![("X-Forwarded-User", "unknown@example.com")]);

        // No connecting IP means we can't verify trust
        let result = try_proxy_auth_auth(&headers, None, &state).await.unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_proxy_auth_multiple_cidrs() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: false,
                cidrs: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![("X-Forwarded-User", "eve@example.com")]);

        // Request from second CIDR range should be trusted
        let result = try_proxy_auth_auth(&headers, Some("172.16.0.1".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().external_id, "eve@example.com");
    }

    #[tokio::test]
    async fn test_proxy_auth_ipv6_cidr() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: false,
                cidrs: vec!["fd00::/8".to_string()],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![("X-Forwarded-User", "frank@example.com")]);

        // Request from trusted IPv6 range
        let result =
            try_proxy_auth_auth(&headers, Some("fd12:3456:789a::1".parse().unwrap()), &state)
                .await
                .unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().external_id, "frank@example.com");
    }

    // ========== Edge cases ==========

    #[tokio::test]
    async fn test_proxy_auth_missing_identity_header() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: true,
                cidrs: vec![],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        // No X-Forwarded-User header
        let headers = make_headers(vec![("X-Other-Header", "value")]);

        let result = try_proxy_auth_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_proxy_auth_groups_json_array() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: true,
                cidrs: vec![],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![
            ("X-Forwarded-User", "grace@example.com"),
            ("X-Groups", r#"["admin", "users", "developers"]"#),
        ]);

        let result = try_proxy_auth_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_some());
        let identity = result.unwrap();
        assert_eq!(identity.roles, vec!["admin", "users", "developers"]);
    }

    #[tokio::test]
    async fn test_proxy_auth_groups_comma_separated() {
        let state = create_test_state(
            "X-Forwarded-User",
            TrustedProxiesConfig {
                dangerously_trust_all: true,
                cidrs: vec![],
                real_ip_header: "X-Forwarded-For".to_string(),
            },
        );
        let headers = make_headers(vec![
            ("X-Forwarded-User", "henry@example.com"),
            ("X-Groups", "admin, users, developers"),
        ]);

        let result = try_proxy_auth_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state)
            .await
            .unwrap();

        assert!(result.is_some());
        let identity = result.unwrap();
        assert_eq!(identity.roles, vec!["admin", "users", "developers"]);
    }

    // ========== Emergency Access Tests ==========

    use crate::config::{EmergencyAccessConfig, EmergencyAccount, EmergencyRateLimit};

    /// Create a minimal AppState for testing with Emergency config
    fn create_emergency_test_state(emergency_config: Option<EmergencyAccessConfig>) -> AppState {
        let mut config = GatewayConfig::parse("").unwrap();
        config.auth.emergency = emergency_config;

        AppState {
            config: Arc::new(config),
            db: None,
            services: None,
            cache: None,
            secrets: None,
            dlq: None,
            pricing: Arc::new(crate::pricing::PricingConfig::default()),
            circuit_breakers: crate::providers::CircuitBreakerRegistry::new(),
            provider_health: crate::jobs::ProviderHealthStateRegistry::new(),
            task_tracker: TaskTracker::new(),
            usage_drain: {
                let tracker = TaskTracker::new();
                crate::streaming::UsageDrainHandle::spawn(&tracker, 16)
            },
            #[cfg(feature = "sso")]
            oidc_registry: None,
            #[cfg(feature = "saml")]
            saml_registry: None,
            gateway_jwt_registry: None,
            policy_registry: None,
            usage_buffer: None,
            response_cache: None,
            semantic_cache: None,
            input_guardrails: None,
            output_guardrails: None,
            event_bus: Arc::new(crate::events::EventBus::new()),
            file_search_service: None,
            shell_runtime: None,
            #[cfg(feature = "mcp")]
            mcp_service: None,
            #[cfg(feature = "mcp")]
            tool_search_embeddings: None,
            responses_store: None,
            video_store: None,
            containers_service: None,
            container_session_registry: std::sync::Arc::new(
                crate::services::container_session::ContainerSessionRegistry::new(),
            ),
            response_event_buffer: None,
            #[cfg(any(
                feature = "document-extraction-basic",
                feature = "document-extraction-full"
            ))]
            document_processor: None,
            http_client: reqwest::Client::new(),
            default_user_id: None,
            default_org_id: None,
            provider_metrics: Arc::new(
                crate::services::ProviderMetricsService::with_local_metrics(|| None),
            ),
            model_catalog: crate::catalog::ModelCatalogRegistry::new(),
            static_models_cache: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    // Test key must be at least 32 characters for validation
    const TEST_EMERGENCY_KEY: &str = "test-emergency-key-for-unit-tests-32chars";

    fn test_emergency_account() -> EmergencyAccount {
        EmergencyAccount {
            id: "test-admin".to_string(),
            name: "Test Emergency Admin".to_string(),
            key: TEST_EMERGENCY_KEY.to_string(),
            email: Some("admin@example.com".to_string()),
            roles: vec!["super_admin".to_string()],
            allowed_ips: vec![],
        }
    }

    #[tokio::test]
    async fn test_emergency_auth_disabled() {
        // When emergency access is not configured, auth should pass through
        let state = create_emergency_test_state(None);
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_emergency_auth_disabled_explicitly() {
        // When emergency access is explicitly disabled
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: false,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_emergency_auth_success() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        let identity = result.unwrap().expect("Expected successful auth");
        assert_eq!(identity.external_id, "_emergency:test-admin");
        assert_eq!(identity.email, Some("admin@example.com".to_string()));
        assert_eq!(identity.name, Some("Test Emergency Admin".to_string()));
        // Should include both configured roles and _emergency_admin
        assert!(identity.roles.contains(&"super_admin".to_string()));
        assert!(identity.roles.contains(&EMERGENCY_ADMIN_ROLE.to_string()));
    }

    #[tokio::test]
    async fn test_emergency_auth_with_authorization_header() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));
        let headers = make_headers(vec![(
            "Authorization",
            "EmergencyKey test-emergency-key-for-unit-tests-32chars",
        )]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        let identity = result.unwrap().expect("Expected successful auth");
        assert_eq!(identity.external_id, "_emergency:test-admin");
    }

    #[tokio::test]
    async fn test_emergency_auth_wrong_key() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));
        let headers = make_headers(vec![("X-Emergency-Key", "wrong-key")]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Invalid key, no identity
    }

    #[tokio::test]
    async fn test_emergency_auth_no_key_header() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));
        let headers = make_headers(vec![("X-Other-Header", "value")]);

        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // No key header, pass through
    }

    #[tokio::test]
    async fn test_emergency_auth_ip_restricted_success() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec!["10.0.0.0/8".to_string()],
        }));
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        // Request from allowed IP range
        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_emergency_auth_ip_restricted_rejected() {
        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![test_emergency_account()],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec!["10.0.0.0/8".to_string()],
        }));
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        // Request from IP outside allowed range
        let result =
            try_emergency_auth(&headers, Some("192.168.1.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // IP not allowed
    }

    #[tokio::test]
    async fn test_emergency_auth_per_account_ip_restriction() {
        let mut account = test_emergency_account();
        account.allowed_ips = vec!["203.0.113.0/24".to_string()];

        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![account],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![], // No global IP restriction
        }));
        let headers = make_headers(vec![(
            "X-Emergency-Key",
            "test-emergency-key-for-unit-tests-32chars",
        )]);

        // Request from IP outside per-account range
        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Per-account IP not allowed

        // Request from IP inside per-account range
        let result =
            try_emergency_auth(&headers, Some("203.0.113.50".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_some()); // Per-account IP allowed
    }

    #[tokio::test]
    async fn test_emergency_auth_per_account_ip_rejection_stops_iteration() {
        // When a key matches account1 but IP is rejected, we should NOT try account2
        // This prevents key enumeration by observing timing differences
        const SHARED_KEY: &str = "shared-key-for-both-accounts-32chars";

        let account1 = EmergencyAccount {
            id: "admin1".to_string(),
            name: "Admin One".to_string(),
            key: SHARED_KEY.to_string(),
            email: Some("admin1@example.com".to_string()),
            roles: vec!["admin".to_string()],
            allowed_ips: vec!["203.0.113.0/24".to_string()], // Restricted to specific IP range
        };
        let account2 = EmergencyAccount {
            id: "admin2".to_string(),
            name: "Admin Two".to_string(),
            key: SHARED_KEY.to_string(), // SAME key as account1
            email: Some("admin2@example.com".to_string()),
            roles: vec!["super_admin".to_string()],
            allowed_ips: vec![], // No IP restriction
        };

        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![account1, account2],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));

        // Use the shared key from an IP outside admin1's allowed range
        // If the old behavior (continue to next account) was still in place,
        // this would succeed because account2 has the same key and no IP restriction.
        // With the fix, this should return None because account1's key matched
        // but its per-account IP check failed.
        let headers = make_headers(vec![("X-Emergency-Key", SHARED_KEY)]);
        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Should return None when key matches but per-account IP fails (don't try other accounts)"
        );
    }

    #[tokio::test]
    async fn test_emergency_auth_multiple_accounts() {
        const KEY_ADMIN_1: &str = "key-admin-1-emergency-long-key-32chars";
        const KEY_ADMIN_2: &str = "key-admin-2-emergency-long-key-32chars";

        let account1 = EmergencyAccount {
            id: "admin1".to_string(),
            name: "Admin One".to_string(),
            key: KEY_ADMIN_1.to_string(),
            email: Some("admin1@example.com".to_string()),
            roles: vec!["admin".to_string()],
            allowed_ips: vec![],
        };
        let account2 = EmergencyAccount {
            id: "admin2".to_string(),
            name: "Admin Two".to_string(),
            key: KEY_ADMIN_2.to_string(),
            email: Some("admin2@example.com".to_string()),
            roles: vec!["super_admin".to_string()],
            allowed_ips: vec![],
        };

        let state = create_emergency_test_state(Some(EmergencyAccessConfig {
            enabled: true,
            accounts: vec![account1, account2],
            rate_limit: EmergencyRateLimit::default(),
            allowed_ips: vec![],
        }));

        // Test first account
        let headers = make_headers(vec![("X-Emergency-Key", KEY_ADMIN_1)]);
        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;
        assert!(result.is_ok());
        let identity = result.unwrap().expect("Expected successful auth");
        assert_eq!(identity.external_id, "_emergency:admin1");

        // Test second account
        let headers = make_headers(vec![("X-Emergency-Key", KEY_ADMIN_2)]);
        let result = try_emergency_auth(&headers, Some("10.0.0.1".parse().unwrap()), &state).await;
        assert!(result.is_ok());
        let identity = result.unwrap().expect("Expected successful auth");
        assert_eq!(identity.external_id, "_emergency:admin2");
    }

    #[tokio::test]
    async fn test_extract_emergency_key_x_emergency_key_header() {
        let headers = make_headers(vec![("X-Emergency-Key", "my-secret-key")]);
        let result = extract_emergency_key(&headers);
        assert_eq!(result, Some("my-secret-key".to_string()));
    }

    #[tokio::test]
    async fn test_extract_emergency_key_authorization_header() {
        let headers = make_headers(vec![("Authorization", "EmergencyKey my-secret-key")]);
        let result = extract_emergency_key(&headers);
        assert_eq!(result, Some("my-secret-key".to_string()));
    }

    #[tokio::test]
    async fn test_extract_emergency_key_prefers_x_emergency_key() {
        // If both headers are present, X-Emergency-Key takes precedence
        let headers = make_headers(vec![
            ("X-Emergency-Key", "from-x-header"),
            ("Authorization", "EmergencyKey from-auth-header"),
        ]);
        let result = extract_emergency_key(&headers);
        assert_eq!(result, Some("from-x-header".to_string()));
    }

    #[tokio::test]
    async fn test_extract_emergency_key_ignores_bearer() {
        // Authorization: Bearer should not be extracted as emergency key
        let headers = make_headers(vec![("Authorization", "Bearer some-jwt-token")]);
        let result = extract_emergency_key(&headers);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_emergency_key_empty() {
        let headers = make_headers(vec![]);
        let result = extract_emergency_key(&headers);
        assert!(result.is_none());
    }
}
