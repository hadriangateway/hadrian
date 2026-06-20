//! Model routing module.
//!
//! Parses model strings to determine which provider to use.
//! Model strings can be in the format:
//! - `provider-name/model-name` - Route to a specific static provider
//! - `model-name` - Use default provider
//!
//! Scoped model strings for dynamic providers:
//! - `:org/{ORG}/{PROVIDER}/{MODEL}` - Organization's dynamic provider
//! - `:org/{ORG}/:user/{USER}/{PROVIDER}/{MODEL}` - User's dynamic provider within org
//! - `:org/{ORG}/:project/{PROJECT}/{PROVIDER}/{MODEL}` - Project's dynamic provider within org
//! - `:org/{ORG}/:team/{TEAM}/{PROVIDER}/{MODEL}` - Team's dynamic provider within org
//!
//! For example:
//! - `my-openrouter/anthropic/claude-sonnet-4.5` - Uses "my-openrouter" provider with model "anthropic/claude-sonnet-4.5"
//! - `gpt-4` - Uses default provider with model "gpt-4"
//! - `:org/acme/my-llm/llama3` - Uses "acme" org's "my-llm" dynamic provider with model "llama3"
//! - `:org/acme/:project/frontend/openai/gpt-4` - Uses "frontend" project's "openai" dynamic provider
//! - `:org/acme/:team/eng/my-provider/gpt-4` - Uses "eng" team's "my-provider" dynamic provider

pub mod resolver;

use crate::config::{ProviderConfig, ProvidersConfig};

/// Scope for a dynamic provider lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderScope {
    /// Organization-level dynamic provider
    Organization { org_slug: String },
    /// Project-level dynamic provider (within an org)
    Project {
        org_slug: String,
        project_slug: String,
    },
    /// Team-level dynamic provider (within an org)
    Team { org_slug: String, team_slug: String },
    /// User-level dynamic provider (within an org)
    User { org_slug: String, user_id: String },
}

impl ProviderScope {
    /// Get the organization slug from any scope
    #[allow(dead_code)] // Public API for dynamic provider scope resolution
    pub fn org_slug(&self) -> &str {
        match self {
            Self::Organization { org_slug }
            | Self::Project { org_slug, .. }
            | Self::Team { org_slug, .. }
            | Self::User { org_slug, .. } => org_slug,
        }
    }
}

/// Result of parsing a model string - either static or dynamic provider.
#[derive(Debug, Clone)]
pub enum RoutedProvider<'a> {
    /// Route to a static provider from config
    Static(StaticRoute<'a>),
    /// Route to a dynamic provider (requires database lookup)
    Dynamic(DynamicRoute),
}

/// A route to a static provider from config.
#[derive(Debug, Clone)]
pub struct StaticRoute<'a> {
    /// The provider name to use.
    pub provider_name: &'a str,
    /// The provider configuration.
    pub provider_config: &'a ProviderConfig,
    /// The model name to send to the provider (with the provider prefix stripped).
    pub model: String,
}

/// A route to a dynamic provider (needs database lookup).
#[derive(Debug, Clone)]
pub struct DynamicRoute {
    /// The scope for this dynamic provider
    pub scope: ProviderScope,
    /// The provider name within the scope
    pub provider_name: String,
    /// The model name to send to the provider
    pub model: String,
}

/// Maximum length for a model string.
const MAX_MODEL_STRING_LENGTH: usize = 512;

/// Validate that a model string contains only safe characters and is within length limits.
///
/// Allowed characters: alphanumeric plus the RFC 3986 "unreserved" punctuation (hyphen, dot,
/// underscore, tilde) and the routing separators (slash, colon, at sign). Tildes appear in some
/// provider model slugs, so they must pass through. This prevents injection of control characters
/// or other unexpected content.
fn validate_model_string(model: &str) -> Result<(), RoutingError> {
    if model.is_empty() {
        return Err(RoutingError::NoModel);
    }
    if model.len() > MAX_MODEL_STRING_LENGTH {
        return Err(RoutingError::InvalidModelFormat(format!(
            "Model string exceeds maximum length of {} characters",
            MAX_MODEL_STRING_LENGTH,
        )));
    }
    if !model
        .chars()
        .all(|c| c.is_alphanumeric() || "-._~/:@".contains(c))
    {
        return Err(RoutingError::InvalidModelFormat(
            "Model string contains invalid characters".to_string(),
        ));
    }
    Ok(())
}

/// Error when routing a model.
#[derive(Debug, Clone)]
pub enum RoutingError {
    /// No model specified and no default available.
    NoModel,
    /// The specified provider was not found.
    ProviderNotFound(String),
    /// No default provider configured and no provider specified in a model.
    NoDefaultProvider,
    /// Invalid scope format in model string.
    InvalidScope(String),
    /// Missing required component in scoped model string.
    MissingComponent(String),
    /// Invalid provider configuration (e.g., forbidden credential type).
    Config(String),
    /// Invalid model string format (bad characters or too long).
    InvalidModelFormat(String),
}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoModel => write!(f, "No model specified"),
            Self::ProviderNotFound(name) => write!(f, "Provider '{}' not found", name),
            Self::NoDefaultProvider => write!(f, "No default provider configured"),
            Self::InvalidScope(msg) => write!(f, "Invalid scope: {}", msg),
            Self::MissingComponent(msg) => write!(f, "Missing component: {}", msg),
            Self::Config(msg) => write!(f, "Provider configuration error: {}", msg),
            Self::InvalidModelFormat(msg) => write!(f, "Invalid model format: {}", msg),
        }
    }
}

impl std::error::Error for RoutingError {}

/// Parse a scoped model string like `:org/acme/my-provider/gpt-4`.
///
/// Returns `Some(DynamicRoute)` if the model starts with `:org/` or `:user/`, otherwise `None`.
fn parse_scoped_model(model_str: &str) -> Result<Option<DynamicRoute>, RoutingError> {
    // Handle direct user scope: :user/{USER_ID}/{PROVIDER}/{MODEL}
    if let Some(rest) = model_str.strip_prefix(":user/") {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() < 3 {
            return Err(RoutingError::MissingComponent(
                "User scope requires user_id/provider/model".to_string(),
            ));
        }
        return Ok(Some(DynamicRoute {
            scope: ProviderScope::User {
                org_slug: String::new(),
                user_id: parts[0].to_string(),
            },
            provider_name: parts[1].to_string(),
            model: parts[2].to_string(),
        }));
    }

    // Check for scoped prefix
    if !model_str.starts_with(":org/") {
        return Ok(None);
    }

    // Remove the ":org/" prefix
    let rest = &model_str[5..];
    let parts: Vec<&str> = rest.splitn(4, '/').collect();

    if parts.len() < 3 {
        return Err(RoutingError::MissingComponent(
            "Scoped model requires at least org/provider/model".to_string(),
        ));
    }

    let org_slug = parts[0].to_string();

    // Check if this is a user or project scope
    if parts[1].starts_with(":user/") || parts[1] == ":user" {
        // :org/{ORG}/:user/{USER}/{PROVIDER}/{MODEL}
        // After org_slug, we have ":user" but need to re-parse
        let after_org = &rest[org_slug.len() + 1..]; // Skip "org_slug/"
        if !after_org.starts_with(":user/") {
            return Err(RoutingError::InvalidScope(
                "Expected :user/{USER} after org".to_string(),
            ));
        }
        let user_rest = &after_org[6..]; // Skip ":user/"
        let user_parts: Vec<&str> = user_rest.splitn(3, '/').collect();
        if user_parts.len() < 3 {
            return Err(RoutingError::MissingComponent(
                "User scope requires user_id/provider/model".to_string(),
            ));
        }
        return Ok(Some(DynamicRoute {
            scope: ProviderScope::User {
                org_slug,
                user_id: user_parts[0].to_string(),
            },
            provider_name: user_parts[1].to_string(),
            model: user_parts[2].to_string(),
        }));
    } else if parts[1].starts_with(":project/") || parts[1] == ":project" {
        // :org/{ORG}/:project/{PROJECT}/{PROVIDER}/{MODEL}
        let after_org = &rest[org_slug.len() + 1..];
        if !after_org.starts_with(":project/") {
            return Err(RoutingError::InvalidScope(
                "Expected :project/{PROJECT} after org".to_string(),
            ));
        }
        let project_rest = &after_org[9..]; // Skip ":project/"
        let project_parts: Vec<&str> = project_rest.splitn(3, '/').collect();
        if project_parts.len() < 3 {
            return Err(RoutingError::MissingComponent(
                "Project scope requires project_slug/provider/model".to_string(),
            ));
        }
        return Ok(Some(DynamicRoute {
            scope: ProviderScope::Project {
                org_slug,
                project_slug: project_parts[0].to_string(),
            },
            provider_name: project_parts[1].to_string(),
            model: project_parts[2].to_string(),
        }));
    } else if parts[1].starts_with(":team/") || parts[1] == ":team" {
        // :org/{ORG}/:team/{TEAM}/{PROVIDER}/{MODEL}
        let after_org = &rest[org_slug.len() + 1..];
        if !after_org.starts_with(":team/") {
            return Err(RoutingError::InvalidScope(
                "Expected :team/{TEAM} after org".to_string(),
            ));
        }
        let team_rest = &after_org[6..]; // Skip ":team/"
        let team_parts: Vec<&str> = team_rest.splitn(3, '/').collect();
        if team_parts.len() < 3 {
            return Err(RoutingError::MissingComponent(
                "Team scope requires team_slug/provider/model".to_string(),
            ));
        }
        return Ok(Some(DynamicRoute {
            scope: ProviderScope::Team {
                org_slug,
                team_slug: team_parts[0].to_string(),
            },
            provider_name: team_parts[1].to_string(),
            model: team_parts[2].to_string(),
        }));
    }

    // Just org scope: :org/{ORG}/{PROVIDER}/{MODEL}
    if parts.len() < 3 {
        return Err(RoutingError::MissingComponent(
            "Org scope requires provider/model".to_string(),
        ));
    }

    // Rejoin remaining parts as model (in case model has slashes)
    let model = if parts.len() > 3 {
        format!("{}/{}", parts[2], parts[3])
    } else {
        parts[2].to_string()
    };

    Ok(Some(DynamicRoute {
        scope: ProviderScope::Organization { org_slug },
        provider_name: parts[1].to_string(),
        model,
    }))
}

/// Parse a model string and return either a static or dynamic route.
///
/// Checks for scoped model strings first, then falls back to static provider routing.
pub fn route_model_extended<'a>(
    model: Option<&'a str>,
    providers: &'a ProvidersConfig,
) -> Result<RoutedProvider<'a>, RoutingError> {
    let model_str = model.ok_or(RoutingError::NoModel)?;

    // Validate model string format before any routing
    validate_model_string(model_str)?;

    // Check for scoped/dynamic provider first
    if let Some(dynamic_route) = parse_scoped_model(model_str)? {
        return Ok(RoutedProvider::Dynamic(dynamic_route));
    }

    // Fall back to static provider routing
    let static_route = route_model_static(model_str, providers)?;
    Ok(RoutedProvider::Static(static_route))
}

/// Route to a static provider (internal helper).
fn route_model_static<'a>(
    model_str: &'a str,
    providers: &'a ProvidersConfig,
) -> Result<StaticRoute<'a>, RoutingError> {
    // Try to extract provider prefix
    // Check if first segment (before first /) matches a provider name
    if let Some(slash_pos) = model_str.find('/') {
        let potential_provider = &model_str[..slash_pos];
        let remaining_model = &model_str[slash_pos + 1..];

        // If we found a slash, treat the prefix as an explicit provider reference
        if let Some(config) = providers.get(potential_provider) {
            return Ok(StaticRoute {
                provider_name: potential_provider,
                provider_config: config,
                model: remaining_model.to_string(),
            });
        } else {
            // Explicitly requested provider doesn't exist
            return Err(RoutingError::ProviderNotFound(
                potential_provider.to_string(),
            ));
        }
    }

    // No provider prefix found - use default
    let (provider_name, provider_config) = providers
        .get_default()
        .ok_or(RoutingError::NoDefaultProvider)?;

    Ok(StaticRoute {
        provider_name,
        provider_config,
        model: model_str.to_string(),
    })
}

/// Route from a list of models with dynamic provider support.
/// Falls back to trying each model in order.
pub fn route_models_extended<'a>(
    model: Option<&'a str>,
    models: Option<&'a [String]>,
    providers: &'a ProvidersConfig,
) -> Result<RoutedProvider<'a>, RoutingError> {
    // Surface the *first* error if every candidate fails. The primary model's
    // failure is the most actionable for the caller — fallback errors are a
    // secondary signal.
    let mut first_error: Option<RoutingError> = None;

    if let Some(m) = model {
        match route_model_extended(Some(m), providers) {
            Ok(routed) => return Ok(routed),
            Err(e) => first_error.get_or_insert(e),
        };
    }

    if let Some(model_list) = models {
        for m in model_list {
            match route_model_extended(Some(m.as_str()), providers) {
                Ok(routed) => return Ok(routed),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
    }

    Err(first_error.unwrap_or(RoutingError::NoModel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_providers() -> ProvidersConfig {
        toml::from_str(
            r#"
            default_provider = "openrouter"

            [openrouter]
            type = "open_ai"
            api_key = "sk-or-xxx"
            base_url = "https://openrouter.ai/api/v1"

            [anthropic-direct]
            type = "anthropic"
            api_key = "sk-ant-xxx"

            [local]
            type = "open_ai"
            base_url = "http://localhost:11434/v1"
        "#,
        )
        .unwrap()
    }

    // Tests for scoped/dynamic routing

    #[test]
    fn test_parse_org_scoped_model() {
        let result = parse_scoped_model(":org/acme/my-provider/gpt-4").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::Organization {
                org_slug: "acme".to_string()
            }
        );
        assert_eq!(route.provider_name, "my-provider");
        assert_eq!(route.model, "gpt-4");
    }

    #[test]
    fn test_parse_org_scoped_model_with_model_slash() {
        let result = parse_scoped_model(":org/acme/openrouter/anthropic/claude-3").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::Organization {
                org_slug: "acme".to_string()
            }
        );
        assert_eq!(route.provider_name, "openrouter");
        assert_eq!(route.model, "anthropic/claude-3");
    }

    #[test]
    fn test_parse_project_scoped_model() {
        let result = parse_scoped_model(":org/acme/:project/frontend/my-provider/gpt-4").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::Project {
                org_slug: "acme".to_string(),
                project_slug: "frontend".to_string()
            }
        );
        assert_eq!(route.provider_name, "my-provider");
        assert_eq!(route.model, "gpt-4");
    }

    #[test]
    fn test_parse_user_scoped_model() {
        let result = parse_scoped_model(":org/acme/:user/user-123/my-provider/gpt-4").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::User {
                org_slug: "acme".to_string(),
                user_id: "user-123".to_string()
            }
        );
        assert_eq!(route.provider_name, "my-provider");
        assert_eq!(route.model, "gpt-4");
    }

    #[test]
    fn test_parse_direct_user_scoped_model() {
        let result =
            parse_scoped_model(":user/550e8400-e29b-41d4-a716-446655440000/my-provider/gpt-4")
                .unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::User {
                org_slug: String::new(),
                user_id: "550e8400-e29b-41d4-a716-446655440000".to_string()
            }
        );
        assert_eq!(route.provider_name, "my-provider");
        assert_eq!(route.model, "gpt-4");
    }

    #[test]
    fn test_non_scoped_model_returns_none() {
        let result = parse_scoped_model("openrouter/gpt-4").unwrap();
        assert!(result.is_none());

        let result = parse_scoped_model("gpt-4").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_validate_model_string_allows_unreserved_chars() {
        // RFC 3986 unreserved punctuation plus routing separators.
        validate_model_string("openrouter/~openai/gpt-latest").unwrap();
        validate_model_string("anthropic/claude-3.5-sonnet:beta").unwrap();
        validate_model_string(":org/acme/my-provider/gpt-4").unwrap();
        validate_model_string("vendor/model_name@v1").unwrap();
    }

    #[test]
    fn test_validate_model_string_rejects_unsafe_chars() {
        // Control characters and other unexpected content stay rejected.
        for bad in ["gpt 4", "gpt\n4", "gpt\t4", "model;rm", "model$(x)", "a*b"] {
            assert!(
                matches!(
                    validate_model_string(bad),
                    Err(RoutingError::InvalidModelFormat(_))
                ),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn test_route_model_extended_static() {
        let providers = make_test_providers();
        let result = route_model_extended(Some("openrouter/gpt-4"), &providers).unwrap();
        match result {
            RoutedProvider::Static(route) => {
                assert_eq!(route.provider_name, "openrouter");
                assert_eq!(route.model, "gpt-4");
            }
            RoutedProvider::Dynamic(_) => panic!("Expected static route"),
        }
    }

    #[test]
    fn test_route_model_extended_dynamic() {
        let providers = make_test_providers();
        let result = route_model_extended(Some(":org/acme/my-llm/llama3"), &providers).unwrap();
        match result {
            RoutedProvider::Static(_) => panic!("Expected dynamic route"),
            RoutedProvider::Dynamic(route) => {
                assert_eq!(
                    route.scope,
                    ProviderScope::Organization {
                        org_slug: "acme".to_string()
                    }
                );
                assert_eq!(route.provider_name, "my-llm");
                assert_eq!(route.model, "llama3");
            }
        }
    }

    #[test]
    fn test_parse_team_scoped_model() {
        let result = parse_scoped_model(":org/acme/:team/eng/my-provider/gpt-4").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::Team {
                org_slug: "acme".to_string(),
                team_slug: "eng".to_string()
            }
        );
        assert_eq!(route.provider_name, "my-provider");
        assert_eq!(route.model, "gpt-4");
    }

    #[test]
    fn test_parse_team_scoped_model_with_model_slash() {
        let result =
            parse_scoped_model(":org/acme/:team/eng/openrouter/anthropic/claude-3").unwrap();
        assert!(result.is_some());
        let route = result.unwrap();
        assert_eq!(
            route.scope,
            ProviderScope::Team {
                org_slug: "acme".to_string(),
                team_slug: "eng".to_string()
            }
        );
        assert_eq!(route.provider_name, "openrouter");
        assert_eq!(route.model, "anthropic/claude-3");
    }

    #[test]
    fn test_invalid_scope_missing_components() {
        let result = parse_scoped_model(":org/acme");
        assert!(matches!(result, Err(RoutingError::MissingComponent(_))));

        let result = parse_scoped_model(":org/acme/provider");
        assert!(matches!(result, Err(RoutingError::MissingComponent(_))));
    }
}
