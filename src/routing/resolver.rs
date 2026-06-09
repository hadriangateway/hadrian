//! Dynamic provider resolution.
//!
//! Resolves DynamicRoute structs to actual provider configurations by
//! looking them up in the database and resolving secrets.

use std::{sync::Arc, time::Duration};

use uuid::Uuid;

use super::{DynamicRoute, ProviderScope, RoutedProvider, RoutingError};
use crate::{
    auth::AuthenticatedRequest,
    cache::{Cache, CacheKeys},
    config::ProviderConfig,
    db::DbPool,
    models::{DynamicProvider, ProviderOwner},
    observability::metrics,
    secrets::SecretManager,
};

/// A resolved dynamic provider with its configuration.
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    /// The provider name
    pub provider_name: String,
    /// The resolved provider configuration
    pub provider_config: ProviderConfig,
    /// The model name to send to the provider
    pub model: String,
}

/// Resolve a dynamic route to a provider configuration.
///
/// This looks up the provider in the database, resolves the API key secret,
/// and converts it to a ProviderConfig.
#[tracing::instrument(
    skip(db, cache, secrets, auth),
    fields(
        provider_name = %route.provider_name,
        model = %route.model,
        scope = ?route.scope
    )
)]
#[allow(clippy::collapsible_if)]
pub async fn resolve_dynamic_provider(
    route: &DynamicRoute,
    db: &Arc<DbPool>,
    cache: Option<&Arc<dyn Cache>>,
    secrets: Option<&Arc<dyn SecretManager>>,
    auth: Option<&AuthenticatedRequest>,
) -> Result<ResolvedProvider, RoutingError> {
    // Try cache first if available
    if let Some(cache) = cache {
        if let Ok(cached_provider) = get_cached_provider(route, cache).await {
            // Verify access even for cached providers to prevent cross-user access
            verify_provider_access(&cached_provider.owner, auth, db, Some(cache)).await?;
            let provider_config = dynamic_provider_to_config(&cached_provider, secrets).await?;
            return Ok(ResolvedProvider {
                provider_name: cached_provider.name.clone(),
                provider_config,
                model: route.model.clone(),
            });
        }
    }

    // Look up the provider in the database based on scope
    let provider = match &route.scope {
        ProviderScope::Organization { org_slug } => {
            // Get org ID from slug
            let org = db
                .organizations()
                .get_by_slug(org_slug)
                .await
                .map_err(|e| RoutingError::InvalidScope(format!("Failed to lookup org: {}", e)))?
                .ok_or_else(|| {
                    RoutingError::InvalidScope(format!("Organization '{}' not found", org_slug))
                })?;

            // Get provider by name
            db.providers()
                .get_by_name(
                    &ProviderOwner::Organization { org_id: org.id },
                    &route.provider_name,
                )
                .await
                .map_err(|e| {
                    RoutingError::ProviderNotFound(format!("Failed to lookup provider: {}", e))
                })?
                .ok_or_else(|| {
                    RoutingError::ProviderNotFound(format!(
                        "Provider '{}' not found for org '{}'",
                        route.provider_name, org_slug
                    ))
                })?
        }

        ProviderScope::Project {
            org_slug,
            project_slug,
        } => {
            // Get org ID from slug
            let org = db
                .organizations()
                .get_by_slug(org_slug)
                .await
                .map_err(|e| RoutingError::InvalidScope(format!("Failed to lookup org: {}", e)))?
                .ok_or_else(|| {
                    RoutingError::InvalidScope(format!("Organization '{}' not found", org_slug))
                })?;

            // Get project ID from slug
            let project = db
                .projects()
                .get_by_slug(org.id, project_slug)
                .await
                .map_err(|e| {
                    RoutingError::InvalidScope(format!("Failed to lookup project: {}", e))
                })?
                .ok_or_else(|| {
                    RoutingError::InvalidScope(format!(
                        "Project '{}' not found in org '{}'",
                        project_slug, org_slug
                    ))
                })?;

            // Get provider by name
            db.providers()
                .get_by_name(
                    &ProviderOwner::Project {
                        project_id: project.id,
                    },
                    &route.provider_name,
                )
                .await
                .map_err(|e| {
                    RoutingError::ProviderNotFound(format!("Failed to lookup provider: {}", e))
                })?
                .ok_or_else(|| {
                    RoutingError::ProviderNotFound(format!(
                        "Provider '{}' not found for project '{}'",
                        route.provider_name, project_slug
                    ))
                })?
        }

        ProviderScope::Team {
            org_slug,
            team_slug,
        } => {
            // Get org ID from slug
            let org = db
                .organizations()
                .get_by_slug(org_slug)
                .await
                .map_err(|e| RoutingError::InvalidScope(format!("Failed to lookup org: {}", e)))?
                .ok_or_else(|| {
                    RoutingError::InvalidScope(format!("Organization '{}' not found", org_slug))
                })?;

            // Get team by slug within org
            let team = db
                .teams()
                .get_by_slug(org.id, team_slug)
                .await
                .map_err(|e| RoutingError::InvalidScope(format!("Failed to lookup team: {}", e)))?
                .ok_or_else(|| {
                    RoutingError::InvalidScope(format!(
                        "Team '{}' not found in org '{}'",
                        team_slug, org_slug
                    ))
                })?;

            // Get provider by name
            db.providers()
                .get_by_name(
                    &ProviderOwner::Team { team_id: team.id },
                    &route.provider_name,
                )
                .await
                .map_err(|e| {
                    RoutingError::ProviderNotFound(format!("Failed to lookup provider: {}", e))
                })?
                .ok_or_else(|| {
                    RoutingError::ProviderNotFound(format!(
                        "Provider '{}' not found for team '{}'",
                        route.provider_name, team_slug
                    ))
                })?
        }

        ProviderScope::User {
            org_slug: _,
            user_id,
        } => {
            // Parse user_id as UUID or external_id
            let user = if let Ok(uuid) = user_id.parse() {
                db.users().get_by_id(uuid).await.map_err(|e| {
                    RoutingError::InvalidScope(format!("Failed to lookup user: {}", e))
                })?
            } else {
                // Try as external_id
                db.users().get_by_external_id(user_id).await.map_err(|e| {
                    RoutingError::InvalidScope(format!("Failed to lookup user: {}", e))
                })?
            }
            .ok_or_else(|| RoutingError::InvalidScope(format!("User '{}' not found", user_id)))?;

            // Get provider by name
            db.providers()
                .get_by_name(
                    &ProviderOwner::User { user_id: user.id },
                    &route.provider_name,
                )
                .await
                .map_err(|e| {
                    RoutingError::ProviderNotFound(format!("Failed to lookup provider: {}", e))
                })?
                .ok_or_else(|| {
                    RoutingError::ProviderNotFound(format!(
                        "Provider '{}' not found for user '{}'",
                        route.provider_name, user_id
                    ))
                })?
        }
    };

    // Check if provider is enabled
    if !provider.is_enabled {
        return Err(RoutingError::ProviderNotFound(format!(
            "Provider '{}' is disabled",
            route.provider_name
        )));
    }

    // Verify the requesting principal has access to this provider's scope
    verify_provider_access(&provider.owner, auth, db, cache).await?;

    // Cache the provider for future requests (10 minute TTL)
    if let Some(cache) = cache {
        let _ = cache_provider(route, &provider, cache).await;
    }

    // Convert DynamicProvider to ProviderConfig, resolving secrets
    let provider_config = dynamic_provider_to_config(&provider, secrets).await?;

    Ok(ResolvedProvider {
        provider_name: provider.name.clone(),
        provider_config,
        model: route.model.clone(),
    })
}

/// Verify that the requesting principal has access to a provider's scope.
///
/// - User scope: requesting user's ID must match the provider's owner
/// - Org scope: requesting user must belong to the org
/// - Project scope: requesting user must belong to the project's org
/// - Team scope: requesting user must belong to the team's org
async fn verify_provider_access(
    owner: &ProviderOwner,
    auth: Option<&AuthenticatedRequest>,
    db: &Arc<DbPool>,
    cache: Option<&Arc<dyn Cache>>,
) -> Result<(), RoutingError> {
    // If no auth, there's no principal to check — the API middleware will enforce
    // auth requirements separately. We only gate dynamic providers here.
    let Some(auth) = auth else {
        return Ok(());
    };

    match owner {
        ProviderOwner::User { user_id } => {
            let requesting_user = auth.user_id();
            if requesting_user != Some(*user_id) {
                return Err(RoutingError::ProviderNotFound(
                    "Provider not found".to_string(),
                ));
            }
        }
        ProviderOwner::Organization { org_id } => {
            if !user_has_org_access(auth, *org_id, db, cache).await {
                return Err(RoutingError::ProviderNotFound(
                    "Provider not found".to_string(),
                ));
            }
        }
        ProviderOwner::Project { project_id } => {
            // Look up the project's org, then check org membership
            if let Ok(Some(project)) = db.projects().get_by_id(*project_id).await {
                if !user_has_org_access(auth, project.org_id, db, cache).await {
                    return Err(RoutingError::ProviderNotFound(
                        "Provider not found".to_string(),
                    ));
                }
            } else {
                return Err(RoutingError::ProviderNotFound(
                    "Provider not found".to_string(),
                ));
            }
        }
        ProviderOwner::Team { team_id } => {
            // Look up the team's org, then check org membership
            if let Ok(Some(team)) = db.teams().get_by_id(*team_id).await {
                if !user_has_org_access(auth, team.org_id, db, cache).await {
                    return Err(RoutingError::ProviderNotFound(
                        "Provider not found".to_string(),
                    ));
                }
            } else {
                return Err(RoutingError::ProviderNotFound(
                    "Provider not found".to_string(),
                ));
            }
        }
    }

    Ok(())
}

/// Check if the authenticated user belongs to the given organization.
async fn user_has_org_access(
    auth: &AuthenticatedRequest,
    org_id: Uuid,
    db: &Arc<DbPool>,
    cache: Option<&Arc<dyn Cache>>,
) -> bool {
    // Fast path: API key is scoped to this org
    if let Some(api_key) = auth.api_key()
        && api_key.org_id == Some(org_id)
    {
        return true;
    }

    let Some(user_id) = auth.user_id() else {
        return false;
    };

    // Check cache first
    if let Some(cache) = cache {
        let key = CacheKeys::org_access(user_id, org_id);
        if let Ok(Some(bytes)) = cache.get_bytes(&key).await {
            return bytes.first() == Some(&1);
        }
    }

    // Check org membership via database
    let has_access = db
        .users()
        .get_org_memberships_for_user(user_id)
        .await
        .map(|memberships| memberships.iter().any(|m| m.org_id == org_id))
        .unwrap_or(false);

    // Cache result with 5-minute TTL
    if let Some(cache) = cache {
        let key = CacheKeys::org_access(user_id, org_id);
        let _ = cache
            .set_bytes(&key, &[u8::from(has_access)], Duration::from_secs(300))
            .await;
    }

    has_access
}

#[allow(clippy::collapsible_if)]
async fn get_cached_provider(
    route: &DynamicRoute,
    cache: &Arc<dyn Cache>,
) -> Result<DynamicProvider, RoutingError> {
    let (scope_str, scope_id) = match &route.scope {
        ProviderScope::Organization { org_slug } => ("org", org_slug.clone()),
        ProviderScope::Project {
            org_slug,
            project_slug,
        } => ("project", format!("{}:{}", org_slug, project_slug)),
        ProviderScope::Team {
            org_slug,
            team_slug,
        } => ("team", format!("{}:{}", org_slug, team_slug)),
        ProviderScope::User { org_slug, user_id } => ("user", format!("{}:{}", org_slug, user_id)),
    };

    let cache_key = CacheKeys::dynamic_provider(scope_str, &scope_id, &route.provider_name);

    match cache.get_bytes(&cache_key).await {
        Ok(Some(bytes)) => {
            if let Ok(provider) = serde_json::from_slice::<DynamicProvider>(&bytes) {
                metrics::record_cache_operation("provider_config", "get", "hit");
                return Ok(provider);
            }
            // Deserialization failed - treat as miss
            metrics::record_cache_operation("provider_config", "get", "miss");
        }
        Ok(None) => {
            metrics::record_cache_operation("provider_config", "get", "miss");
        }
        Err(_) => {
            metrics::record_cache_operation("provider_config", "get", "error");
        }
    }

    Err(RoutingError::ProviderNotFound(
        "Not found in cache".to_string(),
    ))
}

async fn cache_provider(
    route: &DynamicRoute,
    provider: &DynamicProvider,
    cache: &Arc<dyn Cache>,
) -> Result<(), RoutingError> {
    let (scope_str, scope_id) = match &route.scope {
        ProviderScope::Organization { org_slug } => ("org", org_slug.clone()),
        ProviderScope::Project {
            org_slug,
            project_slug,
        } => ("project", format!("{}:{}", org_slug, project_slug)),
        ProviderScope::Team {
            org_slug,
            team_slug,
        } => ("team", format!("{}:{}", org_slug, team_slug)),
        ProviderScope::User { org_slug, user_id } => ("user", format!("{}:{}", org_slug, user_id)),
    };

    let cache_key = CacheKeys::dynamic_provider(scope_str, &scope_id, &route.provider_name);

    if let Ok(bytes) = serde_json::to_vec(provider) {
        match cache
            .set_bytes(&cache_key, &bytes, Duration::from_secs(600))
            .await
        {
            Ok(_) => metrics::record_cache_operation("provider_config", "set", "success"),
            Err(_) => metrics::record_cache_operation("provider_config", "set", "error"),
        }
    }

    Ok(())
}

/// Resolve a secret reference to an actual secret value.
///
/// Secret references can be:
/// - A literal value (used as-is ONLY when no secret manager is configured)
/// - A key to look up in the secret manager (e.g., "providers/acme/openai-key")
///
/// When a secret manager IS configured, ALL refs must resolve through it.
/// This prevents secret references from being silently treated as literal API keys
/// if the secret manager is temporarily unavailable or the key was deleted.
async fn resolve_secret(
    secret_ref: Option<&str>,
    secrets: Option<&Arc<dyn SecretManager>>,
) -> Result<Option<String>, RoutingError> {
    let secret_ref = match secret_ref {
        Some(r) => r,
        None => return Ok(None),
    };

    // If we have a secret manager, ALL refs must resolve through it
    if let Some(secrets) = secrets {
        match secrets.get(secret_ref).await {
            Ok(Some(value)) => return Ok(Some(value)),
            Ok(None) => {
                tracing::warn!(secret_ref, "Secret not found in secret manager");
                return Err(RoutingError::Config(
                    "Secret not found in secret manager".to_string(),
                ));
            }
            Err(e) => {
                tracing::warn!(secret_ref, error = %e, "Failed to resolve secret");
                return Err(RoutingError::Config(
                    "Failed to resolve secret from secret manager".to_string(),
                ));
            }
        }
    }

    // No secret manager configured — use the reference as a literal value
    Ok(Some(secret_ref.to_string()))
}

/// Extract a string value from a JSON config object.
#[cfg(any(feature = "provider-bedrock", feature = "provider-vertex"))]
fn config_str(config: Option<&serde_json::Value>, key: &str) -> Option<String> {
    config
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract a required string value from a JSON config object.
#[cfg(any(feature = "provider-bedrock", feature = "provider-vertex"))]
fn config_str_required(
    config: Option<&serde_json::Value>,
    key: &str,
    provider_type: &str,
) -> Result<String, RoutingError> {
    config_str(config, key).ok_or_else(|| {
        RoutingError::InvalidScope(format!(
            "{} provider config requires '{}'",
            provider_type, key
        ))
    })
}

/// Build AWS credentials from config JSON.
///
/// Only `static` credentials are allowed for dynamic providers. Other types
/// (default, profile, assume_role) would source from the server's environment.
#[cfg(feature = "provider-bedrock")]
fn parse_aws_credentials(
    config: Option<&serde_json::Value>,
) -> Result<crate::config::AwsCredentials, RoutingError> {
    let creds = config.and_then(|c| c.get("credentials"));
    let cred_type = creds
        .and_then(|c| c.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("static");

    match cred_type {
        "static" => {
            let access_key = creds
                .and_then(|c| c.get("access_key_id"))
                .or_else(|| creds.and_then(|c| c.get("access_key_id_ref")))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let secret_key = creds
                .and_then(|c| c.get("secret_access_key"))
                .or_else(|| creds.and_then(|c| c.get("secret_access_key_ref")))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let session_token = creds
                .and_then(|c| c.get("session_token"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(crate::config::AwsCredentials::Static {
                access_key_id: access_key,
                secret_access_key: secret_key,
                session_token,
            })
        }
        other => Err(RoutingError::Config(format!(
            "Dynamic providers cannot use AWS credential type '{other}' \
             (sources from server environment). Use 'static' credentials instead."
        ))),
    }
}

/// Build GCP credentials from config JSON.
///
/// Only `service_account_json` is allowed for dynamic providers. Other types
/// (default, service_account) would source from the server's environment or filesystem.
#[cfg(feature = "provider-vertex")]
fn parse_gcp_credentials(
    config: Option<&serde_json::Value>,
) -> Result<crate::config::GcpCredentials, RoutingError> {
    let creds = config.and_then(|c| c.get("credentials"));
    let cred_type = creds
        .and_then(|c| c.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("service_account_json");

    match cred_type {
        "service_account_json" => {
            let json = creds
                .and_then(|c| c.get("json"))
                .or_else(|| creds.and_then(|c| c.get("json_ref")))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Ok(crate::config::GcpCredentials::ServiceAccountJson { json })
        }
        other => Err(RoutingError::Config(format!(
            "Dynamic providers cannot use GCP credential type '{other}' \
             (sources from server environment). Use 'service_account_json' \
             or API key mode instead."
        ))),
    }
}

/// Convert a DynamicProvider from the database to a ProviderConfig.
///
/// Resolves `api_key_secret_ref` through the secret manager if available.
pub async fn dynamic_provider_to_config(
    provider: &DynamicProvider,
    secrets: Option<&Arc<dyn SecretManager>>,
) -> Result<ProviderConfig, RoutingError> {
    // Resolve the API key secret
    let api_key = resolve_secret(provider.api_key_secret_ref.as_deref(), secrets).await?;

    match provider.provider_type.as_str() {
        "openai" | "open_ai" | "openai_compatible" => Ok(ProviderConfig::OpenAi(
            crate::config::OpenAiProviderConfig {
                base_url: provider.base_url.clone(),
                api_key,
                organization: None,
                project: None,
                timeout_secs: 60,
                allowed_models: provider.models.clone(),
                model_aliases: std::collections::HashMap::new(),
                headers: std::collections::HashMap::new(),
                supports_tools: false,
                supports_vision: false,
                models: std::collections::HashMap::new(),
                retry: Default::default(),
                circuit_breaker: Default::default(),
                fallback_providers: Vec::new(),
                model_fallbacks: std::collections::HashMap::new(),
                health_check: Default::default(),
                catalog_provider: None,
                sovereignty: provider.sovereignty.clone(),
            },
        )),
        "anthropic" => Ok(ProviderConfig::Anthropic(
            crate::config::AnthropicProviderConfig {
                api_key: api_key.unwrap_or_default(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.anthropic.com".to_string()
                } else {
                    provider.base_url.clone()
                },
                timeout_secs: 60,
                default_model: None,
                default_max_tokens: None,
                allowed_models: provider.models.clone(),
                model_aliases: std::collections::HashMap::new(),
                models: std::collections::HashMap::new(),
                retry: Default::default(),
                circuit_breaker: Default::default(),
                streaming_buffer: Default::default(),
                fallback_providers: Vec::new(),
                model_fallbacks: std::collections::HashMap::new(),
                health_check: Default::default(),
                catalog_provider: None,
                sovereignty: provider.sovereignty.clone(),
                interleaved_thinking_models: crate::config::default_interleaved_thinking_models(),
                adaptive_thinking_models: crate::config::default_adaptive_thinking_models(),
                strict_thinking_models: crate::config::default_strict_thinking_models(),
            },
        )),
        #[cfg(feature = "provider-azure")]
        "azure_openai" | "azure_open_ai" => {
            // For Azure, the base_url is used as the resource name
            // (e.g., "https://myresource.openai.azure.com" → resource_name = "myresource")
            let resource_name = provider
                .base_url
                .trim_start_matches("https://")
                .trim_end_matches(".openai.azure.com")
                .trim_end_matches(".openai.azure.com/")
                .to_string();

            Ok(ProviderConfig::AzureOpenAi(
                crate::config::AzureOpenAiProviderConfig {
                    resource_name,
                    api_version: "2024-02-01".to_string(),
                    auth: crate::config::AzureAuth::ApiKey {
                        api_key: api_key.unwrap_or_default(),
                    },
                    timeout_secs: 60,
                    deployments: std::collections::HashMap::new(),
                    allowed_models: provider.models.clone(),
                    model_aliases: std::collections::HashMap::new(),
                    models: std::collections::HashMap::new(),
                    retry: Default::default(),
                    circuit_breaker: Default::default(),
                    fallback_providers: Vec::new(),
                    model_fallbacks: std::collections::HashMap::new(),
                    health_check: Default::default(),
                    catalog_provider: None,
                    sovereignty: provider.sovereignty.clone(),
                },
            ))
        }
        #[cfg(feature = "provider-bedrock")]
        "bedrock" => {
            let config = provider.config.as_ref();
            // Resolve secrets within config credentials
            let mut resolved_config = config.cloned();
            if let Some(ref mut cfg) = resolved_config
                && let Some(creds) = cfg.get("credentials").cloned()
            {
                let cred_type = creds
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                if cred_type == "static" {
                    // Resolve access_key_id_ref and secret_access_key_ref
                    let mut creds_obj = creds.as_object().cloned().unwrap_or_default();
                    if let Some(ref_val) = creds.get("access_key_id_ref").and_then(|v| v.as_str())
                        && let Ok(Some(resolved)) = resolve_secret(Some(ref_val), secrets).await
                    {
                        creds_obj.insert(
                            "access_key_id".to_string(),
                            serde_json::Value::String(resolved),
                        );
                    }
                    if let Some(ref_val) =
                        creds.get("secret_access_key_ref").and_then(|v| v.as_str())
                        && let Ok(Some(resolved)) = resolve_secret(Some(ref_val), secrets).await
                    {
                        creds_obj.insert(
                            "secret_access_key".to_string(),
                            serde_json::Value::String(resolved),
                        );
                    }
                    cfg["credentials"] = serde_json::Value::Object(creds_obj);
                }
            }

            let region = config_str_required(config, "region", "Bedrock")?;
            let credentials = parse_aws_credentials(resolved_config.as_ref())?;
            let inference_profile_arn = config_str(config, "inference_profile_arn");
            let converse_base_url = config_str(config, "converse_base_url");

            Ok(ProviderConfig::Bedrock(
                crate::config::BedrockProviderConfig {
                    region,
                    credentials,
                    timeout_secs: 60,
                    allowed_models: provider.models.clone(),
                    model_aliases: std::collections::HashMap::new(),
                    inference_profile_arn,
                    models: std::collections::HashMap::new(),
                    retry: Default::default(),
                    circuit_breaker: Default::default(),
                    streaming_buffer: Default::default(),
                    fallback_providers: Vec::new(),
                    model_fallbacks: std::collections::HashMap::new(),
                    converse_base_url,
                    health_check: Default::default(),
                    catalog_provider: None,
                    sovereignty: provider.sovereignty.clone(),
                    interleaved_thinking_models: crate::config::default_interleaved_thinking_models(
                    ),
                },
            ))
        }
        #[cfg(feature = "provider-vertex")]
        "vertex" => {
            let config = provider.config.as_ref();
            if api_key.is_some() {
                // API key mode — simple Gemini access
                let publisher =
                    config_str(config, "publisher").unwrap_or_else(|| "google".to_string());
                let base_url = config_str(config, "base_url");

                Ok(ProviderConfig::Vertex(
                    crate::config::VertexProviderConfig {
                        api_key,
                        project: None,
                        region: None,
                        publisher,
                        base_url,
                        credentials: crate::config::GcpCredentials::Default,
                        timeout_secs: 60,
                        allowed_models: provider.models.clone(),
                        model_aliases: std::collections::HashMap::new(),
                        models: std::collections::HashMap::new(),
                        retry: Default::default(),
                        circuit_breaker: Default::default(),
                        streaming_buffer: Default::default(),
                        fallback_providers: Vec::new(),
                        model_fallbacks: std::collections::HashMap::new(),
                        health_check: Default::default(),
                        catalog_provider: None,
                        sovereignty: provider.sovereignty.clone(),
                    },
                ))
            } else {
                // OAuth/ADC mode — requires project + region
                let project = config_str_required(config, "project", "Vertex")?;
                let region = config_str_required(config, "region", "Vertex")?;
                let publisher =
                    config_str(config, "publisher").unwrap_or_else(|| "google".to_string());
                let base_url = config_str(config, "base_url");

                // Resolve secrets within credentials
                let mut resolved_config = config.cloned();
                if let Some(ref mut cfg) = resolved_config
                    && let Some(creds) = cfg.get("credentials").cloned()
                {
                    let cred_type = creds
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default");
                    if cred_type == "service_account_json" {
                        let mut creds_obj = creds.as_object().cloned().unwrap_or_default();
                        if let Some(ref_val) = creds.get("json_ref").and_then(|v| v.as_str())
                            && let Ok(Some(resolved)) = resolve_secret(Some(ref_val), secrets).await
                        {
                            creds_obj
                                .insert("json".to_string(), serde_json::Value::String(resolved));
                        }
                        cfg["credentials"] = serde_json::Value::Object(creds_obj);
                    }
                }

                let credentials = parse_gcp_credentials(resolved_config.as_ref())?;

                Ok(ProviderConfig::Vertex(
                    crate::config::VertexProviderConfig {
                        api_key: None,
                        project: Some(project),
                        region: Some(region),
                        publisher,
                        base_url,
                        credentials,
                        timeout_secs: 60,
                        allowed_models: provider.models.clone(),
                        model_aliases: std::collections::HashMap::new(),
                        models: std::collections::HashMap::new(),
                        retry: Default::default(),
                        circuit_breaker: Default::default(),
                        streaming_buffer: Default::default(),
                        fallback_providers: Vec::new(),
                        model_fallbacks: std::collections::HashMap::new(),
                        health_check: Default::default(),
                        catalog_provider: None,
                        sovereignty: provider.sovereignty.clone(),
                    },
                ))
            }
        }
        "test" => Ok(ProviderConfig::Test(crate::config::TestProviderConfig {
            model_name: "test-model".to_string(),
            failure_mode: Default::default(),
            timeout_secs: 60,
            allowed_models: provider.models.clone(),
            model_aliases: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            retry: Default::default(),
            circuit_breaker: Default::default(),
            fallback_providers: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            health_check: Default::default(),
            catalog_provider: None,
            sovereignty: provider.sovereignty.clone(),
        })),
        _ => Err(RoutingError::InvalidScope(format!(
            "Unsupported provider type: {}",
            provider.provider_type
        ))),
    }
}

/// Resolved provider information including source tracking.
pub struct ResolvedProviderInfo {
    pub provider_name: String,
    pub provider_config: ProviderConfig,
    pub model: String,
    /// "static" for config-defined providers, "dynamic" for DB-defined providers
    pub source: &'static str,
}

/// Resolve a routed provider to a concrete provider info struct.
///
/// This is a convenience function that handles both static and dynamic routes,
/// returning the same format for easy use in API handlers.
///
/// For static routes, it directly extracts the information.
/// For dynamic routes, it performs database lookup with caching and secret resolution.
pub async fn resolve_to_provider(
    routed: RoutedProvider<'_>,
    db: Option<&Arc<DbPool>>,
    cache: Option<&Arc<dyn Cache>>,
    secrets: Option<&Arc<dyn SecretManager>>,
    auth: Option<&AuthenticatedRequest>,
) -> Result<ResolvedProviderInfo, RoutingError> {
    match routed {
        RoutedProvider::Static(static_route) => Ok(ResolvedProviderInfo {
            provider_name: static_route.provider_name.to_string(),
            provider_config: static_route.provider_config.clone(),
            model: static_route.model.to_string(),
            source: "static",
        }),
        RoutedProvider::Dynamic(dynamic_route) => {
            // Resolve dynamic provider from database (with caching and secret resolution)
            let db = db.ok_or_else(|| {
                RoutingError::InvalidScope("Database required for dynamic providers".to_string())
            })?;

            let resolved =
                resolve_dynamic_provider(&dynamic_route, db, cache, secrets, auth).await?;

            Ok(ResolvedProviderInfo {
                provider_name: resolved.provider_name,
                provider_config: resolved.provider_config,
                model: resolved.model,
                source: "dynamic",
            })
        }
    }
}
