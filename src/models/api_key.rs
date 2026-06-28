use std::{fmt, net::IpAddr, str::FromStr};

use chrono::{DateTime, Utc};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::config::sovereignty::SovereigntyRequirements;

/// Permission scope for API keys.
///
/// Scopes control which API endpoints an API key can access.
/// When `scopes` is `None` on an API key, the key has full access.
/// When `scopes` is set, the key can only access endpoints matching those scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyScope {
    /// Access to chat completions and responses endpoints (`/v1/chat/completions`, `/v1/responses`)
    Chat,
    /// Access to legacy completions endpoint (`/v1/completions`)
    Completions,
    /// Access to embeddings endpoint (`/v1/embeddings`)
    Embeddings,
    /// Access to image generation endpoints (`/v1/images/*`)
    Images,
    /// Access to video generation endpoints (`/v1/videos/*`)
    Videos,
    /// Access to audio endpoints (`/v1/audio/*`)
    Audio,
    /// Access to files and vector store endpoints (`/v1/files/*`, `/v1/vector_stores/*`)
    Files,
    /// Access to models listing endpoint (`/v1/models`)
    Models,
    /// Access to admin endpoints (`/admin/*`)
    Admin,
}

impl ApiKeyScope {
    /// Returns the string representation of the scope.
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiKeyScope::Chat => "chat",
            ApiKeyScope::Completions => "completions",
            ApiKeyScope::Embeddings => "embeddings",
            ApiKeyScope::Images => "images",
            ApiKeyScope::Videos => "videos",
            ApiKeyScope::Audio => "audio",
            ApiKeyScope::Files => "files",
            ApiKeyScope::Models => "models",
            ApiKeyScope::Admin => "admin",
        }
    }

    /// Returns all valid scope values.
    pub fn all_values() -> &'static [ApiKeyScope] {
        &[
            ApiKeyScope::Chat,
            ApiKeyScope::Completions,
            ApiKeyScope::Embeddings,
            ApiKeyScope::Images,
            ApiKeyScope::Videos,
            ApiKeyScope::Audio,
            ApiKeyScope::Files,
            ApiKeyScope::Models,
            ApiKeyScope::Admin,
        ]
    }

    /// Returns all valid scope names as strings.
    pub fn all_names() -> Vec<&'static str> {
        Self::all_values().iter().map(|s| s.as_str()).collect()
    }
}

impl fmt::Display for ApiKeyScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ApiKeyScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "chat" => Ok(ApiKeyScope::Chat),
            "completions" => Ok(ApiKeyScope::Completions),
            "embeddings" => Ok(ApiKeyScope::Embeddings),
            "images" => Ok(ApiKeyScope::Images),
            "videos" => Ok(ApiKeyScope::Videos),
            "audio" => Ok(ApiKeyScope::Audio),
            "files" => Ok(ApiKeyScope::Files),
            "models" => Ok(ApiKeyScope::Models),
            "admin" => Ok(ApiKeyScope::Admin),
            _ => Err(format!(
                "Invalid scope '{}'. Valid scopes: {}",
                s,
                ApiKeyScope::all_names().join(", ")
            )),
        }
    }
}

/// Validate a list of scope strings.
///
/// Returns `Ok(())` if all scopes are valid, or `Err` with a list of invalid scopes.
pub fn validate_scopes(scopes: &[String]) -> Result<(), Vec<String>> {
    let invalid: Vec<String> = scopes
        .iter()
        .filter(|s| ApiKeyScope::from_str(s).is_err())
        .cloned()
        .collect();

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(invalid)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ApiKey {
    pub id: Uuid,
    /// Prefix of the key (for identification without exposing full key)
    pub key_prefix: String,
    pub name: String,
    pub owner: ApiKeyOwner,
    /// Budget limit in cents
    pub budget_limit_cents: Option<i64>,
    pub budget_period: Option<BudgetPeriod>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Permission scopes (null = full access)
    /// Valid scopes: chat, completions, embeddings, images, videos, audio, files, models, admin
    pub scopes: Option<Vec<String>>,
    /// Allowed models (null = all models, supports wildcards like "gpt-4*")
    pub allowed_models: Option<Vec<String>>,
    /// IP allowlist in CIDR notation (null = all IPs allowed)
    pub ip_allowlist: Option<Vec<String>>,
    /// Requests per minute override (null = use global default)
    pub rate_limit_rpm: Option<i32>,
    /// Tokens per minute override (null = use global default)
    pub rate_limit_tpm: Option<i32>,
    /// ID of the key this was rotated from (for audit trail)
    pub rotated_from_key_id: Option<Uuid>,
    /// If set, this key is being rotated out but still valid until this time
    pub rotation_grace_until: Option<DateTime<Utc>>,
    /// Sovereignty requirements that restrict which models this key can access
    pub sovereignty_requirements: Option<SovereigntyRequirements>,
}

impl ApiKey {
    /// Check if this API key has the required scope.
    ///
    /// Returns `true` if:
    /// - `scopes` is `None` (full access)
    /// - `scopes` contains the required scope
    pub fn has_scope(&self, required: ApiKeyScope) -> bool {
        match &self.scopes {
            None => true, // No scopes = full access
            Some(scopes) => scopes.iter().any(|s| s == required.as_str()),
        }
    }

    /// Check if a model is allowed by this API key's `allowed_models` restriction.
    ///
    /// Returns `true` if:
    /// - `allowed_models` is `None` (all models allowed)
    /// - `allowed_models` is empty (all models allowed)
    /// - Any pattern in `allowed_models` matches the model name
    ///
    /// Patterns support trailing wildcard: `"gpt-4*"` matches `"gpt-4"`, `"gpt-4o"`, `"gpt-4-turbo"`
    pub fn is_model_allowed(&self, model: &str) -> bool {
        match &self.allowed_models {
            None => true,
            Some(patterns) if patterns.is_empty() => true,
            Some(patterns) => patterns.iter().any(|p| model_matches_pattern(model, p)),
        }
    }

    /// Check if an IP address is allowed by this API key's `ip_allowlist`.
    ///
    /// Returns `true` if:
    /// - `ip_allowlist` is `None` (all IPs allowed)
    /// - `ip_allowlist` is empty (all IPs allowed)
    /// - The IP matches any CIDR/IP in the allowlist
    pub fn is_ip_allowed(&self, ip: IpAddr) -> bool {
        match &self.ip_allowlist {
            None => true,
            Some(allowlist) if allowlist.is_empty() => true,
            Some(allowlist) => allowlist.iter().any(|entry| ip_matches_entry(ip, entry)),
        }
    }
}

/// Check if a model name matches a pattern.
///
/// Supports exact match and trailing wildcard:
/// - `"gpt-4"` matches `"gpt-4"` exactly
/// - `"gpt-4*"` matches `"gpt-4"`, `"gpt-4o"`, `"gpt-4-turbo"`
fn model_matches_pattern(model: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        model.starts_with(prefix)
    } else {
        model == pattern
    }
}

/// Validate model patterns for API key configuration.
///
/// Returns `Ok(())` if all patterns are valid, or `Err` with a list of invalid patterns.
///
/// Valid patterns:
/// - Non-empty strings
/// - Wildcards only allowed at the end (trailing `*`)
/// - No bare `*` (would match everything, use `None` for all models)
pub fn validate_model_patterns(patterns: &[String]) -> Result<(), Vec<String>> {
    let invalid: Vec<String> = patterns
        .iter()
        .filter(|p| !is_valid_model_pattern(p))
        .cloned()
        .collect();

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(invalid)
    }
}

/// Check if a single model pattern is valid.
fn is_valid_model_pattern(pattern: &str) -> bool {
    // Empty patterns are invalid
    if pattern.is_empty() {
        return false;
    }

    // Bare "*" is invalid (use None for all models)
    if pattern == "*" {
        return false;
    }

    // Check for wildcards in the middle (only trailing * allowed)
    if let Some(star_pos) = pattern.find('*') {
        // Wildcard must be at the very end
        if star_pos != pattern.len() - 1 {
            return false;
        }
        // Must have at least one character before the wildcard
        if star_pos == 0 {
            return false;
        }
    }

    true
}

/// Check if an IP address matches an allowlist entry.
///
/// Supports both CIDR notation (e.g., "192.168.1.0/24") and single IPs (e.g., "10.0.0.1").
fn ip_matches_entry(ip: IpAddr, entry: &str) -> bool {
    // Try parsing as CIDR
    if let Ok(net) = entry.parse::<IpNet>() {
        return net.contains(&ip);
    }
    // Try parsing as single IP (exact match)
    if let Ok(allowlist_ip) = entry.parse::<IpAddr>() {
        return ip == allowlist_ip;
    }
    false // Invalid entry format - shouldn't happen if validation is done at creation
}

/// Validate IP allowlist entries as valid CIDR notation or IP addresses.
///
/// Returns `Ok(())` if all entries are valid, or `Err` with a list of invalid entries.
/// Valid formats: "192.168.1.0/24", "10.0.0.1", "2001:db8::/32", "::1"
pub fn validate_ip_allowlist(entries: &[String]) -> Result<(), Vec<String>> {
    let invalid: Vec<String> = entries
        .iter()
        .filter(|e| !is_valid_ip_allowlist_entry(e))
        .cloned()
        .collect();

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(invalid)
    }
}

/// Check if a single IP allowlist entry is valid.
fn is_valid_ip_allowlist_entry(entry: &str) -> bool {
    // Try parsing as CIDR first
    if entry.parse::<IpNet>().is_ok() {
        return true;
    }
    // Try parsing as single IP (will be treated as /32 or /128)
    entry.parse::<IpAddr>().is_ok()
}

/// Owner of an API key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiKeyOwner {
    Organization { org_id: Uuid },
    Team { team_id: Uuid },
    Project { project_id: Uuid },
    User { user_id: Uuid },
    ServiceAccount { service_account_id: Uuid },
}

/// Budget period for spending limits
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Daily,
    Monthly,
}

impl BudgetPeriod {
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetPeriod::Daily => "daily",
            BudgetPeriod::Monthly => "monthly",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateApiKey {
    #[validate(length(min = 1, max = 255))]
    pub name: String,
    pub owner: ApiKeyOwner,
    /// Budget limit in cents
    pub budget_limit_cents: Option<i64>,
    pub budget_period: Option<BudgetPeriod>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Permission scopes (null = full access)
    pub scopes: Option<Vec<String>>,
    /// Allowed models (null = all models)
    pub allowed_models: Option<Vec<String>>,
    /// IP allowlist in CIDR notation (null = all IPs)
    pub ip_allowlist: Option<Vec<String>>,
    /// Requests per minute override
    pub rate_limit_rpm: Option<i32>,
    /// Tokens per minute override
    pub rate_limit_tpm: Option<i32>,
    /// Sovereignty requirements for model access
    pub sovereignty_requirements: Option<SovereigntyRequirements>,
}

/// Self-service API key creation request (owner auto-set to current user).
#[derive(Debug, Clone, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateSelfServiceApiKey {
    #[validate(length(min = 1, max = 255))]
    pub name: String,
    /// Budget limit in cents
    pub budget_limit_cents: Option<i64>,
    pub budget_period: Option<BudgetPeriod>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Permission scopes (null = full access)
    pub scopes: Option<Vec<String>>,
    /// Allowed models (null = all models)
    pub allowed_models: Option<Vec<String>>,
    /// IP allowlist in CIDR notation (null = all IPs)
    pub ip_allowlist: Option<Vec<String>>,
    /// Requests per minute override
    pub rate_limit_rpm: Option<i32>,
    /// Tokens per minute override
    pub rate_limit_tpm: Option<i32>,
    /// Sovereignty requirements for model access
    pub sovereignty_requirements: Option<SovereigntyRequirements>,
}

/// Returned on creation only (contains the raw key)
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreatedApiKey {
    #[serde(flatten)]
    pub api_key: ApiKey,
    /// The raw API key (only shown once at creation)
    pub key: String,
}

/// Cached version for fast lookups - contains all data needed for auth
/// This allows us to skip the database query entirely on cache hit.
/// Cache is invalidated on revoke, so we can trust this data for the TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedApiKey {
    /// The full API key data
    pub key: ApiKey,
    /// Resolved organization ID (for project/team/user/service_account-owned keys, this is the parent org)
    pub org_id: Option<Uuid>,
    /// Team ID if key is owned by a team
    pub team_id: Option<Uuid>,
    /// Project ID if key is owned by a project or user within a project
    pub project_id: Option<Uuid>,
    /// User ID if key is owned by a user
    pub user_id: Option<Uuid>,
    /// Service account ID if key is owned by a service account
    pub service_account_id: Option<Uuid>,
    /// Roles from the service account (pre-fetched for RBAC evaluation)
    pub service_account_roles: Option<Vec<String>>,
}

/// API key with ownership details loaded
#[derive(Debug, Clone)]
pub struct ApiKeyWithOwner {
    pub key: ApiKey,
    pub org_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub service_account_id: Option<Uuid>,
    /// Roles from the service account (pre-fetched for RBAC evaluation)
    pub service_account_roles: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_key_scope_from_str() {
        assert_eq!(ApiKeyScope::from_str("chat"), Ok(ApiKeyScope::Chat));
        assert_eq!(
            ApiKeyScope::from_str("completions"),
            Ok(ApiKeyScope::Completions)
        );
        assert_eq!(
            ApiKeyScope::from_str("embeddings"),
            Ok(ApiKeyScope::Embeddings)
        );
        assert_eq!(ApiKeyScope::from_str("images"), Ok(ApiKeyScope::Images));
        assert_eq!(ApiKeyScope::from_str("audio"), Ok(ApiKeyScope::Audio));
        assert_eq!(ApiKeyScope::from_str("files"), Ok(ApiKeyScope::Files));
        assert_eq!(ApiKeyScope::from_str("models"), Ok(ApiKeyScope::Models));
        assert_eq!(ApiKeyScope::from_str("admin"), Ok(ApiKeyScope::Admin));
    }

    #[test]
    fn test_api_key_scope_from_str_invalid() {
        let result = ApiKeyScope::from_str("invalid");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid scope 'invalid'"));
    }

    #[test]
    fn test_api_key_scope_as_str() {
        assert_eq!(ApiKeyScope::Chat.as_str(), "chat");
        assert_eq!(ApiKeyScope::Completions.as_str(), "completions");
        assert_eq!(ApiKeyScope::Embeddings.as_str(), "embeddings");
        assert_eq!(ApiKeyScope::Images.as_str(), "images");
        assert_eq!(ApiKeyScope::Videos.as_str(), "videos");
        assert_eq!(ApiKeyScope::Audio.as_str(), "audio");
        assert_eq!(ApiKeyScope::Files.as_str(), "files");
        assert_eq!(ApiKeyScope::Models.as_str(), "models");
        assert_eq!(ApiKeyScope::Admin.as_str(), "admin");
    }

    #[test]
    fn test_api_key_scope_display() {
        assert_eq!(ApiKeyScope::Chat.to_string(), "chat");
        assert_eq!(ApiKeyScope::Admin.to_string(), "admin");
    }

    #[test]
    fn test_api_key_scope_all_values() {
        let all = ApiKeyScope::all_values();
        assert_eq!(all.len(), 9);
        assert!(all.contains(&ApiKeyScope::Chat));
        assert!(all.contains(&ApiKeyScope::Videos));
        assert!(all.contains(&ApiKeyScope::Admin));
    }

    #[test]
    fn test_api_key_scope_all_names() {
        let names = ApiKeyScope::all_names();
        assert_eq!(names.len(), 9);
        assert!(names.contains(&"chat"));
        assert!(names.contains(&"videos"));
        assert!(names.contains(&"admin"));
    }

    #[test]
    fn test_validate_scopes_valid() {
        let scopes = vec!["chat".to_string(), "embeddings".to_string()];
        assert!(validate_scopes(&scopes).is_ok());
    }

    #[test]
    fn test_validate_scopes_empty() {
        let scopes: Vec<String> = vec![];
        assert!(validate_scopes(&scopes).is_ok());
    }

    #[test]
    fn test_validate_scopes_invalid() {
        let scopes = vec!["chat".to_string(), "invalid".to_string()];
        let result = validate_scopes(&scopes);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["invalid".to_string()]);
    }

    #[test]
    fn test_validate_scopes_multiple_invalid() {
        let scopes = vec!["foo".to_string(), "bar".to_string()];
        let result = validate_scopes(&scopes);
        assert!(result.is_err());
        let invalid = result.unwrap_err();
        assert_eq!(invalid.len(), 2);
        assert!(invalid.contains(&"foo".to_string()));
        assert!(invalid.contains(&"bar".to_string()));
    }

    fn make_test_api_key(scopes: Option<Vec<String>>) -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            key_prefix: "test_".to_string(),
            name: "Test Key".to_string(),
            owner: ApiKeyOwner::Organization {
                org_id: Uuid::new_v4(),
            },
            budget_limit_cents: None,
            budget_period: None,
            created_at: chrono::Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            scopes,
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
    fn test_has_scope_none_means_full_access() {
        let key = make_test_api_key(None);
        assert!(key.has_scope(ApiKeyScope::Chat));
        assert!(key.has_scope(ApiKeyScope::Embeddings));
        assert!(key.has_scope(ApiKeyScope::Admin));
    }

    #[test]
    fn test_has_scope_with_specific_scopes() {
        let key = make_test_api_key(Some(vec!["chat".to_string(), "embeddings".to_string()]));
        assert!(key.has_scope(ApiKeyScope::Chat));
        assert!(key.has_scope(ApiKeyScope::Embeddings));
        assert!(!key.has_scope(ApiKeyScope::Admin));
        assert!(!key.has_scope(ApiKeyScope::Images));
    }

    #[test]
    fn test_has_scope_empty_scopes() {
        let key = make_test_api_key(Some(vec![]));
        assert!(!key.has_scope(ApiKeyScope::Chat));
        assert!(!key.has_scope(ApiKeyScope::Admin));
    }

    // Helper function to create test API key with allowed_models
    fn make_test_api_key_with_models(allowed_models: Option<Vec<String>>) -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            key_prefix: "test_".to_string(),
            name: "Test Key".to_string(),
            owner: ApiKeyOwner::Organization {
                org_id: Uuid::new_v4(),
            },
            budget_limit_cents: None,
            budget_period: None,
            created_at: chrono::Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            scopes: None,
            allowed_models,
            ip_allowlist: None,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
            rotated_from_key_id: None,
            rotation_grace_until: None,
            sovereignty_requirements: None,
        }
    }

    #[test]
    fn test_is_model_allowed_none_means_all_allowed() {
        let key = make_test_api_key_with_models(None);
        assert!(key.is_model_allowed("gpt-4"));
        assert!(key.is_model_allowed("claude-3"));
        assert!(key.is_model_allowed("any-model-at-all"));
    }

    #[test]
    fn test_is_model_allowed_empty_means_all_allowed() {
        let key = make_test_api_key_with_models(Some(vec![]));
        assert!(key.is_model_allowed("gpt-4"));
        assert!(key.is_model_allowed("claude-3"));
    }

    #[test]
    fn test_is_model_allowed_exact_match() {
        let key = make_test_api_key_with_models(Some(vec!["gpt-4".to_string()]));
        assert!(key.is_model_allowed("gpt-4"));
        assert!(!key.is_model_allowed("gpt-4o"));
        assert!(!key.is_model_allowed("gpt-4-turbo"));
        assert!(!key.is_model_allowed("claude-3"));
    }

    #[test]
    fn test_is_model_allowed_wildcard_match() {
        let key = make_test_api_key_with_models(Some(vec!["gpt-4*".to_string()]));
        assert!(key.is_model_allowed("gpt-4"));
        assert!(key.is_model_allowed("gpt-4o"));
        assert!(key.is_model_allowed("gpt-4-turbo"));
        assert!(key.is_model_allowed("gpt-4o-mini"));
        assert!(!key.is_model_allowed("gpt-3.5"));
        assert!(!key.is_model_allowed("claude-3"));
    }

    #[test]
    fn test_is_model_allowed_multiple_patterns() {
        let key = make_test_api_key_with_models(Some(vec![
            "gpt-4*".to_string(),
            "claude-3-opus".to_string(),
        ]));
        assert!(key.is_model_allowed("gpt-4"));
        assert!(key.is_model_allowed("gpt-4o"));
        assert!(key.is_model_allowed("claude-3-opus"));
        assert!(!key.is_model_allowed("claude-3-sonnet"));
        assert!(!key.is_model_allowed("gpt-3.5"));
    }

    #[test]
    fn test_model_matches_pattern_exact() {
        assert!(model_matches_pattern("gpt-4", "gpt-4"));
        assert!(!model_matches_pattern("gpt-4o", "gpt-4"));
        assert!(!model_matches_pattern("gpt-4-turbo", "gpt-4"));
    }

    #[test]
    fn test_model_matches_pattern_wildcard() {
        assert!(model_matches_pattern("gpt-4", "gpt-4*"));
        assert!(model_matches_pattern("gpt-4o", "gpt-4*"));
        assert!(model_matches_pattern("gpt-4-turbo", "gpt-4*"));
        assert!(model_matches_pattern("gpt-4o-mini", "gpt-4*"));
        assert!(!model_matches_pattern("gpt-3.5", "gpt-4*"));
    }

    #[test]
    fn test_validate_model_patterns_valid() {
        // Exact patterns
        assert!(
            validate_model_patterns(&["gpt-4".to_string(), "claude-3-opus".to_string()]).is_ok()
        );

        // Wildcard patterns
        assert!(validate_model_patterns(&["gpt-4*".to_string(), "claude-*".to_string()]).is_ok());

        // Mixed patterns
        assert!(validate_model_patterns(&["gpt-4".to_string(), "claude-*".to_string()]).is_ok());
    }

    #[test]
    fn test_validate_model_patterns_empty_list() {
        assert!(validate_model_patterns(&[]).is_ok());
    }

    #[test]
    fn test_validate_model_patterns_invalid_empty_string() {
        let result = validate_model_patterns(&["".to_string()]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["".to_string()]);
    }

    #[test]
    fn test_validate_model_patterns_invalid_bare_wildcard() {
        let result = validate_model_patterns(&["*".to_string()]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["*".to_string()]);
    }

    #[test]
    fn test_validate_model_patterns_invalid_middle_wildcard() {
        let result = validate_model_patterns(&["gpt-*-turbo".to_string()]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["gpt-*-turbo".to_string()]);
    }

    #[test]
    fn test_validate_model_patterns_invalid_multiple_wildcards() {
        let result = validate_model_patterns(&["gpt-**".to_string()]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["gpt-**".to_string()]);
    }

    #[test]
    fn test_validate_model_patterns_mixed_valid_invalid() {
        let result = validate_model_patterns(&[
            "gpt-4*".to_string(),   // valid
            "*".to_string(),        // invalid
            "claude-3".to_string(), // valid
            "".to_string(),         // invalid
        ]);
        assert!(result.is_err());
        let invalid = result.unwrap_err();
        assert_eq!(invalid.len(), 2);
        assert!(invalid.contains(&"*".to_string()));
        assert!(invalid.contains(&"".to_string()));
    }

    // ---- IP allowlist tests ----

    #[test]
    fn test_validate_ip_allowlist_valid_ipv4() {
        assert!(validate_ip_allowlist(&["192.168.1.1".to_string()]).is_ok());
        assert!(validate_ip_allowlist(&["10.0.0.0/8".to_string()]).is_ok());
        assert!(
            validate_ip_allowlist(&["192.168.0.0/16".to_string(), "10.0.0.1".to_string()]).is_ok()
        );
    }

    #[test]
    fn test_validate_ip_allowlist_valid_ipv6() {
        assert!(validate_ip_allowlist(&["::1".to_string()]).is_ok());
        assert!(validate_ip_allowlist(&["2001:db8::/32".to_string()]).is_ok());
    }

    #[test]
    fn test_validate_ip_allowlist_invalid() {
        let result = validate_ip_allowlist(&["not-an-ip".to_string()]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["not-an-ip".to_string()]);
    }

    #[test]
    fn test_validate_ip_allowlist_empty() {
        assert!(validate_ip_allowlist(&[]).is_ok());
    }

    #[test]
    fn test_validate_ip_allowlist_mixed_valid_invalid() {
        let result = validate_ip_allowlist(&[
            "192.168.1.0/24".to_string(), // valid
            "invalid".to_string(),        // invalid
            "10.0.0.1".to_string(),       // valid
        ]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["invalid".to_string()]);
    }

    // Helper function to create test API key with IP allowlist
    fn make_test_api_key_with_ip_allowlist(ip_allowlist: Option<Vec<String>>) -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            key_prefix: "test_".to_string(),
            name: "Test Key".to_string(),
            owner: ApiKeyOwner::Organization {
                org_id: Uuid::new_v4(),
            },
            budget_limit_cents: None,
            budget_period: None,
            created_at: chrono::Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            scopes: None,
            allowed_models: None,
            ip_allowlist,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
            rotated_from_key_id: None,
            rotation_grace_until: None,
            sovereignty_requirements: None,
        }
    }

    #[test]
    fn test_is_ip_allowed_none_means_all_allowed() {
        let key = make_test_api_key_with_ip_allowlist(None);
        assert!(key.is_ip_allowed("192.168.1.1".parse().unwrap()));
        assert!(key.is_ip_allowed("::1".parse().unwrap()));
    }

    #[test]
    fn test_is_ip_allowed_empty_means_all_allowed() {
        let key = make_test_api_key_with_ip_allowlist(Some(vec![]));
        assert!(key.is_ip_allowed("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn test_is_ip_allowed_exact_match() {
        let key = make_test_api_key_with_ip_allowlist(Some(vec!["192.168.1.100".to_string()]));
        assert!(key.is_ip_allowed("192.168.1.100".parse().unwrap()));
        assert!(!key.is_ip_allowed("192.168.1.101".parse().unwrap()));
    }

    #[test]
    fn test_is_ip_allowed_cidr_match() {
        let key = make_test_api_key_with_ip_allowlist(Some(vec!["10.0.0.0/8".to_string()]));
        assert!(key.is_ip_allowed("10.0.0.1".parse().unwrap()));
        assert!(key.is_ip_allowed("10.255.255.255".parse().unwrap()));
        assert!(!key.is_ip_allowed("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn test_is_ip_allowed_multiple_entries() {
        let key = make_test_api_key_with_ip_allowlist(Some(vec![
            "10.0.0.0/8".to_string(),
            "192.168.1.100".to_string(),
        ]));
        assert!(key.is_ip_allowed("10.0.0.1".parse().unwrap()));
        assert!(key.is_ip_allowed("192.168.1.100".parse().unwrap()));
        assert!(!key.is_ip_allowed("192.168.1.101".parse().unwrap()));
    }

    #[test]
    fn test_is_ip_allowed_ipv6() {
        let key = make_test_api_key_with_ip_allowlist(Some(vec!["2001:db8::/32".to_string()]));
        assert!(key.is_ip_allowed("2001:db8::1".parse().unwrap()));
        assert!(!key.is_ip_allowed("2001:db9::1".parse().unwrap()));
    }

    #[test]
    fn test_ip_matches_entry_exact_ipv4() {
        assert!(ip_matches_entry(
            "192.168.1.1".parse().unwrap(),
            "192.168.1.1"
        ));
        assert!(!ip_matches_entry(
            "192.168.1.2".parse().unwrap(),
            "192.168.1.1"
        ));
    }

    #[test]
    fn test_ip_matches_entry_cidr_ipv4() {
        assert!(ip_matches_entry(
            "192.168.1.50".parse().unwrap(),
            "192.168.1.0/24"
        ));
        assert!(!ip_matches_entry(
            "192.168.2.1".parse().unwrap(),
            "192.168.1.0/24"
        ));
    }

    #[test]
    fn test_ip_matches_entry_exact_ipv6() {
        assert!(ip_matches_entry("::1".parse().unwrap(), "::1"));
        assert!(!ip_matches_entry("::2".parse().unwrap(), "::1"));
    }

    #[test]
    fn test_ip_matches_entry_cidr_ipv6() {
        assert!(ip_matches_entry(
            "2001:db8::1".parse().unwrap(),
            "2001:db8::/32"
        ));
        assert!(!ip_matches_entry(
            "2001:db9::1".parse().unwrap(),
            "2001:db8::/32"
        ));
    }

    #[test]
    fn test_ip_matches_entry_invalid_entry() {
        // Invalid entry should not match anything
        assert!(!ip_matches_entry(
            "192.168.1.1".parse().unwrap(),
            "not-valid"
        ));
    }
}
