use std::sync::Arc;

use crate::{config, providers};

/// Create a provider instance from a ProviderConfig.
///
/// This is a general-purpose helper for instantiating providers, used by:
/// - Re-ranker initialization (via `AppState::create_reranker_provider`)
/// - Provider health checker
///
/// Returns an error message if the provider type is not supported.
pub(crate) fn create_provider_instance(
    provider_config: &config::ProviderConfig,
    provider_name: &str,
    circuit_breakers: &providers::CircuitBreakerRegistry,
) -> Result<Arc<dyn providers::Provider>, String> {
    let provider: Arc<dyn providers::Provider> = match provider_config {
        config::ProviderConfig::OpenAi(cfg) => Arc::new(
            providers::open_ai::OpenAICompatibleProvider::from_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        config::ProviderConfig::Anthropic(cfg) => Arc::new(
            providers::anthropic::AnthropicProvider::from_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        #[cfg(feature = "provider-azure")]
        config::ProviderConfig::AzureOpenAi(cfg) => Arc::new(
            providers::azure_openai::AzureOpenAIProvider::from_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        #[cfg(feature = "provider-vertex")]
        config::ProviderConfig::Vertex(cfg) => Arc::new(
            providers::vertex::VertexProvider::from_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        #[cfg(feature = "provider-vertex")]
        config::ProviderConfig::Gemini(cfg) => Arc::new(
            providers::vertex::VertexProvider::from_gemini_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        #[cfg(feature = "provider-bedrock")]
        config::ProviderConfig::Bedrock(cfg) => Arc::new(
            providers::bedrock::BedrockProvider::from_config_with_registry(
                cfg,
                provider_name,
                circuit_breakers,
            ),
        ),
        config::ProviderConfig::Test(cfg) => {
            Arc::new(providers::test::TestProvider::from_config(cfg))
        }
    };

    Ok(provider)
}

/// Initialize a secret manager from the config.
///
/// Used by `run_bootstrap` (CLI mode) to initialize a secret manager from config.
#[cfg(feature = "sso")]
use crate::secrets;

#[cfg(feature = "sso")]
pub(crate) async fn init_secret_manager(
    config: &config::GatewayConfig,
) -> Result<Arc<dyn secrets::SecretManager>, String> {
    match &config.secrets {
        config::SecretsConfig::None | config::SecretsConfig::Env => {
            Ok(Arc::new(secrets::MemorySecretManager::new()))
        }
        #[cfg(feature = "vault")]
        config::SecretsConfig::Vault(vault_config) => {
            use config::VaultAuth;
            use secrets::SecretManager;

            let vault_cfg = match &vault_config.auth {
                VaultAuth::Token { token } => {
                    secrets::VaultConfig::new(&vault_config.address, token)
                }
                VaultAuth::AppRole {
                    role_id,
                    secret_id,
                    auth_mount,
                } => secrets::VaultConfig::with_approle(
                    &vault_config.address,
                    role_id,
                    secret_id,
                )
                .with_auth_mount(auth_mount),
                VaultAuth::Kubernetes {
                    role,
                    token_path,
                    auth_mount,
                } => {
                    let jwt = std::fs::read_to_string(token_path).map_err(|e| {
                        format!(
                            "Failed to read Kubernetes ServiceAccount token from '{token_path}': {e}"
                        )
                    })?;
                    secrets::VaultConfig::with_kubernetes(
                        &vault_config.address,
                        role,
                        jwt.trim(),
                    )
                    .with_auth_mount(auth_mount)
                }
            }
            .with_mount(&vault_config.mount)
            .with_path_prefix(&vault_config.path_prefix);

            let manager = secrets::VaultSecretManager::new(vault_cfg)
                .await
                .map_err(|e| format!("Failed to create Vault client: {e}"))?;

            manager
                .health_check()
                .await
                .map_err(|e| format!("Vault health check failed: {e}"))?;

            Ok(Arc::new(manager))
        }
        #[cfg(feature = "secrets-aws")]
        config::SecretsConfig::Aws(aws_config) => {
            use secrets::SecretManager;

            let mut cfg = match &aws_config.region {
                Some(region) => secrets::AwsSecretsManagerConfig::new(region),
                None => secrets::AwsSecretsManagerConfig::from_env(),
            }
            .with_prefix(&aws_config.prefix);

            if let Some(endpoint_url) = &aws_config.endpoint_url {
                cfg = cfg.with_endpoint_url(endpoint_url);
            }

            let manager = secrets::AwsSecretsManager::new(cfg)
                .await
                .map_err(|e| format!("Failed to create AWS Secrets Manager client: {e}"))?;

            manager
                .health_check()
                .await
                .map_err(|e| format!("AWS Secrets Manager health check failed: {e}"))?;

            Ok(Arc::new(manager))
        }
        #[cfg(feature = "secrets-azure")]
        config::SecretsConfig::Azure(azure_config) => {
            use secrets::SecretManager;

            let cfg = secrets::AzureKeyVaultConfig::new(&azure_config.vault_url)
                .with_prefix(&azure_config.prefix);

            let manager = secrets::AzureKeyVaultManager::new(cfg)
                .await
                .map_err(|e| format!("Failed to create Azure Key Vault client: {e}"))?;

            manager
                .health_check()
                .await
                .map_err(|e| format!("Azure Key Vault health check failed: {e}"))?;

            Ok(Arc::new(manager))
        }
        #[cfg(feature = "secrets-gcp")]
        config::SecretsConfig::Gcp(gcp_config) => {
            use secrets::SecretManager;

            let cfg = secrets::GcpSecretManagerConfig::new(&gcp_config.project_id)
                .with_prefix(&gcp_config.prefix);

            let manager = secrets::GcpSecretManager::new(cfg)
                .await
                .map_err(|e| format!("Failed to create GCP Secret Manager client: {e}"))?;

            manager
                .health_check()
                .await
                .map_err(|e| format!("GCP Secret Manager health check failed: {e}"))?;

            Ok(Arc::new(manager))
        }
    }
}

/// Initialize embedding service and vector store for the worker.
#[cfg(any(
    feature = "document-extraction-basic",
    feature = "document-extraction-full"
))]
use crate::cache;

#[cfg(any(
    feature = "document-extraction-basic",
    feature = "document-extraction-full"
))]
pub(crate) async fn init_worker_embedding_service(
    config: &config::GatewayConfig,
    db: Arc<crate::db::DbPool>,
) -> (
    Option<Arc<cache::EmbeddingService>>,
    Option<Arc<dyn cache::vector_store::VectorBackend>>,
) {
    #[cfg(not(feature = "database-postgres"))]
    let _ = &db;
    // Get embedding configuration (same priority as init_file_search_service)
    let file_search_config = match &config.features.file_search {
        Some(cfg) if cfg.enabled => cfg,
        _ => {
            tracing::warn!("File search not configured, chunks will not have embeddings");
            return (None, None);
        }
    };

    let embedding_config = file_search_config.embedding.as_ref().or_else(|| {
        config
            .features
            .response_caching
            .as_ref()
            .and_then(|rc| rc.semantic.as_ref())
            .map(|sc| &sc.embedding)
    });

    let embedding_config = match embedding_config {
        Some(cfg) => cfg,
        None => {
            tracing::warn!("No embedding configuration found, chunks will not have embeddings");
            return (None, None);
        }
    };

    // Get the embedding provider configuration
    let provider_config = match config.providers.get(&embedding_config.provider) {
        Some(cfg) => cfg,
        None => {
            tracing::error!(
                provider = %embedding_config.provider,
                "Embedding provider '{}' not configured",
                embedding_config.provider
            );
            return (None, None);
        }
    };

    // Create circuit breaker registry (empty for worker)
    let circuit_breakers = providers::CircuitBreakerRegistry::new();

    // Create HTTP client
    let http_client = reqwest::Client::new();

    // Create embedding service
    let embedding_service = match cache::EmbeddingService::new(
        embedding_config,
        provider_config,
        &circuit_breakers,
        http_client,
    ) {
        Ok(service) => Arc::new(service),
        Err(e) => {
            tracing::error!(error = %e, "Failed to create embedding service");
            return (None, None);
        }
    };

    // Create vector store
    let vector_store: Arc<dyn cache::vector_store::VectorBackend> = if let Some(rag_backend) =
        &file_search_config.vector_backend
    {
        match rag_backend {
            #[cfg(feature = "database-postgres")]
            config::RagVectorBackend::Pgvector {
                table_name,
                index_type,
                distance_metric,
            } => {
                let pg_pool = match db.pg_write_pool() {
                    Some(pool) => pool.clone(),
                    None => {
                        tracing::error!("pgvector requires PostgreSQL database");
                        return (Some(embedding_service), None);
                    }
                };

                let store = cache::vector_store::PgvectorStore::new(
                    pg_pool,
                    format!("{}_semantic", table_name.trim_end_matches("_chunks")),
                    embedding_config.dimensions,
                    index_type.clone(),
                    *distance_metric,
                );

                if let Err(e) = store.initialize().await {
                    tracing::error!(error = %e, "Failed to initialize pgvector store");
                    return (Some(embedding_service), None);
                }

                Arc::new(store)
            }
            #[cfg(not(feature = "database-postgres"))]
            config::RagVectorBackend::Pgvector { .. } => {
                tracing::error!(
                    "pgvector requires the 'database-postgres' feature. \
                         Rebuild with --features database-postgres or use a different vector backend."
                );
                return (Some(embedding_service), None);
            }
            config::RagVectorBackend::Qdrant {
                url,
                api_key,
                qdrant_collection_name,
                distance_metric,
            } => {
                let store = cache::vector_store::QdrantStore::new(
                    url.clone(),
                    api_key.clone(),
                    qdrant_collection_name.clone(),
                    embedding_config.dimensions,
                    *distance_metric,
                );

                if let Err(e) = store.initialize().await {
                    tracing::error!(error = %e, "Failed to initialize Qdrant store");
                    return (Some(embedding_service), None);
                }

                Arc::new(store)
            }
        }
    } else {
        // Default to pgvector
        #[cfg(not(feature = "database-postgres"))]
        {
            tracing::warn!(
                "No vector store configured and the 'database-postgres' feature is not enabled. \
                     Configure [features.file_search.vector_backend] or rebuild with --features database-postgres."
            );
            return (Some(embedding_service), None);
        }

        #[cfg(feature = "database-postgres")]
        {
            let pg_pool = match db.pg_write_pool() {
                Some(pool) => pool.clone(),
                None => {
                    tracing::warn!("No vector store configured and not using PostgreSQL");
                    return (Some(embedding_service), None);
                }
            };

            let store = cache::vector_store::PgvectorStore::new(
                pg_pool,
                "rag".to_string(),
                embedding_config.dimensions,
                config::PgvectorIndexType::IvfFlat,
                config::DistanceMetric::default(), // Cosine (default)
            );

            if let Err(e) = store.initialize().await {
                tracing::error!(error = %e, "Failed to initialize pgvector store");
                return (Some(embedding_service), None);
            }

            Arc::new(store)
        }
    };

    (Some(embedding_service), Some(vector_store))
}
