use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Default limits configuration.
///
/// These limits are applied when no specific limits are set at the
/// org, project, or user level.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    /// Rate limiting defaults.
    #[serde(default)]
    pub rate_limits: RateLimitDefaults,

    /// Budget defaults.
    #[serde(default)]
    pub budgets: BudgetDefaults,

    /// Resource limits for entity counts.
    #[serde(default)]
    pub resource_limits: ResourceLimits,
}

/// Resource limits for entity counts.
///
/// These limits prevent unbounded growth of resources that could cause
/// performance issues or resource exhaustion. Set any limit to 0 for unlimited.
///
/// **Enforcement model:** Limits are best-effort. Under concurrent load, the
/// `count → compare → create` pattern may allow a small number of requests
/// to exceed the configured limit. This is acceptable for configuration
/// guardrails; use database-level constraints for strict enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    /// Maximum RBAC policies per organization. Default: 100.
    #[serde(default = "default_max_policies_per_org")]
    pub max_policies_per_org: u32,

    /// Maximum dynamic providers per user (BYOK). Default: 10.
    #[serde(default = "default_max_providers_per_user")]
    pub max_providers_per_user: u32,

    /// Maximum dynamic providers per organization. Default: 100.
    #[serde(default = "default_max_providers_per_org")]
    pub max_providers_per_org: u32,

    /// Maximum dynamic providers per team. Default: 50.
    #[serde(default = "default_max_providers_per_team")]
    pub max_providers_per_team: u32,

    /// Maximum dynamic providers per project. Default: 50.
    #[serde(default = "default_max_providers_per_project")]
    pub max_providers_per_project: u32,

    /// Maximum API keys per user (self-service). Default: 25.
    #[serde(default = "default_max_api_keys_per_user")]
    pub max_api_keys_per_user: u32,

    /// Maximum API keys per organization. Default: 500.
    #[serde(default = "default_max_api_keys_per_org")]
    pub max_api_keys_per_org: u32,

    /// Maximum API keys per team. Default: 100.
    #[serde(default = "default_max_api_keys_per_team")]
    pub max_api_keys_per_team: u32,

    /// Maximum API keys per project. Default: 100.
    #[serde(default = "default_max_api_keys_per_project")]
    pub max_api_keys_per_project: u32,

    /// Maximum teams per organization. Default: 100.
    #[serde(default = "default_max_teams_per_org")]
    pub max_teams_per_org: u32,

    /// Maximum projects per organization. Default: 1000.
    #[serde(default = "default_max_projects_per_org")]
    pub max_projects_per_org: u32,

    /// Maximum service accounts per organization. Default: 50.
    #[serde(default = "default_max_service_accounts_per_org")]
    pub max_service_accounts_per_org: u32,

    /// Maximum vector stores per owner (org/team/project/user). Default: 100.
    #[serde(default = "default_max_vector_stores_per_owner")]
    pub max_vector_stores_per_owner: u32,

    /// Maximum files per vector store. Default: 10,000.
    #[serde(default = "default_max_files_per_vector_store")]
    pub max_files_per_vector_store: u32,

    /// Maximum conversations per owner (project/user). Default: 10,000.
    #[serde(default = "default_max_conversations_per_owner")]
    pub max_conversations_per_owner: u32,

    /// Maximum templates per owner (org/team/project/user). Default: 5,000.
    #[serde(default = "default_max_templates_per_owner")]
    pub max_templates_per_owner: u32,

    /// Maximum skills per owner (org/team/project/user). Default: 5,000.
    #[serde(default = "default_max_skills_per_owner")]
    pub max_skills_per_owner: u32,

    /// Maximum total size of a skill's files in bytes (SKILL.md + bundled
    /// files). Default: 512,000 (500 KiB). Set to 0 for unlimited.
    #[serde(default = "default_max_skill_bytes")]
    pub max_skill_bytes: u32,

    /// Maximum number of immutable versions retained per skill. Default: 0
    /// (unlimited). When set, creating a version past the limit is rejected.
    #[serde(default = "default_max_skill_versions_per_skill")]
    pub max_skill_versions_per_skill: u32,

    /// Maximum domain verifications per SSO configuration. Default: 50.
    #[serde(default = "default_max_domains_per_sso_config")]
    pub max_domains_per_sso_config: u32,

    /// Maximum SSO group mappings per organization. Default: 500.
    #[serde(default = "default_max_sso_group_mappings_per_org")]
    pub max_sso_group_mappings_per_org: u32,

    /// Maximum members per organization. Default: 10,000.
    #[serde(default = "default_max_members_per_org")]
    pub max_members_per_org: u32,

    /// Maximum members per team. Default: 10,000.
    #[serde(default = "default_max_members_per_team")]
    pub max_members_per_team: u32,

    /// Maximum members per project. Default: 10,000.
    #[serde(default = "default_max_members_per_project")]
    pub max_members_per_project: u32,

    /// Maximum uploaded files per owner (org/team/project/user). Default: 10,000.
    #[serde(default = "default_max_files_per_owner")]
    pub max_files_per_owner: u32,

    /// Maximum projects per team. Default: 100.
    #[serde(default = "default_max_projects_per_team")]
    pub max_projects_per_team: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_policies_per_org: default_max_policies_per_org(),
            max_providers_per_user: default_max_providers_per_user(),
            max_providers_per_org: default_max_providers_per_org(),
            max_providers_per_team: default_max_providers_per_team(),
            max_providers_per_project: default_max_providers_per_project(),
            max_api_keys_per_user: default_max_api_keys_per_user(),
            max_api_keys_per_org: default_max_api_keys_per_org(),
            max_api_keys_per_team: default_max_api_keys_per_team(),
            max_api_keys_per_project: default_max_api_keys_per_project(),
            max_teams_per_org: default_max_teams_per_org(),
            max_projects_per_org: default_max_projects_per_org(),
            max_service_accounts_per_org: default_max_service_accounts_per_org(),
            max_vector_stores_per_owner: default_max_vector_stores_per_owner(),
            max_files_per_vector_store: default_max_files_per_vector_store(),
            max_conversations_per_owner: default_max_conversations_per_owner(),
            max_templates_per_owner: default_max_templates_per_owner(),
            max_skills_per_owner: default_max_skills_per_owner(),
            max_skill_bytes: default_max_skill_bytes(),
            max_skill_versions_per_skill: default_max_skill_versions_per_skill(),
            max_domains_per_sso_config: default_max_domains_per_sso_config(),
            max_sso_group_mappings_per_org: default_max_sso_group_mappings_per_org(),
            max_members_per_org: default_max_members_per_org(),
            max_members_per_team: default_max_members_per_team(),
            max_members_per_project: default_max_members_per_project(),
            max_files_per_owner: default_max_files_per_owner(),
            max_projects_per_team: default_max_projects_per_team(),
        }
    }
}

fn default_max_policies_per_org() -> u32 {
    100
}

fn default_max_providers_per_user() -> u32 {
    10
}

fn default_max_providers_per_org() -> u32 {
    100
}

fn default_max_providers_per_team() -> u32 {
    50
}

fn default_max_providers_per_project() -> u32 {
    50
}

fn default_max_api_keys_per_user() -> u32 {
    25
}

fn default_max_api_keys_per_org() -> u32 {
    500
}

fn default_max_api_keys_per_team() -> u32 {
    100
}

fn default_max_api_keys_per_project() -> u32 {
    100
}

fn default_max_teams_per_org() -> u32 {
    100
}

fn default_max_projects_per_org() -> u32 {
    1000
}

fn default_max_service_accounts_per_org() -> u32 {
    50
}

fn default_max_vector_stores_per_owner() -> u32 {
    100
}

fn default_max_files_per_vector_store() -> u32 {
    10_000
}

fn default_max_conversations_per_owner() -> u32 {
    10_000
}

fn default_max_templates_per_owner() -> u32 {
    5_000
}

fn default_max_skills_per_owner() -> u32 {
    5_000
}

fn default_max_skill_bytes() -> u32 {
    // 500 KiB — generous enough for SKILL.md plus a handful of bundled
    // scripts/references, small enough to keep tool-result tokens bounded.
    512_000
}

fn default_max_skill_versions_per_skill() -> u32 {
    // Unlimited by default; operators can cap version history if desired.
    0
}

fn default_max_domains_per_sso_config() -> u32 {
    50
}

fn default_max_sso_group_mappings_per_org() -> u32 {
    500
}

fn default_max_members_per_org() -> u32 {
    10_000
}

fn default_max_members_per_team() -> u32 {
    10_000
}

fn default_max_members_per_project() -> u32 {
    10_000
}

fn default_max_files_per_owner() -> u32 {
    10_000
}

fn default_max_projects_per_team() -> u32 {
    100
}

/// Rate limiting defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RateLimitDefaults {
    /// Requests per minute per identity.
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,

    /// Requests per day per identity.
    #[serde(default)]
    pub requests_per_day: Option<u32>,

    /// Tokens per minute per identity.
    #[serde(default = "default_tpm")]
    pub tokens_per_minute: u32,

    /// Tokens per day per identity.
    #[serde(default)]
    pub tokens_per_day: Option<u32>,

    /// Concurrent request limit per identity.
    #[serde(default = "default_concurrent")]
    pub concurrent_requests: u32,

    /// Rate limit window type.
    #[serde(default)]
    pub window_type: RateLimitWindowType,

    /// Estimated tokens per request for atomic token rate limit reservation.
    /// This is reserved before the request is processed to prevent race conditions.
    /// After the request completes, the actual token count replaces the estimate.
    /// Default is 1000 tokens which is conservative for most prompts.
    #[serde(default = "default_estimated_tokens")]
    pub estimated_tokens_per_request: i64,

    /// IP-based rate limiting for unauthenticated requests.
    /// Protects public endpoints (health, auth) from abuse.
    #[serde(default)]
    pub ip_rate_limits: IpRateLimitConfig,

    /// Allow per-API-key rate limits to exceed global defaults.
    /// When false (default), API keys cannot have higher rate limits than the global config.
    /// When true, API keys can have any positive rate limit value.
    #[serde(default)]
    pub allow_per_key_above_global: bool,
}

/// IP-based rate limiting configuration for unauthenticated traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct IpRateLimitConfig {
    /// Enable IP-based rate limiting for unauthenticated requests.
    #[serde(default = "default_ip_rate_limit_enabled")]
    pub enabled: bool,

    /// Requests per minute per IP address.
    #[serde(default = "default_ip_rpm")]
    pub requests_per_minute: u32,

    /// Requests per hour per IP address.
    /// Provides longer-term protection against sustained abuse.
    #[serde(default)]
    pub requests_per_hour: Option<u32>,
}

impl Default for IpRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: default_ip_rate_limit_enabled(),
            requests_per_minute: default_ip_rpm(),
            requests_per_hour: None,
        }
    }
}

fn default_ip_rate_limit_enabled() -> bool {
    true
}

fn default_ip_rpm() -> u32 {
    120 // 2 requests per second average
}

impl Default for RateLimitDefaults {
    fn default() -> Self {
        Self {
            requests_per_minute: default_rpm(),
            requests_per_day: None,
            tokens_per_minute: default_tpm(),
            tokens_per_day: None,
            concurrent_requests: default_concurrent(),
            window_type: RateLimitWindowType::default(),
            estimated_tokens_per_request: default_estimated_tokens(),
            ip_rate_limits: IpRateLimitConfig::default(),
            allow_per_key_above_global: false,
        }
    }
}

fn default_estimated_tokens() -> i64 {
    1000 // Conservative estimate for most prompts
}

fn default_rpm() -> u32 {
    60
}

fn default_tpm() -> u32 {
    100_000
}

fn default_concurrent() -> u32 {
    10
}

/// Rate limit window type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RateLimitWindowType {
    /// Fixed window (resets at interval boundaries).
    Fixed,
    /// Sliding window (rolling count over the interval).
    #[default]
    Sliding,
}

/// Budget defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BudgetDefaults {
    /// Default monthly budget in USD. None means unlimited.
    #[serde(default)]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<String>"))]
    pub monthly_budget_usd: Option<Decimal>,

    /// Default daily budget in USD. None means unlimited.
    #[serde(default)]
    #[cfg_attr(feature = "json-schema", schemars(with = "Option<String>"))]
    pub daily_budget_usd: Option<Decimal>,

    /// Warning threshold as a percentage (0.0-1.0).
    /// Notifications are sent when this threshold is reached.
    #[serde(default = "default_warning_threshold")]
    pub warning_threshold: f64,

    /// Estimated cost per request in cents for budget reservation.
    /// This is reserved before the request is processed to prevent race conditions.
    /// After the request completes, the actual cost replaces the estimate.
    /// Default is 10 cents ($0.10) which is conservative for most models.
    #[serde(default = "default_estimated_cost_cents")]
    pub estimated_cost_cents: i64,
}

impl Default for BudgetDefaults {
    fn default() -> Self {
        Self {
            monthly_budget_usd: None,
            daily_budget_usd: None,
            warning_threshold: default_warning_threshold(),
            estimated_cost_cents: default_estimated_cost_cents(),
        }
    }
}

fn default_estimated_cost_cents() -> i64 {
    10 // $0.10 conservative estimate
}

fn default_warning_threshold() -> f64 {
    0.8 // 80%
}
