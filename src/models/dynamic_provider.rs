use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::config::sovereignty::SovereigntyMetadata;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DynamicProvider {
    pub id: Uuid,
    pub name: String,
    pub owner: ProviderOwner,
    /// Provider type (e.g., "open_ai", "anthropic", "bedrock", "vertex", "gemini")
    pub provider_type: String,
    pub base_url: String,
    /// Reference to API key in secrets manager (or literal key if no SM configured)
    pub api_key_secret_ref: Option<String>,
    /// Provider-specific configuration (e.g., region, credentials for Bedrock/Vertex)
    pub config: Option<serde_json::Value>,
    /// List of supported model names
    pub models: Vec<String>,
    /// Sovereignty and compliance metadata
    pub sovereignty: Option<SovereigntyMetadata>,
    pub is_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Response DTO for dynamic providers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct DynamicProviderResponse {
    pub id: Uuid,
    pub name: String,
    pub owner: ProviderOwner,
    pub provider_type: String,
    pub base_url: String,
    /// Whether this provider has an API key configured
    pub has_api_key: bool,
    /// Provider-specific configuration
    pub config: Option<serde_json::Value>,
    pub models: Vec<String>,
    /// Sovereignty and compliance metadata
    pub sovereignty: Option<SovereigntyMetadata>,
    pub is_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Keys in provider config JSON that may contain credentials and must be redacted.
const SENSITIVE_CONFIG_KEYS: &[&str] = &[
    "access_key_id",
    "secret_access_key",
    "session_token",
    "api_key",
    "json",
];

/// Recursively redact sensitive keys from a JSON config value.
fn redact_config_credentials(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let redacted = map
                .into_iter()
                .map(|(k, v)| {
                    let is_sensitive =
                        SENSITIVE_CONFIG_KEYS.contains(&k.as_str()) || k.ends_with("_ref");
                    if is_sensitive && !v.is_null() {
                        (k, serde_json::Value::String("**REDACTED**".to_string()))
                    } else {
                        (k, redact_config_credentials(v))
                    }
                })
                .collect();
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(redact_config_credentials).collect())
        }
        other => other,
    }
}

impl From<DynamicProvider> for DynamicProviderResponse {
    fn from(p: DynamicProvider) -> Self {
        Self {
            id: p.id,
            name: p.name,
            owner: p.owner,
            provider_type: p.provider_type,
            base_url: p.base_url,
            has_api_key: p.api_key_secret_ref.is_some(),
            config: p.config.map(redact_config_credentials),
            models: p.models,
            sovereignty: p.sovereignty,
            is_enabled: p.is_enabled,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// Result of a provider connectivity test
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ConnectivityTestResponse {
    /// Whether the test was successful
    pub status: String,
    /// Human-readable message
    pub message: String,
    /// Latency in milliseconds
    pub latency_ms: Option<u64>,
}

/// Owner of a dynamic provider
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderOwner {
    Organization { org_id: Uuid },
    Team { team_id: Uuid },
    Project { project_id: Uuid },
    User { user_id: Uuid },
}

impl ProviderOwner {
    /// Returns the scope label and owner ID for constructing secret manager paths.
    pub fn secret_namespace(&self) -> (&'static str, Uuid) {
        match self {
            ProviderOwner::Organization { org_id } => ("organization", *org_id),
            ProviderOwner::Team { team_id } => ("team", *team_id),
            ProviderOwner::Project { project_id } => ("project", *project_id),
            ProviderOwner::User { user_id } => ("user", *user_id),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateDynamicProvider {
    #[validate(length(min = 1, max = 64))]
    pub name: String,
    pub owner: ProviderOwner,
    /// Provider type (e.g., "open_ai", "anthropic", "bedrock", "vertex", "gemini")
    #[validate(length(min = 1, max = 64))]
    pub provider_type: String,
    /// Base URL for the provider (required for OpenAI/Anthropic/Azure, optional for Bedrock/Vertex)
    #[serde(default)]
    pub base_url: String,
    /// Raw API key (stored in secrets manager if available, otherwise stored directly)
    pub api_key: Option<String>,
    /// Provider-specific configuration (e.g., region, credentials for Bedrock/Vertex)
    pub config: Option<serde_json::Value>,
    /// List of supported model names
    pub models: Option<Vec<String>>,
    /// Sovereignty and compliance metadata
    pub sovereignty: Option<SovereigntyMetadata>,
}

/// Self-service create DTO (owner is inferred from the authenticated user)
#[derive(Debug, Clone, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CreateSelfServiceProvider {
    #[validate(length(min = 1, max = 64))]
    pub name: String,
    /// Provider type (e.g., "openai", "anthropic", "bedrock", "vertex", "gemini")
    #[validate(length(min = 1, max = 64))]
    pub provider_type: String,
    /// Base URL for the provider (required for OpenAI/Anthropic/Azure, optional for Bedrock/Vertex)
    #[serde(default)]
    pub base_url: String,
    /// Raw API key (stored in secrets manager if available, otherwise stored directly)
    pub api_key: Option<String>,
    /// Provider-specific configuration (e.g., region, credentials for Bedrock/Vertex)
    pub config: Option<serde_json::Value>,
    /// List of supported model names
    pub models: Option<Vec<String>>,
    /// Sovereignty and compliance metadata
    pub sovereignty: Option<SovereigntyMetadata>,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct UpdateDynamicProvider {
    pub base_url: Option<String>,
    /// Raw API key (stored in secrets manager if available, otherwise stored directly)
    pub api_key: Option<String>,
    /// Provider-specific configuration (e.g., region, credentials for Bedrock/Vertex)
    pub config: Option<serde_json::Value>,
    /// List of supported model names
    pub models: Option<Vec<String>>,
    /// Sovereignty and compliance metadata (set to null to clear)
    #[serde(default, deserialize_with = "deserialize_optional_sovereignty")]
    pub sovereignty: Option<Option<SovereigntyMetadata>>,
    pub is_enabled: Option<bool>,
}

/// Custom deserializer: absent → None (don't update), null → Some(None) (clear), value → Some(Some(v)) (set).
fn deserialize_optional_sovereignty<'de, D>(
    deserializer: D,
) -> Result<Option<Option<SovereigntyMetadata>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_redact_config_credentials_flat() {
        let config = json!({
            "region": "us-east-1",
            "access_key_id": "AKIAIOSFODNN7EXAMPLE",
            "secret_access_key": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        });
        let redacted = redact_config_credentials(config);
        assert_eq!(redacted["region"], "us-east-1");
        assert_eq!(redacted["access_key_id"], "**REDACTED**");
        assert_eq!(redacted["secret_access_key"], "**REDACTED**");
    }

    #[test]
    fn test_redact_config_credentials_nested() {
        let config = json!({
            "credentials": {
                "type": "static",
                "access_key_id": "AKID",
                "secret_access_key": "SECRET",
                "session_token": "TOKEN"
            },
            "region": "us-west-2"
        });
        let redacted = redact_config_credentials(config);
        assert_eq!(redacted["region"], "us-west-2");
        assert_eq!(redacted["credentials"]["type"], "static");
        assert_eq!(redacted["credentials"]["access_key_id"], "**REDACTED**");
        assert_eq!(redacted["credentials"]["secret_access_key"], "**REDACTED**");
        assert_eq!(redacted["credentials"]["session_token"], "**REDACTED**");
    }

    #[test]
    fn test_redact_config_credentials_ref_suffix() {
        let config = json!({
            "api_key_ref": "vault://secret/key",
            "safe_field": "visible"
        });
        let redacted = redact_config_credentials(config);
        assert_eq!(redacted["api_key_ref"], "**REDACTED**");
        assert_eq!(redacted["safe_field"], "visible");
    }

    #[test]
    fn test_redact_preserves_null() {
        let config = json!({
            "api_key": null,
            "region": "eu-west-1"
        });
        let redacted = redact_config_credentials(config);
        assert!(redacted["api_key"].is_null());
        assert_eq!(redacted["region"], "eu-west-1");
    }
}
